//! libcosmic overlay: per-output `Layer::Overlay` surfaces with the bottom
//! toolbar and a native region selector. Pixels are captured natively (cosmic
//! screencopy); the result is saved to disk and shared (clipboard / notify).
//!
//! Modeled on xdg-desktop-portal-cosmic's app.rs / widget/screenshot.rs.

use crate::selection::{GlobalRect, Selection};
use crate::widgets::{OutputSelection, RegionSelection};
use crate::platform::compositor::WinRect;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use cosmic::iced::core::event::wayland::OutputEvent;
// Layer-shell surface creation/destruction lives entirely in `shell` (the
// per-platform surface seam); only the output HANDLE type leaks up here.
#[cfg(target_os = "linux")]
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::IcedOutput;
use cosmic::iced::{
    Alignment, Background, Border, Event, Length, Subscription, event, window,
};
use cosmic::app::ApplicationExt;
use cosmic::{Element, Task, app, widget};
use std::rc::Rc;

/// The per-monitor output handle. On Linux this is the Wayland `WlOutput` the
/// layer-shell overlay + OutputEvent subscription drive. On macOS/Windows the
/// output list comes from NSScreen/SCK (DRAGON-94 phase 2), so it's a name-keyed
/// placeholder; the Wayland OutputEvent path is gated off, so the mac output list
/// stays empty in phase 1 and no overlay is minted.
#[cfg(target_os = "linux")]
pub(crate) type OutputHandle = wayland_client::protocol::wl_output::WlOutput;
#[cfg(not(target_os = "linux"))]
pub(crate) type OutputHandle = String;

// Implementation split across submodules (all operate on `App`); each does
// `use super::*;` to share these imports and the types/helpers defined here.
mod application;
mod update;
mod subscriptions;
mod keyboard;
mod num_field;
mod persist;
mod recording;
mod capture_flow;
mod preview;
mod audio_ui;
mod shell;
mod surfaces;
mod portal;
mod overlay;
mod settings;
// pub(crate): the macOS daemon (platform/mac/daemon.rs) reads the auto-open decision +
// probe from here at startup; `App`'s own routing uses it directly.
pub(crate) mod permissions;
// pub(crate): theme.rs is THE appearance seam (DRAGON-117) — the widgets/ and
// tray modules outside `app` read the accent / rounding / record-red helpers too.
pub(crate) mod theme;
mod layout;

// Re-export so existing `super::foo()` call-sites in submodules keep working.
// `theme_is_dark` is also called as `super::theme_is_dark()` from actions + settings.
pub(crate) use theme::theme_is_dark;
// These are called unqualified via `use super::*;` in application.rs.
pub(super) use theme::{
    window_radius,
    active_hint_color,
    wallpaper_path,
};
// Theme-level state-mix helpers, called unqualified via `use super::*;` in
// overlay/toolbar/mod.rs and preview/mod.rs.
pub(super) use theme::{state_mix, MIX_OFF};
// These are called unqualified via `use super::*;` in toolbar.rs / marks.rs / actions.rs.
pub(super) use layout::{
    ICON_BOX, BTN_PAD, GROUP_PAD, GROUP_H_BASE,
    meter_background, positioned_mark, inset_region, placement,
};

// The numeric value+text-buffer pair behind every settings num-input row.
// Available unqualified in the submodules via their `use super::*;`.
pub(crate) use num_field::NumField;

pub use settings::SettingsState;
pub(crate) use settings::{ConfigTab, ResetScope};
pub(crate) use settings::WINDOW_TITLE;

/// How the app was launched: a normal capture overlay, the settings window
/// (`--settings`), or straight into the preview overlay for an existing file
/// (`--preview <file>`).
#[derive(Clone, Default)]
pub struct Startup {
    pub settings_only: bool,
    /// Launch straight into the macOS permission-checker window (`--permissions`),
    /// with no capture machinery — mirrors `settings_only`. On Linux the flag has no
    /// window to open (no TCC grants), so it falls through to a normal launch.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub permissions_only: bool,
    pub preview: Option<std::path::PathBuf>,
    /// Launch straight into this capture mode (`--region`/`--window`/`--monitor`);
    /// `None` uses the default (Region).
    pub mode: Option<Mode>,
    /// Launch with this capture kind (`--image`/`--video`/`--scan`); `None` uses the
    /// default (Image). A `Scanner` kind forces Region mode.
    pub kind: Option<Kind>,
    /// Pre-capture countdown seconds (`--countdown <secs>`) — an EXACT value that may
    /// not match a UI preset (e.g. 7). `None` uses the persisted delay.
    pub countdown_secs: Option<u64>,
    /// Override the preview appearance for this launch: `Some(true)` = windowed,
    /// `Some(false)` = overlay. `--preview` defaults to windowed unless `--overlay` is
    /// also given; `None` uses the persisted setting.
    pub preview_windowed: Option<bool>,
}

// Re-exported so the message enum can carry a decoded shader frame across a task.
pub(crate) use preview::PixelFrame;

/// Classify a `--preview` file by extension: `Some(true)` = video, `Some(false)` =
/// image, `None` = unsupported. Used by the CLI to reject non-previewable files.
pub fn preview_media_kind(path: &std::path::Path) -> Option<bool> {
    if preview::is_video_path(path) {
        Some(true)
    } else if preview::is_image_path(path) {
        Some(false)
    } else {
        None
    }
}

pub fn run(startup: Startup) -> cosmic::iced::Result {
    // macOS (DRAGON-150 -> DRAGON-151): the installed bundle carries LSUIElement=true
    // (for the menu-bar DAEMON), so a GUI child spawned from it runs as a
    // never-activated ACCESSORY app — which is exactly what we WANT for CAPTURE
    // launches (the menu bar keeps showing the app the user was in, no Dock icon, no
    // focus theft; captures of the menu-bar area stay authentic). The one thing macOS
    // denies an inactive app is pointer-cursor control (DRAGON-150's plain-arrow bug;
    // a Regular-policy promotion fixed cursors but stamped "Cosmic Capture Kit" into
    // the menu bar, DRAGON-151). The surgical fix is the SkyLight per-connection
    // property `SetsCursorInBackground` (the background-utility escape hatch): with
    // it set, winit's normal cursor plumbing works while the app stays inactive.
    // Keyboard needs no activation at all: winit windows override
    // `canBecomeKeyWindow`, and a key window of an inactive app still receives keys
    // (Escape worked this way all along). Verified end-to-end with a standalone
    // accessory-app harness (panel key + Escape delivery + crosshair via cursor
    // probe).
    #[cfg(target_os = "macos")]
    crate::platform::mac::window::enable_background_cursor();
    // macOS (DRAGON-153): launches whose UI is a REAL window (settings /
    // permissions / a --preview viewer) should behave like a normal app — Cmd+Tab
    // presence, Dock icon, focusable from other apps — so they boot with the
    // REGULAR policy. Policy is boot-time-only (the DRAGON-150 lesson: a
    // post-launch flip half-activates the app and kills key-window delivery), which
    // is why this is decided here and capture children never change theirs.
    #[cfg(target_os = "macos")]
    if (startup.settings_only || startup.permissions_only || startup.preview.is_some())
        && let Some(mtm) = objc2_foundation::MainThreadMarker::new()
    {
        objc2_app_kit::NSApplication::sharedApplication(mtm)
            .setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
    }
    // macOS (DRAGON-154): CAPTURE launches install the tiling-WM AX opt-out — the
    // pre-order-front chrome strip. AeroSpace classifies an accessory-policy window
    // with no AXCloseButton as an unmanaged POPUP, decided once at its first AX
    // exposure — so the traffic lights must be gone BEFORE the overlay is first
    // ordered on screen, not at the title-matched `place_overlay`. The accessory
    // policy itself comes from the bundle's LSUIElement (capture children NEVER set
    // a policy: an explicit boot-time `setActivationPolicy(Accessory)` was tried
    // here and stamped the app name into the menu bar on unbundled dev launches —
    // the DRAGON-150/151 lesson again; do not re-add it).
    #[cfg(target_os = "macos")]
    if !(startup.settings_only || startup.permissions_only || startup.preview.is_some()) {
        crate::platform::mac::window::install_overlay_chrome_strip();
    }
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .exit_on_close(false);
    let result = cosmic::app::run::<App>(settings, startup);
    // Once `cosmic::app::run` returns, this one-shot session is over: the App and
    // every teardown guard it owns (recording / audio children, meters, tray) have
    // already been dropped on THIS (main) thread inside the call above. What remains
    // is libc's `exit()` phase — and that is exactly where we crash. libcosmic's
    // wayland backend (`iced_winit`'s `SctkEventLoop::new`) spawns its event-loop
    // thread and DROPS the `JoinHandle`, so that thread is NEVER joined; it borrows
    // the winit-owned wayland display (`from_foreign_display`) and, as it tears its
    // own windows down at shutdown, issues a `cosmic_corner_radius_toplevel_v1
    // ::destroy`. When it is descheduled just long enough to run that request AFTER
    // the main thread has entered `exit()` (freeing the display / running the
    // libgomp atexit handler), it dereferences freed wayland state — a SIGSEGV
    // (SEGV_MAPERR) in `wl_proxy_destroy` at process exit, which under load degrades
    // the compositor. We hold no handle to join that thread, so we remove the phase
    // it races against: `_exit(2)` asks the kernel to terminate every thread now,
    // running no atexit handlers and freeing nothing further, so the unjoined thread
    // cannot fault against teardown. Nothing after this point needs to run — share /
    // clipboard / notify are handed to detached child processes that outlive us by
    // design (see `main`).
    let code = match &result {
        Ok(()) => 0,
        Err(e) => {
            log::error!("cosmic-capture-kit exited with error: {e:?}");
            1
        }
    };
    // Exit-path backstop: re-enable a tiling WM we paused for a capture session, in case
    // the normal teardown seam didn't (no-op unless we paused it).
    #[cfg(target_os = "macos")]
    crate::platform::mac::window::resume_tiling_wm();
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::Write::flush(&mut std::io::stderr());
    // SAFETY: `_exit` is async-signal-safe and merely asks the kernel to terminate
    // the process immediately; App teardown already completed above, so there is
    // nothing left to unwind or flush. It diverges (`-> !`), which satisfies the
    // `cosmic::iced::Result` return type.
    unsafe { libc::_exit(code) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Region,
    Window,
    Monitor,
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mode::Region => "Region",
            Mode::Window => "Window",
            Mode::Monitor => "Monitor",
        })
    }
}

/// What a capture produces — the leftmost toolbar segment trio. `Scanner` captures
/// exactly like `Image` but is the only kind QR/OCR scanning runs in; it forces
/// Region mode and skips the countdown (the delay chip and mode group hide).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Scanner,
    Image,
    Video,
}

/// Which floating overlay button the pointer is over (for hover styling).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hover {
    None,
    Cancel,
}

/// Pre-capture countdown options (label, seconds).
const DELAYS: [(&str, u64); 4] =
    [("No delay", 0), ("3s delay", 3), ("5s delay", 5), ("10s delay", 10)];

/// The [`DELAYS`] index whose seconds are closest to `secs` — maps a CLI
/// `--countdown <secs>` value onto the fixed preset set (0/3/5/10).
pub fn countdown_index(secs: u64) -> usize {
    DELAYS
        .iter()
        .enumerate()
        .min_by_key(|(_, (_, s))| s.abs_diff(secs))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Recording max-resolution preset labels (the dropdown). The index maps to
/// `record_res_preset`; the last entry (Custom) uses `record_max_width/height`.
pub(super) const RES_LABELS: [&str; 8] = [
    "Original",
    "360p (640×360)",
    "480p (854×480)",
    "720p (1280×720)",
    "1080p (1920×1080)",
    "2K (2560×1440)",
    "4K (3840×2160)",
    "Custom",
];
/// Index of the Custom preset in `RES_LABELS`.
pub(super) const RES_CUSTOM: usize = 7;

/// The (max_w, max_h) box for a non-custom preset index. (0, 0) = no limit. The
/// recording is downscaled to fit this box (aspect preserved); Custom is handled
/// by the caller from `record_max_width/height`.
pub(super) fn res_dims(preset: usize) -> (u32, u32) {
    match preset {
        1 => (640, 360),
        2 => (854, 480),
        3 => (1280, 720),
        4 => (1920, 1080),
        5 => (2560, 1440),
        6 => (3840, 2160),
        _ => (0, 0),
    }
}

/// Pre-capture each window (active workspace) via cosmic screencopy before any
/// overlay is shown, so window mode can display clean thumbnails. Toplevels are
/// captured directly by handle (so even occluded windows thumbnail correctly),
/// then corner-rounded and downscaled. Transparency is PRESERVED (the picker draws
/// them over the wallpaper, so translucent windows preview see-through like
/// cosmic-screenshot). Runs once at launch.
fn build_window_thumbs(
    groups: &HashMap<String, Vec<crate::platform::compositor::Toplevel>>,
    raw: &HashMap<String, image::RgbaImage>,
    radius: f32,
) -> HashMap<String, Vec<WindowThumb>> {
    let mut out: HashMap<String, Vec<WindowThumb>> = HashMap::new();
    for (name, wins) in groups {
        let mut v = Vec::new();
        for win in wins {
            if win.rect.2 < 1 || win.rect.3 < 1 {
                continue;
            }
            let Some(img) = raw.get(&win.id) else {
                continue;
            };
            // DRAGON-190 (platform-agnostic): trim any dead FULLY-transparent gutter off
            // the raw grab so the picker tile matches the trimmed CAPTURE, then size the
            // tile's layout slot to the trimmed content (`layout_size`). `rect` stays the
            // raw frame — the click passes it as the selection and `WindowCaptureJob`
            // derives scale from + re-trims the full grab, so the two agree. A capture with
            // no dead gutter (e.g. an opaque server-side-decorated window) trims to a
            // no-op, leaving `layout_size` at the frame size.
            let (img, layout_size) = {
                let cr = crate::decoration::corner_radius_from_alpha(img)
                    .map(|r| r.round() as u32)
                    .unwrap_or(0);
                let (trimmed, (_, _, tw, th)) =
                    crate::compose::trim_transparent_gutter(img, cr);
                let sx = win.rect.2.max(1) as f32 / img.width().max(1) as f32;
                let sy = win.rect.3.max(1) as f32 / img.height().max(1) as f32;
                let ls = (
                    ((tw as f32 * sx).round() as i32).max(1),
                    ((th as f32 * sy).round() as i32).max(1),
                );
                (std::borrow::Cow::Owned(trimmed), ls)
            };
            let img: &image::RgbaImage = &img;
            // Downscale FIRST (borrowing sampler, capped at native size), THEN round the
            // corners at thumb scale. The old order cloned + corner-rounded the full-res
            // capture (a ~30 MB copy + full-res pass per window, at launch) only to throw
            // those pixels away — and DynamicImage::thumbnail even UPSCALED windows
            // smaller than the 2560x1600 box, retaining more bytes than the picker can
            // ever draw (it never renders above native logical size). Transparency is
            // preserved (the picker draws over the wallpaper, so translucent windows
            // preview see-through). Stored as an in-memory handle (no file I/O).
            let (w, h) = (img.width().max(1), img.height().max(1));
            let ratio = (2560.0 / w as f32).min(1600.0 / h as f32).min(1.0);
            let (tw, th) = (
                ((w as f32 * ratio).round() as u32).max(1),
                ((h as f32 * ratio).round() as u32).max(1),
            );
            let thumb = if (tw, th) == (w, h) {
                img.clone()
            } else {
                image::imageops::thumbnail(img, tw, th)
            };
            // Scale the logical radius to the thumb's pixels.
            let r = (radius * (tw as f32 / win.rect.2.max(1) as f32)).round() as u32;
            let finished = crate::compose::finish_window(thumb, r, true);
            let handle = widget::image::Handle::from_rgba(
                finished.width(),
                finished.height(),
                finished.into_raw(),
            );
            v.push(WindowThumb {
                rect: win.rect,
                id: win.id.clone(),
                title: win.title.clone(),
                handle,
                layout_size,
            });
        }
        if !v.is_empty() {
            out.insert(name.clone(), v);
        }
    }
    out
}

/// Per-session capture-scene acquisition — factored out of `App::init` (pure code
/// motion; behaviour is byte-identical to the original inline blocks). It (1) spawns
/// the background window pre-capture thread into a `precapture` slot the UI polls
/// each loading tick, and (2) grabs the frozen full-output snapshots.
///
/// `active` is `!settings_only && !preview_mode`: those launches skip the capture
/// overlays entirely, so they pay for neither the pre-capture nor the snapshot.
/// `want_cursor`/`want_freeze` are the persisted `capture_cursor`/`freeze`
/// settings; `radius` the theme corner radius for the window thumbs.
/// Grab every output's frozen snapshot and wrap each into a [`FrozenOutput`]
/// (one shared allocation backs the crop source + the display handle). Factored
/// out of `acquire_scene` so BOTH the synchronous Linux path and the deferred
/// macOS thread build the map identically. `want_cursor` paints the pointer into
/// the flats exactly as before (the freeze-with-wallpaper region/monitor crop
/// relies on that painted cursor).
fn grab_frozen_flats(want_cursor: bool) -> HashMap<String, FrozenOutput> {
    crate::screenshot::all_outputs(want_cursor)
        .into_iter()
        .map(|(name, (img, logical_pos, logical_size))| {
            // One allocation backs both the crop source and the display
            // handle (the old byte-clone doubled ~30 MB per monitor).
            let img = std::sync::Arc::new(img);
            let handle = shared_rgba_handle(&img);
            (
                name,
                FrozenOutput {
                    img,
                    handle,
                    logical_pos,
                    logical_size,
                },
            )
        })
        .collect()
}

/// Resolve the window-picker background wallpaper PER OUTPUT (keyed by output name,
/// matching [`OutputState::name`]) so no full-size decode/grab lands on the UI thread
/// (DRAGON-195).
///
/// - **macOS**: for each live output, grab the TRUE displayed wallpaper via
///   ScreenCaptureKit (`platform::mac::capture_wallpaper`) — this handles dynamic
///   `.heic`, per-Space, solid-color AND per-monitor wallpapers, which the
///   file-decode path (rejected HEIC -> None -> dark gray) could not. An output
///   whose grab misses (permission / empty frame) is simply absent from the map
///   and falls back to the dark picker fill. Each grab is an SCK call, and SCK
///   SERIALIZES internally — so on mac this runs on its OWN deferred thread kicked
///   AFTER the launch-critical frozen-flats grab (DRAGON-200), NOT joined into the
///   precapture, so the region overlay's still is never delayed by it. The window
///   picker shows its dark fill until the wallpaper lands a beat later via
///   `WallpaperReady` (the picker is not the initial region view, so that's fine).
/// - **Linux (and any non-mac)**: keep the historical behavior — `detect()`
///   returns a single desktop-picture path; decode it once (through the shared
///   memo) and associate that ONE wallpaper with every output, so each output's
///   picker still shows the (single) wallpaper exactly as before.
fn resolve_wallpaper_handles(
    wallpaper: Option<std::path::PathBuf>,
) -> HashMap<String, std::sync::Arc<image::RgbaImage>> {
    let mut out: HashMap<String, std::sync::Arc<image::RgbaImage>> = HashMap::new();
    #[cfg(target_os = "macos")]
    {
        // The desktop-picture FILE path is irrelevant on mac: SCK grabs the real
        // rendered wallpaper per display.
        let _ = wallpaper;
        for desc in crate::screenshot::output_descs() {
            if let Some(img) = crate::platform::mac::capture_wallpaper(&desc.name) {
                out.insert(desc.name, std::sync::Arc::new(img));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // One detected wallpaper, decoded once, shared across every output (the
        // pre-DRAGON-195 single-handle behavior, now keyed per output).
        if let Some(px) = wallpaper.and_then(|p| crate::wallpaper::decode_wallpaper(&p)) {
            for desc in crate::screenshot::output_descs() {
                out.insert(desc.name, px.clone());
            }
        }
    }
    out
}

/// Spawn the window pre-capture (gather toplevels + per-window screencopy/SCK grabs
/// -> [`build_window_thumbs`], the picker wallpaper on Linux) on a DEDICATED OS thread
/// and deposit the [`PrecaptureResult`] into `slot`. Extracted from [`acquire_scene`]
/// (DRAGON-204) so it can run EITHER at launch (window-mode launch) OR lazily the first
/// time the user switches into window mode — the picker's loading spinner covers the
/// wait in both cases. Byte-identical work either way.
///
/// The launch-locked cursor is NOT captured here anymore (DRAGON-213): this thread can
/// land mid-selection (a region/monitor/scan launch defers it until the user switches
/// into window mode), so locking the pointer here recorded a STALE position. The cursor
/// now rides its own dedicated launch thread in [`acquire_scene`] (fired at launch,
/// drained into `frozen_cursor` via `CursorReady`).
fn spawn_window_precapture(
    slot: PrecaptureSlot,
    want_freeze: bool,
    wallpaper: Option<std::path::PathBuf>,
    radius: f32,
) {
    let wp = wallpaper;
    std::thread::spawn(move || {
        // Resolve the picker wallpaper PER OUTPUT off the UI thread so window
        // mode's background never blocks the first render (DRAGON-195). On Linux
        // the single detected wallpaper file is decoded once (through the shared
        // memo, so the capture-time composite reuses these exact pixels) and
        // associated with every output — cheap, so it rides HERE, joined into the
        // precapture tuple exactly as before. On macOS each output's wallpaper is
        // an SCK grab, and SCK serializes internally; if it ran here it would
        // contend with the launch-critical frozen-flats grab and delay the region
        // still (DRAGON-200), so on mac it is DEFERRED to its own thread below and
        // this tuple carries an EMPTY wallpaper map (drained later via
        // `WallpaperReady`).
        #[cfg(not(target_os = "macos"))]
        let wp_thread = std::thread::spawn(move || resolve_wallpaper_handles(wp));
        #[cfg(target_os = "macos")]
        let _ = wp;
        let groups = crate::platform::compositor::list_toplevels();
        // Capture only the ACTIVE-workspace toplevels — the only ones any consumer can
        // reach (the picker thumbs iterate `groups`; the freeze recomposite looks its ids
        // up from `groups` too). Capturing every enumerated toplevel did a full-res
        // screencopy per hidden/other-workspace window and retained pixels nothing could
        // ever read. Skipping the call entirely on an empty desktop also skips its
        // toplevel-stability wait.
        let ids: Vec<String> =
            groups.values().flatten().map(|t| t.id.clone()).collect();
        let raw = if ids.is_empty() {
            HashMap::new()
        } else {
            crate::screenshot::windows(&ids)
        };
        // Linux: join the cheap single-file decode into the tuple (byte-identical
        // to before). macOS: the tuple carries an empty map; the real per-output
        // wallpaper lands later via the deferred thread + `WallpaperReady`.
        #[cfg(not(target_os = "macos"))]
        let wallpaper_px = wp_thread.join().unwrap_or_default();
        #[cfg(target_os = "macos")]
        let wallpaper_px: HashMap<String, std::sync::Arc<image::RgbaImage>> = HashMap::new();
        let origin = groups
            .values()
            .flatten()
            .find(|w| w.active)
            .map(|w| w.id.clone());
        let windows = build_window_thumbs(&groups, &raw, radius);
        // Keep the frozen scene: full-res per-window pixels + flattened geometry/z-order, so
        // a freeze capture can recomposite from the launch instant (see PrecaptureResult).
        let toplevels: Vec<_> = groups.values().flatten().cloned().collect();
        // Retain the per-window pixels only when freeze can actually consume them (every
        // reader is gated on `freezing()`, which requires the freeze setting): with freeze
        // off the map is dead weight (~10-30 MB per window, for the whole session). If
        // freeze is toggled on mid-session the existing fallbacks cover the gap until the
        // next launch (flat-snapshot crop for region/monitor, live grab for a window).
        let raw = if want_freeze { raw } else { HashMap::new() };
        if let Ok(mut g) = slot.lock() {
            *g = Some((windows, origin, wallpaper_px, raw, toplevels));
        }
    });
}

/// Whether the launch-time window pre-capture should run (DRAGON-204). The window
/// pre-capture (gather + per-window screencopy/SCK grabs for the picker thumbnails)
/// costs ~1s of SCK-serialized work that ONLY window mode consumes — a region /
/// monitor / scan launch never touches it, so running it at launch just blocks the
/// overlay from becoming visible. Gate it on a WINDOW-mode launch; every other launch
/// defers it to the first switch into window mode (kicked lazily, spinner-covered).
/// Pure so the gating is unit-testable without the App.
fn launch_precapture_runs(active: bool, mode: Mode) -> bool {
    active && mode == Mode::Window
}

fn acquire_scene(
    active: bool,
    launch_mode: Mode,
    want_cursor: bool,
    want_freeze: bool,
    wallpaper: Option<std::path::PathBuf>,
    radius: f32,
) -> (PrecaptureSlot, HashMap<String, FrozenOutput>, FrozenSlot, WallpaperSlot, CursorSlot) {
    // The window pre-capture runs on a DEDICATED OS thread (never the UI thread) and
    // deposits its result into a shared slot the UI polls each loading tick. It costs
    // ~1s of SCK-serialized work that ONLY window mode needs, so DRAGON-204 defers it
    // OFF the launch critical path unless this IS a window-mode launch — a region /
    // monitor / scan launch kicks it lazily on the first switch into window mode
    // (`SetMode(Window)`), showing the picker's loading spinner until it lands.
    let precapture: PrecaptureSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    // `--settings` and `--preview` skip the whole capture-overlay path, so `active` is
    // false and nothing is spawned; a non-window capture launch skips it too (lazy).
    if launch_precapture_runs(active, launch_mode) {
        spawn_window_precapture(precapture.clone(), want_freeze, wallpaper, radius);
    }
    // DRAGON-213: lock the pointer sprite AT LAUNCH on its OWN dedicated thread — before
    // the user can move toward / click in the overlay. The old lock rode the window
    // pre-capture (DRAGON-204), which a region/monitor/scan launch defers until the user
    // switches into window mode, so the "launch-locked" cursor was actually locked
    // mid-selection at a stale position (the DRAGON-213 bug). This thread is small and
    // fast (one cursor screencopy on Linux / an NSEvent read on macOS), opens its own
    // connection, and never touches the init thread — so it preserves DRAGON-212's
    // launch-speed win (the overlay still maps immediately). Only when the scene is
    // active AND "Preserve mouse cursor" is on; otherwise the slot stays `None` and no
    // drain poll arms.
    let cursor_slot: CursorSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    if active && want_cursor {
        let slot = cursor_slot.clone();
        crate::util::timing_mark("acquire_scene: cursor capture (kick DEDICATED thread)");
        std::thread::spawn(move || {
            let cur = crate::screenshot::capture_cursor();
            crate::util::timing_mark("acquire_scene: cursor capture (dedicated thread done)");
            if let Ok(mut g) = slot.lock() {
                *g = Some(cur);
            }
        });
    }
    // Freeze pixels: snapshot every output NOW (before our overlay maps), so
    // selection happens over a still image and a playing video stops moving.
    // We also need this clean pre-overlay snapshot to scan codes from (the live
    // screen would have our dimmed overlay over it), so grab it for scanning too.
    // Always grab the clean pre-overlay snapshot so freeze + the QR/OCR scanners
    // can be toggled on/off live (they apply on the redraw after the settings
    // panel closes); the freeze display still gates on `self.freeze`.
    let frozen_slot: FrozenSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    // macOS (DRAGON-200): the per-output picker wallpaper is resolved via SCK, which
    // serializes internally — so it must NOT run alongside the launch-critical
    // frozen-flats grab. It lands here and the UI drains it (`WallpaperReady`) a beat
    // after the region still is ready; the window picker shows its dark fill until
    // then (acceptable — the picker isn't the initial region view). Empty until the
    // deferred grab posts; Linux never uses this slot (its cheap single-file decode
    // stays joined into the precapture tuple, byte-identical).
    let wallpaper_slot: WallpaperSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    // macOS (DRAGON-148 option C): DEFER the flats grab off the init thread so the
    // region overlay maps IMMEDIATELY against the live (dimmed) screen instead of
    // after the ~300ms full-output snapshot. The grab runs on its own thread and
    // deposits into `frozen_slot`; the UI drains it (`CaptureMsg::FrozenReady`) and
    // redraws against the still. `init` returns an EMPTY `frozen`. This shifts the
    // "frozen instant" ~300ms later than the keypress, which CLAUDE.md allows (the
    // freeze must be ready before COMMIT, not before the overlay). Kicked as early
    // as init can, before the rest of init runs, to keep the deferral short.
    #[cfg(target_os = "macos")]
    let frozen: HashMap<String, FrozenOutput> = {
        // Empty here; the flats land later via the deferred thread + `FrozenReady`.
        if active {
            let slot = frozen_slot.clone();
            let wp_slot = wallpaper_slot.clone();
            crate::util::timing_mark("acquire_scene: frozen all_outputs (kick DEFERRED thread)");
            std::thread::spawn(move || {
                let flats = grab_frozen_flats(want_cursor);
                crate::util::timing_mark("acquire_scene: frozen all_outputs (deferred thread done)");
                if let Ok(mut g) = slot.lock() {
                    *g = Some(flats);
                }
                // Only NOW (the launch-critical still is ready and drainable) resolve
                // the per-output picker wallpaper — same thread, so its SCK grabs are
                // strictly AFTER the frozen flats and can never contend with them
                // (DRAGON-200). The file path is irrelevant on mac (SCK grabs the real
                // rendered wallpaper per display), so pass None.
                crate::util::timing_mark("acquire_scene: wallpaper resolve (begin, after flats)");
                let wp = resolve_wallpaper_handles(None);
                crate::util::timing_mark("acquire_scene: wallpaper resolve (done)");
                if let Ok(mut g) = wp_slot.lock() {
                    *g = Some(wp);
                }
            });
        }
        HashMap::new()
    };
    // Linux (and any non-mac): DRAGON-212 DEFERS the screencopy flats grab off the
    // init/main thread (like macOS above, flats only — the Linux wallpaper rides the
    // precapture tuple, not here), so the layer-shell overlay maps IMMEDIATELY against the
    // live screen instead of ~300ms later; the still lands via `FrozenReady`. Safe to
    // thread: `all_outputs` opens its OWN wayland connection (`connect_to_env`) and
    // screencopy already runs off-thread for window capture. `init` returns an EMPTY map.
    #[cfg(not(target_os = "macos"))]
    let frozen: HashMap<String, FrozenOutput> = {
        if active {
            let slot = frozen_slot.clone();
            crate::util::timing_mark("acquire_scene: frozen all_outputs (kick DEFERRED thread)");
            std::thread::spawn(move || {
                let flats = grab_frozen_flats(want_cursor);
                crate::util::timing_mark("acquire_scene: frozen all_outputs (deferred thread done)");
                if let Ok(mut g) = slot.lock() {
                    *g = Some(flats);
                }
            });
        }
        HashMap::new()
    };
    (precapture, frozen, frozen_slot, wallpaper_slot, cursor_slot)
}

struct OutputState {
    output: OutputHandle,
    id: window::Id,
    name: String,
    logical_pos: (i32, i32),
    logical_size: (u32, u32),
    /// The output's point→pixel buffer scale (physical / logical), COSMIC integer OR
    /// fractional. Cached into `preview_output_scale` when a capture picks this output,
    /// so the windowed preview opens at the capture's true on-screen (logical) size on
    /// scaled displays (DRAGON-221). `1.0` on 1× outputs. Linux-only; macOS derives the
    /// backing scale live from `NSScreen` (`platform::mac::scale_for`).
    #[cfg(target_os = "linux")]
    scale: f32,
    /// macOS (DRAGON-204): whether `place_overlay` has raised this overlay to the shielding
    /// level and framed it to the full display. The overlay is CREATED clamped below the
    /// menu bar (winit's AlwaysOnTop level), so it renders TRANSPARENT (empty) until this is
    /// set — the clamp-then-reframe jump happens on an invisible window, never seen. Set on a
    /// successful placement AND when placement gives up (so a never-matched overlay still
    /// draws). Interior-mutable because `configure_overlay` observes placement behind `&self`.
    #[cfg(target_os = "macos")]
    placed: std::cell::Cell<bool>,
}

/// A pre-captured window thumbnail (screencopy at launch) + its global rect and
/// stable toplevel identifier (used to capture the window's pixels on click).
#[derive(Clone, Debug)]
pub struct WindowThumb {
    rect: WinRect,
    id: String,
    /// Toplevel title (may be empty), used to name window captures.
    title: String,
    /// In-memory RGBA handle — no PNG encode/decode round-trip (fast to render).
    handle: widget::image::Handle,
    /// The thumbnail's logical `(w, h)` for the picker slot. Normally equals
    /// `(rect.2, rect.3)`; on macOS a window with a dead transparent gutter
    /// (DRAGON-190) has its thumbnail trimmed, so the slot is sized to the TRIMMED
    /// content while `rect` stays the raw frame the capture re-derives scale from.
    layout_size: (i32, i32),
}

/// Result the background window pre-capture thread deposits; the UI polls it.
/// Pre-capture result, filled by the background thread: window thumbnails per
/// output, the origin (active) window id, and the wallpaper pre-resolved to raw
/// pixels PER OUTPUT (keyed by output name) so window mode doesn't pay a full-size
/// image decode/grab on the UI thread and each display's picker shows its own
/// wallpaper (DRAGON-195).
type PrecaptureResult = (
    HashMap<String, Vec<WindowThumb>>,
    Option<String>,
    HashMap<String, std::sync::Arc<image::RgbaImage>>,
    // The frozen scene's per-window full-res pixels (by toplevel id) + the flattened toplevel
    // geometry/z-order, so a freeze capture can recomposite windows-over-black (region/monitor,
    // no wallpaper) or a single decorated window from the launch instant instead of the live screen.
    HashMap<String, image::RgbaImage>,
    Vec<crate::platform::compositor::Toplevel>,
);
type PrecaptureSlot = std::sync::Arc<std::sync::Mutex<Option<PrecaptureResult>>>;

/// Shared slot the DEDICATED launch cursor grab fills (DRAGON-213). The
/// launch-locked pointer sprite MUST be locked at LAUNCH — before the user
/// interacts with the overlay — so it rides its OWN thread kicked the instant
/// `acquire_scene` runs, NOT the deferred flats (DRAGON-212) nor the lazy window
/// pre-capture (DRAGON-204), either of which lands mid-selection and would lock a
/// stale position. The thread deposits its `Option<CursorSprite>` (`None` = no
/// pointer on any output) here; the UI drains it (`CaptureMsg::CursorReady` ->
/// `frozen_cursor`). Outer `None` = still in flight. Kicked only when the scene is
/// active AND "Preserve mouse cursor" is on, else it stays `None` forever (no poll).
type CursorSlot = std::sync::Arc<std::sync::Mutex<Option<Option<crate::screenshot::CursorSprite>>>>;

/// Shared slot the deferred frozen-flats grab fills (DRAGON-148 option C, macOS):
/// on mac the full-output snapshot grab is moved OFF the init thread so the region
/// overlay maps immediately against the live screen; the grab lands here and the UI
/// drains it (fills `self.frozen`, redraws against the still). `None` while the grab
/// is in flight; `Some` once it's ready. Linux keeps the synchronous grab (fast
/// screencopy) and never uses this slot.
type FrozenSlot = std::sync::Arc<std::sync::Mutex<Option<HashMap<String, FrozenOutput>>>>;

/// Shared slot the DEFERRED per-output picker wallpaper resolution fills (DRAGON-200,
/// macOS): each display's wallpaper is an SCK grab, and SCK serializes internally, so
/// the resolution runs on the frozen-flats deferred thread AFTER the launch-critical
/// still is ready (never contending with it). The pixels land here and the UI drains
/// them (`CaptureMsg::WallpaperReady` -> `wallpaper_handles`). `None` until the grab
/// posts. Linux keeps its cheap single-file decode joined into the precapture tuple
/// and never uses this slot (it stays permanently `None` there).
type WallpaperSlot =
    std::sync::Arc<std::sync::Mutex<Option<HashMap<String, std::sync::Arc<image::RgbaImage>>>>>;

/// A frozen full-output snapshot (freeze-pixels mode): the pixels (for cropping
/// on capture) + a display handle (for the overlay background) + the output's
/// logical geometry (so we can map a global region even after teardown clears
/// the live output list). `img` and `handle` SHARE one pixel allocation (see
/// [`shared_rgba_handle`]) — a 5120x1440 output is ~30 MB, so the old byte-cloned
/// handle doubled every monitor's snapshot for the whole session.
struct FrozenOutput {
    img: std::sync::Arc<image::RgbaImage>,
    handle: widget::image::Handle,
    logical_pos: (i32, i32),
    logical_size: (i32, i32),
}

/// An iced image Handle that shares `img`'s pixel allocation instead of cloning it:
/// the Arc keeps the pixels alive for as long as the handle (or any clone iced keeps)
/// needs them, `Bytes::from_owner` wraps the ref zero-copy.
fn shared_rgba_handle(img: &std::sync::Arc<image::RgbaImage>) -> widget::image::Handle {
    struct Px(std::sync::Arc<image::RgbaImage>);
    impl AsRef<[u8]> for Px {
        fn as_ref(&self) -> &[u8] {
            self.0.as_raw()
        }
    }
    widget::image::Handle::from_rgba(
        img.width(),
        img.height(),
        bytes::Bytes::from_owner(Px(img.clone())),
    )
}

/// Turn the pre-capture's per-output wallpaper PIXELS into per-output ready-to-upload
/// HANDLES (DRAGON-195), each sharing the source Arc's allocation via
/// [`shared_rgba_handle`] (no decode, no byte clone). Keyed by output name, matching
/// [`OutputState::name`]; an output absent from the input is absent from the output
/// (the picker falls back to the dark fill for it).
fn wallpaper_handles_from_px(
    px: HashMap<String, std::sync::Arc<image::RgbaImage>>,
) -> HashMap<String, widget::image::Handle> {
    px.into_iter()
        .map(|(name, img)| (name, shared_rgba_handle(&img)))
        .collect()
}

/// Whether the precapture drain (`LoadingTick`) should assign the wallpaper map it
/// carries into `wallpaper_handles` (DRAGON-200). On Linux the precapture always
/// carries the real (possibly-empty) map, so it always assigns — byte-identical to
/// the pre-DRAGON-200 behavior. On macOS the wallpaper is resolved on a DEFERRED
/// thread and drained via `WallpaperReady`, so the precapture map is an empty
/// placeholder that must NOT clobber an already-drained deferred wallpaper. Pure so
/// the "don't overwrite deferred pixels with the empty placeholder" invariant is
/// unit-testable without the App.
#[cfg(target_os = "macos")]
fn precapture_should_assign_wallpaper<T>(precapture_map: &HashMap<String, T>) -> bool {
    // On mac the placeholder is always empty; guarding on emptiness also means a
    // future inline mac wallpaper (non-empty) would still win, never silently lost.
    !precapture_map.is_empty()
}

/// Live microphone-test state, present only while the test dialog is open. A
/// background ffmpeg streams raw PCM from the chosen mic; a reader thread reduces it
/// to a rolling peak envelope (0..1, the same dBFS->norm scale as the meters) in
/// `shared`, which the waveform canvas reads directly each vsync frame.
struct MicTest {
    /// ffmpeg capture process — explicitly killed when the dialog closes.
    child: std::process::Child,
    /// Reader thread's rolling envelope of `(clean, raw)` columns (oldest..newest) plus
    /// the total columns ever produced (monotonic, for smooth scrolling). The canvas
    /// holds an Arc clone and reads it directly; the watchdog tick reads the counter.
    shared: std::sync::Arc<
        std::sync::Mutex<(std::collections::VecDeque<crate::audio::clean_mic::MicColumn>, usize)>,
    >,
    /// The produced counter seen at the last watchdog tick (to detect a stall).
    produced: usize,
    /// Consecutive watchdog ticks where `produced` didn't advance after data had started
    /// flowing — the reader/ffmpeg stalled. Drives the auto-restart so a frozen graph
    /// recovers without the user dismissing and reopening the modal.
    stall_ticks: u32,
}

/// Local-time stamp for a capture filename: `YYYY-MM-DD-HH-MM-SS-mmm` (the
/// millisecond suffix keeps rapid captures distinct).
fn capture_timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d-%H-%M-%S-%3f")
        .to_string()
}

/// Which save directory a folder pick targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirTarget {
    Screenshot,
    Recording,
}

/// Windows: no native folder picker wired yet. Stubbed.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
async fn pick_folder() -> Option<std::path::PathBuf> {
    None
}

/// macOS (DRAGON-157): the native `NSOpenPanel` folder browser. The panel is
/// app-modal on the MAIN thread, so run it on a dedicated blocking thread and await
/// the pick over a oneshot — `pick_folder` runs on iced's async executor, and
/// blocking that thread on the main run loop would stall the whole UI. The
/// return-to-surface semantics around this are platform-agnostic (see the callers).
#[cfg(target_os = "macos")]
async fn pick_folder() -> Option<std::path::PathBuf> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::platform::mac::file_panel::pick_folder());
    });
    rx.await.ok().flatten()
}

/// Open the XDG desktop-portal folder picker, returning the chosen directory.
#[cfg(target_os = "linux")]
async fn pick_folder() -> Option<std::path::PathBuf> {
    let files = ashpd::desktop::file_chooser::SelectedFiles::open_file()
        .title("Choose a save folder")
        .directory(true)
        .modal(true)
        .send()
        .await
        .ok()?
        .response()
        .ok()?;
    files.uris().first()?.to_file_path().ok()
}

/// Windows: no native save panel wired yet. Stubbed.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
async fn pick_save_path(_suggested_name: String) -> Option<std::path::PathBuf> {
    None
}

/// macOS (DRAGON-157): the native `NSSavePanel` "Save As" panel, pre-filled with
/// `suggested_name`. App-modal on the MAIN thread, so run it on a dedicated blocking
/// thread and await the pick over a oneshot (same reasoning as `pick_folder`). Used
/// by the preview window's Save As; the overlay-vs-window return semantics around the
/// result are platform-agnostic (see `save_as_dialog` / `SaveAsResult`).
#[cfg(target_os = "macos")]
async fn pick_save_path(suggested_name: String) -> Option<std::path::PathBuf> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::platform::mac::file_panel::pick_save_path(suggested_name));
    });
    rx.await.ok().flatten()
}

/// Open the XDG desktop-portal "save file" picker (pre-filled with `suggested_name`),
/// returning the chosen destination path. Used by the preview window's "Save As".
#[cfg(target_os = "linux")]
async fn pick_save_path(suggested_name: String) -> Option<std::path::PathBuf> {
    let files = ashpd::desktop::file_chooser::SelectedFiles::save_file()
        .title("Save capture as")
        .current_name(suggested_name.as_str())
        .modal(true)
        .send()
        .await
        .ok()?
        .response()
        .ok()?;
    files.uris().first()?.to_file_path().ok()
}

/// Probe whether the ScreenCast portal is reachable with usable source types — for
/// the "Prefer PipeWire" indicator and to gate the portal recording path. Returns
/// `(reachable, source-type bitflags)` (1=monitor, 2=window, 4=virtual).
#[cfg(target_os = "linux")]
async fn probe_pipewire() -> (bool, u32) {
    let Ok(sc) = ashpd::desktop::screencast::Screencast::new().await else {
        return (false, 0);
    };
    match sc.available_source_types().await {
        Ok(t) if !t.is_empty() => (true, t.bits()),
        _ => (false, 0),
    }
}

/// macOS/Windows: no xdg ScreenCast portal — capture is SCK/WGC, so nothing to probe.
#[cfg(not(target_os = "linux"))]
async fn probe_pipewire() -> (bool, u32) {
    (false, 0)
}

/// Slugify a window title / monitor name for use in a filename: lowercase
/// alphanumerics, every other run collapsed to a single `-`, trimmed, capped.
/// Returns an empty string when there's nothing usable.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = true; // leading: suppress a starting '-'
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// Progress + results of the encoder benchmark, shared between the GUI and its
/// worker thread.
#[derive(Default)]
pub struct EncoderBench {
    /// Number of encoders being tested + how many are done.
    pub total: usize,
    pub done: usize,
    /// Friendly label currently under test (for the progress line).
    pub current: String,
    /// One finished encoder's outcome, self-describing so the result row makes the
    /// tested reality visible (DRAGON-163): the ENCODE dimensions the recording plan
    /// resolved to for this monitor + encoder (downscaled where the plan downscales),
    /// and the codec it landed on (h264 vs the >4096 HEVC route).
    pub results: Vec<BenchResult>,
    /// The monitor the run tested (its label already carries the TRUE footprint, e.g.
    /// "Display-2 (6400x3600)"), for the results header.
    pub monitor_label: String,
    pub finished: bool,
}

/// One encoder's benchmark outcome plus what was actually tested (DRAGON-163).
pub struct BenchResult {
    /// Friendly encoder label (e.g. "Apple M1 Pro (VideoToolbox)").
    pub label: String,
    pub score: crate::encode::BenchScore,
    /// The encode dimensions the recording plan resolved to for the tested monitor on
    /// this encoder (after the codec + software real-time caps).
    pub enc_w: u32,
    pub enc_h: u32,
    /// Whether the plan resolved to HEVC (vs H.264) for this encoder at that size.
    pub is_hevc: bool,
}

/// Lazily-resolved encoder state (DRAGON-201). Probing the usable encoders spawns
/// `ffmpeg -encoders` (see `crate::encode::available_encoders`), a cost every launch
/// used to pay synchronously in `App::init` even for a screenshot that never encodes.
/// This holder defers that probe until the encoder list / preferred encoder is FIRST
/// actually read (entering the recording UI, the settings video/Health pages, or
/// starting a recording), so a region/window/scan capture launch never spawns ffmpeg.
///
/// Interior-mutable so the `&self` settings-view accessors can trigger the probe on
/// first read. The resolution is IDENTICAL to the old eager init block — probe the
/// list (dropping hardware under `CCK_HEALTH_FORCE_WARN`), keep the persisted choice
/// when still available else pick+persist the best, then map `record_hardware=off`
/// to software — only WHEN it runs has changed.
#[derive(Default)]
pub struct EncoderResolve {
    /// The probed encoder list, computed once on first access.
    list: std::cell::OnceCell<Vec<crate::encode::EncoderInfo>>,
    /// The resolved preferred-encoder id. `None` until first resolved; thereafter the
    /// live user choice (SetPreferredEncoder / persist apply set it directly).
    preferred: std::cell::RefCell<Option<String>>,
}

impl EncoderResolve {
    /// The probed encoder list, resolving (and caching) it on first access. This is
    /// the ONLY place `available_encoders()` runs, so no launch pays the ffmpeg probe
    /// until the list is genuinely needed.
    fn list(&self) -> &[crate::encode::EncoderInfo] {
        self.list.get_or_init(|| {
            let mut e = crate::encode::available_encoders();
            if std::env::var_os("CCK_HEALTH_FORCE_WARN").is_some() {
                e.retain(|enc| enc.id == "software");
            }
            e
        })
    }

    /// The resolved preferred-encoder id, computing it on first access from the probed
    /// list and the persisted choice (mirroring the old init block byte-for-byte):
    /// keep the saved choice when still available, else pick the best available and
    /// PERSIST it; then honour a legacy `record_hardware=off` by mapping to software.
    fn preferred(&self) -> String {
        if let Some(p) = self.preferred.borrow().as_ref() {
            return p.clone();
        }
        let list = self.list();
        let mut persisted = crate::state::load();
        let base = if list.iter().any(|e| e.id == persisted.preferred_encoder) {
            persisted.preferred_encoder.clone()
        } else {
            let best = list
                .first()
                .map(|e| e.id.clone())
                .unwrap_or_else(|| "software".to_string());
            persisted.preferred_encoder = best.clone();
            crate::state::save(&persisted);
            best
        };
        // The "use hardware encoding" toggle was removed (the Software entry in the
        // encoder picker covers it); honour an old off setting by picking software.
        let resolved = if persisted.record_hardware {
            base
        } else {
            "software".to_string()
        };
        *self.preferred.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// Overwrite the live preferred-encoder id (user picked one, or persist apply
    /// resolved it). Also seeds the cache so a later `preferred()` returns this.
    fn set_preferred(&self, id: String) {
        *self.preferred.borrow_mut() = Some(id);
    }

    /// Whether the ffmpeg-spawning encoder probe has run yet (test-only inspector: the
    /// DRAGON-201 guarantee is that a screenshot launch leaves this `false`).
    #[cfg(test)]
    fn probed(&self) -> bool {
        self.list.get().is_some()
    }
}

pub struct App {
    core: app::Core,
    outputs: Vec<OutputState>,
    mode: Mode,
    kind: Kind,
    delay_idx: usize,
    /// An exact pre-capture countdown from `--countdown <secs>` that overrides the
    /// `delay_idx` preset (so a CLI value like 7 works even though no chip offers it).
    /// Cleared the moment the delay is changed from the UI. `None` = use the preset.
    countdown_override: Option<u64>,
    /// Current region selection in global coords (region mode).
    region: Option<GlobalRect>,
    /// True while the region is being drawn/resized/moved — the Capture button is
    /// hidden until the drag settles (cheaper than repositioning every frame).
    region_dragging: bool,
    /// Manual toolbar nudge (logical px) from dragging it, keyed by output name so each
    /// monitor's toolbar moves independently (DRAGON-207 renders one per monitor); reset
    /// whenever the region changes. Applied only while selecting, never while a capture is
    /// active, so it can't end up in the recorded pixels.
    toolbar_offset: HashMap<String, (f32, f32)>,
    /// Pre-captured window thumbnails per output (window mode).
    windows: HashMap<String, Vec<WindowThumb>>,
    /// The background window pre-capture is still running.
    windows_loading: bool,
    /// Frames to keep the loading overlay up *after* windows are ready, so the
    /// picker renders (and GPU-uploads) behind it and is visible the instant the
    /// overlay lifts — no blank flash between the spinner and the picker.
    window_warmup: u8,
    /// Shared slot the pre-capture thread fills; polled while `windows_loading`.
    precapture: PrecaptureSlot,
    /// Whether the window pre-capture has been kicked yet (DRAGON-204). A window-mode
    /// launch kicks it in `acquire_scene` (true from init); every other launch defers
    /// it, kicking it lazily the FIRST time the user switches into window mode. Guards
    /// against re-spawning the grab on a second switch back into window mode.
    window_precapture_started: bool,
    /// Index into `view::LOADING_MESSAGES`, chosen at random per launch.
    loading_msg: usize,
    /// Which floating button the pointer is over (hover styling).
    hover: Hover,
    /// Monitor mode: the output whose overlay the cursor is currently over (the single
    /// highlighted monitor). Tracked in app state — not per-overlay — because each
    /// overlay is a separate window on macOS and can't rely on cursor-left to un-hover.
    hovered_output: Option<String>,
    /// Whether the region capture group's delay menu is open.
    delay_menu_open: bool,
    /// Active pre-capture countdown (remaining seconds) + the pending capture.
    countdown: Option<u8>,
    pending: Option<Selection>,
    /// Set once the overlay has been torn down and we're waiting (a tick) for it
    /// to clear the screen before grabbing pixels. Consumed by `DoPixelCapture`.
    capturing: Option<Selection>,
    /// DRAGON-216 (Linux only): a window pick pre-opened its preview spinner as a
    /// FOCUS-NEUTRAL layer surface (`KeyboardInteractivity::None`) so it's visible DURING
    /// the off-thread focus-then-grab without stealing the picked window's focus (the
    /// DRAGON-194 invariant — the only focus-neutral primitive cosmic-comp offers; a real
    /// toplevel open always steals focus on this rev). `WindowGrabbed` resolves it per the
    /// preview appearance: OVERLAY mode promotes the same surface to `Exclusive`; WINDOWED
    /// mode swaps it for the real preview window (`swap_neutral_spinner_to_window`). False
    /// on macOS (no layer shell) and for the defocus-sink pick (it opens `Exclusive` on
    /// purpose to BE the focus sink).
    window_spinner_neutral: bool,
    /// DRAGON-221 follow-up (both platforms): a WINDOWED window-pick's cover→window swap
    /// is DEFERRED from `WindowGrabbed` to `present_capture` (ShotSaved), where the
    /// COMPOSED image dims are in hand — the window then opens at its correct size once
    /// (padding/shadow/wallpaper margins change the composed size vs the selection, and a
    /// post-open `window::resize` is not honored on COSMIC). Set when `WindowGrabbed`
    /// would have swapped; consumed by `present_capture`; reset at `begin_capture`.
    windowed_swap_pending: bool,
    /// DRAGON-216 (Linux windowed only): the focus-neutral OVERLAY spinner id kept alive,
    /// still painting its loading cover (`grab_cover_view`), after `WindowGrabbed` swapped
    /// the preview to a real WINDOW — closed on the window's FIRST configure so the window
    /// maps UNDER the cover with no desktop flash between them. `None` at rest.
    grab_overlay_closing: Option<window::Id>,
    /// DRAGON-216 (macOS windowed only): a window pick PRE-OPENED its preview window during
    /// the focus-then-grab, but ORDER-FRONT ONLY (`orderFront:`, opened `visible:false` so
    /// winit's create-time `makeKeyAndOrderFront` never keyed it) — so it's a visible spinner
    /// WITHOUT activating our app or keying the window, leaving the picked window's focus
    /// state (the DRAGON-194 frontmost-verify) undisturbed. `WindowGrabbed` clears this and
    /// re-kicks the preview finalize to take focus for real (Regular policy + activate +
    /// makeKey). While set, `preview_window` opens `visible:false` and the finalize orders
    /// front without stealing focus. Never set off macOS.
    #[cfg(target_os = "macos")]
    mac_preview_preopen: bool,
    /// Settings window UI state (the toplevel window, nav rail, search, …).
    settings: SettingsState,
    /// Permission-checker window UI state (macOS onboarding surface; only ever
    /// opened on macOS — a default empty state on Linux, never minted).
    permissions: permissions::PermissionsState,
    /// Live keyboard-shortcut bindings (`Action -> Shortcut`) — the single source of
    /// truth for key handling and the Keyboard Shortcuts settings page.
    keymap: crate::shortcuts::Keymap,
    /// Show the post-capture preview window instead of immediately saving/copying.
    preview_after_capture: bool,
    /// Set by the "Copy selection" region quick-action (primary+C in region-draw mode):
    /// the in-flight capture must force-copy to the clipboard and finish WITHOUT the
    /// preview, regardless of the persisted `preview_after_capture` / `copy_to_clipboard`
    /// settings. Consumed once by the capture-completion share path.
    copy_selection_pending: bool,
    /// Preview editor appearance: `true` = resizable window, `false` = overlay (setting).
    preview_windowed: bool,
    /// Auto-close the preview editor after a Save / Save As / Copy (setting; default on).
    auto_close_preview: bool,
    /// COSMIC-only: float the windowed preview via a tiling exception (persisted).
    preview_float_cosmic: bool,
    /// Mute other apps' audio while a video preview with sound is playing (restored on close).
    mute_others_during_preview: bool,
    /// Duck the recorded system audio while the mic hears speech (DRAGON-128; persisted).
    duck_system_audio: bool,
    /// Appearance (DRAGON-139): follow the system theme. When true (default) the
    /// override fields below are ignored and the app follows the system; when false
    /// the overrides compose onto the resolved base and apply live + on startup.
    appearance_use_system: bool,
    /// Appearance override: base mode (0 automatic / 1 dark / 2 light). Only used
    /// while `appearance_use_system` is false.
    appearance_mode: u8,
    /// Appearance override: accent colour as sRGB `[r, g, b]` (0..1), or `None` to keep
    /// the base theme's own accent. Only used while `appearance_use_system` is false.
    appearance_accent: Option<[f32; 3]>,
    /// Appearance override: corner-rounding style (0 round / 1 slightly / 2 square).
    /// Only used while `appearance_use_system` is false.
    appearance_roundness: u8,
    /// Region selection box thickness (logical px, 1-8), applied to the viewfinder corner
    /// brackets AND side lines uniformly. Always applies (not gated by system appearance).
    /// DRAGON-209.
    selection_box_thickness: u32,
    /// About (DRAGON-177): whether to show the launch-time update dialog when the
    /// settings-open update check resolves `Available`. Default ON; the About page
    /// toggle "Notify me when an update is available" and the dialog's "Don't remind
    /// me again" checkbox are two views of this one setting.
    notify_updates: bool,
    /// The open post-capture preview window + the capture it's previewing (`None`
    /// while no preview is up).
    preview: Option<preview::PreviewState>,
    /// While a preview with a soundtrack is open, the guard pausing OTHER apps' media
    /// (Spotify/browsers/…). Dropped when the overlay closes → those players resume.
    preview_duck: Option<crate::audio::ducking::OtherAudioDuck>,
    /// The monitor (output + its logical size) the in-flight capture is on — captured
    /// before the overlay (and `self.outputs`) is torn down, so the post-capture preview
    /// can open a fullscreen overlay there and scale the image within it.
    preview_output: Option<(OutputHandle, (u32, u32))>,
    /// The point→pixel backing scale of `preview_output` — the capture output's
    /// physical-pixels-per-logical-point (COSMIC integer OR fractional scaling). Cached
    /// with `preview_output` (before the overlay tears `self.outputs` down) so the
    /// windowed-preview open-fit can divide the capture's PHYSICAL pixels back into the
    /// LOGICAL points it occupied on screen — a scaled COSMIC grab opens at its true
    /// on-screen size, not `scale`× too large (DRAGON-221, the Linux counterpart of the
    /// macOS `NSScreen.backingScaleFactor` used by [`Self::preview_source_scale`]).
    /// Always `1.0` on 1× outputs (every field stays byte-identical there).
    preview_output_scale: f32,
    /// `--preview <file>` launch: the file (and whether it's a video) to open straight
    /// into the preview overlay once an output appears. Taken once consumed.
    startup_preview: Option<(std::path::PathBuf, bool)>,
    /// Whether this is a `--preview` launch — suppresses the capture overlays entirely.
    preview_mode: bool,
    /// Last known settings-window size (logical w, h), persisted so the window
    /// reopens at the size it was closed at (clamped to the monitor).
    settings_size: Option<(u32, u32)>,
    /// Whether an `ffmpeg` binary was found on PATH at launch (recording needs it).
    ffmpeg_available: bool,
    /// Whether `ffprobe` is on PATH (the video preview probes recordings with it).
    ffprobe_available: bool,
    /// Whether tesseract has usable language data — resolved lazily on first Health/
    /// Scanner query (it shells out to `tesseract --list-langs`, so launch never pays).
    tesseract_langs: std::cell::OnceCell<bool>,
    /// Whether a `pactl` binary was found on PATH at launch (audio device
    /// enumeration needs it; otherwise only the system default device is offered).
    /// Unread on macOS (DRAGON-132: mic enumeration gates on ffmpeg there, and there
    /// is no output picker), where it is always false anyway.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pactl_available: bool,
    /// Whether the NVIDIA driver stack was in the post-update NVML "driver/library
    /// version mismatch" state at launch (kernel module ≠ userspace libraries;
    /// NVENC can't initialise until a reboot). Drives the Health-page warning;
    /// recordings fall back via `nvenc_plan` refusing NVENC while it holds.
    nvenc_driver_mismatch: bool,
    /// Include the mouse cursor in captures (persisted; default off).
    capture_cursor: bool,
    /// Keep a window's own transparency in window captures (persisted; default
    /// off → flattened opaque, like cosmic-screenshot's picker look).
    capture_transparency: bool,
    /// Include the wallpaper in region/monitor captures (persisted; default on).
    /// When off, only the windows are composited (transparent/black elsewhere).
    capture_wallpaper: bool,
    /// Window-capture ACTIVE (focused) border colour (persisted; DRAGON-191). `None`
    /// = follow the system accent (resolved at draw time); `Some` = a pinned custom
    /// colour.
    active_border_color: Option<[u8; 4]>,
    /// Window-capture ACTIVE border width (persisted; logical px, 0-10; default 3).
    active_border_width: u32,
    /// Window-capture INACTIVE border colour (persisted; default 0xff414550).
    inactive_border_color: [u8; 4],
    /// Window-capture INACTIVE border width (persisted; logical px, 0-10; default 1).
    inactive_border_width: u32,
    /// Draw the reconstructed drop shadow behind window captures (persisted; default on).
    window_drop_shadow: bool,
    /// Single-window capture focus appearance (persisted; default Active/true): draw
    /// the Active border when true, the Inactive border when false. DRAGON-191.
    window_single_active: bool,
    /// Extra transparency multiplier for window captures (persisted; 0..1). The fraction
    /// of translucent pixels' alpha to remove (1.0 = fully transparent).
    window_transparency_multiplier: f32,
    /// Add a transparent margin around window captures (persisted; default on).
    window_padding: bool,
    /// Margin width (logical px) when `window_padding` is on (persisted; default 8)
    /// + its settings num-input text buffer.
    window_padding_px: NumField<u32>,
    /// Allow more than one overlay instance at once (persisted; default off).
    /// Read at startup to decide whether to take the single-instance lock.
    allow_multiple: bool,
    /// macOS (DRAGON-130): stay resident after a finished session instead of
    /// exiting, so a new capture session can be re-triggered (persisted; default
    /// off). Read by `finish_session` on macOS; Linux keeps the one-shot model
    /// and never consults it.
    resident: bool,
    /// macOS (DRAGON-130): the resident daemon's global "Start Capture" hotkey spec
    /// (e.g. "PrintScreen", "Cmd+Shift+2"); persisted, default "PrintScreen". Edited
    /// on the Shortcuts settings page (macOS-only row); the daemon reads it from disk
    /// at startup. Carried on `App` only to round-trip it through save and drive the
    /// settings row; Linux never registers it (its capture key is a COSMIC shortcut).
    capture_hotkey: String,
    /// macOS (DRAGON-130): the death-pipe babysitter guard held for a capture session
    /// that paused a tiling WM (AeroSpace). Armed once the pause completes
    /// (`seed_overlays_mac`), dropped on session end (`finish_session`/`quit_now` +
    /// `reset_capture_state`); a crash/force-quit closes its pipe → the child restores
    /// the WM anyway. `None` when no tiling WM was paused. See `mac::window`.
    #[cfg(target_os = "macos")]
    aerospace_guard: Option<crate::platform::mac::window::AerospaceGuard>,
    /// macOS (DRAGON-151): the countdown/recording overlays are click-through
    /// (`recreate_active_overlays` set every overlay to mouse passthrough); while
    /// true, `sub_passthrough` polls the pointer against each output's toolbar-chip
    /// rect and re-solidifies just the hovered overlay so the chip stays clickable.
    #[cfg(target_os = "macos")]
    passthrough_active: bool,
    /// macOS (DRAGON-151): the overlay currently made SOLID because the pointer is
    /// over its toolbar chip (`None` = all overlays passthrough).
    #[cfg(target_os = "macos")]
    passthrough_solid: Option<window::Id>,
    /// Opacity of the dim outside the region selection (persisted; default 0.70).
    region_overlay_opacity: f32,
    /// Opacity of the dim + selection lines while a capture is active — counting
    /// down (and, later, recording) (persisted; default 0.70).
    active_overlay_opacity: f32,
    /// Opacity of the dim behind the post-capture preview overlay (persisted; default 0.90).
    preview_overlay_opacity: f32,
    /// Recording frame rate (persisted; default 15) + its live text-field buffer.
    record_fps: NumField<u32>,
    /// Recording target bitrate in Kbps (persisted) + its live text-field buffer.
    record_bitrate_kbps: NumField<u32>,
    /// Max-resolution preset index (persisted) + custom width/height (persisted)
    /// and their text-field buffers. The recording is downscaled to fit.
    record_res_preset: u8,
    record_max_width: NumField<u32>,
    record_max_height: NumField<u32>,
    /// Per-encoder speed/quality preset (persisted). The settings UI shows the one
    /// matching the active encoder; VAAPI has none (driver default). Defaults: NVENC
    /// `p4`, x264 `veryfast`.
    nvenc_preset: String,
    x264_preset: String,
    /// VAAPI `-compression_level` (the real AMD/Intel speed/quality knob); `-1` =
    /// driver default.
    vaapi_compression_level: i32,
    /// Experimental GPU zero-copy capture for PipeWire recordings (persisted; off).
    record_zero_copy: bool,
    /// Video codec choice (persisted): `auto` | `h264` | `hevc`.
    record_codec: String,
    /// Audio→video sync offset in ms (persisted) + its text-field buffer.
    audio_sync_offset_ms: NumField<i32>,
    /// Auto-calibrate the A/V offset from each recording's measured latency.
    audio_sync_auto: bool,
    /// End-to-end calibration base (ms) added on top of each recording's measured
    /// median by the auto-calibration (persisted; set by `--calibrate-sync`) — the
    /// delivery lag the app can't observe live (DRAGON-119).
    av_calibration_base_ms: i32,
    /// Directory recordings save to (persisted; `~` expanded).
    record_dir: String,
    /// Lazily-resolved encoder list + preferred encoder (DRAGON-201). Probing spawns
    /// `ffmpeg -encoders`; deferred to first read so a screenshot launch never pays it.
    /// Read via `encoders()` / `preferred_encoder()`; set via `set_preferred_encoder()`.
    encoders: EncoderResolve,
    /// Running/finished encoder benchmark shared with its worker thread.
    bench: Option<std::sync::Arc<std::sync::Mutex<EncoderBench>>>,
    /// Connected monitors (with their TRUE capture pixel footprint) offered by the
    /// benchmark's monitor dropdown, enumerated once when the settings window opens
    /// (DRAGON-163). Empty on a non-settings launch / without capture permission.
    bench_monitors: Vec<crate::platform::backend::BenchMonitor>,
    /// The dropdown's selected monitor (index into `bench_monitors`). SESSION-ONLY:
    /// the benchmark is a one-off diagnostic, so the pick is not persisted (it defaults
    /// to the largest monitor each time the settings window opens).
    bench_monitor_idx: usize,
    /// Detect QR/barcodes / OCR text in region mode (persisted settings).
    scan_codes: bool,
    scan_text: bool,
    /// Minimum OCR word confidence (0–100) to keep (persisted; the "Text Confidence
    /// Threshold" slider).
    text_confidence: f32,
    /// Whether the `tesseract` OCR binary is available (text scanning needs it).
    tesseract_available: bool,
    /// Latest region QR/barcode scan result (re-run when the region changes).
    code_scan: std::sync::Arc<std::sync::Mutex<Option<Vec<crate::detect::Mark>>>>,
    /// Latest region OCR result (re-run when the region changes).
    text_scan: std::sync::Arc<std::sync::Mutex<Option<Vec<crate::detect::TextWord>>>>,
    /// Whether a QR / an OCR pass is in flight (so we don't queue overlapping ones).
    code_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ocr_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Region the last QR / OCR pass ran for, to re-scan only when it changes.
    last_code_region: Option<(i32, i32, u32, u32)>,
    last_ocr_region: Option<(i32, i32, u32, u32)>,
    /// QR/barcode marks for the current region (the clickable overlay). `marks` is the
    /// live, toggle-filtered set used for the overlay / hover / click.
    code_marks: Vec<crate::detect::Mark>,
    marks: Vec<crate::detect::Mark>,
    /// Index (into `marks`) of the code mark currently hovered, for the tooltip.
    hovered_mark: Option<usize>,
    /// OCR words for the current region (reading order) — a selectable text layer.
    text_words: Vec<crate::detect::TextWord>,
    /// Index of the text word currently hovered (translucent highlight when idle).
    hovered_word: Option<usize>,
    /// Selected OCR word indices (into `text_words`). A drag/shift-click fills a
    /// contiguous range, ctrl-click toggles individuals, ctrl+A selects all; the set is
    /// highlighted and joined (in reading order) when copied.
    text_sel: std::collections::BTreeSet<usize>,
    /// When a right-click opened the text "Copy" menu: its global cursor position.
    text_menu: Option<(i32, i32)>,
    /// When a right-click opened a code's "Copy contents" menu: (mark index, global pos).
    code_menu: Option<(usize, i32, i32)>,
    /// In-progress range selection `(anchor, additive, base)`: the base snapshot lets a
    /// drag recompute `base ∪ range(anchor, cursor)` each move (so additive ctrl+shift
    /// drags stay continuous and shrinking works).
    text_drag: Option<(usize, bool, std::collections::BTreeSet<usize>)>,
    /// In-progress region recording (worker handle), if any.
    recording: Option<crate::record::RecordHandle>,
    /// When the current recording started + its output path (for the chip's
    /// elapsed-time / size readout).
    recording_started: Option<std::time::Instant>,
    recording_path: Option<std::path::PathBuf>,
    /// Pause bookkeeping (DRAGON-111): when the current pause began (`Some` =
    /// paused right now) and the total time spent paused before it. Together
    /// with `recording_started` they yield the RECORDED elapsed time — frozen
    /// while paused — via [`App::recording_elapsed_secs`].
    recording_paused_at: Option<std::time::Instant>,
    recording_paused_accum: std::time::Duration,
    /// Set when the user cancels a recording: the worker is stopped, then the
    /// finalized file is deleted (no save, no notification).
    recording_cancelled: bool,
    /// Where screenshots are saved (persisted; `~` expanded).
    screenshot_dir: String,
    /// Copy a capture to the clipboard when it's at or under the size limit
    /// (persisted; default on) + the limit in MB + its live text-field buffer.
    copy_to_clipboard: bool,
    clipboard_max_mb: NumField<u32>,
    /// Record microphone / system audio with videos (persisted; default off). Only
    /// toggleable in video mode.
    record_mic: bool,
    record_system_audio: bool,
    /// Setting (DRAGON-174): hide the floating recording toolbar on full-screen
    /// captures — when the toolbar can't fit outside the recording area, hide it
    /// instead of placing it in-frame (the tray icon still carries the controls).
    /// Persisted; default off (do not hide). The ONLY thing that hides the toolbar
    /// now: nothing about the tray/icon content depends on it.
    hide_toolbar_fullscreen: bool,
    /// The live status-icon session for this capture (DRAGON-174): raised at capture
    /// start (idle, during selection) and kept for the WHOLE session — the idle icon +
    /// capture menu while not recording, the recording icon + menu once recording
    /// begins — then torn down at `finish_session`. `Some` only when the icon
    /// registered AND no resident/daemon already owns the menu-bar/tray (then the child
    /// relays instead of raising a second icon). A daemon-relay backing is likewise
    /// held here once recording starts.
    tray: Option<crate::tray::TraySession>,
    /// Whether the active tray/daemon control surface REPLACES the in-frame toolbar
    /// (DRAGON-172). Decoupled from `tray.is_some()`: on macOS a daemon relay can be
    /// attached (the daemon menu is live) while the in-frame toolbar STAYS visible in
    /// toolbar-placement mode — both surfaces dispatch the same actions. True only when
    /// the tray OWNS the whole control surface: a raised own status item / the Linux SNI
    /// item, or a daemon relay standing in for an oversized systray-mode capture. Drives
    /// the toolbar-hidden / overlay-click-through paths; `tray.is_some()` still drives
    /// polling (the daemon menu must be drained even when the toolbar shows too).
    tray_hides_toolbar: bool,
    /// Setting: push-to-talk — mute an armed mic during recording except while the
    /// push-to-talk hotkey is held (persisted; default off).
    push_to_talk: bool,
    /// Whether the push-to-talk hotkey is currently held (mic un-muted). Transient;
    /// tracked so key auto-repeat doesn't log spurious toggles.
    ptt_held: bool,
    /// Recording hotkeys via the portal GlobalShortcuts interface — bound once at
    /// the first recording start (DRAGON-109). Delivers PTT press/RELEASE + stop
    /// focus-free; where the desktop doesn't ship the interface (COSMIC today),
    /// `dead` flips and the keyboard paths stand unchanged.
    hotkeys: Option<crate::platform::global_shortcuts::Hotkeys>,
    /// Live perceived level (0..1) of each channel, for the on-button volume
    /// meters. Polled from the meter files whenever a meter is running.
    mic_level: f32,
    sys_level: f32,
    /// Live level for the Input Sensitivity bar: the voice gate's DECISION level (denoised,
    /// pre-gate/gain) from the mic-test capture, so the bar matches what the threshold gates on.
    /// Separate from `mic_level` (the raw on-button meter), updated while the capture runs.
    sens_level: f32,
    /// Mic level source while the mic is armed (video mode + on): the FULL input
    /// chain (the same clean_mic capture the mic test uses), so the button meter
    /// shows the processed voice that would be recorded — noise reduction, gate,
    /// auto-gain and all — not the raw device level.
    mic_chain: Option<MicTest>,
    /// System-audio level meter (raw ffmpeg RMS sidecar — no filter chain applies
    /// to system audio), alive whenever that channel is armed. `PR_SET_PDEATHSIG`
    /// keeps it from orphaning if we exit.
    sys_meter: Option<std::process::Child>,
    /// macOS (DRAGON-130 Bug B): the armed-idle system-audio METERING capture. On macOS
    /// there is no pulse-monitor to run an ffmpeg meter sidecar from, so `sys_meter`
    /// stays `None` and the speaker button would sit flat while armed-but-not-recording.
    /// This is a metering-only `MonitorCapture` (an audio-only SCK stream) alive ONLY in
    /// the armed-idle window: its chunks are discarded (`try_send` drops them) and it
    /// publishes the sys RMS via `publish_sys_level` on its own thread, exactly like the
    /// recording capture does. It is STOPPED before a recording's owned capture starts so
    /// the two never fight over the single SCK system-audio stream.
    #[cfg(target_os = "macos")]
    sys_idle_meter: Option<(
        crate::audio::capture::MonitorCapture,
        std::sync::mpsc::Receiver<crate::audio::capture::CaptureChunk>,
    )>,
    /// Apply real-time noise reduction (RNNoise) to the captured mic.
    /// Persisted; default on.
    noise_reduction: bool,
    /// Chosen mic input source (PulseAudio name); empty = system default (auto).
    /// Persisted; pushed into `crate::audio::config::set_mic_source` so recordings + meters
    /// capture from it.
    mic_device: String,
    /// Enumerated input sources `(name, description)` for the settings dropdown,
    /// refreshed when the settings window opens. Monitors excluded.
    mic_devices: Vec<(String, String)>,
    /// Dropdown labels `["System (automatic)", <descriptions>…]`, rebuilt with
    /// `mic_devices` so the dropdown can borrow a stable slice.
    mic_device_labels: Vec<String>,
    /// Cancel speaker audio bleeding into the mic (WebRTC AEC3). Persisted; default on.
    echo_cancellation: bool,
    /// Chosen speaker sink (PulseAudio name) whose monitor is the echo-cancellation
    /// reference; empty = system default. Persisted.
    speaker_device: String,
    /// Enumerated output sinks `(name, description)` for the speaker dropdown,
    /// refreshed when the settings window opens.
    speaker_devices: Vec<(String, String)>,
    /// Dropdown labels `["System (automatic)", <descriptions>…]` for speakers.
    speaker_device_labels: Vec<String>,
    /// Voice-gate ("Input Sensitivity") threshold mode. Persisted; default automatic.
    input_sensitivity_auto: bool,
    /// Manual voice-gate threshold, 0..1 on the meter dBFS scale. Persisted.
    input_sensitivity: f32,
    /// Automatic Gain Control (AGC2). Persisted; default on.
    auto_gain: bool,
    /// Advanced Voice Activity (earshot neural VAD). Persisted; default on.
    advanced_vad: bool,
    /// Live microphone-test capture (InputProcessor → rolling waveform + the bar's decision
    /// level). Runs whenever the test modal is open OR the Audio page's manual sensitivity bar
    /// is showing — decoupled from the modal's visibility (`mic_test_modal_open`).
    mic_test: Option<MicTest>,
    /// Whether the mic-test MODAL is shown. The capture (`mic_test`) can run without it (to feed
    /// the live sensitivity bar), so the modal's visibility is tracked separately.
    mic_test_modal_open: bool,
    /// Window corner radius (logical px), the default the window-decoration seam
    /// falls back to when it has no radius of its own. DRAGON-186 Phase 5 moved the
    /// active/inactive border colour + width off the App struct into
    /// `crate::decoration` (resolved per platform from JankyBorders / the COSMIC
    /// theme), so those fields no longer live here.
    window_radius: f32,
    /// The user's frosted-glass ("liquid glass") config (cosmic-settings →
    /// Appearance → Style), read ONCE at launch (DRAGON-217). Drives the two
    /// toplevel WINDOWS' translucent chrome (`theme::frost_color`) so the
    /// compositor blur enrolled on them shows through. `None` off COSMIC / when
    /// frosted windows are off → fully-opaque chrome, today's look.
    glass: Option<crate::app::theme::GlassConfig>,
    /// Per-output wallpaper handles, pre-resolved to ready-to-upload handles by the
    /// background pre-capture thread, so entering window mode never blocks the UI
    /// thread decoding/grabbing a full-size image. Keyed by output name (the same
    /// name as [`OutputState::name`]). Empty until the pre-capture finishes (the
    /// loading overlay covers it); a missing entry for an output falls back to the
    /// dark picker fill. On macOS each output's entry is the true displayed
    /// wallpaper grabbed per-display via ScreenCaptureKit (`.heic`/dynamic /
    /// per-Space / solid-color safe); on Linux the single detected wallpaper is
    /// associated with every output (behaviorally identical to the old single
    /// handle, DRAGON-195).
    wallpaper_handles: HashMap<String, widget::image::Handle>,
    /// The window that was focused when we launched — re-activated before the
    /// annotation tool opens, so it appears on the monitor we started on.
    origin_window: Option<String>,
    /// Freeze the screen while selecting (persisted; default off).
    freeze: bool,
    /// Text for the preview's "Custom text" covermark (persisted).
    covermark_text: String,
    /// Remembered covermark zoom, applied when a covermark is chosen (persisted).
    covermark_zoom: f32,
    /// Remembered covermark opacity (0..1), applied when a covermark is chosen. Also the
    /// fallback for an option with no per-option pref yet.
    covermark_opacity: f32,
    /// Per-option remembered (zoom, opacity), keyed by `CovermarkKind::pref_key` — each
    /// covermark option keeps its own last-used scale + opacity (persisted).
    covermark_prefs: HashMap<String, (f32, f32)>,
    /// Per-output frozen snapshots. Grabbed on a DEFERRED thread on BOTH platforms
    /// (DRAGON-148 option C / DRAGON-212) and landed here via `CaptureMsg::FrozenReady` —
    /// empty until then, so every reader handles the not-ready window (see `freezing`).
    frozen: HashMap<String, FrozenOutput>,
    /// Deferred frozen-flats grab slot (DRAGON-148 / DRAGON-212). `None` while in flight,
    /// then drained into `frozen` on `FrozenReady`.
    frozen_slot: FrozenSlot,
    /// The deferred flats grab hasn't landed yet (macOS). Drives the poll
    /// subscription that drains `frozen_slot`; always false on Linux (synchronous
    /// grab, ready before `init` returns).
    frozen_pending: bool,
    /// Deferred per-output picker wallpaper slot (macOS, DRAGON-200). `None` while
    /// the grab is in flight (it runs AFTER the frozen flats, on the same deferred
    /// thread, so SCK never contends with the launch-critical still), then drained
    /// into `wallpaper_handles`. On Linux this is always empty (the cheap single-file
    /// decode rides the precapture tuple instead).
    wallpaper_slot: WallpaperSlot,
    /// The deferred wallpaper grab hasn't landed yet (macOS). Drives the poll
    /// subscription that drains `wallpaper_slot`; always false on Linux.
    wallpaper_pending: bool,
    /// Dedicated launch cursor grab slot (DRAGON-213). `None` while the grab is in
    /// flight, then `Some(Option<CursorSprite>)` once its own launch thread posts
    /// (inner `None` = no pointer on any output). Drained into `frozen_cursor` via
    /// `CursorReady` (and at commit). Both platforms.
    cursor_slot: CursorSlot,
    /// The dedicated launch cursor grab hasn't been drained yet (DRAGON-213). Drives
    /// the `sub_cursor_ready` poll; armed only when the scene is active AND "Preserve
    /// mouse cursor" is on, on both platforms.
    cursor_pending: bool,
    /// The frozen scene's per-window pixels (by toplevel id) + flattened geometry/z-order, captured
    /// at launch. Lets a freeze capture recomposite windows-over-black (region/monitor, no wallpaper)
    /// or a single decorated window from the launch instant. Empty until the precapture posts.
    frozen_win_px: HashMap<String, image::RgbaImage>,
    frozen_toplevels: Vec<crate::platform::compositor::Toplevel>,
    /// The previously-active window's pixels (by toplevel id), grabbed SYNCHRONOUSLY
    /// just BEFORE our overlay activation (`gain_focus`) fires. On macOS, activating our
    /// accessory process deactivates whatever app was frontmost, so its window re-renders
    /// in the INACTIVE appearance (grayed traffic lights, dimmed title bar); every
    /// window-pixel grab AT/AFTER activation captures that gray look (DRAGON-186
    /// Phase 5b). Grabbing the active window's pixels before activation captures its LIVE
    /// active appearance; the window-mode commit prefers these over any post-activation
    /// grab, independent of the freeze setting. Only the frontmost window changes
    /// appearance on activation (macOS renders every other window inactive already), so a
    /// single active-window grab is sufficient. Empty on Linux (no activation deactivates
    /// another app there) and until the pre-activation grab runs.
    active_win_px: HashMap<String, image::RgbaImage>,
    /// Display handle for the frozen cursor sprite, built ONCE when the cursor lands.
    /// (Minting a Handle inside view() gave a new id every frame — a texture upload
    /// + atlas entry per frame while the indicator showed.)
    frozen_cursor_handle: Option<widget::image::Handle>,
    /// The frozen cursor (sprite, global position, hotspot) captured at launch when "Preserve mouse
    /// cursor" is on — overlaid onto a freeze capture's windows-only composite. `None` otherwise.
    frozen_cursor: Option<crate::screenshot::CursorSprite>,
    /// This capture should grab LIVE pixels even if freeze is on — set for
    /// delayed shots, where the whole point of the delay is to change the screen.
    capture_live: bool,
    /// The capture backend recordings go through (persisted; a stable
    /// `platform::backend` id — Linux: "screencopy" | "portal", macOS: "sck").
    record_backend: String,
    /// The capture backend screenshots go through (persisted; same id space).
    screenshot_backend: String,
    /// The Screenshots / Recordings "Capture method" dropdown contents, derived
    /// from `platform::backend::backends()` (each backend whose relevant cap is
    /// present). Cached because the dropdown widget borrows the label slice;
    /// rebuilt when the portal probe lands (the only mid-session input).
    screenshot_methods: crate::platform::backend::MethodChoices,
    record_methods: crate::platform::backend::MethodChoices,
    /// True when no state file existed at startup (very first launch), so we can
    /// pick smart capture-method defaults once the portal probe completes.
    first_launch: bool,
    /// Whether the ScreenCast portal is reachable with usable source types
    /// (probed once at startup). Drives the indicator + whether the path is tried.
    pipewire_available: bool,
    /// Source types the portal advertises (bitflags: 1=monitor, 2=window, 4=virtual),
    /// for the indicator label. 0 until probed / when unavailable.
    pipewire_source_types: u32,
    /// Transient overlay message — e.g. "selected region not found in selected
    /// output" after a wrong-monitor portal pick. Auto-dismissed by a timer.
    toast: Option<String>,
    /// ScreenCast restore token from the last grant (replayed to skip the dialog).
    pw_restore_token: Option<String>,
    /// In-flight portal recording: context kept while the async ScreenCast request
    /// runs (its result lands in `pw_slot`, which the handler then consumes).
    pw_pending: Option<PwPending>,
    /// Hand-off slot for the async portal result (a `CastSession` holds a non-Clone
    /// fd, so it can't ride in a `Msg`; the task drops it here and signals readiness).
    pw_slot: std::sync::Arc<std::sync::Mutex<Option<Result<crate::platform::screencast::CastSession, crate::platform::screencast::CastError>>>>,
    /// A granted portal stream awaiting the start of recording (held across the
    /// countdown). When set, the recorder uses it instead of direct screencopy.
    pw_held: Option<HeldStream>,
    /// In-app update state (DRAGON-175): the cached result of the last update
    /// check, which drives the About nav-rail tint/icon and the About page's
    /// update rows. Checked when the settings window opens + on the manual
    /// "Check for updates" button.
    update_status: crate::update::UpdateStatus,
    /// True while a one-click update install is running (download/verify/stage)
    /// so the About page shows progress and the button is disabled.
    update_installing: bool,
    /// Launch-time update dialog state (DRAGON-177): `Some` while the "a new update
    /// is available" dialog is shown over the settings window. It appears once per
    /// settings session when the update check resolves `Available` AND
    /// `notify_updates` is on; the bool is the live "Don't remind me again" checkbox
    /// state. `None` = no dialog (never shown, or dismissed for this session).
    update_dialog: Option<UpdateDialog>,
    /// Whether this session already DECIDED the launch update dialog (shown, or
    /// deliberately suppressed because About was active). Re-checks after the
    /// decision (the network refresh behind a cache seed, About-tab re-checks)
    /// must never re-pop it.
    update_dialog_decided: bool,
    /// The last known release notes as (version, parsed markdown), DECOUPLED from
    /// `update_status` so the About page's "What's new" block never blinks out
    /// while a re-check runs (Checking) or after a failed refresh - the stale
    /// notes stay until a NEW result replaces them. Seeded from the on-disk
    /// manifest cache at settings launch (instant render before any network),
    /// then refreshed by `UpdateChecked`. The parse lives here (not in the view)
    /// because `markdown::view` borrows the parsed `Item`s for the element's
    /// lifetime.
    update_notes: Option<(String, cosmic::widget::markdown::Content)>,
}

/// The launch-time update dialog's transient state (DRAGON-177). Present only while
/// the dialog is shown over the settings window; carries the available update's info
/// (for the "Update Now" action) and the live "Don't remind me again" checkbox.
#[derive(Debug, Clone)]
pub struct UpdateDialog {
    /// The available update the dialog is offering (drives "Update Now"). Read only
    /// on macOS, where "Update Now" installs it via the dialog's own captured info
    /// (`UpdateDialogNow`); on Linux "Update Now" just opens the releases page (no
    /// one-click yet), so the field is carried but unread there.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub info: crate::update::UpdateInfo,
    /// The "Don't remind me again" checkbox state; when checked at the moment either
    /// button is clicked, `notify_updates` is turned OFF (persisted).
    pub dont_remind: bool,
}

/// Context for an in-flight portal recording request, kept until its async result
/// arrives (to start the recorder, fall back, or show the wrong-monitor toast).
struct PwPending {
    sel: Selection,
    /// Region mode only: the target monitor's logical geometry + the clamped region
    /// (global logical), used to validate the granted output and compute the crop.
    region: Option<RegionTarget>,
}

/// The monitor a region was clamped to, for validating the portal pick + cropping.
struct RegionTarget {
    out_pos: (i32, i32),
    out_size: (u32, u32),
    rect: (i32, i32, u32, u32),
}

/// A granted portal stream held between the permission grant and the actual start
/// of recording (it survives the pre-capture countdown). Consumed by the recorder.
struct HeldStream {
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    /// Region crop in stream pixels; `None` for whole monitor/window.
    crop: Option<(u32, u32, u32, u32)>,
}

mod message;
pub use message::{
    BorderColorTarget, CaptureMsg, RecordingMsg, DetectMsg, SettingsMsg, PermissionsMsg,
    WindowChromeMsg, PreviewMsg, VideoMeta,
};

#[derive(Debug, Clone)]
pub enum Msg {
    Capture(CaptureMsg),
    Recording(RecordingMsg),
    Detect(DetectMsg),
    Settings(SettingsMsg),
    /// Only constructed by the macOS permission-checker window; compiled (and
    /// type-checked) everywhere on purpose.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    Permissions(PermissionsMsg),
    WindowChrome(WindowChromeMsg),
    Preview(PreviewMsg),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_index_matches_exact_presets() {
        assert_eq!(countdown_index(0), 0);
        assert_eq!(countdown_index(3), 1);
        assert_eq!(countdown_index(5), 2);
        assert_eq!(countdown_index(10), 3);
    }

    #[test]
    fn countdown_index_rounds_to_the_nearest_preset() {
        assert_eq!(countdown_index(1), 0); // closer to 0 than to 3
        assert_eq!(countdown_index(7), 2); // closer to 5 than to 10
        assert_eq!(countdown_index(100), 3); // 10s is the closest of all presets
    }

    #[test]
    fn countdown_index_breaks_a_tie_toward_the_earlier_preset() {
        // 4s is equidistant from 3s (index 1) and 5s (index 2); ties keep the first minimum.
        assert_eq!(countdown_index(4), 1);
    }

    // DRAGON-201: the encoder probe (which spawns `ffmpeg -encoders`) is deferred off
    // the launch critical path. A screenshot / scan launch that never reads the encoder
    // list must leave the ffmpeg-spawning probe unrun.
    #[test]
    fn encoder_probe_is_deferred_until_first_read() {
        let enc = EncoderResolve::default();
        // Fresh holder (a screenshot launch): the probe has NOT run.
        assert!(!enc.probed(), "a fresh EncoderResolve must not have probed ffmpeg");
    }

    // Setting the preferred encoder (user pick / persist apply) must resolve WITHOUT
    // forcing the ffmpeg probe: `preferred()` returns the set value short-circuit, so a
    // path that only needs the preferred id (already known) never spawns ffmpeg.
    #[test]
    fn set_preferred_does_not_trigger_the_ffmpeg_probe() {
        let enc = EncoderResolve::default();
        enc.set_preferred("videotoolbox".to_string());
        assert_eq!(enc.preferred(), "videotoolbox");
        assert!(
            !enc.probed(),
            "reading a pre-set preferred encoder must not spawn the ffmpeg probe"
        );
    }

    fn tiny_wallpaper(w: u32, h: u32) -> std::sync::Arc<image::RgbaImage> {
        std::sync::Arc::new(image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255])))
    }

    #[test]
    fn wallpaper_handles_from_px_keeps_one_handle_per_output() {
        let mut px = HashMap::new();
        px.insert("Display-1".to_string(), tiny_wallpaper(4, 4));
        px.insert("Display-2".to_string(), tiny_wallpaper(8, 8));
        let handles = wallpaper_handles_from_px(px);
        assert_eq!(handles.len(), 2);
        // Each output name is preserved as its own key (per-monitor wallpaper).
        assert!(handles.contains_key("Display-1"));
        assert!(handles.contains_key("Display-2"));
    }

    #[test]
    fn wallpaper_handles_from_px_of_empty_is_empty() {
        // No wallpaper resolved (e.g. every SCK grab missed): the map is empty, so
        // every output's picker falls back to the dark fill.
        let handles = wallpaper_handles_from_px(HashMap::new());
        assert!(handles.is_empty());
    }

    #[test]
    fn wallpaper_handles_lookup_is_per_output_with_fallback() {
        // Model the window_view lookup: an output present in the map gets its
        // wallpaper; an absent output falls back (None).
        let mut px = HashMap::new();
        px.insert("Display-1".to_string(), tiny_wallpaper(4, 4));
        let handles = wallpaper_handles_from_px(px);
        assert!(handles.contains_key("Display-1"));
        assert!(!handles.contains_key("Display-2"));
    }

    // DRAGON-200: on macOS the precapture carries an EMPTY wallpaper placeholder (the
    // real per-output wallpaper is deferred + drained via `WallpaperReady`), so the
    // precapture drain must NOT assign it — otherwise it would clobber an
    // already-drained deferred wallpaper back to the dark fill. A (hypothetical
    // future) non-empty inline mac map would still win.
    #[cfg(target_os = "macos")]
    #[test]
    fn precapture_skips_empty_mac_wallpaper_placeholder_but_honors_a_real_map() {
        let empty: HashMap<String, std::sync::Arc<image::RgbaImage>> = HashMap::new();
        assert!(
            !precapture_should_assign_wallpaper(&empty),
            "an empty placeholder must never overwrite the deferred wallpaper"
        );
        let mut real = HashMap::new();
        real.insert("Display-1".to_string(), tiny_wallpaper(4, 4));
        assert!(
            precapture_should_assign_wallpaper(&real),
            "a non-empty inline map must still be assigned"
        );
    }

    // DRAGON-204: the ~1s window pre-capture runs at LAUNCH only for a window-mode
    // launch; every other capture mode defers it to the first switch into window mode,
    // and a non-capture (settings/preview) launch never runs it at all.
    #[test]
    fn launch_precapture_runs_only_for_a_window_mode_capture_launch() {
        // A window-mode capture launch: run it now (the thumbnails are needed immediately).
        assert!(launch_precapture_runs(true, Mode::Window));
        // Region / monitor capture launches DEFER it (lazy on switch to window mode).
        assert!(!launch_precapture_runs(true, Mode::Region));
        assert!(!launch_precapture_runs(true, Mode::Monitor));
        // A non-capture launch (settings / preview / permissions -> active=false) never
        // runs it, even if the mode happens to be Window.
        assert!(!launch_precapture_runs(false, Mode::Window));
        assert!(!launch_precapture_runs(false, Mode::Region));
    }

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("Hello World!!"), "hello-world");
    }

    #[test]
    fn slugify_trims_leading_and_trailing_separators() {
        assert_eq!(slugify("  --Foo Bar--  "), "foo-bar");
    }

    #[test]
    fn slugify_of_only_punctuation_or_empty_is_empty() {
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_caps_at_48_chars() {
        assert_eq!(slugify(&"a".repeat(60)), "a".repeat(48));
    }

    #[test]
    fn slugify_cap_trims_a_trailing_separator_landing_on_the_boundary() {
        // The 48th character processed is the space after 47 'a's, which would emit a
        // dash right at the cap boundary — the trailing separator must still be trimmed.
        let input = format!("{} next", "a".repeat(47));
        assert_eq!(slugify(&input), "a".repeat(47));
    }
}

