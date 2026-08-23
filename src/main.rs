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
// The colour-picker's pure colour model (DRAGON-582): the seven notations the picker
// window shows, their formatters and their parsers, over ONE sRGB / linear / XYZ
// conversion stack. Crate root next to `geometry` for the same reason that one is: it is
// shared by the picker overlay, the picker window and the persisted recent-colours list,
// and belongs to none of them.
mod color;
mod geometry;
// Keyboard navigation over a list or a grid (DRAGON-680): which entry an arrow key lands on,
// and what wrapping means at each end. Crate root for the same reason `geometry` is: the
// preview editor's toolbar flyouts and the colour picker window's mode activator and colour
// history all navigate with the arrows, and the rules belong to none of them.
mod keynav;
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
// Live recording annotation (DRAGON-691): the pure model behind drawing on the screen while a
// recording runs: the Magic Marker's dissolve curve, the per-point timestamps that drive it,
// the availability gate, the region clamp and the Linux input-zone rect subtraction. Crate
// root next to `pen_stroke` for the same reason: it is shared by `app::ink`, the toolbar and
// the ink widget, and belongs to none of them. The SURFACE and the platform work are
// elsewhere; nothing here touches a toolkit, so `cargo test` proves it on every host.
mod ink;
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
// Since DRAGON-563 every mount also exposes `CountdownTraySession` (the pre-capture
// countdown's tray digits + Cancel countdown entry) on the same platform-free shape.
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
// The command line (DRAGON-695): `cli::parse` is the ONE table of flag spellings and the
// only thing that reads the user's argv, plus the bodies of the one-shot commands it
// dispatches (the diagnostics harness, the metadata dump, the A/V sync tools, the recording
// controls, the scanner refresh). Read its module doc before touching `main`'s dispatch: it
// records the flat-flag grammar this replaced, and why a parser that FELL THROUGH on an
// argument it did not recognise was the shape behind three separate shipped bugs.
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
// children. A `tray` launch (and a bare GUI launch with the setting on) early-branches
// here (see `main`), never
// touching the iced/wgpu stack. cfg(macos) inside the module keeps Linux clean.
#[cfg(target_os = "macos")]
#[path = "platform/mac/daemon.rs"]
mod daemon;
// The resident tray DAEMON (Windows, DRAGON-237): the full-parity port of the mac
// menu-bar daemon — a tiny Win32-only process (NO iced/wgpu) owning a `Shell_NotifyIcon`
// tray item with the same three-state icon + menu, a `RegisterHotKey` global capture
// hotkey, and one-shot capture children. A `tray` launch early-branches here
// (see `main`). cfg(windows) inside the module keeps Linux/mac clean.
#[cfg(target_os = "windows")]
#[path = "platform/windows/daemon.rs"]
mod daemon;
// The resident system-tray RESIDENT (Linux, DRAGON-173): the full-parity port of the
// mac daemon — a tiny process (NO iced/wgpu) owning ONE ksni StatusNotifierItem with the
// same three-state icon + menu, spawning one-shot capture children. A `tray`
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
///
/// DRAGON-709 added a second gate beside the main-thread one, shared with the Windows twin:
/// `app::failure::crash_alert_warranted` withholds the dialog for std's own console-write
/// panic. That was found on Windows, but the shape is identical here, a `println!` into a
/// pipe whose reader has gone, and the alert would be just as wrong about the capture.
#[cfg(target_os = "macos")]
fn install_macos_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // DRAGON-709, decided before anything is written so the breadcrumb below is
        // unaffected: std's own "could not write to the console" panic is logged like any
        // other, and is the one panic that does not get a dialog.
        let warranted =
            app::failure::crash_alert_warranted(info.payload_as_str().unwrap_or_default());
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
                // ONE formatted record, ONE `write_all` (DRAGON-709). `writeln!` issues a
                // separate write per format fragment and an append is only atomic per CALL,
                // so two panicking processes spliced their records mid-token: the file grew
                // headers like `===== panic @ unix 58:1784484836` and timestamps that parse
                // as 1969 and 6146. `diag::write_line` has always done it this way.
                let record = format!("\n===== panic @ unix {ts} =====\n{info}\n{bt}\n");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("panic.log"))
                {
                    let _ = f.write_all(record.as_bytes());
                }
            }
        }
        // The log is written FIRST, so the breadcrumb exists even if presenting the alert
        // goes wrong. The alert's own guards make this at most one dialog per process.
        if warranted && objc2_foundation::MainThreadMarker::new().is_some() {
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

/// The hook itself. Its full rationale is the long block further up, which physically sits
/// with [`windows_crash_alert_message`] because the two were written together.
///
/// DRAGON-709 added a SECOND gate beside the main-thread one:
/// `app::failure::crash_alert_warranted` withholds the dialog for std's own console-write
/// panic, which says nothing about the capture and cannot describe itself through the channel
/// that just failed. That predicate is pure and unit-tested; its doc says why it is a backstop
/// and not the fix.
#[cfg(windows)]
fn install_windows_panic_hook() {
    let default_hook = std::panic::take_hook();
    let main_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        // DRAGON-709, decided before anything is written so the breadcrumb below is
        // unaffected: std's own "could not write to the console" panic is logged like any
        // other, and is the one panic that does not get a dialog.
        let warranted =
            app::failure::crash_alert_warranted(info.payload_as_str().unwrap_or_default());
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
            // ONE formatted record, ONE `write_all` (DRAGON-709). `writeln!` issues a
            // separate write per format fragment and an append is only atomic per CALL, so
            // two panicking processes spliced their records mid-token: the file grew headers
            // like `===== panic @ unix 58:1784484836` and timestamps that parse as 1969 and
            // 6146. `diag::write_line` has always done it this way.
            let record =
                format!("\n===== panic @ unix {ts} (thread {tname}) =====\n{info}\n{bt}\n");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = f.write_all(record.as_bytes());
            }
        }
        // The log is written FIRST, so the breadcrumb exists even if presenting the alert
        // goes wrong.
        //
        // Only from the main thread (see the fn doc). `show` returns `None` when this
        // session already put an alert up — a session that reported a failure and THEN
        // panicked gets one box, not two, which is the DRAGON-436 latch doing its job — and
        // then there is nothing to wait for.
        if warranted
            && std::thread::current().id() == main_thread
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
/// `settings` from RSS 239.0 -> 204.2 MB and PSS 84.1 -> 75.7 MB.
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
    // (the `debug test` diagnostics + capture run before winit's own DPI-aware call, so
    // we can't rely on it). Must be the first statement; idempotent w.r.t. winit's later
    // Once-guarded call. Byte-identical for Linux/mac (cfg-gated one-liner).
    #[cfg(windows)]
    platform::windows::dpi::ensure_process_dpi_aware();
    // DRAGON-233 fix 1: with the GUI subsystem above, a terminal launch has no console
    // of its own — attach to the parent's (a no-op on a GUI/shortcut launch, which is
    // what keeps it console-free) so `--help` / `debug test` / diagnostics still print.
    // First-thing, before any output; redirected streams (pipe / file) are preserved.
    // DRAGON-695: it also REPORTS whether a console was found, which is Windows' only honest
    // answer to "was this launched from a terminal" (a GUI-subsystem process has no console of
    // its own to ask). A bare launch turns on that answer; see `platform::launched_from_terminal`.
    #[cfg(windows)]
    let console_attached = platform::windows::console::attach_parent_console();
    #[cfg(not(windows))]
    let console_attached = false;
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
    // === INTERNAL PLUMBING, parsed before the subcommand grammar (DRAGON-695) ===
    //
    // Everything below this comment and above `cli::parse` is argv this app writes to ITSELF:
    // the detached post-capture helpers, the cloud-upload child, the Windows toast URI, the
    // babysitters and the update swapper. None of it is a user-facing command, none of it
    // appears in the help, and every branch RETURNS, so the parser never sees these words.
    //
    // They stay flat flags on purpose. A subcommand grammar is for a human choosing what to
    // do; these are one program handing a job to a copy of itself, where the argv is a private
    // wire format and renaming it buys nothing but a compatibility window.
    let args: Vec<String> = std::env::args().collect();
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
    // before the grammar further down, so the parser never sees it. Nothing is logged about the file beyond `diag::path_shape`, and
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
            crate::errln!("--cloud-upload: {safe}");
            log::warn!("cloud upload: {safe}");
        }
        return Ok(());
    }
    // DRAGON-450 (Windows): clicking a capture toast activates us through our OWN URI
    // scheme (`activationType="protocol"` — no COM activator), so the click arrives as a
    // bare URI argument rather than a flag. It is the same helper job as `--reveal` above
    // and belongs in the same place: before any GUI init and before the grammar (a bare
    // positional URI is not a command, and the parser would rightly refuse it).
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
    // HERE — before the grammar below (which does not know this internal flag) and before any
    // GUI init.
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
    // DRAGON-427 (Windows 10): this process IS the preview EDITOR for a capture another
    // process just took. The argument is one `preview_ipc::OpenRequest` line — the very same
    // wire format the unix socket handoff sends — so the document opens with the capture's
    // own fields (dims, scale, size, and `external=0`, which is what makes the editor treat
    // the file as ours to manage) rather than as a bare viewer.
    //
    // Spawning is the transport here, not a socket: the capture child creates this process
    // rather than finding one already running, so there is nothing to connect to and no ack
    // to wait for. It CREATES the first host — once open, later captures can reach it over
    // `preview_ipc`'s named pipes (DRAGON-651), but nothing can create a host by connecting
    // to one, so this spawn route stays.
    //
    // A malformed line is a hard error rather than a fallback: only our own capture child
    // ever writes one, so a bad line means the two halves disagree and silently opening
    // something slightly different would be exactly the wrongness the wire format's strict
    // parser exists to prevent.
    //
    // It is INTERNAL plumbing, so it is read here rather than by the grammar (DRAGON-695):
    // no user types a serialized wire line at a shell, and the parser would have to reject it
    // as an unknown command if it ever reached one.
    if let Some(line) = after("--preview-handoff") {
        let line = line.to_string_lossy().into_owned();
        match crate::preview_ipc::OpenRequest::parse(&line) {
            Ok(req) => {
                if !req.path.is_file() {
                    crate::errln!("--preview-handoff: file not found: {}", req.path.display());
                    std::process::exit(1);
                }
                return run_gui(
                    app::Startup {
                        // The editor is ALWAYS the windowed variant on Windows 10 (this
                        // flag's only user): the overlay one would need the software
                        // rasterizer that cannot draw the preview's `iced::widget::shader`
                        // layers at all.
                        preview_windowed: Some(true),
                        preview_handoff: Some(req),
                        ..Default::default()
                    },
                    false,
                );
            }
            Err(e) => {
                crate::errln!("--preview-handoff: {e}");
                std::process::exit(1);
            }
        }
    }

    // === THE SUBCOMMAND GRAMMAR (DRAGON-695) ===
    //
    // Every branch above returned without touching a GUI. From here the argv is the USER's,
    // and `cli::parse` is the only thing that reads it: one pure function, one table of
    // spellings, and no fall-through. What that replaced — a flat bag of flags, three
    // near-duplicate per-platform "is this launch bare?" lists, and an unknown word quietly
    // opening a capture overlay — is written up in `cli/parse.rs`'s module doc, along with the
    // two shipped bugs that came out of it.
    let plan = match cli::parse(&args) {
        Ok(plan) => plan,
        Err(e) => {
            // Both channels, like every other command here: stderr for the person who typed
            // it, the debug log for a desktop hotkey that has no terminal at all. The message
            // is our own copy and names no path.
            //
            // `debug!` rather than `warn!` for the log half, and only here. The default
            // stderr filter is `warn`, so a `warn!` would print the SAME line twice to the
            // same terminal, and a usage error is the one failure that is nearly always read
            // at a terminal. The debug log still records it (our records at `debug` and above
            // go to the file), which is what a desktop hotkey with a stale argv needs.
            eprintln!("{}", e.message);
            log::debug!("{}", e.message);
            std::process::exit(e.code);
        }
    };
    // Only a BARE launch asks this; see that arm below for why it is the question.
    let terminal = platform::launched_from_terminal(console_attached);
    let launch = match plan {
        cli::Plan::Help => {
            println!("{}", cli::parse::HELP);
            return Ok(());
        }
        cli::Plan::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        cli::Plan::SetTimer(secs) => {
            run_set_timer(secs);
            return Ok(());
        }
        cli::Plan::SetSound(arms) => {
            run_set_sound(arms);
            return Ok(());
        }
        cli::Plan::ScanRefresh => {
            cli::run_scan_refresh();
            return Ok(());
        }
        cli::Plan::RecordControl(cmds) => {
            cli::run_recording_commands(&cmds);
            return Ok(());
        }
        cli::Plan::Debug(plan) => {
            run_debug(plan);
            return Ok(());
        }
        // An EXPLICIT request for the resident, so it runs whether or not the persisted
        // `resident` setting is on. That setting still gates autostart registration and what a
        // bare GUI launch does; neither is a reason to refuse a direct command.
        cli::Plan::Tray => run_resident(),
        // A launch with no arguments at all, and the login-time bug DRAGON-695 fixes.
        //
        // It used to mean "capture NOW" on every platform, which is how a login item that
        // raced the tray it was starting could end up asking the winner of that race for a
        // capture: a region overlay on screen at login, with nobody having pressed anything.
        // It now means one of three things, and never that one.
        cli::Plan::Bare => {
            if terminal {
                // A shell, no arguments: this is someone asking what the program does.
                println!("{}", cli::parse::HELP);
                return Ok(());
            }
            if state::load().resident {
                // A login item, Finder, the Start Menu, the Dock. With the tray turned on
                // this is TRAY intent on EVERY platform and nothing else: become the
                // resident, or find one already up and leave quietly.
                run_resident();
            }
            // Tray off, so there is no resident to raise. The only thing a double-click can
            // sensibly mean then is "show me the app", which is the settings window.
            cli::parse::Launch { what: cli::parse::What::Settings, ..Default::default() }
        }
        cli::Plan::Launch(launch) => launch,
    };
    run_launch(launch)
}

/// Become the RESIDENT (the macOS menu-bar daemon, the Windows tray daemon, the Linux ksni
/// resident), or leave quietly when one is already up.
///
/// The two ways in are the `tray` command and a bare GUI launch with the `resident` setting
/// on. Both pass DAEMON intent, which is what keeps the bounded restart-handoff retry on the
/// lock (`instance::daemon_lock_attempts`): a tray restarting itself has to be able to wait
/// out the outgoing process.
///
/// **It never asks a live daemon for a capture.** That is the whole DRAGON-695 fix: the old
/// bare-launch branch treated a held lock as "somebody already has the tray, so this launch
/// must have wanted a picture", which was never true of a login item and was the cause of the
/// phantom overlay at login.
fn run_resident() -> ! {
    // DRAGON-419: from here on this pid's lines read `daemon`, which is what lets a reader
    // untangle the long-lived tray process from the one-shot capture children it spawns into
    // the same shared file.
    diag::mark_component(diag::Component::Daemon);
    #[cfg(target_os = "macos")]
    daemon::run(true); // never returns — runs the AppKit run loop or exits
    #[cfg(target_os = "linux")]
    daemon_linux::run(true); // never returns — runs the tray loop or exits
    #[cfg(target_os = "windows")]
    {
        // DRAGON-438's takeover, with DRAGON-695's ending. `claim_or_signal(true)` is the
        // ProbeOnly path: it asks a live daemon only whether it is still pumping, hands it no
        // work, and takes the lock itself if that daemon is wedged.
        if daemon::claim_or_signal(true) {
            // `false`: a tray launch owes the user no capture overlay.
            daemon::run(false);
        }
        // A healthy daemon is already up. Nothing to do and nothing to say to the user: the
        // tray they asked for is on screen. This used to fall through to a one-shot capture.
        log::info!("tray: a resident is already running; this launch has nothing to do");
        std::process::exit(0);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        eprintln!("tray: this platform has no tray resident");
        log::warn!("tray: this platform has no tray resident");
        std::process::exit(1);
    }
}

/// `timer [0-99]`: write the persisted default countdown, or print the current one.
///
/// The value is EXACT seconds (DRAGON-695), not one of the overlay's four chips, which is why
/// the setting had to stop being an index. `0` clears it; the chips light the nearest preset.
///
/// It prints the bare number, with no `s` suffix and no prose, for the reason [`run_set_sound`]
/// gives: what a setting command prints has to be a value that command accepts.
fn run_set_timer(secs: Option<u64>) {
    let mut p = state::load();
    let value = match secs {
        None => p.countdown_secs,
        Some(secs) => {
            p.countdown_secs = secs;
            state::save(&p);
            secs
        }
    };
    // Both paths print the SAME thing, and it is exactly the token this command accepts back
    // (`timer 7`, never `timer 7s`). See `run_set_sound` for the rule and why it is a rule.
    println!("timer: {value}");
}

/// `sound [both|mic|system|none]`: write the persisted default recording arms, or print them.
///
/// **What a setting command prints is a value that command accepts.** Both paths print through
/// `AudioArmState::cli_word`, not the tray's `word()` UI copy, which is what this used to do:
/// bare `sound` answered `sound: None` and a set answered `sound: Mic + System`, neither of
/// which is a word the command takes. A user pasting back what they were just told got exit 2.
///
/// The same rule shapes [`run_set_timer`], which prints a bare number rather than `7s`. Two
/// commands, one property: read it, type it back, get the same state.
fn run_set_sound(arms: Option<recording_ui::AudioArmState>) {
    let mut p = state::load();
    let value = match arms {
        None => recording_ui::AudioArmState::from_arms(p.record_mic, p.record_system_audio),
        Some(arms) => {
            let (mic, system) = arms.arms();
            p.record_mic = mic;
            p.record_system_audio = system;
            state::save(&p);
            arms
        }
    };
    println!("sound: {}", value.cli_word());
}

/// The hidden `debug` namespace: the diagnostics harness and the one-shot tools. Never in the
/// help, and never a GUI launch.
fn run_debug(plan: cli::parse::DebugPlan) {
    use cli::parse::DebugPlan;
    match plan {
        DebugPlan::Test { name, args } => cli::run_test(&name, &args),
        DebugPlan::Inspect(file) => cli::inspect(&file),
        DebugPlan::MakeSyncClip(path) => cli::make_sync_clip(path.as_deref()),
        DebugPlan::CalibrateSync { file, apply } => cli::calibrate_sync(file.as_deref(), apply),
        DebugPlan::CloudUpload { file, account, auto_share } => {
            // The same body the editor's own detached child runs; only the argv differs (that
            // one carries `share::reexec::CLOUD_UPLOAD`, read far above with the rest of the
            // internal plumbing). A hand-run upload is unwatched: it mints no session id, so
            // nothing cross-process is waiting on its progress.
            if let Err(e) = cloud::child::run_cloud_upload(&file, &account, auto_share, "") {
                let safe = diag::redact_oauth(&e);
                crate::errln!("debug cloud-upload: {safe}");
                log::warn!("cloud upload: {safe}");
            }
        }
    }
}

/// Turn a parsed [`cli::parse::Launch`] into an `app::Startup` and run it.
///
/// This is where a plan that parsed cleanly can still fail, because these are the questions a
/// pure parser may not ask: does the file exist, is this platform the one that has a
/// permission checker, is a settings window already open somewhere, is a colour picker window
/// already up. Each answers with a message and an exit code rather than opening something the
/// user did not ask for.
fn run_launch(launch: cli::parse::Launch) -> cosmic::iced::Result {
    use cli::parse::What;

    // The macOS permission checker is the one command with no meaning elsewhere. It must NOT
    // fall through to an ordinary launch (the old `permissions` flag did exactly that off
    // macOS, which is a capture overlay in answer to a question about permissions).
    if matches!(launch.what, What::Permissions) && !cfg!(target_os = "macos") {
        let why = "the permission checker is macOS only; this platform grants no TCC \
                   permissions to check";
        eprintln!("permissions: {why}");
        log::warn!("permissions: {why}");
        std::process::exit(1);
    }

    let mut startup = app::Startup {
        mode: launch.mode,
        kind: launch.kind(),
        countdown_secs: launch.timer,
        audio_arms: launch.sound,
        immediate: launch.immediate,
        no_editor: launch.skip_editor,
        ..Default::default()
    };
    match &launch.what {
        What::Screenshot | What::Recording | What::Scanner => {}
        What::ColorPicker => startup.color_picker = true,
        What::PaletteViewer => {
            startup.palette_viewer = true;
            // The single-window rule (DRAGON-613) is CROSS-PROCESS, so it has to be answered
            // before this process pays for a GUI: if a picker window is already open anywhere,
            // this launch asks it to come forward and exits, rather than putting a second one
            // on screen. A refusal (no window, one mid-close, a build with no transport) falls
            // through to the ordinary launch below, which is the same never-lose-it ladder
            // every pick takes.
            match crate::preview_ipc::send_raise_to_picker() {
                Ok(pid) => {
                    log::info!("color --viewer: the open picker window at pid {pid} came forward");
                    return Ok(());
                }
                Err(e) => log::info!(
                    "color --viewer: no open picker window took it ({e}); opening one here"
                ),
            }
        }
        What::Settings => {
            startup.settings_only = true;
            // Only one settings pane may exist (across all instances); a capture instance can
            // still run alongside it (nothing else is locked).
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
        What::Permissions => {
            // Reached on macOS only; the refusal above covers every other platform.
            startup.permissions_only = true;
        }
        What::Editor { file, overlay } => {
            // A viewer, not a capture: no lock, and unsupported types are rejected up front.
            if !file.exists() {
                eprintln!("editor: file not found: {}", file.display());
                std::process::exit(1);
            }
            if app::preview_media_kind(file).is_none() {
                eprintln!("editor: unsupported file type: {}", file.display());
                eprintln!("supported: images (png, jpg, gif, …) and videos (mp4, mkv, webm, …)");
                std::process::exit(1);
            }
            // `editor` opens in a WINDOW by default (a file viewer wants a normal, resizable
            // window); `--overlay` asks for the fullscreen one instead.
            startup.preview = Some(file.clone());
            startup.preview_windowed = Some(!overlay);
        }
    }

    // A CAPTURE launch takes no lock at all (DRAGON-351): every launch runs as its own
    // instance, so there is nothing to step aside for. The step-aside that used to live
    // here — "another capture instance holds the capture lock, don't open a duplicate
    // overlay" — WAS the "allow multiple capture instances = off" behaviour, and it went
    // with the setting (so did the lock itself; see `instance`'s module doc).

    run_gui(startup, launch.opens_overlays())
}

/// The one GUI tail: the preamble every window and overlay shares, then `app::run`. Shared
/// with the `--preview-handoff` branch in `main`, which bypasses the grammar entirely.
///
/// `arm_guard` is the DRAGON-413 startup self-exit budget, and it is exactly
/// [`cli::parse::Launch::opens_overlays`]: a capture-shaped launch gets a bounded window to
/// put something on screen, and a plain window (settings, the permission checker, either
/// editor flavour, the palette viewer) keeps its historical unbounded life, because arming a
/// 90 second self-exit against a window the user is reading would close it under them.
fn run_gui(startup: app::Startup, arm_guard: bool) -> cosmic::iced::Result {
    // DRAGON-336: every path that reaches here is a GUI launch, so this is the one spot that
    // covers them all. Self-disabling and Linux-only — see `pin_vulkan_icd` for the six
    // conditions it insists on before touching anything.
    #[cfg(target_os = "linux")]
    pin_vulkan_icd();
    // DRAGON-527: seed the OCR language directory from the bundled `eng.traineddata`.
    //
    // Here, sharing the anchor above, for the same reason: every remaining path is a GUI
    // launch, so this runs exactly once per such process and never for a helper subcommand
    // or for the long-lived resident daemon (both already returned / never return above).
    //
    // Synchronous on purpose, not on a thread: this app is one-shot and can exit within a
    // second of launch, and a copy interrupted halfway would leave a half-written pack that
    // the "never overwrite" rule then keeps forever. It is also not a launch cost worth
    // deferring: with a distro tesseract it returns after a couple of `stat`s (the packs
    // are the distro's), and with a bundled one it copies only on the very first run.
    util::seed_tessdata();
    // DRAGON-413: arm the startup self-exit guard. From here the child has a bounded window to
    // put something on screen; if it never does — a wedged TCC probe, a stalled scene grab, a
    // GUI that never maps — it ends itself quietly instead of lingering forever with nothing
    // to show and nothing to reap it. Armed BEFORE `app::run` so a hang inside `App::init` is
    // covered too (an in-app timer could never fire there). macOS-only, so Linux/Windows stay
    // byte-identical (see the module doc).
    #[cfg(target_os = "macos")]
    if arm_guard {
        startup_guard::arm(startup_guard::budget_from_env(
            std::env::var(startup_guard::BUDGET_ENV).ok().as_deref(),
        ));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = arm_guard;
    util::timing_mark("about to call app::run (into iced/cosmic runtime)");
    app::run(startup)
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
