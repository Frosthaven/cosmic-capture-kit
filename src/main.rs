// DRAGON-233 fix 1: link the Windows binary as a GUI-subsystem app so a launch from
// Start Menu / the komorebi hotkey / PrintScreen never pops (or flashes) a console
// window. CLI output for terminal launches is restored explicitly via
// `platform::windows::console::attach_parent_console` (called first-thing in `main`).
// A crate-level inner attribute, so it sits above every item; a no-op off Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

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
#[cfg(target_os = "macos")]
#[path = "platform/mac/screenshot.rs"]
mod screenshot;
// Windows still-grab layer (DRAGON-229): the mac arm above was `not(linux)` and
// resolved to ScreenCaptureKit/AppKit code that cannot build on Windows, so it is
// narrowed to `macos` and Windows mounts its own API-compatible module (M0 stubs,
// the M1 capture backend fills it in). Same public surface as the mac/Linux modules.
#[cfg(target_os = "windows")]
#[path = "platform/windows/screenshot.rs"]
mod screenshot;
mod compose;
mod decoration;
mod glass;
mod instance;
mod media;
mod wallpaper;
mod detect;
mod geometry;
// Freehand pen-stroke beautification (DRAGON-342): the pure smoothing / pseudo-pressure /
// ribbon-outline math the preview's canvas AND its full-res bake both render through. Sits at
// the crate root (next to `geometry`) because it is shared by `app::preview::annotate` and
// `widgets::annotation_canvas` and belongs to neither.
mod pen_stroke;
// The sequence badge's geometry / numerals / ink rule (DRAGON-340) — the same shared-by-both-
// renderers role `pen_stroke` plays for the pencil, and for the same reason: the live canvas
// and the full-resolution bake must build the badge from ONE set of pure functions.
mod badge;
// The arrow's curve geometry (DRAGON-470) — the third, bend handle's model plus every quantity
// derived from it (control point, end tangents, arc length, hit polyline, bounds). Same shared
// role again: the live canvas and the full-resolution bake must draw ONE parabola, so neither
// owns the maths.
mod arrow_curve;
mod platform;
mod widgets;
mod encode;
mod selection;
mod share;
mod shortcuts;
mod state;
// The startup self-exit guard (DRAGON-413): a detached budget clock that ends a capture
// child which never presents anything, so a startup stall can no longer pile up
// invisible processes. Armed on macOS only (see the module doc for why, and for the
// permission-window suspension); compiled elsewhere ONLY under `cfg(test)` so its pure
// decision logic is covered by the Linux suite while Linux/Windows behaviour stays
// byte-identical.
#[cfg(any(target_os = "macos", test))]
mod startup_guard;
// System-tray recording controls: ksni/StatusNotifierItem (D-Bus) on Linux; a menu-bar
// NSStatusItem + NSMenu on macOS (DRAGON-159); a no-op stub on any other platform. All
// three expose the same `TraySession` seam so the app's tray handling is platform-free.
#[cfg(target_os = "linux")]
#[path = "platform/linux/tray.rs"]
mod tray;
#[cfg(target_os = "macos")]
#[path = "platform/mac/tray.rs"]
mod tray;
// Windows (DRAGON-237): the resident tray daemon's IN-RECORDING relay lives here — a real
// `TraySession` (mounted like the mac/linux one) that connects to the daemon's named pipe
// and reflects the recording state into the daemon's tray icon. `start_recording` (the
// own-icon path) returns None, so a non-daemon recording keeps its in-frame toolbar exactly
// as the stub did. Narrows the no-op stub below to the truly resident-less platforms.
#[cfg(target_os = "windows")]
#[path = "platform/windows/tray.rs"]
mod tray;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[path = "platform/tray_stub.rs"]
mod tray;
mod util;
// DRAGON-419: the opt-in cross-platform debug log — the ONE logging mechanism. Installs the
// process logger, resolves the per-platform log folder, owns the failure vocabulary every
// silent-exit path is classified with, and states the privacy rule every new line must obey.
// Read its module doc before adding a log line anywhere.
mod diag;
mod cli;
// Cloud accounts (DRAGON-482): the provider registry, the account store, the platform
// keyring seam, and the curl transport that keeps every token out of
// argv. Portable and always compiled: the accounts a user connects
// belong to the app rather than to any one OS, and only the KEYRING body
// behind `cloud::secrets` is per-platform. Its own module doc explains the staging (this is
// the foundation; the OAuth, provider and upload modules are the mount points later stages
// fill), so no shared file has to change again as the feature lands.
mod cloud;
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
// The resident tray DAEMON (Windows, DRAGON-237): the full-parity port of the mac
// menu-bar daemon — a tiny Win32-only process (NO iced/wgpu) owning a `Shell_NotifyIcon`
// tray item with the same three-state icon + menu, a `RegisterHotKey` global capture
// hotkey, and one-shot capture children. A bare `resident` launch early-branches here
// (see `main`). cfg(windows) inside the module keeps Linux/mac clean.
#[cfg(target_os = "windows")]
#[path = "platform/windows/daemon.rs"]
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
// Windows joins the IPC (DRAGON-237): the wire protocol (`RecordingState`/`Command`
// encode/parse) is pure and portable; only the transport differs (Windows named pipe vs
// the unix socket). The child relay + daemon both speak it, so the surfaces can't drift.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[path = "platform/daemon_ipc.rs"]
mod daemon_ipc;
// Preview HANDOFF IPC (DRAGON-336, multi-document preview): a one-shot capture child
// hands its finished file to an ALREADY RUNNING preview host (discovered through the
// per-pid preview marker in `instance.rs`, reached over that host's per-pid unix socket)
// instead of paying a second process's ~233 MB launch overhead. The transport + discovery
// live here; how the App binds, drains and falls back is documented in the module doc.
// `daemon_ipc`'s sibling, same runtime-dir/plain-words conventions.
#[path = "platform/preview_ipc.rs"]
mod preview_ipc;
// The REGION / MONITOR half of the capture-extras behaviour matrix (DRAGON-463). One
// shared test module, deliberately not per-platform: `crate::screenshot` is a `#[path]`
// mount and `region_windows_frozen` has the same signature everywhere, so these rows
// assert real output PIXELS against whichever implementation the host platform ships.
// Gated to the three real platforms because the `screenshot` mount only exists there.
#[cfg(all(test, any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod region_matrix_tests;
// The SINGLE-WINDOW half of the same matrix (DRAGON-463). It drives the shared
// `decoration` seam directly (decorate / backdrop_for / apply_backdrop / cursor_offset),
// which every platform now assembles its window capture from, so one module covers all of
// them and a new compositor RUNS these rows rather than rewriting them.
#[cfg(test)]
mod decoration_matrix_tests;

/// Install a macOS crash logger: chain a panic hook that appends the panic
/// message + a backtrace to `~/Library/Logs/cosmic-capture-kit/panic.log`
/// before delegating to the default hook (so terminal runs still print).
/// The app runs bundled (no terminal) in resident mode, where AppKit/winit
/// panics like the stop→preview `frame_did_change` crash would otherwise be
/// invisible. Kept tiny: last-writer-wins single file, best-effort I/O.
///
/// DRAGON-415: additionally, a panic on the MAIN thread is the one way this app can
/// "just close itself" with no message at all — `app::run` ends in `libc::_exit`, there is
/// no crash dialog for a bundled accessory app, and the panic text goes to a stderr no
/// packaged mac build has. So the hook also puts up the failure alert before the process
/// dies. It says only what we know (it stopped unexpectedly, nothing was saved, here is
/// where the details went); it does not diagnose.
///
/// MAIN THREAD ONLY, deliberately. Showing it from a worker would mean dispatching
/// synchronously onto the main thread — which, when the main thread is blocked waiting on
/// that very worker (the capture oneshot), deadlocks the child forever: precisely the
/// invisible-immortal-process failure this work exists to remove. A worker panic already
/// reports itself through `ShotOutcome::WorkerDied` on the seam that awaits it.
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
        // The log is written FIRST, so the breadcrumb exists even if presenting the alert
        // goes wrong. The alert's own guards make this at most one dialog per process.
        if objc2_foundation::MainThreadMarker::new().is_some() {
            let msg = app::failure::crash_alert();
            platform::mac::alert::show(&msg.title, &msg.body);
        }
        default_hook(info);
    }));
}

/// Install a Windows crash logger (DRAGON-276): chain a panic hook that appends the panic
/// message + a backtrace to `~/.config/cosmic-capture-kit/panic.log` before delegating to the
/// default hook. A shortcut / daemon-spawned capture launch has NO console (GUI subsystem), so
/// a winit/wgpu panic on the stop→preview path — reported as a silent crash — would otherwise
/// leave no trace. Kept tiny: append to one file, best-effort I/O, backtrace forced on.
///
/// DRAGON-442: it also puts the failure alert up now. Writing `panic.log` fixed the half of
/// the problem that faces US; the user still watched the app vanish. There is no console on a
/// shortcut launch, no crash dialog for a GUI-subsystem process that unwinds, and
/// `app::run` ends in `libc::_exit` — so a panic was the one way this app could close itself
/// with nothing said at all.
///
/// MAIN THREAD ONLY, like the mac twin, but for a DIFFERENT reason worth stating because the
/// mac one does not apply here. Mac gates to avoid a deadlock: showing from a worker means
/// dispatching synchronously onto a main thread that may be blocked waiting on that very
/// worker. Windows has no such risk — `alert::show` spawns its own presenting thread and
/// returns at once. The reason here is that a worker panic is usually RECOVERED: the capture
/// worker's panic drops its oneshot sender, the app reads that as `ShotOutcome::WorkerDied`,
/// and reports it properly through `fail_session`. Two things would go wrong if we spoke from
/// a worker. The dialog would be the wrong one (a crash box for a failure the app is about to
/// explain in its own words), and — worse — blocking the panicking thread here delays its
/// unwind, so the sender is not dropped, so the app's own recovery is stalled for the whole
/// budget. A recovered panic must stay recovered.
///
/// The main thread is identified by the `ThreadId` captured at install time, which runs on it
/// in `main`. That is exact, needs no Win32, and cannot drift.
/// The panic alert's message, with both real folders resolved (DRAGON-442).
///
/// The two logs live in DIFFERENT places on Windows — `panic.log` in the app config dir and
/// the DRAGON-419 debug log under `%LOCALAPPDATA%` — so both are named. The copy itself is
/// `app::failure::windows_crash_alert`, which is pure and unit-tested; this only resolves the
/// paths, and hands `None` for either one that cannot be resolved.
#[cfg(windows)]
fn windows_crash_alert_message() -> app::failure::AlertMessage {
    let panic_dir = util::app_config_dir().map(|d| d.display().to_string());
    let debug_dir = diag::log_dir().map(|d| d.display().to_string());
    app::failure::windows_crash_alert(panic_dir.as_deref(), debug_dir.as_deref())
}

/// DRAGON-442 QA hook (Windows only, inert unless set). A crash is not something you can
/// ask a machine for, so the alert this ticket adds has no other way to be checked by hand.
///
/// * `CCK_TEST_PANIC=1` panics on the MAIN thread — the alert should appear, and the process
///   should not die until it is dismissed or the 120s budget elapses.
/// * `CCK_TEST_PANIC=worker` panics on a worker thread and joins it, which is a RECOVERED
///   panic. No box may appear: that is the main-thread gate doing its job. `panic.log` still
///   gets both, so the difference is visible.
///
/// A review aid in the same spirit as `CCK_TEST_FORCE_NO_OUTPUTS` and
/// `CCK_STARTUP_BUDGET_SECS`, not a supported setting: read once, here, and nowhere else.
/// Called straight after the hook is installed, so both shapes exercise the real hook.
#[cfg(windows)]
fn maybe_forced_panic() {
    const ENV: &str = "CCK_TEST_PANIC";
    let Some(mode) = std::env::var_os(ENV) else { return };
    match mode.to_string_lossy().as_ref() {
        "1" => {
            log::warn!("{ENV}=1: panicking on the main thread to exercise the crash alert");
            panic!("{ENV}=1: forced main-thread panic (DRAGON-442 QA hook)");
        }
        "worker" => {
            log::warn!(
                "{ENV}=worker: panicking on a worker thread; the panic is RECOVERED by the \
                 join below, so NO alert should appear"
            );
            let h = std::thread::Builder::new()
                .name("cck-test-panic".into())
                .spawn(|| panic!("{ENV}=worker: forced worker panic (DRAGON-442 QA hook)"))
                .expect("spawn the forced-panic thread");
            let _ = h.join();
            log::warn!("{ENV}=worker: the worker panic was recovered; continuing");
        }
        other => log::warn!("{ENV}={other}: not a recognised mode (use 1 or worker); ignored"),
    }
}

#[cfg(windows)]
fn install_windows_panic_hook() {
    let default_hook = std::panic::take_hook();
    let main_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = crate::util::app_config_dir()
            && std::fs::create_dir_all(&dir).is_ok()
        {
            use std::io::Write as _;
            let bt = std::backtrace::Backtrace::force_capture();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let thread = std::thread::current();
            let tname = thread.name().unwrap_or("<unnamed>").to_string();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = writeln!(
                    f,
                    "\n===== panic @ unix {ts} (thread {tname}) =====\n{info}\n{bt}"
                );
            }
        }
        // The log is written FIRST, so the breadcrumb exists even if presenting the alert
        // goes wrong.
        //
        // Only from the main thread (see the fn doc). `show` returns `None` when this
        // session already put an alert up — a session that reported a failure and THEN
        // panicked gets one box, not two, which is the DRAGON-436 latch doing its job — and
        // then there is nothing to wait for.
        if std::thread::current().id() == main_thread
            && let msg = windows_crash_alert_message()
            && let Some(dismissal) = platform::windows::alert::show(&msg.title, &msg.body)
        {
            // Blocking the thread that owns our windows is normally forbidden (DWM ghosts
            // them after ~5s). Here it is right: the process is unwinding to its death, those
            // windows have no future to protect, and the alternative is the box vanishing
            // before it can be read. `wait` also arms the alert module's own process
            // backstop, so this can never become a permanent hang.
            let outcome = dismissal.wait(app::failure::ALERT_DISMISS_BUDGET);
            log::warn!("DRAGON-442: crash alert ended as {outcome:?}");
        }
        default_hook(info);
    }));
}

// DRAGON-336: a settings-only launch USED to force iced's software renderer
// (`ICED_BACKEND=tiny-skia`), which skipped the whole GPU stack and measured
// RSS 239.0 -> 83.3 MB / PSS 84.1 -> 56.6 MB on a dual-GPU rig. REMOVED after
// testing — the premise was wrong. The settings window is not the small, STATIC
// chrome the saving assumed: the microphone test is a live, continuously
// redrawing level meter, which is the worst case for a CPU rasterizer and
// starves the audio work it is supposed to be visualising. The window is also
// freely resizable, and software rasterization cost scales with pixel count.
//
// Do not reintroduce this without re-testing the mic meter specifically. The
// same reasoning already rules out the other two surfaces: the preview draws
// through `cosmic::iced::widget::shader` (`app/preview/layers.rs`,
// `widgets/annotation_fx.rs`), which tiny-skia cannot render at all and which
// tested as too laggy regardless; and the capture overlay is fullscreen, where
// the CPU framebuffer (~59 MB here) costs more than the GPU stack it replaces.
// `ICED_BACKEND` is still honoured from the environment if you want to
// experiment — nothing in-process sets it any more.

/// Env vars through which the user (or a launcher script) has already expressed
/// a GPU / Vulkan-driver choice. If ANY of them is set we keep our hands off the
/// loader entirely — their intent outranks our memory optimization.
#[cfg(target_os = "linux")]
const VULKAN_SELECTION_ENV: [&str; 10] = [
    "VK_LOADER_DRIVERS_SELECT",
    "VK_LOADER_DRIVERS_DISABLE",
    "VK_DRIVER_FILES",
    "VK_ADD_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "MESA_VK_DEVICE_SELECT",
    "DRI_PRIME",
    "__NV_PRIME_RENDER_OFFLOAD",
    "WGPU_ADAPTER_NAME",
    "WGPU_POWER_PREF",
];

/// The first name in `names` that `is_set` reports as present. Split out from
/// [`pin_vulkan_icd`] so the guard list is unit-testable without touching the
/// real environment.
#[cfg(target_os = "linux")]
fn first_set_env<'a>(names: &[&'a str], is_set: impl Fn(&str) -> bool) -> Option<&'a str> {
    names.iter().copied().find(|n| is_set(n))
}

/// DRAGON-336 (Linux, memory): pin the Vulkan loader to the ICD of the GPU this
/// session actually renders on.
///
/// The loader initializes EVERY installed ICD just to enumerate devices. On a
/// dual-vendor box that means Mesa's `libvulkan_radeon` + `libLLVM` (32.5 MB RSS
/// / 8.6 MB PSS measured) stay resident purely to describe a GPU we never draw
/// on. Naming the one driver we want in `VK_LOADER_DRIVERS_SELECT` took
/// `--settings` from RSS 239.0 -> 204.2 MB and PSS 84.1 -> 75.7 MB.
///
/// This is deliberately SELF-DISABLING — every step below must positively
/// succeed or we do nothing at all, because a wrong pin is far worse than the
/// memory it saves:
/// 1. nothing in [`VULKAN_SELECTION_ENV`] is already set;
/// 2. at least two ICD manifests are installed (otherwise there is nothing to
///    exclude, and we skip the compositor round-trip entirely);
/// 3. the compositor positively names the device it wants clients to render on
///    (the `zwp_linux_dmabuf_v1` default-feedback `main_device`) — no Wayland,
///    or a compositor too old for feedback, means no pin;
/// 4. that device resolves through sysfs to a known DRM kernel driver;
/// 5. that driver is one we can only ever see as a DEDICATED GPU
///    ([`dedicated_icd_families`]);
/// 6. the resulting selection keeps at least one installed manifest and drops at
///    least one ([`select_icds`]) — the loader must never end up with an empty
///    driver set.
///
/// `VK_LOADER_DRIVERS_SELECT` is also the SAFE knob of the two: an older loader
/// that doesn't know it simply ignores it (we just don't save the memory),
/// whereas `VK_ICD_FILENAMES`/`VK_DRIVER_FILES` would REPLACE the search path.
#[cfg(target_os = "linux")]
fn pin_vulkan_icd() {
    if let Some(var) = first_set_env(&VULKAN_SELECTION_ENV, |n| std::env::var_os(n).is_some()) {
        log::debug!("vulkan ICD pin: skipped, {var} is already set");
        return;
    }
    let installed = installed_vulkan_icds();
    if installed.len() < 2 {
        return; // one (or no) driver: nothing to exclude, don't even ask the compositor
    }
    let Some(dev) = compositor_main_device() else { return };
    let Some(driver) = drm_driver_for_dev(dev) else { return };
    let Some(families) = dedicated_icd_families(&driver) else { return };
    let Some(keep) = select_icds(&installed, families) else { return };
    log::debug!("vulkan ICD pin: session renders on {driver}; VK_LOADER_DRIVERS_SELECT={keep}");
    // SAFETY: `set_var` is unsound only if another thread may touch the environment
    // concurrently. This runs on the main thread inside `main`, before `app::run`
    // builds the iced/winit runtime (and therefore before the Vulkan loader is ever
    // touched) and before any capture/audio thread is spawned — everything above it
    // (argument parsing, `state::load`, the instance locks) is straight-line
    // single-threaded code, so the process is still effectively single-threaded.
    unsafe { std::env::set_var("VK_LOADER_DRIVERS_SELECT", keep) };
}

/// Every directory the Vulkan loader scans for ICD manifests, in its own
/// precedence order (`XDG_DATA_HOME`, then each `XDG_DATA_DIRS` entry, then
/// `/etc`). We enumerate the same set the loader does so "is there more than one
/// driver installed?" is answered honestly.
#[cfg(target_os = "linux")]
fn vulkan_icd_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut raw: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        raw.push(home.join("vulkan/icd.d"));
    }
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let dirs = if dirs.is_empty() { "/usr/local/share:/usr/share".to_string() } else { dirs };
    for d in dirs.split(':').filter(|s| !s.is_empty()) {
        raw.push(PathBuf::from(d).join("vulkan/icd.d"));
    }
    raw.push(PathBuf::from("/etc/vulkan/icd.d"));
    let mut out: Vec<PathBuf> = Vec::with_capacity(raw.len());
    for d in raw {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// The installed ICD manifest FILE NAMES (sorted, de-duplicated). Names, not
/// paths: `VK_LOADER_DRIVERS_SELECT` globs match the manifest's file name, and a
/// manifest in a higher-precedence directory shadows a same-named one below it.
#[cfg(target_os = "linux")]
fn installed_vulkan_icds() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in vulkan_icd_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".json") && !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names.sort();
    names
}

/// A DRM kernel driver -> the ICD manifest name PREFIXES belonging to it (one
/// driver can ship several manifests: per-architecture `radeon_icd.x86_64.json`
/// / `radeon_icd.i686.json`, or two Vulkan implementations for the same kernel
/// driver).
///
/// ONLY drivers that can never be anything but a DEDICATED (discrete) GPU are
/// listed, and that is the entire safety argument for the pin: `iced_wgpu` asks
/// wgpu for `PowerPreference::HighPerformance`, which ranks `DiscreteGpu` above
/// every other device type, so pinning a dedicated GPU cannot move our rendering
/// onto a LESSER adapter than the one we would have picked anyway. `amdgpu`,
/// `i915` and `xe` are deliberately ABSENT: each of them can be an integrated
/// GPU sitting beside a discrete one, exactly the case where the compositor's
/// main device and wgpu's preference legitimately disagree — there we no-op
/// rather than guess. Adding a driver here means arguing that same point for it.
#[cfg(target_os = "linux")]
fn dedicated_icd_families(driver: &str) -> Option<&'static [&'static str]> {
    match driver {
        // NVIDIA proprietary: `nvidia_icd.json`.
        "nvidia" => Some(&["nvidia_icd"]),
        // Nouveau's Vulkan driver is Mesa NVK: `nouveau_icd.x86_64.json`.
        "nouveau" => Some(&["nouveau_icd"]),
        _ => None,
    }
}

/// Build the `VK_LOADER_DRIVERS_SELECT` value: every installed manifest whose
/// name belongs to `families`, comma-joined (the loader takes a comma-delimited
/// list of globs matched against manifest file names).
///
/// `None` when the pin would be pointless or dangerous: nothing matched (we'd
/// leave the loader with NO drivers) or everything matched (nothing to exclude).
#[cfg(target_os = "linux")]
fn select_icds(installed: &[String], families: &[&str]) -> Option<String> {
    let keep: Vec<&str> = installed
        .iter()
        .map(String::as_str)
        .filter(|n| families.iter().any(|f| n.starts_with(f)))
        .collect();
    if keep.is_empty() || keep.len() == installed.len() {
        return None;
    }
    Some(keep.join(","))
}

/// Split a Linux `dev_t` into `(major, minor)` — the kernel's split encoding,
/// transcribed from glibc's `major(3)`/`minor(3)` (both halves truncate to 32
/// bits, which is what makes the low/high pieces line up).
#[cfg(target_os = "linux")]
fn major_minor(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | (((dev >> 32) & !0xfff) & 0xffff_ffff);
    let minor = (dev & 0xff) | (((dev >> 12) & !0xff) & 0xffff_ffff);
    (major as u32, minor as u32)
}

/// Pull `DRIVER=<name>` out of a sysfs `uevent` file's contents.
#[cfg(target_os = "linux")]
fn driver_from_uevent(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix("DRIVER="))
        .map(str::trim)
        .filter(|d| !d.is_empty())
}

/// Resolve a DRM device number to its kernel driver name. Works for either node
/// type — the protocol explicitly leaves primary-vs-render unspecified, and both
/// character devices hang off the same physical device in sysfs, so
/// `/sys/dev/char/<major>:<minor>/device/uevent` answers for both.
///
/// `pub(crate)` since DRAGON-425: the zero-copy recorder's pre-flight asks the same two
/// questions this file's Vulkan pin does — which GPU is the session on, and what drives it.
#[cfg(target_os = "linux")]
pub(crate) fn drm_driver_for_dev(dev: u64) -> Option<String> {
    let (major, minor) = major_minor(dev);
    let uevent =
        std::fs::read_to_string(format!("/sys/dev/char/{major}:{minor}/device/uevent")).ok()?;
    driver_from_uevent(&uevent).map(str::to_owned)
}

/// Decode a Wayland `array` argument carrying a `dev_t` (native byte order).
#[cfg(target_os = "linux")]
fn dev_t_from_bytes(bytes: &[u8]) -> Option<u64> {
    match bytes.len() {
        8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
        4 => Some(u64::from(u32::from_ne_bytes(bytes.try_into().ok()?))),
        _ => None,
    }
}

/// Ask the compositor which DRM device it wants clients to render on: the
/// `main_device` of the default `zwp_linux_dmabuf_v1` feedback. This is the ONE
/// authoritative, protocol-defined answer to "which GPU is this session on" —
/// everything else (connected connectors, `boot_vga`, render-node numbering) is
/// a guess, and on a multi-GPU box the guesses disagree with each other.
///
/// A short-lived private connection: bind, one default-feedback object, bounded
/// round-trips (each returns on its own `wl_display.sync` callback), teardown.
/// Any failure — no Wayland socket, no dmabuf global, a compositor below
/// version 4 — returns `None`, and the caller then does nothing. `main_device`
/// is gone from version 6 on (clients read the sampling tranche instead), so we
/// bind the 4..=5 window that still sends it.
///
/// `pub(crate)` since DRAGON-425. It is NOT cached anywhere: the Vulkan pin below is
/// self-disabling and skips this round-trip entirely on a single-ICD box, so there is no
/// startup value to read. The zero-copy pre-flight therefore asks again, once per recording
/// — a short-lived private connection with bounded round-trips, paid only when a recording
/// starts.
#[cfg(target_os = "linux")]
pub(crate) fn compositor_main_device() -> Option<u64> {
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
        self, ZwpLinuxDmabufFeedbackV1,
    };
    use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
        self, ZwpLinuxDmabufV1,
    };

    /// Scratch state for the one-shot probe: the device the compositor named,
    /// plus whether the feedback batch has been fully delivered.
    #[derive(Default)]
    struct Probe {
        device: Option<u64>,
        done: bool,
    }
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    impl Dispatch<ZwpLinuxDmabufV1, ()> for Probe {
        fn event(
            _: &mut Self,
            _: &ZwpLinuxDmabufV1,
            _: zwp_linux_dmabuf_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for Probe {
        fn event(
            state: &mut Self,
            _: &ZwpLinuxDmabufFeedbackV1,
            event: zwp_linux_dmabuf_feedback_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // The format-table event carries an fd; ignoring the event drops it.
            match event {
                zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                    state.device = dev_t_from_bytes(&device);
                }
                zwp_linux_dmabuf_feedback_v1::Event::Done => state.done = true,
                _ => {}
            }
        }
    }

    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Probe>(&conn).ok()?;
    let qh = queue.handle();
    let dmabuf: ZwpLinuxDmabufV1 = globals.bind(&qh, 4..=5, ()).ok()?;
    let feedback = dmabuf.get_default_feedback(&qh, ());
    let mut probe = Probe::default();
    for _ in 0..3 {
        if queue.roundtrip(&mut probe).is_err() {
            break;
        }
        if probe.done || probe.device.is_some() {
            break;
        }
    }
    feedback.destroy();
    dmabuf.destroy();
    probe.device
}

fn main() -> cosmic::iced::Result {
    // DRAGON-229: make the process Per-Monitor-Aware-V2 BEFORE any screen measurement
    // (the CLI `--test` diagnostics + capture run before winit's own DPI-aware call, so
    // we can't rely on it). Must be the first statement; idempotent w.r.t. winit's later
    // Once-guarded call. Byte-identical for Linux/mac (cfg-gated one-liner).
    #[cfg(windows)]
    platform::windows::dpi::ensure_process_dpi_aware();
    // DRAGON-233 fix 1: with the GUI subsystem above, a terminal launch has no console
    // of its own — attach to the parent's (a no-op on a GUI/shortcut launch, which is
    // what keeps it console-free) so `--help` / `--test` / diagnostics still print.
    // First-thing, before any output; redirected streams (pipe / file) are preserved.
    #[cfg(windows)]
    platform::windows::console::attach_parent_console();
    util::timing_start();
    // DRAGON-419: install the logger. This is the FIRST logging statement of `main`, before
    // every subcommand return and before the resident-daemon branch, so that EVERY way of
    // starting this program is covered: a hotkey capture, a daemon-spawned child, a CLI run,
    // Finder / Start Menu, the settings window, each detached share helper. Stderr behaviour
    // is unchanged (same `warn` default, same `RUST_LOG`); when the opt-in debug log is on it
    // ALSO appends this crate's records at `debug` and above (plus any dependency's
    // `warn`/`error`) to one shared, pid-tagged file.
    //
    // It replaces the DRAGON-233 `CCK_LOG_FILE` block, which was `#[cfg(windows)]` — which is
    // precisely why macOS had nothing to show when a customer's captures all failed. The
    // variable still works, on every platform, through `diag::PATH_ENV`.
    diag::init();
    util::timing_mark("main() entry (after dyld + static init)");
    #[cfg(target_os = "macos")]
    install_macos_panic_hook();
    #[cfg(windows)]
    install_windows_panic_hook();
    #[cfg(windows)]
    maybe_forced_panic();
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
    // Cloud upload (DRAGON-482): `--cloud-upload <file> --account <id> [--auto-share]
    // [--session <id>]`. A detached single-threaded child, exactly like the helpers above,
    // because an upload can run for minutes and this app's model is one-shot: the editor
    // must be free to close while the transfer finishes.
    //
    // It sits HERE, with the other helpers, and returns either way, so it is already gone
    // before the three resident-daemon `bare` checks further down, and none of them needs
    // to learn this flag. Nothing is logged about the file beyond `diag::path_shape`, and
    // the account id and the session id (DRAGON-490) are both random hex that identifies
    // nothing about the user.
    if let Some(p) = after(share::reexec::CLOUD_UPLOAD) {
        let account = args
            .iter()
            .position(|a| a == share::reexec::CLOUD_ACCOUNT)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .unwrap_or("");
        let auto_share = args.iter().any(|a| a == share::reexec::CLOUD_AUTO_SHARE);
        // DRAGON-490: the session id the editor minted, so this child can be watched
        // cross-process. Optional (a bare `--cloud-upload` still runs, just unwatched), and
        // validated before it becomes part of any filename (`cloud::session::state_path`).
        let session = args
            .iter()
            .position(|a| a == share::reexec::CLOUD_SESSION)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .filter(|s| cloud::session::is_valid_session_id(s))
            .unwrap_or("");
        if let Err(e) = cloud::child::run_cloud_upload(&p, account, auto_share, session) {
            // Both channels on purpose: a terminal run gets the reason on stderr, and a
            // detached child (the normal case, with no terminal at all) gets it in the
            // debug log. The message is user-facing copy and names no file.
            //
            // Through `redact_oauth` on BOTH (DRAGON-482): most of these reasons are ours,
            // but one is the provider's own text passed straight through, and stderr is not
            // a safer place for a credential than the log is. A stderr line lands in a
            // journal, a terminal's scrollback, or a screenshot of the terminal.
            let safe = diag::redact_oauth(&e);
            eprintln!("--cloud-upload: {safe}");
            log::warn!("cloud upload: {safe}");
        }
        return Ok(());
    }
    // DRAGON-450 (Windows): clicking a capture toast activates us through our OWN URI
    // scheme (`activationType="protocol"` — no COM activator), so the click arrives as a
    // bare URI argument rather than a flag. It is the same helper job as `--reveal` above
    // and belongs in the same place: before any GUI init, before the resident-daemon
    // branch.
    //
    // The branch is taken on the SCHEME and returns either way. A stale toast — clicked
    // long after its capture was moved or deleted — must end here as a no-op; falling
    // through would reach the normal launch path and take a fresh CAPTURE, which is not
    // what any click on an old notification means. `reveal_uri_path` is what decides
    // whether there is anything to reveal (ours, well-formed, an absolute path that
    // exists); nothing from the URI is ever executed.
    #[cfg(windows)]
    if let Some(uri) = args.iter().skip(1).find(|a| platform::windows::services::is_reveal_uri(a)) {
        match platform::windows::services::reveal_uri_path(uri) {
            Some(p) => share::run_reveal(&p),
            None => log::warn!("toast click: the reveal URI names no file we can show"),
        }
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
    // The banner's wording rides the helper's own argv (DRAGON-450): which flag launched
    // it says whether the capture reached the clipboard, and the optional `--notify-kind` /
    // `--notify-reason` tokens say what was captured and, when it was not copied, why.
    if let Some(p) = after(share::reexec::NOTIFY_COPIED) {
        let (kind, outcome) = share::notify_from_argv(&args, true);
        share::run_notify(&p, kind, outcome);
        return Ok(());
    }
    if let Some(p) = after(share::reexec::NOTIFY_SAVED) {
        let (kind, outcome) = share::notify_from_argv(&args, false);
        share::run_notify(&p, kind, outcome);
        return Ok(());
    }
    // Babysitter for pausing other apps' media while a preview soundtrack is open — runs here, in a
    // clean single-threaded child (Linux: zbus from the GUI's threads can stall; macOS: the
    // MediaRemote proxy launches perl). It pauses, waits for the GUI to let go (stdin EOF), resumes.
    if args.iter().any(|a| a == crate::audio::ducking::DUCK_FLAG) {
        crate::audio::ducking::run_duck();
        return Ok(());
    }
    // Windows in-app updater (DRAGON-287): the detached MSI swap helper. A temp COPY of this
    // exe is re-launched as `--update-swap <msi> <installed-exe> <log>`; it waits for the app
    // + daemon to exit, runs `msiexec /i <msi> /qn` (per-user, no UAC), then relaunches. Runs
    // HERE — before the resident-daemon early-branch below (which would otherwise mistake this
    // flagged launch for a bare one) and before any GUI init.
    #[cfg(windows)]
    if let Some(i) = args.iter().position(|a| a == platform::windows::update_install::SWAP_FLAG) {
        platform::windows::update_install::run_swap(&args[i + 1..]);
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
                    | "--all-in-one"
                    | "--active-window"
                    | "--active-monitor"
                    | "--no-editor"
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
        // DRAGON-427 adds `--preview-handoff` (the spawned editor child) to the same
        // check: a resident install must not read an editor launch as a bare one.
        let bare = bare
            && !args.iter().any(|a| a == "--preview" || a == "--preview-handoff");
        if bare && state::load().resident {
            // DRAGON-419: from here on this pid's lines read `daemon`, which is what lets a
            // reader untangle the long-lived menu-bar process from the one-shot capture
            // children it spawns into the same shared file.
            diag::mark_component(diag::Component::Daemon);
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
                    // DRAGON-295: the picker-free immediate captures + All In One are GUI
                    // capture launches (a COSMIC custom shortcut runs them), NOT resident
                    // triggers — treat them as non-bare so they reach `app::run`.
                    | "--all-in-one"
                    | "--active-window"
                    | "--active-monitor"
                    | "--no-editor"
                    | "--image"
                    | "--video"
                    | "--scan"
                    | "--scanner"
                    | "--countdown"
                    | "--overlay"
                    | "--preview"
                    // DRAGON-427: the spawned preview EDITOR child. Without it here a
                    // resident install would read the editor launch as bare and run the
                    // tray daemon instead of opening the capture the user just took.
                    | "--preview-handoff"
            )
        });
        if bare && state::load().resident {
            // Intent from the argv shape (DRAGON-180): the autostart entry launches
            // with a literal `resident` argument (daemon-intent: keep the restart-
            // handoff lock retry); a truly bare launch is the global hotkey asking
            // to capture (single lock attempt, signal the live daemon immediately).
            // DRAGON-465 moved the rule itself into `instance` so every launcher that
            // spawns this binary reads the same one; the answer here is unchanged.
            let daemon_intent = instance::daemon_intent_from_args(&args);
            // DRAGON-419: re-tag this pid's lines as the resident (see the mac branch).
            diag::mark_component(diag::Component::Daemon);
            daemon_linux::run(daemon_intent); // never returns — runs the tray loop or exits
        }
    }
    // Windows resident tray DAEMON (DRAGON-237): the full-parity counterpart of the mac /
    // Linux branches above. A BARE launch (no capture-mode / settings / preview / worker
    // flag) with the persisted `resident` setting on runs the tiny Win32 tray daemon and
    // NEVER touches the iced/wgpu stack — the same lightweight technique. Every other launch
    // falls through to `app::run` exactly as before. `--permissions` is macOS-only, so it is
    // not in the Windows bare check. DRAGON-296: `resident` now defaults ON on Windows (matching
    // macOS — the tray owns the global capture hotkey), so a fresh install's bare launch runs the
    // daemon; a user who turns the tray off falls through to the historical "bare launch opens the
    // capture overlay" behaviour. The daemon takes its own lock and, if one is already up, signals
    // it to capture and exits (the mac "capture NOW" UX).
    #[cfg(target_os = "windows")]
    {
        let bare = !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--settings"
                    | "--region"
                    | "--window"
                    | "--monitor"
                    | "--all-in-one"
                    | "--active-window"
                    | "--active-monitor"
                    | "--no-editor"
                    | "--image"
                    | "--video"
                    | "--scan"
                    | "--scanner"
                    | "--countdown"
                    | "--overlay"
                    | "--preview"
                    // DRAGON-427: the spawned preview EDITOR child. Without it here a
                    // resident install would read the editor launch as bare and run the
                    // tray daemon instead of opening the capture the user just took.
                    | "--preview-handoff"
            )
        });
        if bare && state::load().resident {
            // Intent from the argv shape (DRAGON-180, mirroring the Linux branch above):
            // the autostart entry and the resident toggle launch with a literal `resident`
            // argument (daemon-intent — keep the restart-handoff lock retry); a truly bare
            // launch is the user asking to CAPTURE (one lock attempt, then hand the request
            // to the live daemon).
            // DRAGON-465: the rule now lives in `instance`, because a launcher that gets it
            // wrong asks for a capture without meaning to — which is exactly what the
            // post-update relaunch was doing (see `update::post_update_relaunch_args`).
            let daemon_intent = instance::daemon_intent_from_args(&args);
            // DRAGON-438: THE choke point. Either we become the daemon, or a live daemon
            // acknowledged taking this capture (and `claim_or_signal` exited), or we fall
            // through below to a one-shot capture. What used to happen instead was a silent
            // `exit(0)` on any held lock, which turned every capture launch into a no-op
            // whenever the daemon was wedged. These launcher-side lines stay tagged `app` —
            // this process is not the daemon unless the call says so.
            if daemon::claim_or_signal(daemon_intent) {
                // DRAGON-419: re-tag this process's debug-log lines as the DAEMON before its
                // message loop takes over, so the shared file distinguishes the long-lived tray
                // process from the one-shot capture children it spawns (see the mac branch).
                diag::mark_component(diag::Component::Daemon);
                // Never returns — runs the Win32 message loop or exits. A capture-intent
                // launch that became the daemon still owes the user their overlay
                // (DRAGON-181's rule, which Linux has had all along).
                daemon::run(!daemon_intent);
            }
            // Otherwise: no daemon could be reached AND none could be started. Fall through
            // to `app::run` for a one-shot capture — the historical behaviour of a bare
            // launch with the tray turned off. The worst case of a stuck daemon is now "the
            // tray is broken", not "the app does nothing".
        }
    }
    // DRAGON-336: every path from here on either returns without a GUI or ends in
    // `app::run`, so this is the one spot that covers them all (the `--preview` arm
    // below returns `app::run` itself). Self-disabling and Linux-only — see
    // `pin_vulkan_icd` for the six conditions it insists on before touching anything.
    #[cfg(target_os = "linux")]
    pin_vulkan_icd();
    // DRAGON-427 (Windows 10): this process IS the preview EDITOR for a capture another
    // process just took. The argument is one `preview_ipc::OpenRequest` line — the very same
    // wire format the unix socket handoff sends — so the document opens with the capture's
    // own fields (dims, scale, size, and `external=0`, which is what makes the editor treat
    // the file as ours to manage) rather than as a bare `--preview` viewer.
    //
    // Spawning is the transport here, not a socket: the capture child creates this process
    // rather than finding one already running, so there is nothing to connect to and no ack
    // to wait for. That also means it needs no `#[cfg(unix)]` transport — it works on
    // Windows, which is the whole point (`preview_ipc` has no named-pipe host yet).
    //
    // A malformed line is a hard error rather than a fallback: only our own capture child
    // ever writes one, so a bad line means the two halves disagree and silently opening
    // something slightly different would be exactly the wrongness the wire format's strict
    // parser exists to prevent.
    if let Some(line) = after("--preview-handoff") {
        let line = line.to_string_lossy().into_owned();
        match crate::preview_ipc::OpenRequest::parse(&line) {
            Ok(req) => {
                if !req.path.is_file() {
                    eprintln!("--preview-handoff: file not found: {}", req.path.display());
                    std::process::exit(1);
                }
                return app::run(app::Startup {
                    // The editor is ALWAYS the windowed variant on Windows 10 (this flag's
                    // only user): the overlay one would need the software rasterizer that
                    // cannot draw the preview's `iced::widget::shader` layers at all.
                    preview_windowed: Some(true),
                    preview_handoff: Some(req),
                    ..Default::default()
                });
            }
            Err(e) => {
                eprintln!("--preview-handoff: {e}");
                std::process::exit(1);
            }
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
    // overlay). On non-macOS the flag is inert (no TCC grants / no permission window) —
    // it falls through to a normal launch, keeping Linux byte-identical to a bare launch.
    #[cfg(target_os = "macos")]
    let permissions_only = args.iter().any(|a| a == "--permissions");
    #[cfg(not(target_os = "macos"))]
    let permissions_only = false;
    // `--settings` opens the settings window directly (no capture overlay).
    let settings_only = args.iter().any(|a| a == "--settings");
    if settings_only {
        // Only one settings pane may exist (across all instances); a capture instance
        // can still run alongside it (nothing else is locked).
        if !instance::acquire_settings_lock() {
            log::info!("settings already open; not opening another");
            // macOS (DRAGON-153) / Windows (DRAGON-246): don't vanish silently — poke the
            // live holder to COME FORWARD + un-hide its settings window (the daemon's
            // "Settings" menu item lands here each click when a pane is already open; the
            // real Windows bug is the holder's window ending up HIDDEN, so a silent return
            // leaves it stuck + the mutex held). The holder's `sub_settings_poke` picks up
            // the poke and self-activates (or exits if its window is truly gone, so this
            // retry then opens fresh). Linux keeps its historical quiet return (the in-app
            // gear path covers focus there via the --focus-settings helper).
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            platform::compositor::activate_title(app::WINDOW_TITLE);
            return Ok(());
        }
    }
    // A CAPTURE launch takes no lock at all (DRAGON-351): every launch runs as its own
    // instance, so there is nothing to step aside for. The step-aside that used to live
    // here — "another capture instance holds the capture lock, don't open a duplicate
    // overlay" — WAS the "allow multiple capture instances = off" behaviour, and it went
    // with the setting (so did the lock itself; see `instance`'s module doc). A
    // permission-checker launch (`--permissions`) took no lock before either, and the
    // macOS resident "capture NOW on second launch" UX is unaffected: it lives in the
    // daemon, which early-branches on a bare `resident` launch and owns its own lock.

    // Capture-mode flags: launch straight into a mode / kind / countdown. Absent
    // flags leave the fields `None`, so a bare launch is byte-identical to before.
    let has = |flag: &str| args.iter().any(|a| a == flag);
    // DRAGON-295: the picker-free immediate captures (a daemon global hotkey / direct CLI).
    // `--all-in-one` just opens the normal overlay (no `immediate`); it exists so the daemon
    // has a distinct flag from the mode flags and the bare-check treats it as a capture
    // launch. macOS/Windows resolve the target; Linux never spawns these (byte-identical).
    let immediate = if has("--active-window") {
        Some(app::ImmediateCapture::ActiveWindow)
    } else if has("--active-monitor") {
        Some(app::ImmediateCapture::ActiveMonitor)
    } else {
        None
    };
    let mode = if has("--monitor") {
        Some(app::Mode::Monitor)
    } else if has("--window") {
        Some(app::Mode::Window)
    } else if has("--region") {
        Some(app::Mode::Region)
    } else {
        None
    };
    // DRAGON-428: a MODIFIER, not a mode — it composes with every capture launch above
    // (bare picker, --region/--window/--monitor, --active-window/--active-monitor) so one
    // flag gives a no-editor variant of each. Named for the editor rather than the
    // "preview" because `--preview <file>` already means something else entirely (open a
    // file in the viewer), and `--no-preview` would read as that flag's opposite.
    let no_editor = has("--no-editor");
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
    // DRAGON-413: arm the startup self-exit guard for a CAPTURE launch. From here the
    // child has a bounded window to put something on screen; if it never does — a wedged
    // TCC probe, a stalled scene grab, a GUI that never maps — it ends itself quietly
    // instead of lingering forever with nothing to show and nothing to reap it. Armed
    // BEFORE `app::run` so a hang inside `App::init` is covered too (an in-app timer
    // could never fire there). `--settings` / `--permissions` are windows the user asked
    // for and keep their historical unbounded life; `--preview <file>` returned above.
    // macOS-only, so Linux/Windows stay byte-identical (see the module doc).
    #[cfg(target_os = "macos")]
    if !settings_only && !permissions_only {
        startup_guard::arm(startup_guard::budget_from_env(
            std::env::var(startup_guard::BUDGET_ENV).ok().as_deref(),
        ));
    }
    util::timing_mark("about to call app::run (into iced/cosmic runtime)");
    app::run(app::Startup {
        settings_only,
        permissions_only,
        preview: None,
        mode,
        kind,
        countdown_secs,
        immediate,
        preview_windowed: None,
        preview_handoff: None,
        no_editor,
        // DRAGON-440 / DRAGON-443: both filled in by `app::run`'s macOS preamble, which is
        // the only place early enough to influence the boot-time activation policy.
        #[cfg(target_os = "macos")]
        route_probe: None,
        #[cfg(target_os = "macos")]
        post_update: false,
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
    --all-in-one            Open the full capture picker overlay\n\
    --active-window         Capture the active window immediately (no picker)\n\
    --active-monitor        Capture the monitor under the cursor immediately (no picker)\n\
    --no-editor             Skip the preview editor: save, copy and notify instead\n\
                            (combines with any launch flag above)\n\
    --image                 Capture a screenshot (default)\n\
    --video                 Capture a screen recording\n\
    --scan                  Start the QR/OCR scanner (forces region)\n\
    --countdown <secs>      Pre-capture countdown in seconds (any value, e.g. 7)\n\
\n\
Other:\n\
    --preview <file>        Open an existing image/video in the preview (windowed)\n\
    --overlay               With --preview: use the fullscreen overlay, not a window\n\
                            (ignored on Windows 10, which has no overlay editor)\n\
    --cloud-upload <file>   Upload a capture to a connected cloud account\n\
    --account <id>          With --cloud-upload: which connected account to use\n\
    --auto-share            With --cloud-upload: also copy a share link\n\
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

/// DRAGON-336: the pure islands of the Vulkan-ICD pin. Everything that decides
/// WHETHER to pin and WHAT to pin is a plain function over data, so the whole
/// decision is testable without a compositor, a GPU, or the real environment.
#[cfg(all(test, target_os = "linux"))]
mod vulkan_icd_tests {
    use super::*;

    fn icds(names: &[&str]) -> Vec<String> {
        names.iter().copied().map(String::from).collect()
    }

    #[test]
    fn selection_env_guard_spots_a_user_choice() {
        assert_eq!(
            first_set_env(&VULKAN_SELECTION_ENV, |n| n == "DRI_PRIME"),
            Some("DRI_PRIME")
        );
        assert_eq!(
            first_set_env(&VULKAN_SELECTION_ENV, |n| n == "VK_ICD_FILENAMES"),
            Some("VK_ICD_FILENAMES")
        );
        assert_eq!(first_set_env(&VULKAN_SELECTION_ENV, |_| false), None);
    }

    #[test]
    fn only_dedicated_gpu_drivers_map_to_a_family() {
        assert_eq!(dedicated_icd_families("nvidia"), Some(&["nvidia_icd"][..]));
        assert_eq!(dedicated_icd_families("nouveau"), Some(&["nouveau_icd"][..]));
        // Possibly-integrated / unknown drivers must stay unmapped: the pin
        // no-ops rather than risk moving rendering off the adapter wgpu picks.
        for d in ["amdgpu", "i915", "xe", "radeon", "virtio_gpu", "vmwgfx", "evdi", ""] {
            assert_eq!(dedicated_icd_families(d), None, "{d} must not map");
        }
    }

    #[test]
    fn select_keeps_the_family_and_drops_the_rest() {
        // The measured dual-GPU case: amdgpu + nvidia, pin the nvidia ICD.
        let installed = icds(&["nvidia_icd.json", "radeon_icd.json"]);
        assert_eq!(
            select_icds(&installed, &["nvidia_icd"]),
            Some("nvidia_icd.json".to_string())
        );
    }

    #[test]
    fn select_keeps_every_manifest_of_the_same_family() {
        let installed = icds(&["intel_icd.x86_64.json", "nvidia_icd.i686.json", "nvidia_icd.json"]);
        assert_eq!(
            select_icds(&installed, &["nvidia_icd"]),
            Some("nvidia_icd.i686.json,nvidia_icd.json".to_string())
        );
    }

    #[test]
    fn select_refuses_when_it_would_empty_the_loader() {
        let installed = icds(&["radeon_icd.json", "lvp_icd.x86_64.json"]);
        assert_eq!(select_icds(&installed, &["nvidia_icd"]), None);
        assert_eq!(select_icds(&[], &["nvidia_icd"]), None);
    }

    #[test]
    fn select_refuses_when_nothing_would_be_excluded() {
        let installed = icds(&["nvidia_icd.i686.json", "nvidia_icd.json"]);
        assert_eq!(select_icds(&installed, &["nvidia_icd"]), None);
    }

    #[test]
    fn dev_t_splits_into_major_minor() {
        // 0xE280 is what the dmabuf feedback advertises for /dev/dri/renderD128.
        assert_eq!(major_minor(0xE280), (226, 128));
        assert_eq!(major_minor(0xE201), (226, 1));
        // The high halves of the 12+20-bit split encoding.
        assert_eq!(major_minor(0x0000_1000_0000_0000), (0x1000, 0));
        assert_eq!(major_minor(0x0000_0000_0010_0000), (0, 0x100));
    }

    #[test]
    fn driver_is_read_out_of_a_uevent() {
        let uevent = "DRIVER=nvidia\nPCI_CLASS=30000\nPCI_ID=10DE:2684\n";
        assert_eq!(driver_from_uevent(uevent), Some("nvidia"));
        // A platform device (evdi) has no DRIVER line in this shape.
        assert_eq!(driver_from_uevent("MODALIAS=platform:evdi\n"), None);
        assert_eq!(driver_from_uevent("DRIVER=\n"), None);
    }

    #[test]
    fn dev_t_array_decodes_in_native_byte_order() {
        assert_eq!(dev_t_from_bytes(&0xE280_u64.to_ne_bytes()), Some(0xE280));
        assert_eq!(dev_t_from_bytes(&0xE280_u32.to_ne_bytes()), Some(0xE280));
        assert_eq!(dev_t_from_bytes(&[0, 1, 2]), None);
        assert_eq!(dev_t_from_bytes(&[]), None);
    }
}
