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
// How the app starts background work (DRAGON-497, shared here in DRAGON-499). Read its
// module doc before writing a `tokio::task::spawn_blocking` anywhere in `app`.
mod background;
mod update;
mod subscriptions;
mod keyboard;
mod num_field;
mod persist;
mod recording;
mod capture_flow;
// The pointer→monitor resolver is shared with the pickerless daemon handoff seam
// (`platform::cursor_display_name`, DRAGON-309); re-export it out of the private
// `capture_flow` module so `platform` can reach it. Not built on Linux (no capture-hotkey
// daemon and no global pointer there).
#[cfg(not(target_os = "linux"))]
pub(crate) use capture_flow::monitor_for_pointer;
// DRAGON-415: every non-delivery exit routes through `fail_session` here, which on macOS
// tells the user what happened before the one-shot child ends. The message TABLE is
// portable and unit-tested on every platform (it is the part no compiler can check); only
// the presentation is macOS-native, so off macOS the module is unreferenced outside its
// own tests. `pub(crate)` because the macOS panic hook in `main.rs` builds its message
// from here, so a child that dies on the main thread says so instead of vanishing.
#[cfg_attr(all(not(target_os = "macos"), not(test)), allow(dead_code))]
pub(crate) mod failure;
mod preview;
mod audio_ui;
mod shell;
mod surfaces;
mod portal;
mod overlay;
// The colour picker tool (DRAGON-582): the dimmed magnifier overlay and the result
// window. Read its module doc before touching where the picked pixel comes from — the
// `PixelSource` seam and the per-platform live-read analysis live there.
//
// pub(crate) since DRAGON-615: `state::schema`'s serde default for the persisted magnifier
// zoom defers to `geom::MAGNIFIER_ZOOM_DEFAULT` rather than repeating the number, so the
// picker's own `floor < default < ceiling` compile-time assert stays the single authority
// on the bounds and the stored default cannot drift away from the opening lens.
pub(crate) mod color_picker;
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

// The detached-worker seam. Re-exported so every `use super::*;` submodule reaches it
// unqualified, the way the Cloud Accounts page and the update handlers both call it.
pub(crate) use background::off_thread;

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
    // Read on EVERY platform since DRAGON-427: `opens_overlays` consults it to decide the
    // renderer, so it is no longer dead off macOS.
    pub permissions_only: bool,
    pub preview: Option<std::path::PathBuf>,
    /// DRAGON-582: this launch is the COLOUR PICKER (`--color-picker`), not a capture.
    ///
    /// It still opens overlays (see [`Startup::opens_overlays`]), and deliberately so:
    /// the picker's dimmed magnifier surface IS a capture-shaped overlay, minted through
    /// the same `app::shell` seam on every platform, so it inherits the layer-shell /
    /// portal-fallback / PlainWindows / Windows-10-software-rasterizer routing without a
    /// second copy of any of it.
    pub color_picker: bool,
    /// DRAGON-680: this launch is the PALETTE VIEWER (`--palette-viewer`): the colour
    /// picker's result window on its own, with no overlay and no pick.
    ///
    /// The deliberate opposite of [`Self::color_picker`] in the one respect that matters
    /// here. That flag is overlay-FIRST and answers [`Startup::opens_overlays`] true; this
    /// one is window-ONLY and answers it false, so it takes the settings window's routing
    /// (the GPU renderer, no permission probe, no capture surfaces) rather than a
    /// capture's. It never sets `color_picker`: `App::color_picking()` gates the picker
    /// OVERLAY's view, keyboard and flats grab, none of which this launch has.
    ///
    /// The window itself is opened a message-drain later, through
    /// `WindowChromeMsg::OpenPaletteViewer`, for the same reason `--settings` defers its
    /// own open: the appearance `set_theme` has to land first or the first paint flashes
    /// libcosmic's default accent.
    pub palette_viewer: bool,
    /// Launch straight into this capture mode (`--region`/`--window`/`--monitor`);
    /// `None` uses the default (Region).
    pub mode: Option<Mode>,
    /// Launch with this capture kind (`--image`/`--video`/`--scan`); `None` uses the
    /// default (Image). A `Scanner` kind forces Region mode.
    pub kind: Option<Kind>,
    /// Pre-capture countdown seconds (`--countdown <secs>`) — an EXACT value that may
    /// not match a UI preset (e.g. 7). `None` uses the persisted delay.
    pub countdown_secs: Option<u64>,
    /// DRAGON-559: `--audio <channels>` — arm exactly these audio channels FOR THIS
    /// LAUNCH. `None` (no flag) keeps the persisted arms; `Some` overrides them in
    /// memory only, through the pure `recording_ui::launch_audio_arms` decision in
    /// `App::init`. NOTHING writes the override back: the launch itself never saves it,
    /// and only a user's own later toggle persists (as any toggle does).
    ///
    /// A modifier like [`Self::no_editor`], chainable with every capture launch
    /// (`--window --video --audio system`). Meaningful for video launches; inert for a
    /// screenshot, which reads no arms.
    pub audio_arms: Option<crate::recording_ui::AudioArmState>,
    /// DRAGON-295 (macOS/Windows): an IMMEDIATE capture that skips the interactive picker
    /// overlay entirely — `--active-window` grabs the frontmost window, `--active-monitor`
    /// grabs the monitor under the cursor. `None` = the normal overlay launch. The seed
    /// step (`seed_outputs_mac`) consumes this instead of minting overlays.
    pub immediate: Option<ImmediateCapture>,
    /// Override the preview appearance for this launch: `Some(true)` = windowed,
    /// `Some(false)` = overlay. `--preview` defaults to windowed unless `--overlay` is
    /// also given; `None` uses the persisted setting.
    pub preview_windowed: Option<bool>,
    /// DRAGON-427 (Windows 10): this process IS the preview EDITOR for a capture another
    /// process just took (`--preview-handoff <line>`). Carries the whole
    /// [`crate::preview_ipc::OpenRequest`] — the same six fields the socket handoff sends —
    /// so the editor opens the document the capture child would have opened, not a bare
    /// `--preview` viewer (which would mark it `external` and refuse to manage the file).
    ///
    /// Like `preview`, a process launched this way opens NO capture overlay, so it keeps
    /// the GPU renderer while the capture child that spawned it runs on the software one.
    pub preview_handoff: Option<crate::preview_ipc::OpenRequest>,
    /// DRAGON-428: `--no-editor` — deliver this capture WITHOUT opening the preview editor.
    /// The file is still saved, copied to the clipboard and notified exactly as it is when
    /// no editor can be opened; only the editor is skipped.
    ///
    /// A MODIFIER, not a mode: it composes with every capture launch — a bare (region
    /// picker) launch, `--region` / `--window` / `--monitor`, and the picker-free
    /// `--active-window` / `--active-monitor`. That is what lets one flag give the user a
    /// no-editor variant of each capture shortcut rather than needing a second flag per mode.
    ///
    /// DRAGON-353 removed the persisted "Open in preview editor" setting, making the editor
    /// the unconditional destination; this is a per-LAUNCH opt-out, not that setting coming
    /// back. Nothing persists it, so it can only ever be asked for explicitly.
    pub no_editor: bool,
    /// macOS (DRAGON-440): the prompt-free permission snapshot, taken in [`run`]'s mac
    /// preamble for launches that would open overlays, and carried into `App::init`.
    ///
    /// It lives here because the ACTIVATION POLICY is boot-time-only (see [`run`]) and has
    /// to know whether this launch will route to the permission checker — a decision
    /// `App::init` used to make, far too late to influence the policy. Moving the probe
    /// forward keeps it at ONE call per launch: `App::init` consumes this snapshot instead
    /// of taking its own, so the policy decision and the routing decision can never
    /// disagree (no second probe, no TOCTOU window between them).
    ///
    /// `None` on a launch that never routes (settings / permissions / either preview), and
    /// as a belt-and-braces fallback if `run` somehow did not fill it — `App::init` probes
    /// for itself in that case, exactly as it did before.
    #[cfg(target_os = "macos")]
    pub route_probe: Option<permissions::Probe>,
    /// macOS (DRAGON-443): whether this launch is the FIRST after an update install — the
    /// single-shot marker `update::take_post_update_marker` consumes.
    ///
    /// Here for the same reason [`Self::route_probe`] is: a post-update relaunch is a
    /// SETTINGS-shaped launch (`App::init` ORs the marker into `settings_only` to land the
    /// user on About), and the activation policy is boot-time-only. Reading the marker inside
    /// `App::init` — which is where it used to happen, and only there — decided that far too
    /// late, so a post-update relaunch presented the settings window from an Accessory
    /// process: no Dock icon, no Cmd-Tab, and the overlay chrome strip installed for a launch
    /// that mints no overlay.
    ///
    /// CONSUMED exactly once, in `run`'s macOS preamble, and handed down here. `App::init`
    /// reads this field instead of taking the marker again, so the marker stays single-shot
    /// and the boot decision and the settings decision can never disagree. Off macOS there is
    /// no preamble, so `App::init` still consumes it in place, byte-identically.
    #[cfg(target_os = "macos")]
    pub post_update: bool,
}

impl Startup {
    /// Will this launch put a CAPTURE OVERLAY (or a fullscreen preview cover/spinner) on
    /// screen? DRAGON-427 keys the software-renderer decision on exactly this, so every
    /// launch path — daemon-spawned child, global hotkey, Start Menu shortcut, a bare CLI
    /// run, `--active-window`'s picker-free capture — answers it the same way, from this
    /// process's OWN flags rather than from anything it inherited.
    ///
    /// The launches that show only ordinary WINDOWS answer `false` and keep wgpu:
    /// `--settings` (whose live mic level-meter starves under a CPU rasterizer — the
    /// DRAGON-336 finding), the macOS `--permissions` checker, either preview flavour
    /// (`--preview <file>` cold, or `--preview-handoff` as a capture's editor child), and
    /// since DRAGON-680 `--palette-viewer`, which opens the colour picker's result window
    /// with no overlay phase at all.
    pub fn opens_overlays(&self) -> bool {
        !self.settings_only
            && !self.permissions_only
            && !self.palette_viewer
            && self.preview.is_none()
            && self.preview_handoff.is_none()
    }
}

// Re-exported so the message enum can carry a decoded shader frame across a task.
pub(crate) use preview::PixelFrame;
// Re-exported so the message enum can carry an annotation id.
pub(crate) use preview::AnnotId;

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

/// DRAGON-243 (Windows): the `ICED_PRESENT_MODE` value to force given whatever the
/// environment already carries. Returns `Some("fifo")` when nothing meaningful is set —
/// forcing a DWM-COMPOSITED present mode so the transparent capture / preview overlay keeps
/// its per-pixel alpha across continuous redraws — and `None` when the user (or a debug
/// session) already chose one, so their value wins. Pure, so the "only when unset" rule is
/// unit-tested without touching the process environment. See [`run`] for the full rationale.
#[cfg(windows)]
fn present_mode_env_override(existing: Option<&str>) -> Option<&'static str> {
    match existing {
        Some(v) if !v.trim().is_empty() => None,
        _ => Some("fifo"),
    }
}

/// DRAGON-427: this process's effective preview appearance, given the `chosen` one (the
/// persisted setting, or a `--preview` launch's override), whether this process
/// [`opens overlays`](Startup::opens_overlays), whether the machine can show the preview
/// EDITOR as a fullscreen overlay at all ([`crate::platform::overlay_preview_available`]),
/// and whether this process renders its overlays in SOFTWARE
/// ([`crate::platform::software_overlays`], which is Windows 10 and nothing else).
///
/// Pure, and the ONE place the rule is expressed:
///
/// * **Where the overlay editor is available** (macOS, Windows 11, a layer-shell Linux
///   session) nothing changes — the chosen value is returned untouched, so those platforms
///   stay byte-identical.
/// * **A Windows 10 process that opens overlays** is the CAPTURE half. It renders in
///   software and must therefore never mint the real editor, but it still shows fullscreen
///   loaders and covers — which ARE preview surfaces, and which want to be translucent
///   overlays. So it answers `false` (overlay), and the editor it would otherwise open is
///   spawned as its own process instead (`preview::open`'s `try_spawn_editor_child`).
/// * **A Windows 10 process that opens no overlays** is the EDITOR half (`--preview-handoff`,
///   or a cold `--preview <file>`). It kept the GPU renderer, and the editor is always the
///   WINDOW there, so a stored `preview_windowed = false` — or an explicit `--overlay` — is
///   overridden rather than honoured. Hiding the setting while still applying it would drop
///   such a user onto an overlay editor that cannot draw its own media.
/// * **Linux with no layer shell** (`lab/flatpak`) has no fullscreen preview SURFACE of any
///   kind, loaders and covers included, so BOTH halves answer `true`. That is why the Windows
///   10 exception is keyed on `software_overlays` rather than on "opens overlays" alone: read
///   the other way round, a sandboxed capture child would set `preview_windowed = false` in
///   memory and the next `save_state` would write that back, silently flipping the user's
///   persisted appearance to an overlay their normal session would then honour.
pub fn effective_preview_windowed(
    chosen: bool,
    opens_overlays: bool,
    overlay_preview_available: bool,
    software_overlays: bool,
) -> bool {
    if overlay_preview_available {
        return chosen;
    }
    !(opens_overlays && software_overlays)
}

/// The iced renderer name for the software (CPU) rasterizer — the one value DRAGON-427
/// selects on Windows 10. Matches `iced_tiny_skia`'s own backend word.
#[cfg_attr(not(windows), allow(dead_code))]
pub const SOFTWARE_BACKEND: &str = "tiny-skia";

/// DRAGON-427: the `ICED_BACKEND` value to force, given whatever the environment already
/// carries and whether this process wants the software rasterizer.
///
/// `Some("tiny-skia")` only when software rendering is wanted AND nothing meaningful is
/// already set — a user (or a debug session) who chose a backend themselves always wins,
/// and is never silently overridden. Pure, so that rule is unit-tested without touching the
/// process environment.
///
/// **On why this is an env var at all.** iced picks ONE compositor per process, in
/// `iced_winit`'s `create_compositor`, which calls `Compositor::new(..)` — the `backend`
/// argument of `graphics::Compositor::with_backend` is hardcoded `None` there, and neither
/// `iced_graphics::Settings` nor `cosmic::app::Settings` carries a backend field. So in the
/// libcosmic we pin there is NO in-process API for this choice: `ICED_BACKEND` is the only
/// lever, and reaching a real in-process one would mean forking BOTH pop-os/libcosmic and
/// its vendored pop-os/iced submodule. See [`user_backend_for_child`] for the one hazard
/// that creates and how it is closed.
#[cfg_attr(not(windows), allow(dead_code))]
fn backend_env_override(existing: Option<&str>, want_software: bool) -> Option<&'static str> {
    if !want_software {
        return None;
    }
    match existing {
        Some(v) if !v.trim().is_empty() => None,
        _ => Some(SOFTWARE_BACKEND),
    }
}

/// **Pure**, unit-tested: does THIS launch want the Windows 10 software rasterizer
/// (DRAGON-427, narrowed by DRAGON-650)?
///
/// The DRAGON-427 force exists because Windows 10's DWM appears to discard per-pixel alpha
/// on our layered overlay windows (wgpu presents them solid black; the whole account is on
/// `platform::win_build_software_overlays` and `win_overlay_is_layered`), so a process that
/// will put a TRANSLUCENT overlay on screen must render on tiny-skia there.
///
/// The COLOUR PICKER is the one overlay launch that exemption reasoning does not reach
/// (DRAGON-650): its overlay draws an OPAQUE fullscreen frozen snapshot with the dim
/// composited in-app, on top of it, inside our own surface. It never needs per-pixel window
/// translucency, so it has nothing to lose to the alpha bug and everything to gain from the
/// GPU (the magnifier re-rasters on effectively every pointer move, and the shader-backed
/// lens is barred on tiny-skia). So a `--color-picker` process keeps wgpu even on Windows
/// 10. Narrowed HERE, at the one renderer decision, and not inside
/// `Startup::opens_overlays`, whose other callers (flats grabs, boot policy,
/// `effective_preview_windowed`'s overlay term) all still want "the picker opens overlays"
/// to be true. `effective_preview_windowed` in particular keeps reading the raw PLATFORM
/// fact, which stays correct because a picker launch never mints a preview surface of
/// either shape.
///
/// Unverified on real Windows 10 hardware (nobody on the project has any): this is
/// implemented on the alpha-hypothesis reasoning above, and the worst case is a
/// picker-only regression there, reversible by deleting the `!color_picker` term.
/// `dcomp` is the DRAGON-666 experiment (`platform::win_dcomp_requested`): a run that
/// presents through a DirectComposition visual has REAL per-pixel alpha from the surface
/// itself, so the software rasterizer has nothing left to fix and must stand down —
/// otherwise a Windows 10 tester would take tiny-skia before wgpu ever saw the option, and
/// the experiment could not answer the question it exists to ask.
#[cfg_attr(not(windows), allow(dead_code))]
fn wants_software_backend(
    opens_overlays: bool,
    color_picker: bool,
    platform_software: bool,
    dcomp: bool,
) -> bool {
    opens_overlays && !color_picker && platform_software && !dcomp
}

/// DRAGON-650: whether THIS process was actually FORCED onto the software rasterizer by the
/// DRAGON-427 gate — i.e. [`backend_env_override`] returned a backend and `run` wrote it
/// into the environment. Written once, before any window exists; false everywhere the force
/// never runs (Linux, macOS, Windows 11, and every Windows 10 launch the gate exempts).
static FORCED_SOFTWARE_BACKEND: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// DRAGON-650: was THIS process forced onto the software (tiny-skia) rasterizer?
///
/// The question the magnifier's raster form actually needs answered
/// (`color_picker::build_magnifier_raster`): a shader widget is BLANK on tiny-skia, so
/// what matters is the renderer THIS process runs, not the platform fact that decides it.
/// Before DRAGON-650 the two were interchangeable — every Windows 10 overlay process was
/// forced — but the picker now keeps wgpu there, and asking the platform would have parked
/// its lens on the atlas-churning image arm for no reason.
///
/// Deliberately narrower than "is this process on tiny-skia": a user who sets
/// `ICED_BACKEND=tiny-skia` themselves is out of scope, answered `false`, exactly as
/// `platform::software_overlays()` answered before. Honouring a hand-set backend without
/// silently rewiring views around it is the same rule [`backend_env_override`] applies to
/// the variable itself.
pub(in crate::app) fn process_forced_software_backend() -> bool {
    FORCED_SOFTWARE_BACKEND.load(std::sync::atomic::Ordering::Relaxed)
}

/// The value `ICED_BACKEND` held BEFORE this process touched it — captured once, at the
/// moment [`run`] decides, and `None` when the user had not set one.
///
/// **This is what keeps the env-var mechanism from leaking down the process tree.** A child
/// inherits its parent's environment, so a Windows 10 CAPTURE process (software) spawning
/// the preview editor — or a settings window — would otherwise hand it `tiny-skia` and
/// break exactly the surface this ticket exists to keep on the GPU. Every GUI child spawn
/// therefore restores this value instead of passing ours on: set it back when the user had
/// one, remove it when they did not. A user's own choice still reaches their children.
#[cfg_attr(not(windows), allow(dead_code))]
static USER_ICED_BACKEND: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// The `ICED_BACKEND` a GUI child of ours must see: the user's own value, or `None` to
/// remove the variable entirely. `Some(None)` means "remove it"; the outer `None` means
/// this process never forced anything, so a child's environment needs no correction at all.
///
/// Apply with [`restore_user_backend_env`] rather than by hand.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn user_backend_for_child() -> Option<Option<&'static str>> {
    USER_ICED_BACKEND
        .get()
        .map(|v| v.as_deref())
}

/// Undo this process's DRAGON-427 backend forcing in a child `Command`'s environment, so
/// the child chooses its own renderer exactly as if it had been launched from the user's
/// shell. A no-op when we never forced anything (every non-Windows-10 machine, and every
/// Windows 10 launch that shows no overlay).
///
/// Call this on EVERY GUI child spawn. A non-GUI child (ffmpeg, the ducker) is unaffected
/// either way, so calling it there is harmless.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn restore_user_backend_env(cmd: &mut std::process::Command) {
    match user_backend_for_child() {
        Some(Some(v)) => {
            cmd.env("ICED_BACKEND", v);
        }
        Some(None) => {
            cmd.env_remove("ICED_BACKEND");
        }
        None => {}
    }
}

/// Whether this launch boots the macOS REGULAR activation policy (DRAGON-153, widened by
/// DRAGON-440) — i.e. whether its UI is a real WINDOW rather than a capture overlay.
///
/// Regular means a Dock icon, a Cmd+Tab entry, and the app name in the menu bar. That is
/// right for a window the user is meant to look at and wrong for a capture: DRAGON-151
/// found that promoting a capture launch stamps "Cosmic Capture Kit" into the menu bar,
/// which then appears in captures of the menu-bar area. So the answer must stay FALSE for
/// a healthy capture launch, and the tests pin exactly that.
///
/// The fourth argument is the DRAGON-440 addition: a capture launch that routes to the
/// permission checker shows the checker WINDOW and no overlay, so it belongs with the
/// other three.
///
/// The fifth is DRAGON-443's: a POST-UPDATE relaunch. The installer's swap helper relaunches
/// the app with bare argv, and `App::init` turns that into a settings launch on the About
/// page by ORing the marker into `settings_only`. It is therefore window-shaped by exactly
/// the same reasoning — but the OR happened inside `App::init`, long after the policy was
/// decided from `startup.settings_only == false`, so the new version's release notes were
/// presented by an Accessory process. It is the last hole in "Regular policy ⟺ the UI is a
/// real window".
///
/// **The COLOUR PICKER is deliberately not on that list, and this is the tombstone so it is
/// not "fixed" onto it.** A `--color-picker` launch ends in a real window the user is meant
/// to Cmd+Tab to, so it looks like a sixth case, and it is not: it is overlay-FIRST. It
/// mints the same per-output capture-shaped overlays a screenshot does (see
/// [`Startup::color_picker`], which is why [`Startup::opens_overlays`] answers true for it),
/// and only a PICK turns it into a window launch. Booting it Regular would promote the
/// OVERLAY phase and cost three things: the DRAGON-154 AeroSpace opt-out (which only ignores
/// a window whose owner is `.accessory` at its first AX exposure, so the picker's overlays
/// would start being tiled off their target displays), the DRAGON-151 menu-bar stamp, which
/// for this tool lands in the frozen snapshot the picker reads its colours FROM, and a Dock
/// icon for the two picker launches that open no window at all (a pick delivered to an
/// editor, DRAGON-587, or handed to an already-live picker window, DRAGON-613). The picker
/// window takes Regular the OTHER way instead, the post-boot flip the windowed preview has
/// always used: `platform::mac::window::ensure_regular_policy`, called from
/// `App::finalize_color_picker_window` once the window is up. Anything else that is
/// overlay-first and window-second belongs there too, not here.
///
/// Pure so the table is unit-testable — the two macOS gates in [`run`] (the policy, and the
/// inverted overlay chrome strip) both read this one function rather than repeating the
/// condition.
///
/// `preview_launch` is EITHER preview flavour — `--preview <file>` or the
/// `--preview-handoff` editor child — matching [`Startup::opens_overlays`], which counts
/// both. Windows is the only platform that spawns a handoff child today, but keying this
/// on the bare `preview` field alone would mean that if one ever reached macOS it would
/// boot Accessory AND install the overlay chrome strip, for a launch that is a pure editor
/// window. Both flavours answer the question this predicate actually asks.
// `test` as well as macOS so the Linux/Windows suites exercise the table; the two callers
// are macOS-only, hence dead elsewhere.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn boots_regular_policy(
    settings_only: bool,
    permissions_only: bool,
    preview_launch: bool,
    routed_to_permissions: bool,
    post_update: bool,
) -> bool {
    settings_only || permissions_only || preview_launch || routed_to_permissions || post_update
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
    //
    // The call itself now lives a few lines down, INSIDE the DRAGON-440 block, so that the
    // permission pre-flight and the SCK pre-warm are started before it rather than after it
    // and this resolve (~28ms measured, a framework load) overlaps them. Nothing about the
    // property changes: it is still set on this thread, still before any window exists, and
    // still on every macOS launch.
    // macOS (DRAGON-440): take the permission snapshot HERE, before the policy block below,
    // for launches that would otherwise open capture overlays.
    //
    // WHY it moved out of `App::init`: the activation policy is BOOT-TIME-ONLY (see the
    // DRAGON-153 note below), and a capture launch that ROUTES to the permission checker
    // shows a real window — so the policy has to know about the routing before the window
    // exists. `App::init` decided routing long after this point, which is how a routed
    // launch ended up presenting an Accessory-policy checker: no Dock icon, no Cmd+Tab,
    // free to sit behind whatever the user was looking at. The startup guard then SUSPENDED
    // its budget for that invisible window, and the nag was never spent, so every capture
    // launch repeated it.
    //
    // The probe MOVES rather than being duplicated — it is the same prompt-free
    // `probe_now_fast`, handed to `App::init` on `Startup::route_probe` so both decisions
    // read ONE snapshot. `opens_overlays()` is exactly `App::init`'s old guard
    // (`!settings_only && !preview_mode && !permissions_only`), so no launch changes which
    // side of it it lands on.
    // macOS (DRAGON-443): consume the post-update marker HERE too, and for the same reason —
    // it decides that this bare relaunch is really a SETTINGS launch (`App::init` ORs it into
    // `settings_only` to land on About), and the policy below cannot wait for `App::init` to
    // say so.
    //
    // CONSUMED, not peeked, and handed down on `Startup::post_update`. The marker is
    // single-shot by construction (it is a file `take` removes), so a peek here plus a take in
    // `App::init` would be TWO readings of the same fact with a window between them — exactly
    // the TOCTOU shape DRAGON-440 removed for the permission probe. One read, passed forward,
    // is the pattern that already works. Off macOS there is no preamble at all, so `App::init`
    // keeps taking the marker itself, in the same place, byte-identically.
    //
    // The probe is still taken HERE, and the policy below still reads it. What changed is
    // only WHEN it runs relative to the rest of this preamble. It used to run inline, so the
    // launch paid its ~37ms with nothing else in flight, and it cannot be moved any LATER
    // for the reason this whole block exists. So it moves EARLIER instead: it is started on
    // its own thread as this preamble's first act (`ProbePreflight::start`, whose doc carries
    // the boundedness and off-main-thread arguments) and joined at the one line that needs
    // its answer. Everything between those two points — the post-update marker read, the SCK
    // pre-warm kick, the SkyLight resolve — now runs alongside it instead of before it.
    // Nothing downstream can tell the difference: `routed_to_permissions` and
    // `Startup::route_probe` are the same values, from ONE snapshot, computed at the same
    // point in the launch.
    #[cfg(target_os = "macos")]
    let (startup, routed_to_permissions) = {
        let mut startup = startup;
        // The marker read moves ABOVE the pre-flight so the pre-flight can be gated on
        // exactly the condition the inline probe used, with no window where the two could
        // disagree. It is a single file `take`, so it costs the overlap nothing.
        startup.post_update = crate::update::take_post_update_marker();
        // Skipped for a post-update relaunch as well as for the window launches: it shows the
        // About page, mints no overlay, and has no capture to be missing a grant for. Probing
        // there would be the nag interrupting the release notes — and would leave the policy
        // gate and the routing decision reading different pictures of this launch, which is
        // the disagreement DRAGON-440 exists to prevent.
        let preflight = (startup.opens_overlays() && !startup.post_update)
            .then(permissions::ProbePreflight::start);
        if preflight.is_some() {
            crate::util::timing_mark("app::run -> permissions::probe_now_fast (kicked, own thread)");
        }
        // The DRAGON-150/151 SkyLight property (see the long note above). Its ~28ms is now
        // spent alongside the probe instead of in front of it, which is the whole point of
        // the kick above.
        //
        // It stays AHEAD of the SCK pre-warm below, and that order is deliberate, not
        // incidental: this is the call that resolves `SLSMainConnectionID` and stamps
        // `SetsCursorInBackground` onto this process's window-server connection, and before
        // this line NOTHING in the process has talked to the window server — which was true
        // before this change too, and is worth keeping true. ScreenCaptureKit is a
        // window-server client, so kicking it first would put a background thread into that
        // connection ahead of the property being set, for no gain: the pre-warm's answer is
        // not needed for another ~120ms and it still lands ~60ms early from here.
        crate::platform::mac::window::enable_background_cursor();
        // Start the launch's ScreenCaptureKit content fetch, on its own thread. Its first
        // consumer is `App::init`'s trigger-display snapshot, ~120ms from here, which used to
        // pay the whole round trip inline; see `prewarm_shareable_content`. Unconditional
        // because that consumer is unconditional.
        crate::platform::mac::prewarm_shareable_content();
        let routed = if let Some(preflight) = preflight {
            let probe = preflight.join();
            crate::util::timing_mark("app::run <- permissions::probe_now_fast (joined)");
            let routed = permissions::should_auto_open_probe(&probe);
            startup.route_probe = Some(probe);
            routed
        } else {
            false
        };
        (startup, routed)
    };
    // macOS (DRAGON-153): launches whose UI is a REAL window (settings /
    // permissions / a --preview viewer) should behave like a normal app — Cmd+Tab
    // presence, Dock icon, focusable from other apps — so they boot with the
    // REGULAR policy. Policy is boot-time-only (the DRAGON-150 lesson: a
    // post-launch flip half-activates the app and kills key-window delivery), which
    // is why this is decided here and capture children never change theirs.
    // DRAGON-440 added the fourth case: a capture launch ROUTED to the checker is also
    // "a launch whose UI is a real window", and is now treated as one. DRAGON-443 added the
    // fifth: a POST-UPDATE relaunch, which `App::init` turns into a settings launch on About.
    #[cfg(target_os = "macos")]
    if boots_regular_policy(
        startup.settings_only,
        startup.permissions_only,
        startup.preview.is_some() || startup.preview_handoff.is_some(),
        routed_to_permissions,
        startup.post_update,
    ) && let Some(mtm) = objc2_foundation::MainThreadMarker::new()
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
    // DRAGON-440 makes this the exact INVERSE of the policy gate above, which is a
    // deliberate behaviour delta for the routed case: a routed launch never mints an
    // overlay (the strip's only purpose), and the strip is a global swizzle that mangles
    // the chrome of ANY window above level 0 — including the panel a DRAGON-415 failure
    // alert puts up. Installing it for a launch that shows only the checker would be all
    // cost and no benefit.
    #[cfg(target_os = "macos")]
    if !boots_regular_policy(
        startup.settings_only,
        startup.permissions_only,
        startup.preview.is_some() || startup.preview_handoff.is_some(),
        routed_to_permissions,
        startup.post_update,
    ) {
        crate::platform::mac::window::install_overlay_chrome_strip();
    }
    // DRAGON-303: on macOS 26 an NSGlassContainerView in the titlebar swallows clicks meant
    // for content under the transparent titlebar, so the settings / preview CSD header's
    // collapse / search buttons stop responding in fullscreen. Install the hit-test passthrough
    // for EVERY launch (settings / preview included, unlike the capture-only chrome strip).
    #[cfg(target_os = "macos")]
    crate::platform::mac::window::install_glass_container_click_through();
    // Windows (DRAGON-243): force a DWM-COMPOSITED present mode for every wgpu surface this
    // process opens. iced_wgpu's default here is `AutoNoVsync` (resolves to Immediate /
    // Mailbox on the Vulkan backend this machine selects). A fullscreen TRANSPARENT overlay
    // — the capture overlay and the fullscreen preview overlay — that presents CONTINUOUSLY
    // (dragging a region, the recording chip's timer, the preview compositing) is then
    // promoted by Windows to "independent flip" / DirectFlip, which scans the swapchain
    // buffer out to the display DIRECTLY, bypassing DWM composition and therefore the
    // window's per-pixel alpha (winit sets transparency via `DwmEnableBlurBehindWindow`).
    // With DWM bypassed, the surface's premultiplied-black dim/scrim shows as its raw RGB —
    // OPAQUE black — and the desktop behind it vanishes, while opaque top content (the
    // accent selection border, the composited media) still shows. It only goes wrong AFTER
    // the first, still-DWM-composited idle frame, which is exactly the reported "translucent
    // on open, solid black the moment you move/redraw" symptom (BUG 1) and the opaque
    // preview surround (BUG 2). `Fifo` (present interval 1) keeps the surface in DWM
    // composition, so per-pixel alpha survives every redraw. DRAGON-234 verified transparency
    // on this SAME Vulkan/NVIDIA/PreMultiplied rig only because its check was IDLE (a static
    // overlay stays DWM-composited); the regression is the continuous-present path. This env
    // is read ONCE at compositor creation inside `cosmic::app::run` below, so it must be set
    // first; we are still single-threaded here (that call is what spawns the render/runtime
    // threads), so the edition-2024 `set_var` is sound. cfg(windows) only — Linux (layer-
    // shell) and macOS keep their historical present mode byte-for-byte.
    #[cfg(windows)]
    if let Some(mode) =
        present_mode_env_override(std::env::var("ICED_PRESENT_MODE").ok().as_deref())
    {
        // SAFETY: single-threaded at this point — `cosmic::app::run` below is the first
        // thing to spawn the runtime / renderer threads, so no other thread can be reading
        // the environment concurrently with this write.
        unsafe { std::env::set_var("ICED_PRESENT_MODE", mode) };
    }
    // Windows 10 (DRAGON-427): render THIS process with iced's SOFTWARE rasterizer when it
    // is going to put an overlay on screen. wgpu cannot make a Windows 10 window translucent
    // on any backend (the evidence is in `platform::win_build_software_overlays`), and a
    // customer on real Windows 10 hardware proved tiny-skia is translucent where wgpu is
    // solid black. Windows 11 never reaches this — `platform::software_overlays()` is the
    // closed [10240, 22000) band — so it keeps wgpu byte-for-byte.
    //
    // The decision is made HERE, from this process's OWN `Startup`, which is why it is right
    // on every launch path: a daemon-spawned capture child, a global hotkey that runs the
    // binary directly, a Start Menu shortcut, a bare CLI run and `--active-window`'s
    // picker-free capture all arrive at this one line with their own flags. Nothing about it
    // depends on what the launcher's environment happened to contain.
    //
    // The editor is deliberately NOT here: it runs as its OWN process (see
    // `preview::open`'s `try_spawn_editor_child`), so it keeps the GPU renderer. iced picks
    // one compositor per process, so that separation is the only way to have both.
    //
    // The COLOUR PICKER is exempt too (DRAGON-650): its overlay is an OPAQUE frozen
    // snapshot with the dim composited in-app, so it never needs the per-pixel window
    // alpha Windows 10 discards, and it keeps wgpu. `wants_software_backend` carries the
    // reasoning and the tests.
    #[cfg(windows)]
    {
        // DRAGON-666, the DirectComposition experiment, decided BEFORE the software force
        // because it overrules it: a DComp surface has real per-pixel alpha, so tiny-skia
        // has nothing left to fix, and a Windows 10 tester who took the force would never
        // reach a swapchain to configure. The variable it writes is wgpu's own; ours only
        // says whether to write it (`platform::win_dcomp_requested`).
        //
        // A user who has already set `WGPU_DX12_PRESENTATION_SYSTEM` themselves keeps it,
        // the same courtesy `backend_env_override` extends to a hand-set `ICED_BACKEND`.
        // Children inherit `CCK_WIN_DCOMP` and decide for themselves, which is what we
        // want: the editor and the picker should present the same way the overlay does.
        let dcomp = crate::platform::dcomp_enabled();
        if dcomp {
            // The BACKEND first: `WGPU_DX12_PRESENTATION_SYSTEM` is a DX12-only option, and
            // wgpu's default order picks VULKAN on plenty of real hardware (a customer's
            // GTX 1080 Ti did, and every window went solid because the Vulkan surface
            // reports `[Opaque]` just like the HWND one). Asking for a composition visual
            // while running Vulkan asks for nothing at all.
            //
            // A user who pinned `WGPU_BACKEND` themselves keeps it, the same courtesy the
            // presentation system and `ICED_BACKEND` get; they may be working around a
            // driver bug of their own, and we would rather present the old way than fight
            // them for it.
            let backend_env = crate::platform::WGPU_BACKEND_ENV;
            match std::env::var(backend_env) {
                Ok(v) if !v.trim().is_empty() => log::info!(
                    "dcomp experiment: {backend_env}={v:?} is already set — honouring it \
                     (a composition visual needs the dx12 backend)"
                ),
                _ => {
                    // SAFETY: single-threaded here, as for every other write in this block.
                    unsafe {
                        std::env::set_var(backend_env, crate::platform::WGPU_BACKEND_DX12);
                    }
                }
            }
            let wgpu_env = crate::platform::WGPU_PRESENTATION_ENV;
            match std::env::var(wgpu_env) {
                Ok(v) if !v.trim().is_empty() => log::info!(
                    "dcomp experiment: {wgpu_env}={v:?} is already set — honouring it"
                ),
                _ => {
                    log::info!(
                        "dcomp experiment: presenting through a DirectComposition visual \
                         ({wgpu_env}={} on the {} backend); the windows 10 software force \
                         stands down",
                        crate::platform::WGPU_DCOMP_VALUE,
                        crate::platform::WGPU_BACKEND_DX12,
                    );
                    // SAFETY: single-threaded here, same as the writes around it — the
                    // runtime and renderer threads are spawned by `cosmic::app::run` below.
                    unsafe {
                        std::env::set_var(wgpu_env, crate::platform::WGPU_DCOMP_VALUE);
                    }
                }
            }
            // DRAGON-685: do NOT reach for `WGPU_DX12_USE_FRAME_LATENCY_WAITABLE_OBJECT`
            // here. Both non-default values were tried on device and both are worse than
            // the bug they fix: `Wait` (the default) stalled the first frame acquires
            // after every `ResizeBuffers` for 1s each (the palette viewer's panel toggle
            // froze on a stale frame for 1 to 3 seconds), but `DontWait` and `None`
            // remove the only backpressure pacing presents to the display, so a drag's
            // redraw storm queued frames until the ghost trailed the pointer by up to a
            // second. The stall is fixed where it lives instead: the wgpu-hal fork bounds
            // the waitable wait once after an in-place `ResizeBuffers` (see
            // FORKED_CHANGES.md), and every mode keeps its meaning.
        }
        let want_software = wants_software_backend(
            startup.opens_overlays(),
            startup.color_picker,
            crate::platform::software_overlays(),
            dcomp,
        );
        let existing = std::env::var("ICED_BACKEND").ok();
        if let Some(backend) = backend_env_override(existing.as_deref(), want_software) {
            // Remember what the user had (nothing, here) BEFORE we write, so every GUI child
            // this process spawns can be given their environment back rather than ours.
            let _ = USER_ICED_BACKEND.set(existing);
            // The force is now actually APPLYING, which is what the magnifier's raster-form
            // predicate reads (DRAGON-650, `process_forced_software_backend`). Before any
            // window exists, so no view can race the write.
            FORCED_SOFTWARE_BACKEND.store(true, std::sync::atomic::Ordering::Relaxed);
            log::info!(
                "windows 10: rendering this process's overlays with the {backend} \
                 software rasterizer (wgpu cannot present a translucent HWND surface here)"
            );
            // SAFETY: single-threaded at this point, exactly as for `ICED_PRESENT_MODE`
            // above — `cosmic::app::run` below is what spawns the runtime/renderer threads,
            // so nothing can be reading the environment concurrently with this write.
            unsafe { std::env::set_var("ICED_BACKEND", backend) };
        } else if want_software && existing.is_some() {
            log::info!(
                "windows 10: ICED_BACKEND={:?} is already set — honouring it and not \
                 forcing the software rasterizer",
                existing.unwrap_or_default()
            );
        }
        // ONE line that answers "what is this process rendering with", so a reader
        // never has to infer it from the presence or absence of somebody else's log
        // line. The header's `wants_software=` states the platform GATE and is
        // written before this decision exists; the arms above each explain a
        // particular choice; this states the choice itself, on every Windows launch.
        log::info!(
            "renderer: {} (overlays={}, dcomp={})",
            if want_software { "tiny-skia (software)" } else { "wgpu (gpu)" },
            startup.opens_overlays(),
            dcomp,
        );
    }
    // DRAGON-354: register the two embedded annotation faces (Excalifont / Inter) with the
    // GLOBAL cosmic-text font system SYNCHRONOUSLY, here — BEFORE `cosmic::app::run` below
    // creates the renderer/compositor. This is the ONE reliable seam. The prior attempt loaded
    // them with the async `cosmic::iced::font::load` Task dispatched from `App::init`, but that
    // races the compositor's LAZY creation: iced_winit only creates the compositor on the FIRST
    // `WindowCreated`, and its `LoadFont` action handler is a silent no-op while the compositor
    // is still `None` — so an init-time font-load is dropped and never retried, and the family
    // names never enter the shared db (the dropdown labels fell back to the UI font forever).
    // Loading straight into the process-global `font_system()` mirrors libcosmic's own
    // `preload_fonts` (its OpenSans/Noto faces) and guarantees "Excalifont"/"Inter" resolve for
    // `Font::with_name` before any text is shaped, in every window/surface.
    {
        use std::borrow::Cow;
        if let Ok(mut fs) = cosmic::iced::advanced::graphics::text::font_system().write() {
            for (_family, bytes) in preview::text_annot::UI_FONT_FACES {
                fs.load_font(Cow::Borrowed(bytes));
            }
        }
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
            // DRAGON-415: the runtime itself failed (it never started, or it unwound), so
            // no in-app path ever ran and nothing was delivered. On macOS that error goes
            // to a stderr nobody can read and the process `_exit`s — another silent close.
            // The alert's own guards keep this to at most one dialog; the message names no
            // cause, because at this seam we have none.
            #[cfg(target_os = "macos")]
            {
                let msg = failure::runtime_failure_alert();
                crate::platform::mac::alert::show(&msg.title, &msg.body);
            }
            // DRAGON-442: the same seam on Windows, which had nothing here at all. A
            // shortcut / daemon-spawned launch has no console, so the `log::error!` above
            // reaches only the debug log — the user just watched the app fail to open.
            //
            // Blocking here is correct and is NOT the thing `alert`'s module doc forbids:
            // that rule protects the thread owning our windows while a session runs on. By
            // this point `cosmic::app::run` has returned, the `App` and every window it
            // owned are dropped, and the next statement is `_exit`. There is nothing left to
            // keep pumping for, and waiting is the only way the box can be read.
            //
            // `show` returns `None` if this session already put an alert up (the DRAGON-436
            // one-per-process latch), in which case there is nothing to wait for — a session
            // that reported a failure and then failed to run gets one box, not two.
            #[cfg(windows)]
            {
                let msg = failure::windows_runtime_failure_alert(
                    crate::diag::log_dir().map(|d| d.display().to_string()).as_deref(),
                );
                if let Some(dismissal) = crate::platform::windows::alert::show(&msg.title, &msg.body)
                {
                    let outcome = dismissal.wait(failure::ALERT_DISMISS_BUDGET);
                    log::warn!("DRAGON-442: runtime-failure alert ended as {outcome:?}");
                }
            }
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

/// DRAGON-295: an immediate, picker-free capture requested from the CLI / a daemon global
/// hotkey. Unlike [`Mode`] (which is a picker mode inside the overlay), these skip the
/// overlay: the target is resolved at launch (frontmost window / monitor under cursor) and
/// captured straight through the normal capture pipeline. macOS/Windows only; Linux never
/// constructs it (its capture keys are COSMIC custom shortcuts, not owned here).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImmediateCapture {
    /// The frontmost/active window, no picker.
    ActiveWindow,
    /// The monitor under the cursor, no picker.
    ActiveMonitor,
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
    // DRAGON-460 removed `ScanKind`. It existed so the scan segment could swap to the
    // refresh glyph on hover (DRAGON-456); the refresh is now a visible button of its
    // own, so nothing tracks that hover and the variant had no constructor left.
}

/// How far a scanner READ has got (DRAGON-456, redesigned by DRAGON-460).
///
/// The scanner reads a LIVE shot of the selected region, taken on demand — not a crop of
/// the launch-instant flats. That is what makes the scan current without freezing anything,
/// and it works because of one property of our own overlay: `RegionSelection::draw` fills
/// the dim as four bands AROUND the selection and never fills the interior. The pixels
/// inside the selection are therefore untouched by us, so a shot cropped to that rect is
/// clean with the overlay still painting.
///
/// **This is why the overlay no longer blanks.** DRAGON-456 read the flats, which meant a
/// re-read had to photograph the whole screen, which meant hiding everything we draw first.
/// Reading only the selection removes the reason.
///
/// Two states of hiding remain, and only for what is genuinely INSIDE the crop:
/// the scanner's own marks (QR boxes, text-word highlights). A shot taken while those are
/// up would scan our own highlights and feed them back in. They are cleared before the shot
/// rather than hidden, because a shot is only ever taken when the region changed or the
/// user asked for a re-read — in both cases the marks on screen describe pixels that are
/// about to stop being the answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanShot {
    /// Nothing in flight.
    Idle,
    /// Marks cleared, waiting one tick for the frame WITHOUT them to reach the screen
    /// before the shot is taken (`ScanShotTick`).
    Clearing,
    /// The region shot + scan passes are on their thread.
    Shooting,
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
            // A window with no captured pixels. On every platform but Windows this is a
            // window the compositor grab skipped and it drops out of the picker
            // (byte-identical to before). On Windows (DRAGON-232) a skipped window is one
            // the bounded `PrintWindow` ladder pre-filtered or timed out (a hung /
            // uncooperative app, e.g. RustDesk) — it still belongs in the picker, so it
            // is represented by a neutral placeholder tile instead of vanishing silently.
            #[cfg(not(windows))]
            let Some(img) = raw.get(&win.id) else {
                continue;
            };
            #[cfg(windows)]
            let placeholder = if raw.contains_key(&win.id) {
                None
            } else {
                log::info!(
                    "DRAGON-232 build_window_thumbs: no captured pixels for window {:?} \
                     (id {}); showing a placeholder tile (grab skipped/timed out)",
                    win.title,
                    win.id
                );
                Some(crate::platform::windows::placeholder_window_thumb(
                    &win.id, win.rect.2, win.rect.3,
                ))
            };
            #[cfg(windows)]
            let Some(img) = raw.get(&win.id).or(placeholder.as_ref()) else {
                continue; // unreachable: `placeholder` is Some whenever `raw` lacks the id
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
            // The PREWARMED variant, and only here. This is the launch path: the SCK
            // content snapshot it needs was already fetched by `prewarm_shareable_content`
            // moments ago, so the ordinary entry point's DRAGON-188 Bug 4 refresh would
            // throw that away and pay a second full round trip (~52ms measured warm) —
            // once per output, since this loops. `capture_wallpaper_prewarmed`'s doc
            // carries the argument for why reusing it is sound on the include-only filter
            // and how the deny-list fallback still gets its fresh snapshot. Every OTHER
            // caller of `capture_wallpaper` keeps the refresh; see that doc for why they
            // must.
            if let Some(img) = crate::platform::mac::capture_wallpaper_prewarmed(&desc.name) {
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

/// Does this capture want the pointer in the picture AT ALL? Pure, unit-tested
/// (`cursor_wanted_tests`). DRAGON-595 made this the ONE copy of the rule.
///
/// `want_cursor` is the "Preserve mouse cursor" preference (the raw persisted one at
/// launch, the capability-gated effective extra once a backend is resolved).
/// `color_picker` overrides it off: the picker is overlay-shaped but captures nothing
/// it will keep, and a baked pointer there is not a cosmetic blemish, it is a
/// permanent blind spot sitting over the pixels the user is trying to sample.
///
/// Deliberately says nothing about HOW the pointer would get there. That is the
/// backend's business and it answers with [`crate::platform::backend::CursorDelivery`]:
/// a native backend stamps a sprite it grabbed, the portal asks its stream to bake one
/// in. Before DRAGON-595 this rule existed TWICE, as `launch_cursor_needed` here and
/// `portal::cursor_request` there, each computing `want && !picker` for its own
/// mechanism and held together only by a test asserting they still agreed. Splitting
/// the WHETHER from the HOW is what let the two collapse into one.
pub(crate) fn cursor_wanted(want_cursor: bool, color_picker: bool) -> bool {
    want_cursor && !color_picker
}

/// Pure, unit-tested (`kind_cursor_veto_tests`): may a capture of this KIND keep the
/// pointer at all? DRAGON-604.
///
/// The scanner answers no, and this is not a preference the user can overrule. The
/// scanner exists to DECODE the pixels it is handed, QR and barcode finder patterns
/// through `detect::codes`, glyphs through `detect::text`. A pointer composited into
/// those pixels is not a blemish on a picture, it is opaque noise sitting on top of the
/// very thing being read, and it can land exactly on the finder pattern or the glyph
/// that decides whether the scan succeeds. There is no scan a user wants their mouse
/// in, so "Preserve mouse cursor" is not the right question to ask for one.
///
/// Written as an exhaustive `match` rather than a `matches!`, on purpose: a new [`Kind`]
/// must come here and state its answer instead of defaulting into someone else's lane.
pub(crate) fn kind_keeps_pointer(kind: Kind) -> bool {
    match kind {
        Kind::Scanner => false,
        // A screenshot and a screen recording are both PICTURES of the desktop, where
        // the pointer is content the user may legitimately want. They follow the
        // preference.
        Kind::Image | Kind::Video => true,
    }
}

/// Pure, unit-tested (`kind_cursor_veto_tests`): the capture extras a capture of `kind`
/// will actually apply. DRAGON-604.
///
/// THREE terms now, and the third can only ever take an extra away:
///
/// 1. `caps`, the ACTIVE backend's capability set ([`crate::platform::backend::Caps::capture_extras`]).
/// 2. `prefs`, the user's persisted preferences. `CaptureExtras::and` has folded these
///    two together since DRAGON-186, and that half is unchanged.
/// 3. `kind`, the capture MODE, via [`kind_keeps_pointer`].
///
/// # Why the veto lives in ONE function and not at the call sites
///
/// Because "the scanner must not photograph the mouse" is a property of SCANNING, so
/// every route into the scanner has to get the same answer, and a per-call-site check
/// is the shape that leaves one route uncovered. The routes are not a short list: the
/// overlay's scanner button, a `--scan` argv, the tray and menu-bar entries, a global
/// shortcut, and switching INTO the scanner from image or video part-way through a
/// session. They do, however, all share one seam, because every one of them ends up
/// asking [`App::effective_capture_extras`] what this capture applies, and that is the
/// only caller of this function. Putting the rule here means a route added later
/// inherits it without anyone remembering to wire it up.
///
/// It also reaches both MECHANISMS for free, which matters because the owner hit this
/// through the portal and the rule is not portal-specific
/// ([`crate::platform::backend::CursorDelivery`]): the portal path turns this bit into
/// its stream's cursor mode at request time (`portal::cursor_request`), and the native
/// path uses it to decide whether to stamp its launch-locked sprite
/// (`capture_flow::do_pixel_capture`). Two further readers come along at no cost and
/// stay honest: the on-overlay cursor indicator stops promising a pointer the scan will
/// not contain, and `screenshot_metadata` writes `cursor=off`.
///
/// # What deliberately does NOT consult this
///
/// [`launch_cursor_needed`], the launch-time decision to grab a cursor sprite at all.
/// A `--scan` launch still pays for that grab, because the grab is a ONE-SHOT locked at
/// startup (DRAGON-214) while the kind is free to change all session: veto it there and
/// a user who scans and then switches to image mode has silently lost the pointer for
/// the rest of the session, with no way to get it back. An unread sprite costs one
/// thread and nothing else, which its own doc already says. The veto belongs at the
/// point of USE, which is also what makes it survive a mode switch in either direction.
pub(crate) fn capture_extras_for_kind(
    caps: crate::platform::backend::CaptureExtras,
    prefs: crate::platform::backend::CaptureExtras,
    kind: Kind,
) -> crate::platform::backend::CaptureExtras {
    let mut extras = caps.and(prefs);
    if !kind_keeps_pointer(kind) {
        extras.cursor = false;
    }
    extras
}

/// Pure, unit-tested (`fallback_reseed_tests`): must the portal-frozen fallback overlay
/// GRAB ITS SEED FRAME AGAIN? DRAGON-604.
///
/// [`capture_extras_for_kind`] is enough everywhere the pointer is decided against
/// pixels we already hold, because it is re-read per capture. The `lab/flatpak`
/// fallback overlay is the one place that is not true, and the reason is structural:
/// with no layer shell there is nothing to draw a selector on, so the session grabs ONE
/// portal frame at launch and uses it as both the backdrop and, through
/// `scan_reads_frozen`, the pixels the scanner actually decodes. The portal bakes the
/// pointer into that frame at grab time (`CursorDelivery::InStream`), so a user who
/// launches an image capture with the preference on and then presses the scanner button
/// is scanning pixels with a mouse already in them, and no amount of re-reading the
/// rule can take it back out. Only a new frame can.
///
/// `seeded_with_cursor` is what the frame on screen was grabbed with, `None` before any
/// seed has been requested. Re-seeding is worth a portal round trip only when the
/// answer actually CHANGED, which keeps every kind switch that does not move the
/// pointer bit (image to video, or any switch with the preference already off) free.
///
/// The replacement request rides `RequestOrigin::InSession`, so it REPLAYS the monitor
/// token the launch seed banked and the user is not asked again. That is the owner's
/// "handle this intelligently": switching capture mode is not a target choice, which is
/// exactly the distinction `portal::replay_allowed` already draws.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn fallback_reseed_needed(
    fallback_active: bool,
    seeded_with_cursor: Option<bool>,
    wants_cursor: bool,
) -> bool {
    fallback_active && seeded_with_cursor.is_some_and(|had| had != wants_cursor)
}

/// DRAGON-582: should this launch LOCK the pointer sprite at startup? Pure, unit-tested.
///
/// [`cursor_wanted`] with the LAUNCH inputs: the persisted preference, before any
/// backend is resolved. Kept as its own name because the call site's question is
/// "which parts of the capture scene does this launch pay for", alongside
/// [`launch_flats_needed`] and [`launch_precapture_runs`], not "does this capture
/// draw a pointer".
///
/// It stays capability-UNGATED on purpose: the sprite grab is kicked before the
/// session's backend matters, and a sprite that turns out unusable costs one thread
/// and is simply never read (on a sandboxed portal session the native grab fails on
/// its own, since `screencopy::connect_raw` needs protocols that session lacks).
fn launch_cursor_needed(want_cursor: bool, color_picker: bool) -> bool {
    cursor_wanted(want_cursor, color_picker)
}

/// DRAGON-336: whether the LAUNCH-time frozen-flats grab must run. The flats are ONE
/// full-resolution RGBA snapshot PER OUTPUT held for the whole session (a 5120x1440
/// monitor is 28.1 MB), and only TWO features can ever read them:
///
/// - the freeze backdrop + freeze capture (every reader is gated on [`App::freezing`],
///   which requires the persisted `freeze` setting), and
/// - the QR/OCR scanners, whose `MarksPoll` crops its scan source out of the flats
///   (`crop_frozen`) precisely because the live screen carries our own dimmed overlay.
///
/// A launch with freeze OFF in a non-scanner kind can never read them, so it must not
/// pay ~30 MB of resident memory for the whole session. Both consumers can be turned on
/// mid-session: entering the scanner kicks a lazy grab (`App::kick_frozen_flats`), and
/// see that function's doc for the one limitation a lazy grab carries. Pure so the
/// gating is unit-testable without the App.
///
/// DRAGON-582 added the THIRD reader: the COLOUR PICKER samples the flats for every
/// pointer move, and it is not a `Kind` (it is its own launch shape, `--color-picker`),
/// so it needs its own term. Its grab is unconditional rather than
/// preference-gated, because the picker cannot function at all without a pixel source:
/// see `app::color_picker`'s `PixelSource` for why a live read would return our own
/// dimming layer instead of the desktop.
fn launch_flats_needed(
    active: bool,
    want_freeze: bool,
    launch_kind: Kind,
    color_picker: bool,
) -> bool {
    active && (want_freeze || launch_kind == Kind::Scanner || color_picker)
}

/// DRAGON-456: whether pressing the scan kind button REFRESHES the scan rather than
/// switching kind. It refreshes exactly when the scanner is already the active kind — the
/// press has no kind to change, so it re-reads the screen instead.
///
/// This is the whole of the "same button, new meaning" rule, and both the update path and
/// the button's hover face read it, so the affordance can never disagree with the action.
/// Pure so the rule is unit-testable without the App.
fn scan_press_refreshes(current: Kind, pressed: Kind) -> bool {
    current == Kind::Scanner && pressed == Kind::Scanner
}

// DRAGON-460 removed `overlay_blanked_for_flats_grab`.
//
// It existed so a scan could re-read the whole screen without photographing our own
// overlay. The scanner now reads a live shot of the SELECTION, and `RegionSelection::draw`
// never fills that interior, so there is nothing of ours in the crop and nothing to hide.
// The marks are the one exception and they are cleared in `begin_scan_shot`, not blanked.

// DRAGON-460 removed `frozen_delivery_accepted`. It declined an EMPTY re-grab landing on
// top of real flats, so a failed scan refresh could not downgrade the scanner to no source
// at all. Scan refreshes no longer write through the flats slot — a failed live region shot
// is handled where it lands, in `MarksPoll`, by leaving the previous answer standing.

/// What THIS launch does about the frozen-flats grab, resolved by `App::init` from the
/// launch gates and handed to [`acquire_scene`] as ONE value (DRAGON-663).
///
/// One parameter rather than a `want` flag plus a `hold` flag, for two reasons. The states
/// are not independent (a launch that holds necessarily wants them, so the fourth
/// combination has no meaning and could still be written), and [`acquire_scene`] is already
/// at clippy's argument budget, which is what the `want_flats` doc below is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FlatsPlan {
    /// Nothing on this launch can read the flats ([`launch_flats_needed`] false: the common
    /// freeze-off, non-scanner, non-picker screenshot). An EMPTY result is parked so the
    /// very first drain tick clears `frozen_pending` and stops the poll.
    Skip,
    /// Grab them now, on the deferred thread. Every ordinary freeze / scanner / picker
    /// launch, and the historical behaviour of `want_flats == true`.
    Now,
    /// Wanted, but NOT YET, because something of ours is on screen that the grab would
    /// photograph. Nothing is spawned and nothing is parked, so `frozen_pending` stays
    /// armed and the dim cannot begin to fade (`overlay::dim_fade_may_start`); whoever
    /// armed the hold runs [`spawn_frozen_flats_grab`] once its own condition clears.
    ///
    /// TWO arming reasons today, and they release on different signals:
    /// [`capture_flow::menu_flats_hold_needed`] holds a Linux tray-menu launch until our
    /// overlay has taken keyboard focus and retired the dropdown (DRAGON-600), and
    /// [`capture_flow::picker_flats_held`] holds a colour-picker launch until its countdown
    /// has fired and its digits have left the screen (DRAGON-663).
    Hold,
}

/// `want_flats` is [`launch_flats_needed`]'s answer, resolved by the CALLER rather than
/// re-derived here from a kind plus two flags. That keeps the decision visible next to the
/// other launch gates in `App::init`, and keeps this signature inside clippy's argument
/// budget now that the colour picker is a third reader of the flats. Since DRAGON-663 it
/// arrives as a [`FlatsPlan`], which carries the "not yet" case the boolean could not.
fn acquire_scene(
    active: bool,
    launch_mode: Mode,
    want_cursor: bool,
    want_freeze: bool,
    flats: FlatsPlan,
    wallpaper: Option<std::path::PathBuf>,
    radius: f32,
) -> (PrecaptureSlot, HashMap<String, FrozenOutput>, FrozenSlot, WallpaperSlot, CursorSlot) {
    let want_flats = flats != FlatsPlan::Skip;
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
    // DRAGON-336: the clean pre-overlay snapshot is grabbed only when THIS launch can
    // actually consume it — freeze on, or a scanner launch (see `launch_flats_needed`).
    // A plain region/window/monitor launch with freeze off held ~30 MB per output for
    // the whole session that no reader could ever reach. The freeze display still gates
    // on `self.freeze`; a mid-session switch INTO the scanner kicks a lazy grab
    // (`App::kick_frozen_flats`), and turning freeze on mid-session falls back to the
    // live capture path (`freezing()` is false on an empty map) until the next launch —
    // the same "next launch" contract the per-window freeze pixels already carry above.
    let frozen_slot: FrozenSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    // With the grab skipped nothing would ever fill the slot, so `frozen_pending` (armed
    // from `scene_active` in `init`) would poll at 16ms forever and macOS's commit-time
    // `await_frozen_flats` would burn its whole 750ms budget waiting on it. Park an EMPTY
    // result so the very FIRST drain tick clears the flag and stops the poll.
    if active
        && !want_flats
        && let Ok(mut g) = frozen_slot.lock()
    {
        *g = Some(HashMap::new());
    }
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
                // DRAGON-336: skip the per-output flats when nothing this launch can read
                // them (freeze off + not a scanner launch). The deferred per-output
                // WALLPAPER resolve below still runs on this same thread exactly as
                // before — the window picker needs it regardless of freeze.
                // DRAGON-663: `Hold` skips it here too, and for the opposite reason: this
                // launch DOES read them, just not from the screen as it looks right now.
                // `spawn_frozen_flats_grab` runs the identical grab when the hold releases.
                if flats == FlatsPlan::Now {
                    let flats = grab_frozen_flats(want_cursor);
                    crate::util::timing_mark("acquire_scene: frozen all_outputs (deferred thread done)");
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(flats);
                    }
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
        // DRAGON-336: the plan (not bare `active`). A freeze-off, non-scanner launch
        // has no reader for the flats, so it neither grabs nor retains them.
        //
        // `Hold` is the third case (DRAGON-600 for a Linux tray dropdown, DRAGON-663 for a
        // colour picker counting down): something of ours is on screen that this grab would
        // photograph, so it is not started here at all. The releaser runs the identical
        // grab through `spawn_frozen_flats_grab` when its own condition clears.
        match flats {
            FlatsPlan::Now => spawn_frozen_flats_grab(frozen_slot.clone(), want_cursor),
            FlatsPlan::Hold => crate::util::timing_mark(
                "acquire_scene: frozen all_outputs (HELD, see FlatsPlan::Hold)",
            ),
            FlatsPlan::Skip => {}
        }
        HashMap::new()
    };
    (precapture, frozen, frozen_slot, wallpaper_slot, cursor_slot)
}

/// The DEFERRED frozen-flats grab, on its own thread, depositing into `slot` for
/// `CaptureMsg::FrozenReady` to drain (DRAGON-212). Safe to thread: `all_outputs` opens
/// its OWN wayland connection, and the macOS arm is the same `grab_frozen_flats` its
/// launch thread runs.
///
/// Extracted from [`acquire_scene`] by DRAGON-600 because there are now THREE moments it can
/// start from. The usual one is launch. The second is once a tray-menu child's overlay has
/// taken keyboard focus and the dropdown that launched it is gone. The third (DRAGON-663) is
/// once a colour picker's countdown has fired and its digits have left the screen. All three
/// must run the same grab, so there is one body.
///
/// **This was a no-op on macOS until DRAGON-663**, and the tombstone matters because the
/// reasoning was sound at the time and is no longer. The only caller then was
/// `tick_menu_hold`, whose one source ([`menu_flats_held`]) is `cfg!(target_os = "linux")`,
/// so the mac call site was structurally unreachable and the stub existed purely so the
/// portable call in `update/capture.rs` needed no `cfg` of its own. The picker's reveal is a
/// caller that DOES run on macOS, so an empty body there would mean a picker with a delay
/// never got any pixels at all.
fn spawn_frozen_flats_grab(slot: FrozenSlot, want_cursor: bool) {
    crate::util::timing_mark("acquire_scene: frozen all_outputs (kick DEFERRED thread)");
    std::thread::spawn(move || {
        let flats = grab_frozen_flats(want_cursor);
        crate::util::timing_mark("acquire_scene: frozen all_outputs (deferred thread done)");
        if let Ok(mut g) = slot.lock() {
            *g = Some(flats);
        }
    });
}

/// Whether this process was launched by activating a tray-menu row
/// ([`crate::recording_ui::MENU_LAUNCH_ENV`], set by the Linux resident and recording
/// tray). Read from the environment rather than argv because it is not a capture option:
/// it says how we were started, not what to capture.
pub(crate) fn menu_launched() -> bool {
    std::env::var_os(crate::recording_ui::MENU_LAUNCH_ENV).is_some()
}

/// Whether the frozen-flats grab is held for the tray dropdown (DRAGON-600). The `cfg!`
/// (not a `#[cfg]`) keeps ONE compiled body on every platform and answers false off Linux,
/// where the daemon owns its own menu and has already dismissed it before spawning, so
/// macOS and Windows keep byte-identical launch behaviour.
pub(crate) fn menu_flats_held(want_flats: bool) -> bool {
    cfg!(target_os = "linux") && capture_flow::menu_flats_hold_needed(menu_launched(), want_flats)
}

/// Linux (DRAGON-600): the tray-dropdown hold on the frozen-flats grab. `None` for every
/// launch that has no dropdown on screen, which is every launch except a tray-menu one.
#[derive(Debug, Clone, Copy)]
pub struct MenuFlatsHold {
    /// Carried from the launch gates so the held grab is the same grab.
    pub want_cursor: bool,
    /// When the hold was armed. Drives the OUTER bound, `MENU_HOLD_BUDGET_MS`.
    pub armed: std::time::Instant,
    /// When one of our overlays took keyboard focus: the event that dismisses the
    /// dropdown. `None` until it happens, and the settle is counted from it.
    pub focused: Option<std::time::Instant>,
}

struct OutputState {
    output: OutputHandle,
    id: window::Id,
    name: String,
    logical_pos: (i32, i32),
    logical_size: (u32, u32),
    /// CAPTURE units per POINT for THIS output (DRAGON-448) — the factor behind
    /// [`Self::units`]. `logical_pos` / `logical_size` above are CAPTURE space (physical
    /// pixels on Windows, points on macOS and Linux); the overlay's iced viewport is
    /// POINTS. This is the whole gap, resolved per output at mint time by
    /// `platform::overlay_point_scale`, so a mixed-DPI desktop gives each overlay its OWN
    /// factor instead of one global scale. `1.0` everywhere except a Windows monitor above
    /// 100% scaling, which is what keeps every other platform byte-identical.
    point_scale: f32,
    /// The output's point→pixel buffer scale (physical / logical), COSMIC integer OR
    /// fractional. Cached into `preview_output_scale` when a capture picks this output,
    /// so the windowed preview opens at the capture's true on-screen (logical) size on
    /// scaled displays (DRAGON-221). `1.0` on 1× outputs. Linux-only; macOS derives the
    /// backing scale live from `NSScreen` (`platform::mac::scale_for`).
    #[cfg(target_os = "linux")]
    scale: f32,
    /// `lab/flatpak` (Linux, portal-frozen fallback only): the ONE fallback toplevel's
    /// ACTUAL point size, as its last resize event reported it. Wayland gives a client
    /// no say in which monitor a fullscreen toplevel maps on, so the window can land on
    /// a monitor whose geometry differs from this (the granted) output's; [`Self::units`]
    /// then builds the LETTERBOX bridge (`geometry::OverlayUnits::letterbox`) that shows
    /// the frozen frame centred at its OWN aspect and maps window points back onto it,
    /// bar points clamping to the frame's edge. `None` on every layer-surface output:
    /// the uniform bridge, byte-identical to before this existed.
    #[cfg(target_os = "linux")]
    fallback_win_size: Option<(f32, f32)>,
    /// Whether this overlay has been natively placed. Interior-mutable because
    /// `configure_overlay` observes placement behind `&self`.
    ///
    /// macOS (DRAGON-204): whether `place_overlay` has raised this overlay to the shielding
    /// level and framed it to the full display. The overlay is CREATED clamped below the
    /// menu bar (winit's AlwaysOnTop level), so it renders TRANSPARENT (empty) until this is
    /// set — the clamp-then-reframe jump happens on an invisible window, never seen. Set on a
    /// successful placement AND when placement gives up (so a never-matched overlay still
    /// draws).
    ///
    /// Windows (DRAGON-437): the same flag with a NARROWER meaning — it is set ONLY on a
    /// confirmed placement, never on give-up, because on Windows it is what tells
    /// `sub_overlay_finalize` this output is done. The view keeps DRAWING through the
    /// whole placement dance (the window presents real frames while DWM-cloaked, which is
    /// what the cloak phase is for), so `overlay_view`'s draw-nothing gate stays
    /// macOS-only and mac behaviour is byte-identical. What Windows DOES read this flag
    /// for at draw time is the dim fade's latch (DRAGON-653, `dim_now_revealed`): the
    /// faded dim is held at zero, without consulting the latch, until placement lands, so
    /// the 200ms ramp cannot spend itself on cloaked frames nobody can see.
    #[cfg(not(target_os = "linux"))]
    placed: std::cell::Cell<bool>,
    /// macOS (DRAGON-646): `place_overlay` changed this window's frame and iced has not seen
    /// the resize yet, so `placed` is deliberately still false.
    ///
    /// The overlay is minted at winit's `AlwaysOnTop` level, which AppKit clamps below the
    /// menu bar, and `place_overlay` grows it back to the full display. Until winit delivers
    /// the matching `Resized`, iced lays the view out for the OLD size while the window is
    /// already the NEW one, so any content drawn in that gap is stretched and then snaps.
    /// Cleared by `ConfigWindowResized` when the reported size matches, or by the bounded
    /// `OverlayFrameSettled` fallback; either way `placed` is set at the same moment. Not a
    /// second paint gate: `placed` remains the ONE thing the view reads.
    #[cfg(target_os = "macos")]
    frame_pending: std::cell::Cell<bool>,
}

impl OutputState {
    /// This overlay's units bridge (DRAGON-448): CAPTURE space ↔ POINT space, for THIS
    /// output. Every crossing between `OutputState` geometry and anything iced hands us or
    /// renders goes through the returned [`OverlayUnits`] — see its doc for the contract.
    /// The Linux fallback toplevel carries the letterbox bridge (see
    /// `fallback_win_size`); every other output takes the uniform bridge exactly as
    /// before.
    fn units(&self) -> crate::geometry::OverlayUnits {
        #[cfg(target_os = "linux")]
        if let Some(win) = self.fallback_win_size {
            return crate::geometry::OverlayUnits::letterbox(
                self.logical_pos,
                self.logical_size,
                win,
            );
        }
        crate::geometry::OverlayUnits::new(self.logical_pos, self.point_scale)
    }

    /// This overlay surface's extent in POINTS — the iced viewport every layout on it must
    /// fit inside. `logical_size` is CAPTURE space and is `point_scale`× larger on a scaled
    /// Windows monitor, so laying out against it directly is what pushed the toolbar off
    /// the screen (DRAGON-448).
    fn point_size(&self) -> (f32, f32) {
        self.units().size_to_point(self.logical_size)
    }
}

// `test` as well as macOS so the Linux/Windows suites TYPE-CHECK this alongside its one
// caller (`App::startup_presence`), which is compiled under the same cfg for the same
// reason — the macOS build is not run on those boxes (see CLAUDE.md). Dead there, hence
// the allow.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl OutputState {
    /// Whether this overlay is something the USER can actually SEE (DRAGON-439).
    ///
    /// On macOS that is exactly `placed` (see its doc above): the window is minted
    /// clamped below the menu bar and draws a fully transparent `Space` until
    /// `place_overlay` raises and reframes it, so an overlay that merely EXISTS shows the
    /// user nothing.
    ///
    /// Off macOS this answers `true`, which is NOT a claim that those overlays are visible
    /// the moment they are minted — Windows opens the overlay HIDDEN on purpose (`shell.rs`,
    /// the komorebi opt-out) and shows it natively later, so it has the same mint→visible
    /// gap. It answers `true` because nothing off macOS ARMS the startup guard, so the
    /// value is never read outside `cfg(test)` and Linux/Windows stay byte-identical. A
    /// platform opting the guard in later must give this a real per-platform signal (the
    /// Windows one being "the native show has run"); inheriting the `true` would hand it
    /// exactly the DRAGON-439 bug this accessor exists to fix.
    fn user_visible(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.placed.get()
        }
        #[cfg(not(target_os = "macos"))]
        true
    }
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
    /// The live mic capture's stop handle — explicitly stopped when the dialog closes.
    /// An ffmpeg child on Linux/Windows, a native `AVCaptureSession` on macOS.
    mic: crate::audio::clean_mic::MicPcmStop,
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

/// Windows (DRAGON-254): the native Common Item Dialog folder browser (rfd wrapping
/// `IFileOpenDialog` with `FOS_PICKFOLDERS`). The dialog is modal and owns its own
/// COM STA apartment, so run it on a dedicated blocking thread and await the pick
/// over a oneshot — `pick_folder` runs on iced's async executor, and blocking that
/// thread on a native modal would stall the whole UI (same reasoning as macOS). The
/// return-to-surface semantics around this are platform-agnostic (see the callers).
#[cfg(target_os = "windows")]
async fn pick_folder() -> Option<std::path::PathBuf> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::platform::windows::file_panel::pick_folder());
    });
    rx.await.ok().flatten()
}

/// Fallback for any other target: no native folder picker. Stubbed.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
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

/// Windows (DRAGON-254): the native "Save As" file chooser (rfd wrapping
/// `IFileSaveDialog`), pre-filled with BOTH halves of `suggested` — the folder it opens
/// in and the default file name (DRAGON-476). Modal with its own COM STA
/// apartment, so run it on a dedicated blocking thread and await the pick over a
/// oneshot (same reasoning as `pick_folder`). Used by the preview window's Save As;
/// the overlay-vs-window return semantics around the result are platform-agnostic
/// (see `save_as_dialog` / `SaveAsResult`).
#[cfg(target_os = "windows")]
async fn pick_save_path(suggested: std::path::PathBuf) -> Option<std::path::PathBuf> {
    // DRAGON-476: the directory half used to be dropped here on the theory that
    // `IFileSaveDialog` remembering its last folder was good enough ("only the Linux
    // portal accepts a folder" — wrong: rfd's `set_directory` is `SetFolder`). The
    // remembered folder is wherever the dialog last happened to be, which broke the
    // DRAGON-467 contract that the picker opens in the folder the Save setting names.
    // `save_as_dialog` has already best-effort created this folder.
    let name = suggested_file_name(&suggested);
    let dir = suggested
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf);
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx
            .send(crate::platform::windows::file_panel::pick_save_file(dir.as_deref(), &name));
    });
    rx.await.ok().flatten()
}

/// Fallback for any other target: no native save panel. Stubbed.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
async fn pick_save_path(_suggested: std::path::PathBuf) -> Option<std::path::PathBuf> {
    None
}

/// The FILE-NAME half of a suggested save path, for the native panels that take a name only
/// (macOS `NSSavePanel`, Windows `IFileSaveDialog`). Falls back to `"capture"` for a path with
/// no file name at all, which is the same fallback the picker has always used.
///
/// Pure; unit-tested in `pick_save_path_tests`. It is compiled everywhere (its callers are
/// not) so the Linux gate covers the rule, in the house style for a decision whose only
/// consumers live behind a `cfg`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn suggested_file_name(suggested: &std::path::Path) -> String {
    suggested
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "capture".to_string())
}

/// macOS (DRAGON-157): the native `NSSavePanel` "Save As" panel, pre-filled with
/// `suggested_name`. App-modal on the MAIN thread, so run it on a dedicated blocking
/// thread and await the pick over a oneshot (same reasoning as `pick_folder`). Used
/// by the preview window's Save As; the overlay-vs-window return semantics around the
/// result are platform-agnostic (see `save_as_dialog` / `SaveAsResult`).
#[cfg(target_os = "macos")]
async fn pick_save_path(suggested: std::path::PathBuf) -> Option<std::path::PathBuf> {
    // The panel takes a NAME; `NSSavePanel` remembers the last directory itself, so the
    // folder half of the suggestion is dropped here. NOTE (DRAGON-476): the old rationale
    // ("only the Linux portal accepts a folder") is wrong — `setDirectoryURL` exists, and
    // the Windows arm now passes its folder through. This arm was left byte-identical
    // because the change needs mac hardware to build and verify, and because
    // remember-last-folder is a real macOS convention the owner may prefer; the ticket
    // holds that decision.
    let name = suggested_file_name(&suggested);
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::platform::mac::file_panel::pick_save_path(name));
    });
    rx.await.ok().flatten()
}

/// Open the XDG desktop-portal "save file" picker pre-filled with `suggested` — both halves
/// of it, the FOLDER and the NAME (DRAGON-467). Used by the preview editor's Save.
///
/// The folder matters: with "Automatically save originals" off the capture's bytes live in
/// the session runtime directory, and opening the picker THERE would be useless. The
/// suggestion is built from the user's configured save folder instead
/// (`App::preview_save_target`), which is what the setting is for.
#[cfg(target_os = "linux")]
async fn pick_save_path(suggested: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let name = suggested
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture".to_string());
    let req = ashpd::desktop::file_chooser::SelectedFiles::save_file()
        .title("Save capture as")
        .current_name(name.as_str())
        .modal(true);
    // A folder the portal rejects (it validates the path, e.g. an interior NUL) must not take
    // the whole dialog down with it — fall back to the portal's own default location, which
    // is what happened before this pre-fill existed.
    let req = match suggested.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => match ashpd::desktop::file_chooser::SelectedFiles::save_file()
            .title("Save capture as")
            .current_name(name.as_str())
            .modal(true)
            .current_folder(dir)
        {
            Ok(with_dir) => with_dir,
            Err(e) => {
                log::debug!("save picker: no pre-set folder ({e})");
                req
            }
        },
        None => req,
    };
    let files = req.send().await.ok()?.response().ok()?;
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
/// actually read (entering the recording UI or the settings video/Health pages), so a
/// region/window/scan capture launch never spawns ffmpeg.
///
/// Interior-mutable so the `&self` settings-view accessors can trigger the probe on
/// first read.
///
/// DRAGON-571 split the old single "preferred" notion in two. The REQUEST
/// (`requested`) is the persisted intent: "auto", or the concrete id a real
/// settings-picker click wrote (the legacy `record_hardware=off` still maps to
/// software at read time). The DISPLAY resolution (`preferred`) is what the picker
/// shows for that intent right now, computed from the probed list plus the
/// last-known-good hint, and NEVER persisted. The old holder resolved "auto" once and
/// wrote the winner into `preferred_encoder` as if the user had chosen it, which let
/// one transient probe failure pin "software" forever; a recording now carries the
/// request itself (see `request`), so the worker's hint-first ladder re-resolves auto
/// every session.
#[derive(Default)]
pub struct EncoderResolve {
    /// The probed encoder list, computed once on first access.
    list: std::cell::OnceCell<Vec<crate::encode::EncoderInfo>>,
    /// The persisted encoder INTENT ("auto" or a concrete user pick), loaded lazily
    /// from state and overwritten only by `set_preferred` (a picker click or a
    /// whole-config apply). What `App::save_state` persists back.
    requested: std::cell::RefCell<Option<String>>,
    /// The DISPLAY resolution of `requested`. `None` until first computed (or after
    /// an "auto" apply); never written to disk.
    preferred: std::cell::RefCell<Option<String>>,
    /// Windows (DRAGON-238): whether the OFF-THREAD encoder probe has been kicked. The
    /// Windows hardware tier probes each encoder with a real ffmpeg encode (seconds), so
    /// the settings video / Health page must NOT block on the first read — the page peeks
    /// (`peek`), a background task computes the list, and the result arrives as a message
    /// (`finish_probe`). Linux/mac keep the synchronous first-read probe (timing untouched).
    #[cfg(windows)]
    probing: std::cell::Cell<bool>,
}

impl EncoderResolve {
    /// Probe the usable encoders (`ffmpeg -encoders` + the hardware probe-encodes), applying
    /// the `CCK_HEALTH_FORCE_WARN` review filter. IDENTICAL whether run synchronously
    /// (Linux/mac first read) or off the UI thread (Windows async probe, DRAGON-238).
    fn probe_list() -> Vec<crate::encode::EncoderInfo> {
        let mut e = crate::encode::available_encoders();
        if std::env::var_os("CCK_HEALTH_FORCE_WARN").is_some() {
            e.retain(|enc| enc.id == "software");
        }
        e
    }

    /// The probed encoder list, resolving (and caching) it on first access. This is
    /// the ONLY place `available_encoders()` runs, so no launch pays the ffmpeg probe
    /// until the list is genuinely needed. On Windows this still BLOCKS if reached before
    /// the off-thread probe filled the cell (e.g. a recording started without opening
    /// settings) — the settings pages avoid that by peeking instead (`peek`).
    fn list(&self) -> &[crate::encode::EncoderInfo] {
        self.list.get_or_init(Self::probe_list)
    }

    /// Windows (DRAGON-238): a NON-BLOCKING peek at the probed list — `None` until the
    /// off-thread probe has finished. The settings video / Health pages use this so the UI
    /// thread never blocks on the encoder probe.
    #[cfg(windows)]
    fn peek(&self) -> Option<&[crate::encode::EncoderInfo]> {
        self.list.get().map(Vec::as_slice)
    }

    /// Windows (DRAGON-238): whether an off-thread probe should be kicked now — false if the
    /// list is already resolved or a probe is already in flight. Sets the in-flight flag as
    /// a side effect (so a caller that gets `true` owns kicking exactly one task).
    #[cfg(windows)]
    fn begin_probe(&self) -> bool {
        if self.list.get().is_some() || self.probing.get() {
            return false;
        }
        self.probing.set(true);
        true
    }

    /// Windows (DRAGON-238): store the off-thread probe result. Idempotent — a racing
    /// synchronous read (a recording start) may have filled the cell first, in which case
    /// the async result is dropped. Clears the in-flight flag either way.
    #[cfg(windows)]
    fn finish_probe(&self, list: Vec<crate::encode::EncoderInfo>) {
        let _ = self.list.set(list);
        self.probing.set(false);
    }

    /// The persisted encoder INTENT: "auto" or the user's explicit pick, with the
    /// legacy `record_hardware=off` toggle honoured by mapping to "software" (the
    /// same read-time rule as ever). Never a resolution (DRAGON-571): resolving
    /// happens per recording (the worker's hint-first ladder) and per display
    /// (`preferred`), and neither writes back here.
    fn requested(&self) -> String {
        if let Some(r) = self.requested.borrow().as_ref() {
            return r.clone();
        }
        let p = crate::state::load();
        // The "use hardware encoding" toggle was removed (the Software entry in the
        // encoder picker covers it); honour an old off setting by picking software.
        let r = if p.record_hardware { p.preferred_encoder } else { "software".to_string() };
        *self.requested.borrow_mut() = Some(r.clone());
        r
    }

    /// The `(requested, hint)` pair a recording start hands to `RecordSettings`
    /// (DRAGON-571): the intent verbatim, so "auto" travels AS "auto" and the
    /// worker's ladder resolves it fresh each session, plus, for auto only, the
    /// last-known-good hint that keeps the happy path at one probe-encode. Reads NO
    /// probed list: a recording start no longer pays the `ffmpeg -encoders` scan (or,
    /// on Windows, blocks on the off-thread probe) just to name its encoder.
    fn request(&self) -> (String, Option<String>) {
        let requested = self.requested();
        let hint = if requested == "auto" { stored_auto_hint() } else { None };
        (requested, hint)
    }

    /// The encoder id the settings picker DISPLAYS, computing it on first access from
    /// the persisted intent, the probed list and the auto hint
    /// (`display_encoder_choice`). For a concrete pick this is the pick while still
    /// probed-usable; for "auto" it is what auto would resolve to right now. NEVER
    /// persisted (DRAGON-571): displaying a resolution and recording the user's
    /// choice are different facts, and only `set_preferred` writes the latter.
    fn preferred(&self) -> String {
        if let Some(p) = self.preferred.borrow().as_ref() {
            return p.clone();
        }
        let (requested, hint) = self.request();
        let list = self.list();
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        let resolved = display_encoder_choice(&requested, hint.as_deref(), &ids);
        *self.preferred.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// Overwrite the live preferred-encoder INTENT (a real settings-picker click, or
    /// a whole-config apply). A concrete id also seeds the display cache, so a later
    /// `preferred()` returns it without probing; "auto" clears the cache instead, so
    /// the display re-resolves against the probed list on the next read. Persisting
    /// is the caller's save (`save_state` snapshots `requested()`), which is exactly
    /// why an auto RESOLUTION can never land on disk: only intent flows through here.
    fn set_preferred(&self, id: String) {
        *self.preferred.borrow_mut() = if id == "auto" { None } else { Some(id.clone()) };
        *self.requested.borrow_mut() = Some(id);
    }

    /// Whether the ffmpeg-spawning encoder probe has run yet (test-only inspector: the
    /// DRAGON-201 guarantee is that a screenshot launch leaves this `false`).
    #[cfg(test)]
    fn probed(&self) -> bool {
        self.list.get().is_some()
    }
}

/// The persisted last-known-good auto-encoder hint, empty mapped to `None`. Read
/// fresh from disk rather than mirrored on `App`: the recording workers update it
/// directly (`state::note_encoder_auto_hint`), so a live mirror would only be a
/// staleness bug waiting to happen, and `App::save_state` carries the field from
/// disk for the same reason (see `app/persist.rs`).
fn stored_auto_hint() -> Option<String> {
    let h = crate::state::load().encoder_auto_hint;
    (!h.is_empty()).then_some(h)
}

/// Pure, unit-tested (DRAGON-571). What the settings encoder picker displays, given
/// the persisted intent, the auto hint, and the PROBED list's ids (ranked best
/// first): a concrete pick shows itself while still usable; everything else shows
/// what auto would resolve to right now, the hint-first ladder order filtered through
/// the list. The picker deliberately keeps its no-"auto"-row UI: this is the READ
/// side only, and writing `preferred_encoder` stays exclusively a real user click.
fn display_encoder_choice(requested: &str, hint: Option<&str>, available: &[&str]) -> String {
    if requested != "auto" && available.contains(&requested) {
        return requested.to_string();
    }
    crate::encode::auto_probe_order(crate::encode::AUTO_LADDER, hint)
        .into_iter()
        .find(|id| available.contains(id))
        .unwrap_or("software")
        .to_string()
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
    /// Where the window picker's loading state has got to (DRAGON-645): whether the
    /// background pre-capture is still running, whether its spinner has been revealed at
    /// all, and how much longer it stays up once it has.
    ///
    /// Replaces the old `windows_loading` + `window_warmup` pair. That pair drew the spinner
    /// unconditionally from the pre-capture's first frame, so a load that finished in 60ms
    /// got a 60ms spinner, which reads as a glitch rather than as a loading state. See
    /// [`overlay::PickerLoad`] for the reveal threshold and the minimum-once-shown.
    picker_load: overlay::PickerLoad,
    /// Has the window picker drawn a frame yet (DRAGON-645)? Latched once, from
    /// `overlay::window_view`, and read by the `LoadingTick` handler.
    ///
    /// It is what the spinner's reveal threshold is counted from, because the poll driving
    /// that threshold starts in `App::init` while a macOS window-mode launch does not put an
    /// overlay on screen for the better part of a second. Counting from the subscription
    /// spends the whole threshold before anything is visible and reveals a spinner for a wait
    /// the user never had. A `Cell` because the only honest place to observe "we drew a
    /// frame" is inside the view, which holds `&self`.
    picker_painted: std::cell::Cell<bool>,
    /// Shared slot the pre-capture thread fills; polled while `picker_load` is not `Idle`.
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
    /// (padding/shadow/wallpaper margins change the composed size vs the selection).
    /// Set when `WindowGrabbed` would have swapped; consumed by `present_capture`; reset
    /// at `begin_capture`.
    ///
    /// This line used to add "and a post-open `window::resize` is not honored on COSMIC",
    /// which was WRONG about the cause, and cost an hour when the next resize bug was
    /// diagnosed against it (DRAGON-684). cosmic-comp never saw those requests: winit's
    /// Wayland backend skipped any client resize while the last configure was tiled, and
    /// cosmic-comp marks ordinary floating windows tiled. Our winit fork opens that gate
    /// (see FORKED_CHANGES.md, patch 4), so a post-open resize DOES work now. Opening at
    /// the right size is still the better shape here and stays, but it is now a choice
    /// rather than a workaround.
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
    /// Windows (DRAGON-305): a WINDOWED single-window capture PRE-OPENED its fullscreen loading
    /// BLOCKER (the overlay preview spinner + cancel X) to cover the whole off-thread
    /// active-appearance grab + compose/save — the analog of macOS's `mac_preview_preopen`. The
    /// cover is placed NON-ACTIVATING ([`crate::platform::windows::window::place_overlay`] with
    /// `activate=false`) so it never steals the target window's foreground during the grab (which
    /// would capture its INACTIVE chrome). While set, `preview_surface_for` forces the fullscreen
    /// overlay even though the editor is Windowed; `WindowGrabbed` clears it and arms
    /// `windowed_swap_pending` so `present_capture` swaps the cover for the real preview window
    /// once the composed dims land. Never set off Windows, nor in overlay-preview mode (which
    /// already shows the fullscreen blocker after the grab).
    #[cfg(windows)]
    win_preview_preopen: bool,
    /// Windows (DRAGON-246): set true once the settings window has been CONFIRMED matched
    /// (`center_settings_window` matched the born-set title, centered + natively showed it).
    /// Gates the `sub_settings_liveness` watchdog so it can NEVER fire during the legitimate
    /// open-hidden → center-and-show phase — only AFTER the titled window has actually been
    /// matched does its disappearance mean a genuine vanish-without-`Closed`, at which point
    /// the watchdog ends the instance so the settings mutex + pid file don't leak. Never
    /// set off Windows (write-once; the one-shot process exits before it would reset).
    #[cfg(windows)]
    settings_shown_confirmed: bool,
    /// Windows (DRAGON-299/313): `false` from the settings window's open until its size has SETTLED
    /// (`ConfigWindowResettle`, a beat after the show). The window opens hidden and is centered +
    /// natively shown on the first poll now that the title is born-set (`center_settings_window`,
    /// the preview's `show_centered` twin), so the DRAGON-299 sub-min size STOMP is not observed;
    /// while this is `false` the resize handler still DEFENSIVELY ignores open-time transient resizes
    /// (so a transient can never be persisted as the remembered size) and re-asserts on any sub-min.
    /// Once `true`, real USER resizes are tracked into `settings_size`. Never set off Windows (their
    /// settings window opens visible, compositor-managed, with no such stomp — track every resize).
    #[cfg(windows)]
    settings_size_ready: bool,
    /// Windows (DRAGON-281): the preview surface whose native show/place has been
    /// CONFIRMED (finalize succeeded — `show_centered` / `place_overlay` returned true).
    /// Gates `sub_preview_finalize`: while a preview surface exists whose id is not this,
    /// the subscription re-drives its finalize every ~80ms. That safety net exists because
    /// the preview is minted HIDDEN (komorebi opt-out) and shown only by the one-shot
    /// `window::open` follow-up, which is NOT delivered while cck is a BACKGROUND process
    /// — the DRAGON-281 case where the user focuses another window during the (click-
    /// through) countdown, so the capture commits with cck backgrounded and the preview
    /// HWND stays hidden forever. Timer subscriptions DO pump while backgrounded, so this
    /// is the reliable re-driver. Storing the id (not a bool) auto-invalidates on a
    /// re-mint (toggle/swap points `preview.window` at a new id), so the subscription
    /// re-fires for the fresh surface. Never set off Windows.
    #[cfg(windows)]
    preview_shown_confirmed: Option<window::Id>,
    /// Windows (DRAGON-437): whether the overlay finalize driver still has work. Armed when
    /// overlays are minted, cleared when every one is CONFIRMED placed (`OutputState::placed`),
    /// when the driver gives up, and whenever the overlays are torn down.
    ///
    /// Gates `sub_overlay_finalize`, the overlay's answer to the same problem
    /// `sub_preview_finalize` solves for the preview: the overlay is minted HIDDEN (komorebi
    /// opt-out) and only `place_overlay`'s two-phase dance shows it, driven by the one-shot
    /// `window::open` follow-up — which is NOT delivered while cck is a BACKGROUND process
    /// (a tray-daemon-spawned child). Worse, that chain gives up after 30 × 40ms, and
    /// `place_overlay`'s phase 2 needs a call LATER than 120ms after phase 1: a title that
    /// only matches near the end of the budget leaves the window CLOAKED and off-screen
    /// forever, in a process that is still alive. Never set off Windows.
    #[cfg(windows)]
    overlay_finalize_pending: bool,
    /// Windows (DRAGON-437): the give-up deadline's origin, set by the FIRST DELIVERED tick.
    ///
    /// Deliberately NOT stamped at mint, and this is the whole point of it being a separate
    /// field. A stalled process (or a timer that coalesces a backlog) can leave the first
    /// tick we actually HANDLE arriving well past the deadline; measuring from the mint
    /// would then read "expired" on that first tick and give up having never attempted a
    /// placement at all — turning a slow start into a reported failure. Measuring from the
    /// first tick we handle means the budget counts the driver's own attempts, which is what
    /// it was ever supposed to bound. `overlay_finalize_pending` (not this) drives the
    /// subscription, so the clock cannot decide whether the driver runs.
    #[cfg(windows)]
    overlay_finalize_deadline: Option<std::time::Instant>,
    /// Settings window UI state (the toplevel window, nav rail, search, …).
    settings: SettingsState,
    /// Permission-checker window UI state (macOS onboarding surface; only ever
    /// opened on macOS — a default empty state on Linux, never minted).
    permissions: permissions::PermissionsState,
    /// The colour picker tool's state (DRAGON-582): whether this process IS a picker
    /// launch, what the pointer is over, the picked colour and the result window.
    /// A default empty state on every other launch, where nothing reads it.
    color_picker: color_picker::ColorPickerState,
    /// Live keyboard-shortcut bindings (`Action -> Shortcut`) — the single source of
    /// truth for key handling and the Keyboard Shortcuts settings page.
    keymap: crate::shortcuts::Keymap,
    /// DRAGON-479: the ONE-SHOT override on the skip-the-editor decision. Set by the fixed
    /// primary+C region-copy chord just before it commits the capture; `present_capture`
    /// takes it and delivers through `finish_share` — the SAME editor-less path
    /// [`Self::no_editor`] uses, never a second flow.
    ///
    /// One-shot (`mem::take`) because it describes a pending ACTION — one keypress, one
    /// capture — where `no_editor` describes the LAUNCH and is read non-destructively. That
    /// distinction is the whole reason there are two fields rather than one.
    ///
    /// (It was here before, as DRAGON-451's `copy_selection_pending`, and was retired with the
    /// configurable shortcut that set it. The behaviour is back; the configurability is not.
    /// See `shortcuts::is_region_copy_chord`.)
    copy_selection_pending: bool,
    /// DRAGON-428: this LAUNCH asked for no preview editor (`--no-editor`, or a daemon
    /// "(no editor)" capture hotkey, which passes that flag). The capture is saved, copied
    /// and notified through `finish_share` — the same editor-less delivery a capture gets
    /// when no editor CAN be opened — and no editor surface is ever minted.
    ///
    /// It is NOT one-shot: it describes the LAUNCH, so every capture in the process honours
    /// it. In practice the process is one-shot anyway, but reading it non-destructively is
    /// what lets the two spinner-suppression sites consult it before the capture completes.
    no_editor: bool,
    /// Preview editor appearance: `true` = resizable window, `false` = overlay (setting).
    preview_windowed: bool,
    /// Preview editor (DRAGON-478): draw the group CAPTIONS under the top toolbar's clusters
    /// (setting; default on). Read by the view AND by `PreviewSurface::chrome_h`, since the
    /// caption band changes the top bar's height; off is the pre-DRAGON-478 geometry exactly.
    preview_toolbar_labels: bool,
    /// DRAGON-419: the opt-in debug log is on (setting; default OFF). Mirrored here so the
    /// Health page's Debug row can render and toggle it; the SINK's own state lives in
    /// `crate::diag`, which resolves this same key straight from the config in `main` (a
    /// launch that never reaches `App::init` still has to be logged).
    debug_logging: bool,
    /// Preview editor (DRAGON-467): put the EDITED result on the clipboard as the editor
    /// closes (setting; default on). The untouched capture already went on the clipboard when
    /// it was taken, so this is about carrying the edits forward.
    preview_copy_on_exit: bool,
    /// Preview editor (DRAGON-467): write every screenshot into `screenshot_dir` as it is
    /// captured (setting; default on). Off routes the capture through the session runtime
    /// directory instead, so nothing reaches the user's folder until they choose Save.
    preview_save_originals: bool,
    /// Preview editor (DRAGON-467): ask before closing over edits that were never saved
    /// (setting; default on) — the gate on the unsaved-changes card.
    preview_ask_to_save: bool,
    /// Preview editor, VIDEO documents (DRAGON-467): the video editor's own copy of
    /// `preview_copy_on_exit`. Same meaning, same default, separate field — a document reads
    /// one triple or the other by KIND (`preview::preview_automation`), never a mix.
    preview_video_copy_on_exit: bool,
    /// Preview editor, VIDEO documents (DRAGON-467): the video editor's own copy of
    /// `preview_save_originals`, over `record_dir`.
    preview_video_save_originals: bool,
    /// Preview editor, VIDEO documents (DRAGON-467): the video editor's own copy of
    /// `preview_ask_to_save`.
    preview_video_ask_to_save: bool,
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
    /// Appearance (DRAGON-289): "Automatic Contrast Boost" — adapt the selected accent
    /// for optimal contrast so fills, lines, outlines AND chrome text share one colour.
    /// Only consulted while `appearance_use_system` is false (System Default forces ON).
    appearance_contrast_boost: bool,
    /// Region selection box thickness (logical px, 1-8), applied to the viewfinder corner
    /// brackets AND side lines uniformly. Always applies (not gated by system appearance).
    /// DRAGON-209.
    selection_box_thickness: u32,
    /// About (DRAGON-177): whether to show the launch-time update dialog when the
    /// settings-open update check resolves `Available`. Default ON; the About page
    /// toggle "Notify me when an update is available" and the dialog's "Don't remind
    /// me again" checkbox are two views of this one setting.
    notify_updates: bool,
    /// Cloud accounts (DRAGON-482): the connected account the preview editor's Upload
    /// flyout offers first, by id. `None` until one is picked. Only a memory of the last
    /// choice: the account LIST lives in `cloud::accounts` and the tokens in
    /// `cloud::secrets`, neither of which is app state.
    cloud_last_account: Option<String>,
    /// Cloud accounts (DRAGON-482): create and copy a share link as part of an upload.
    /// Default on. Mirrors the flyout's checkbox and the persisted setting.
    cloud_auto_share: bool,
    /// Every OPEN post-capture preview editor, in OPEN ORDER — one entry per previewed
    /// document (DRAGON-336 phase 2: one process can host several preview windows and
    /// exits when the LAST one closes; see [`App::close_preview`]). Empty while no
    /// preview is up. Keyed by [`preview::PreviewState::window`]; look one up with
    /// [`App::preview_for`] / [`App::preview_for_mut`] rather than indexing, and route
    /// every `PreviewMsg` through the `Msg::Preview(window::Id, _)` wrapper so a message
    /// can never land on the wrong document.
    previews: Vec<preview::PreviewState>,
    /// The preview that last took keyboard focus — the routing target for the few paths
    /// that genuinely have no window id of their own (the keymap dispatch in
    /// `keyboard.rs`). Maintained on open/focus/close; [`App::focused_preview_id`] is the
    /// accessor and falls back to the most recently opened preview, so it is `Some`
    /// whenever ANY preview is open (which keeps the single-preview behavior identical to
    /// the pre-multi-document code).
    focused_preview: Option<window::Id>,
    /// The surface that REALLY holds keyboard focus, as the window system last reported it
    /// (`window::Event::Focused` → [`WindowChromeMsg::WindowFocused`]) — as opposed to
    /// [`Self::focused_preview`], which is our own routing pointer and is set the moment a
    /// document opens, long before its surface is mapped.
    ///
    /// `lab/flatpak`: this exists because a clipboard write on the
    /// [`crate::share::CopyRoute::ThisWindow`] route goes out over `wl_data_device`, which
    /// needs a serial from an input event delivered to this client. Without focus there is no
    /// such serial and the compositor refuses the selection, so the open-time automatic copy
    /// has to know whether the window it is writing through is actually focused YET. `None`
    /// until the first focus of the process's life.
    focused_window: Option<window::Id>,
    /// DRAGON-549: the NAME of the output a PORTAL grant was made on, when it resolved to a
    /// registered one ([`portal::output_for_grant_position`], which documents why the grant is
    /// the capture-origin signal in a session with no capture overlay). A WINDOW grant with
    /// no position, which is every one COSMIC's portal makes, resolves instead to the largest
    /// registered output, the SAME synthetic anchor the wallpaper compose stands its crop on
    /// (`capture_flow::largest_output_index`, DRAGON-549 reopened). `None` on every
    /// native-capture launch, where nothing writes it and every ladder that reads it is
    /// byte-identical.
    #[cfg(target_os = "linux")]
    portal_origin_output: Option<String>,
    /// The preview the IN-FLIGHT capture is feeding (its spinner pre-open, the
    /// cover→window swap, the content prep at `present_capture`). A capture produces
    /// exactly ONE preview, so this names it unambiguously even when other documents are
    /// already open. `None` before the capture opens its preview and after that preview
    /// closes.
    capture_preview: Option<window::Id>,
    /// While at least one preview with a soundtrack is open, the guard pausing OTHER
    /// apps' media (Spotify/browsers/…). REFCOUNTED across previews by
    /// [`Self::preview_duck_refs`]: engaged when the first holder appears, dropped only
    /// when the LAST one releases → those players resume. (A plain `Option` would
    /// un-mute the desktop as soon as ONE of several video previews stopped.)
    preview_duck: Option<crate::audio::ducking::OtherAudioDuck>,
    /// The previews currently holding the [`Self::preview_duck`] guard (see
    /// [`preview::DuckRefs`]).
    preview_duck_refs: preview::DuckRefs,
    /// The HANDOFF listener (DRAGON-336 phase 3b, widened by DRAGON-613), dropped at
    /// [`App::finish_session`]. Its presence is what makes this process reachable by a
    /// later one-shot sibling, and `sub_preview_handoff` drains the inbound requests.
    ///
    /// ONE listener per process, at ONE address, serving whichever of the two windows this
    /// process actually owns. It is bound by whichever comes first:
    ///
    /// * a PREVIEW surface (`preview_surface_for`), at the per-pid preview address
    ///   (`instance::preview_host_address`), so a later capture child hands its finished
    ///   file over instead of paying a second ~233 MB process;
    /// * the colour picker's RESULT WINDOW (`color_picker_pick`), at the per-pid picker
    ///   address (`instance::color_picker_host_address`), so a later pick updates this
    ///   window instead of opening a second one.
    ///
    /// A process is only ever one of those, so there is no third case and no contention for
    /// the field. It was called `preview_host` until DRAGON-613; the name was renamed rather
    /// than documented around, because a field that holds either listener and claims to hold
    /// one of them is the kind of small lie that later gets believed.
    ///
    /// `None` before either window exists, or when the bind failed. A failed bind is never
    /// fatal: nothing discovers us and every sibling does the job itself, exactly as before.
    ///
    /// Absent only where no transport exists: unix listens on a per-pid socket, Windows on
    /// the per-pid named-pipe twin (DRAGON-651; see `crate::preview_ipc`'s Platforms note).
    #[cfg(any(unix, windows))]
    handoff_host: Option<crate::preview_ipc::PreviewHost>,
    /// The recording-CONTROL listener (DRAGON-583), bound for the life of a recording and
    /// dropped with it (its `Drop` unlinks the socket). Its presence is what lets a second
    /// process drive this recording: the `--toggle-mic` / `--pause-recording` /
    /// `--finish-recording` / `--cancel-recording` / `--toggle-system-audio` commands a
    /// Linux global hotkey runs connect here and send one `daemon_ipc::Command`, the same
    /// word a resident's menu click sends, drained by the same `RecordingMsg::TrayPoll`.
    ///
    /// Deliberately SEPARATE from `self.tray`, which it might look like it belongs to: a
    /// sandboxed child often fails to register a tray item at all (the DRAGON-563 finding),
    /// and that is exactly the session where these commands are the only control left.
    ///
    /// `None` outside a recording, and when the bind failed (then the recording is
    /// unaffected and only the CLI commands cannot reach it). Linux-only: macOS and Windows
    /// keep their overlay windows and their menu-bar / tray controls through a recording,
    /// so DRAGON-583 leaves them byte-identical.
    #[cfg(target_os = "linux")]
    record_control: Option<crate::daemon_ipc::ControlInlet>,
    /// DRAGON-309: the TRIGGER display's NAME, snapshotted ONCE at launch (in `App::init`,
    /// before the picker overlay is shown / the cursor moves to the target / our overlay grabs
    /// focus). This is the monitor active when the capture was INITIATED, and drives where the
    /// post-capture preview opens — regardless of where the user moves to pick the target.
    /// `active_trigger_display()` resolves it to `(OutputHandle, size)` at commit (off Linux via
    /// `output_descs()`, on Linux by matching the name into `self.outputs`). `None` when nothing
    /// resolved at launch (then the selection's output is used, keeping DRAGON-304 behavior).
    trigger_display: Option<String>,
    /// The monitor (output + its logical size) the in-flight capture is on — captured
    /// before the overlay (and `self.outputs`) is torn down, so the post-capture preview
    /// can open a fullscreen overlay there and scale the image within it.
    preview_output: Option<(OutputHandle, (u32, u32))>,
    /// DRAGON-317 (diagnostic): the NAME of `preview_output`, resolved from `self.outputs`
    /// at the same pre-teardown moment `preview_output` is set — because `destroy_surfaces`
    /// CLEARS `self.outputs` during capture, so by preview-open time the WlOutput in
    /// `preview_output` can no longer be name-matched. The windowed-preview re-home
    /// (`preview_resized`) needs this stable name to tell `move_toplevel_to_output` which
    /// output to target across its throwaway Wayland connection. Linux-only.
    #[cfg(target_os = "linux")]
    preview_output_name: Option<String>,
    /// DRAGON-317 regression fix (Linux): the NAME of the output the CURSOR (hence the user)
    /// is actually on when the capture is initiated — the RELIABLE "capture-origin monitor"
    /// signal. Two fill paths, same field: the interactive picker records the output whose
    /// per-display capture OVERLAY first received the pointer (`CursorEntered`; cosmic-comp
    /// maps our layer-shell overlay under the cursor, so its `wl_pointer` enter names the
    /// pointer's output), and an overlay-less IMMEDIATE capture (`--active-*`, which mints no
    /// overlay) resolves it via the momentary `pointer_output()` probe (the same
    /// `wl_pointer`-enter signal). It SUPERSEDES the launch-time focused-toplevel guess
    /// (`trigger_display`), which points at the focused window's monitor even when the user is
    /// working on a DIFFERENT, empty one (the reported regression: preview flew to the small
    /// monitor holding the only focused window instead of the large monitor the user was on).
    /// It drives BOTH the preview's trigger-display resolution (`active_trigger_display`) and —
    /// as the SOLE source of `preview_output_name` — the windowed-preview re-home target. `None`
    /// only for `--preview` or when neither path resolved (no overlay entered AND the probe
    /// missed), which SUPPRESSES the re-home so cosmic-comp's native pointer-output placement
    /// (already where the user is) stands. First resolution wins. Linux-only.
    #[cfg(target_os = "linux")]
    capture_pointer_output: Option<String>,
    /// The point→pixel backing scale of `preview_output` — the capture output's
    /// physical-pixels-per-logical-point (COSMIC integer OR fractional scaling). Cached
    /// with `preview_output` (before the overlay tears `self.outputs` down) so the
    /// windowed-preview open-fit can divide the capture's PHYSICAL pixels back into the
    /// LOGICAL points it occupied on screen — a scaled COSMIC grab opens at its true
    /// on-screen size, not `scale`× too large (DRAGON-221, the Linux counterpart of the
    /// macOS `NSScreen.backingScaleFactor` used by [`Self::preview_source_scale`]).
    /// `1.0` on 1× outputs (every field stays byte-identical there).
    preview_output_scale: f32,
    /// The windowed preview's INTENDED open size (logical points), captured when the window
    /// is minted (`preview_surface_for`) so the macOS native finalize can re-assert it after
    /// moving the window to the trigger monitor (DRAGON-309). winit births the window on the
    /// capture/active monitor, where macOS clamps its height to that (possibly smaller) screen
    /// before we relocate it — the clamp otherwise sticks. Kept SEPARATE from
    /// `PreviewState.monitor` (which a resize event overwrites with the clamped size before
    /// finalize runs, so it can't be trusted here). Consumed only on macOS; harmless elsewhere.
    preview_open_size: Option<(u32, u32)>,
    /// `--preview <file>` launch: the file (and whether it's a video) to open straight
    /// into the preview overlay once an output appears. Taken once consumed.
    startup_preview: Option<(std::path::PathBuf, bool)>,
    /// DRAGON-427 (Windows 10): the same idea for `--preview-handoff` — the capture another
    /// process just took, handed to this one as the whole `OpenRequest` so the document opens
    /// with the capture's own dims / scale / size and `external = false`. Taken once consumed,
    /// at the same seam `startup_preview` is.
    startup_handoff: Option<crate::preview_ipc::OpenRequest>,
    /// Whether this is a `--preview` / `--preview-handoff` launch — suppresses the capture
    /// overlays entirely.
    preview_mode: bool,
    /// DRAGON-680: whether this is a `--palette-viewer` launch, which suppresses the capture
    /// overlays for the same reason [`Self::preview_mode`] does, and is carried on the App
    /// for the same reason too: BOTH output seeds (`seed_outputs_mac` off Linux, `on_output`
    /// on it) have to refuse, and they run long after `Startup` is gone.
    ///
    /// It also gates the scene acquisition in `init`. Without that a viewer launch with the
    /// freeze capture extra on would grab every display's pixels for a window that shows
    /// none of them, and would need the macOS Screen Recording grant to do it.
    palette_viewer: bool,
    /// DRAGON-295 (macOS/Windows): an IMMEDIATE picker-free capture requested at launch
    /// (`--active-window` / `--active-monitor`). Consumed by `seed_outputs_mac` instead of
    /// minting the capture overlays — the target is resolved and captured straight through.
    /// `None` for a normal overlay launch (every path stays byte-identical then). Never set
    /// on Linux (its capture keys are COSMIC shortcuts).
    /// DRAGON-295: the picker-free immediate capture requested by the launch flag
    /// (`--active-window` / `--active-monitor`), consumed once when the first output lands
    /// (mac/Windows in `seed_outputs_mac`, Linux in `on_output`). Portable so all three
    /// platforms can drive it; Linux resolves the target via the cctk Activated toplevel.
    startup_immediate: Option<ImmediateCapture>,
    /// Linux: whether the deferred immediate capture has already been kicked.
    /// The first output event schedules `CaptureMsg::RunImmediate` a short settle later (so
    /// the remaining outputs register into `self.outputs` first); this guard stops later
    /// output events from kicking it again. Linux-only (mac/Windows resolve immediately).
    #[cfg(target_os = "linux")]
    immediate_kicked: bool,
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
    /// is no output picker) and on Windows (DRAGON-238: mic = DirectShow/ffmpeg, system
    /// = WASAPI — the Pactl health row gates on ffmpeg there), where it is always false.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
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
    /// MASTER switch for the single-window recompositing (persisted; default on):
    /// "Enable single window aesthetic effects". OFF delivers the bare captured
    /// frame (no borders, shadow, rounding, padding, wallpaper backdrop) on every
    /// platform and path while the individual preferences below stay persisted.
    window_recompositing: bool,
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
    /// macOS (DRAGON-130): stay resident after a finished session instead of
    /// exiting, so a new capture session can be re-triggered (persisted; default
    /// off). Read by `finish_session` on macOS; Linux keeps the one-shot model
    /// and never consults it.
    resident: bool,
    /// Launch the resident at login (DRAGON-296): when this AND `resident` are both on, the
    /// OS login item / autostart entry is registered so the tray comes back after a login.
    /// Persisted, default on; the settings row is hidden while `resident` is off. Both
    /// toggles route through `reconcile_login_item`, which computes the desired state as
    /// `resident && autostart_on_login`.
    autostart_on_login: bool,
    /// DRAGON-628: whether the OS login item will REALLY run at the next login, as last
    /// observed, or `None` when this build has no read-only way to find out (a Flatpak, whose
    /// registration lives with the Background portal on the host; an unbundled macOS dev
    /// binary, which has no `SMAppService` to ask). `platform::autostart_registration` is the
    /// probe; the row only needs its `is_live()`, since a registration that exists and cannot
    /// run is not launching anything.
    ///
    /// NOT persisted, and it must never be: it is an OBSERVATION of the machine, and a stored
    /// copy would be exactly the stale claim this ticket exists to remove.
    ///
    /// Exists because the settings row used to render `autostart_on_login`, the persisted
    /// PREFERENCE, which is what the user asked for rather than what the machine will do. The
    /// two really do come apart: the owner's entry named an AppImage DRAGON-590 relocated, so
    /// every login refused it while the row said the feature was on. The row now renders
    /// `platform::autostart_row(self.autostart_on_login, self.autostart_registered)`, which
    /// shows the observation wherever there is one and the preference where there is not.
    ///
    /// Refreshed by `reconcile_login_item` at every exit, so the one function in the app that
    /// can change the login item is also the one that keeps this honest; and by
    /// `autostart_settings_opened`, so the row is right the moment it can first be seen. That
    /// second one is a READ with no side effects at all: the unprompted repair of a stale
    /// registration belongs to the resident daemons, not to opening a window.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    autostart_registered: Option<bool>,
    /// Linux (DRAGON-618): a Background-portal autostart request is in flight.
    ///
    /// NOT persisted, deliberately: an outstanding portal request cannot survive the process
    /// that made it, so a stored `true` would only ever be a stale claim on the next launch.
    ///
    /// Exists because a Flatpak's registration is asynchronous and may show a dialog, so the
    /// toggle has a third state between "asked" and "settled". While it is set,
    /// `SettingsMsg::SetAutostartOnLogin` is IGNORED: the answer to the outstanding request is
    /// not known yet, and acting on a second click would race two registrations and settle the
    /// toggle from whichever replied last. Guarded in the update handler rather than by making
    /// the row inert in `view`, so it covers every route into the setting.
    #[cfg(target_os = "linux")]
    autostart_pending: bool,
    /// Linux (DRAGON-625): why the last autostart registration did not happen, in words
    /// meant for the user, shown as the "Automatically start on login" row's description.
    ///
    /// NOT persisted: it describes one attempt, not a setting, and a stored copy would
    /// outlive the condition that produced it (a desktop that gains a Background portal
    /// would still be told it has none). Cleared on every success, so the row carries a
    /// reason only while there is one.
    ///
    /// Exists because the toggle otherwise just springs back with no explanation. The
    /// honest-failure contract stops us CLAIMING something untrue, but silence is its own
    /// kind of unhelpful: on COSMIC the portal does not exist, the request cannot be made
    /// at all, and the user deserves to be told that rather than left clicking.
    #[cfg(target_os = "linux")]
    autostart_notice: Option<String>,
    /// macOS/Windows (DRAGON-130 / DRAGON-295): the resident daemon's global "Capture All
    /// In One" hotkey spec (e.g. "PrintScreen", "Cmd+Shift+2"); persisted, default UNSET.
    /// Opens the full capture overlay. Edited on the Shortcuts settings page (macOS/Windows
    /// row); the daemon reads it from disk at startup. Carried on `App` only to round-trip
    /// it through save and drive the settings row; Linux never registers it (its capture
    /// key is a COSMIC shortcut).
    capture_hotkey: String,
    /// macOS/Windows (DRAGON-295): the resident daemon's global "Capture Active Window"
    /// hotkey spec; persisted, default UNSET. Immediately captures the frontmost window,
    /// no picker. Same round-trip/settings-row role as `capture_hotkey`; Linux never reads it.
    capture_active_window_hotkey: String,
    /// macOS/Windows (DRAGON-295): the resident daemon's global "Capture Active Monitor"
    /// hotkey spec; persisted, default UNSET. Immediately captures the monitor under the
    /// cursor, no picker. Same round-trip/settings-row role as `capture_hotkey`; Linux
    /// never reads it.
    capture_active_monitor_hotkey: String,
    /// DRAGON-428: the three "(no editor)" capture hotkeys — the same three captures, but
    /// the daemon adds `--no-editor` so the finished capture is saved, copied and notified
    /// without the preview editor. Same round-trip/settings-row role as the three above;
    /// Linux never reads them.
    capture_no_editor_hotkey: String,
    capture_active_window_no_editor_hotkey: String,
    capture_active_monitor_no_editor_hotkey: String,
    /// DRAGON-582 (macOS/Windows): the daemon's global hotkey for the COLOUR PICKER tool.
    /// Same round-trip / settings-row role as the six above; Linux never reads it.
    color_picker_hotkey: String,
    /// macOS (DRAGON-130): the death-pipe babysitter guard held for a capture session
    /// that paused a tiling WM (AeroSpace). Armed once the pause completes
    /// (`seed_overlays_mac`), dropped on session end (`finish_session`/`quit_now` +
    /// `reset_capture_state`); a crash/force-quit closes its pipe → the child restores
    /// the WM anyway. `None` when no tiling WM was paused. See `mac::window`.
    #[cfg(target_os = "macos")]
    aerospace_guard: Option<crate::platform::mac::window::AerospaceGuard>,
    /// macOS (DRAGON-151) / Windows (DRAGON-276): the countdown/recording overlays are
    /// click-through (`recreate_active_overlays` set every overlay to mouse
    /// passthrough); while true, `sub_passthrough` polls the pointer against each
    /// output's toolbar-chip rect and re-solidifies just the hovered overlay so the
    /// chip stays clickable.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    passthrough_active: bool,
    /// macOS (DRAGON-151) / Windows (DRAGON-276): the overlay currently made SOLID
    /// because the pointer is over its toolbar chip (`None` = all overlays passthrough).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    passthrough_solid: Option<window::Id>,
    /// Opacity of the dim outside the region selection (persisted; default 0.70).
    region_overlay_opacity: f32,
    /// Opacity of the dim + selection lines while a capture is active — counting
    /// down (and, later, recording) (persisted; default 0.70).
    active_overlay_opacity: f32,
    /// Opacity of the dim behind the post-capture preview overlay (persisted; default 0.90).
    preview_overlay_opacity: f32,
    /// DRAGON-582: dim opacity (0..1) behind the COLOUR PICKER overlay. Its own setting
    /// (see `state::schema`): the picker dims to make a magnifier readable, not to mark a
    /// selection, so its default is much lighter than region selection's.
    color_picker_overlay_opacity: f32,
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
    /// Which tesseract language pack OCR runs with (persisted; the Settings dropdown).
    /// Empty = pass no `-l`, leaving tesseract on its own `eng` default (DRAGON-527).
    ocr_language: String,
    /// Language codes tesseract reported (`--list-langs`), probed once at settings open.
    ocr_langs: Vec<String>,
    /// Display labels for [`Self::ocr_langs`], with the "Default (eng)" entry at index 0
    /// standing for the empty `ocr_language`.
    ocr_lang_labels: Vec<String>,
    /// The directory tesseract reported reading language packs from, so the Settings row
    /// can name the real one rather than a guess. `None` until the probe runs.
    ocr_lang_dir: Option<String>,
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
    /// DRAGON-456: a user-requested re-read of the screen, mid-flight (see [`ScanShot`]).
    /// Driven by re-pressing the scan kind button; `Idle` at every other moment.
    scan_shot: ScanShot,
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
    /// DRAGON-659: a worker spawned EARLY, during the countdown's last second, but not yet
    /// promoted into `self.recording`. Live only between [`App::arm_warm_spawn`] and the
    /// countdown's zero-tick; `None` for a no-countdown capture, where spawn and promotion
    /// collapse into the one `start_recording` call they have always been.
    ///
    /// `self.recording` deliberately stays `None` for the whole countdown, which is what
    /// keeps this additive: two if/else-if chains test `recording` BEFORE `countdown`
    /// (`view_window`'s view pick and Escape's `WindowChromeMsg::Close`), so an early
    /// `Some` there would render the recording view over the countdown and turn Escape
    /// into "stop and save" instead of "cancel the timer".
    warming: Option<WarmSpawn>,
    /// DRAGON-659: when `self.recording` became `Some` (the promotion instant). Drives ONLY
    /// the warmup spinner's reveal/hold timing, which is why it is separate from
    /// `recording_started`: that one anchors the RECORDED elapsed time and is not set until
    /// the worker reports its pipeline settled.
    recording_promoted_at: Option<std::time::Instant>,
    /// When the current recording started + its output path (for the chip's
    /// elapsed-time / size readout).
    ///
    /// DRAGON-659: `recording_started` is the WORKER's own instant, not the promotion
    /// instant, so the elapsed readout counts real recorded content and cannot drift by a
    /// poll's cadence. It is also the once-per-recording latch for that adoption: `None`
    /// while a promoted recording is still warming.
    ///
    /// DRAGON-673: which instant, though, is `RecordHandle::settled_at` — MEDIA 0, where the
    /// file begins. It was the confirmed first frame (`warm_at`) from DRAGON-659 until here,
    /// on the premise that the frame is where the file's content starts; media 0 moved to the
    /// settled pipeline (DRAGON-672) and the worker moved to countdown START (DRAGON-673),
    /// which left that premise a whole countdown out. A 10s countdown armed its readout at
    /// about 0:10.
    recording_started: Option<std::time::Instant>,
    /// DRAGON-661: has this recording DECLARED ITSELF LIVE yet, i.e. has the tray been
    /// raised for it? The once-per-recording latch for that transition. It reads the same
    /// `RecordHandle::settled_at` as `recording_started` since DRAGON-673, so the two now
    /// flip on the same poll; they keep separate latches because they are separate state
    /// (an instant, and whether the tray was raised).
    ///
    /// A field of its own rather than a test of `self.tray`: a Linux session with no SNI
    /// host leaves that `None` even though the tray was raised as far as it can be, and
    /// keying on it would re-run the transition on every 100ms poll.
    recording_live_declared: bool,
    recording_path: Option<std::path::PathBuf>,
    /// Where the finished recording is being written — the file `finalize` produces from
    /// `recording_path`. Kept only so the session-level bound (DRAGON-423) can see a stop
    /// tail making progress; nothing else reads it.
    recording_out_path: Option<std::path::PathBuf>,
    /// DRAGON-423: whether the USER has asked this recording to stop (or cancel).
    ///
    /// Deliberately not read back off `RecordHandle::stop`. A worker may CLEAR that flag —
    /// the zero-copy decline does exactly that before it retries on the CPU path — and a
    /// session that erased the user's stop and carried on recording is one of the things the
    /// session-level bound exists to catch. What the user asked for is the app's own fact.
    recording_stopping: bool,
    /// DRAGON-423: the session-level bound — is this recording still making progress?
    /// Fed one observation per `RecordingPoll`; see [`crate::record::progress`].
    recording_progress: Option<crate::record::progress::SessionProgress>,
    /// Pause bookkeeping (DRAGON-111): when the current pause began (`Some` =
    /// paused right now) and the total time spent paused before it. Together
    /// with `recording_started` they yield the RECORDED elapsed time — frozen
    /// while paused — via [`App::recording_elapsed_secs`].
    recording_paused_at: Option<std::time::Instant>,
    recording_paused_accum: std::time::Duration,
    /// Set when the user cancels a recording: the worker is stopped, then the
    /// finalized file is deleted (no save, no notification).
    recording_cancelled: bool,
    /// DRAGON-322: whether ANOTHER instance currently has a recording in progress
    /// (seeded from [`crate::instance::any_other_recording`] at launch, refreshed by
    /// `sub_external_recording`). While true the video capture kind is disabled — only
    /// one recording at a time — so a still capture can run alongside a recording.
    external_recording: bool,
    /// Where screenshots are saved (persisted; `~` expanded).
    screenshot_dir: String,
    // DRAGON-353: `clipboard_max_mb` (the "Clipboard size limit" NumField) lived here.
    // The automatic copy is still bounded, but by the fixed
    // `crate::share::AUTO_COPY_MAX_BYTES` rather than a setting — the editor toasts a
    // named error when it declines a copy for size, so there is nothing left for a knob
    // to pre-empt.
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
    /// The countdown digits tray item (DRAGON-563), alive only while a pre-capture
    /// countdown runs: the remaining seconds render in the tray icon (pixel digits in
    /// the recording glyph's red on the tinting platforms, a Cancel countdown menu
    /// entry). Minted in `enter_countdown` on EVERY session, owner's call ("doesn't
    /// need to be gated at all"): normal sessions keep their on-screen countdown and
    /// get the digits in addition; the `lab/flatpak` PORTAL-FROZEN fallback path gets
    /// them as its ONLY countdown surface (there the plain toplevel counted down over
    /// a gray sheet, and window/monitor countdowns had no surface at all, so the
    /// fallback window closes at countdown start when this minted). Re-drawn each
    /// `Tick`, dropped when the countdown fires or cancels. `None` when no tray host
    /// answered (Linux without an SNI host keeps the historical window countdown on
    /// the fallback path).
    countdown_tray: Option<crate::tray::CountdownTraySession>,
    /// The running countdown's START GATE (DRAGON-673), or `None` for a capture with no
    /// countdown. Minted false in `enter_countdown` and RAISED in `App::start_recording`,
    /// the one instant the app begins claiming to record.
    ///
    /// Handed to the recording session as `RecordSettings::start_gate`: the worker is
    /// spawned at countdown START so the whole countdown is warmup cover, and this is what
    /// still holds the FILE's media 0 back, so warming early can never put countdown time in
    /// the recording.
    ///
    /// It carried the PREDICTED countdown-zero instant first (`now + secs`, stamped as the
    /// countdown began), and that is why it is a flag now: the prediction ran ~1s ahead of
    /// the tick that actually fires the capture, so the file began before the UI said
    /// "Recording". A signal the app raises where it promotes cannot drift from the
    /// promotion, because it IS the promotion.
    countdown_gate: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
    /// macOS (DRAGON-130 Bug B) / Windows (DRAGON-248): the armed-idle system-audio METERING
    /// capture. Neither platform has a pulse-monitor to run an ffmpeg meter sidecar from, so
    /// `sys_meter` stays `None` and the speaker button would sit flat while
    /// armed-but-not-recording (the ONLY platform where the armed sys meter was dead — the
    /// user's "system meter never shows any volume"). This is a metering-only
    /// `MonitorCapture` (an audio-only SCK stream on macOS, a WASAPI loopback on Windows)
    /// alive ONLY in the armed-idle window: its chunks are discarded (`try_send` drops them)
    /// and it publishes the sys RMS via `publish_sys_level` on its own thread, exactly like
    /// the recording capture does. It is STOPPED before a recording's owned capture starts so
    /// the two never fight over the system-audio stream.
    #[cfg(any(target_os = "macos", windows))]
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
    /// macOS (DRAGON-268 follow-up): whether the settings toplevel is in NATIVE
    /// fullscreen right now (green traffic-light). Set from the `ConfigWindowResized`
    /// handler (a fullscreen enter/exit fires a resize), read by `config_window_view`
    /// so the CSD header adapts: the traffic lights auto-hide in fullscreen, so the
    /// 72px leading inset reserved for them collapses to 0 and the app's own nav
    /// toggle / search sit flush left where the lights were. Off macOS this field
    /// does not exist (the whole feature is cfg-gated) so other platforms stay
    /// byte-identical.
    #[cfg(target_os = "macos")]
    settings_fullscreen: bool,
    /// macOS: whether the WINDOWED-preview toplevel is in native fullscreen right
    /// now (mirror of `settings_fullscreen` for the preview editor). Read by
    /// `preview_view` so the preview toolbar keeps a reachable Close button in
    /// fullscreen (the native traffic-light close is auto-hidden there).
    #[cfg(target_os = "macos")]
    preview_fullscreen: bool,
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
    /// Persisted last-selected annotation stroke color (RGBA); `None` = the accent-complement
    /// default. Seeds `EditState::annot_color` on every preview open (DRAGON-321).
    annot_color: Option<[u8; 4]>,
    /// Persisted last-selected annotation tool; `None` = neutral. Seeds `EditState::tool`
    /// on every preview open (DRAGON-321).
    annot_tool: Option<crate::widgets::annotation_canvas::Tool>,
    /// Persisted last-selected annotation stroke width (SOURCE px), 5px default. Seeds
    /// `EditState::annot_stroke_w` on every preview open and new box/arrow shapes.
    annot_stroke_w: f32,
    /// Persisted remembered sequence-badge ("step marker") side (SOURCE px) — the last one
    /// placed or resized in ANY editor. `0.0` = unset (fall back to
    /// `preview::annotate::DEFAULT_BADGE_SIZE`). Seeds `EditState::annot_badge_size` on every
    /// preview open, and every placement/resize writes back through
    /// `App::remember_badge_size`, so the size survives new capture processes and restarts.
    annot_badge_size: f32,
    /// Persisted last-selected TEXT size (SOURCE px) and FONT family (DRAGON-354), so a fresh
    /// preview opens the text tool at them. `0.0` size = unset (fall back to
    /// `preview::text_annot::DEFAULT_TEXT_SIZE`). Seed `EditState::annot_text_size` /
    /// `annot_text_font` on every preview open.
    annot_text_size: f32,
    annot_text_font: crate::app::preview::text_annot::TextFont,
    /// Persisted last-5 CUSTOM annotation colors (most-recent-first), shown as MRU swatches
    /// in the color flyout (DRAGON-321).
    annot_recent_colors: Vec<[u8; 4]>,
    /// Per-output frozen snapshots. Grabbed on a DEFERRED thread on BOTH platforms
    /// (DRAGON-148 option C / DRAGON-212) and landed here via `CaptureMsg::FrozenReady` —
    /// empty until then, so every reader handles the not-ready window (see `freezing`).
    frozen: HashMap<String, FrozenOutput>,
    /// Deferred frozen-flats grab slot (DRAGON-148 / DRAGON-212). `None` while in flight,
    /// then drained into `frozen` on `FrozenReady`.
    frozen_slot: FrozenSlot,
    /// DRAGON-460: the scanner's live region shot, in flight. Outer `None` = no shot
    /// pending; `Some(None)` = the shot ran and the platform returned nothing (no
    /// compositor, region off-screen), which `MarksPoll` treats as "leave the last answer
    /// alone" rather than as an empty screen.
    ///
    /// Separate from `frozen_slot` on purpose. That one carries the whole-screen scene the
    /// CAPTURE is built from and is written by the launch grab; this carries a throwaway
    /// crop that only the scan passes read. Sharing a slot is what would let a scan shot
    /// overwrite the pixels a capture is about to commit.
    scan_shot_slot: std::sync::Arc<std::sync::Mutex<Option<Option<image::RgbaImage>>>>,
    /// DRAGON-460: a busy glyph's spin angle, in radians. Advanced by `BusySpinTick`
    /// while something is busy and left where it stopped otherwise — the glyph resting at
    /// an arbitrary angle is invisible for a symmetric refresh arrow, and resetting it to 0
    /// would make every scan start with a visible snap back.
    ///
    /// DRAGON-659 renamed it from `scan_spin`: it now drives TWO glyphs, the scanner's
    /// refresh button and the record chip's warming spinner. They are never DRAWN at the
    /// same time (the toolbar shows either the kind row or the chip, never both), so one
    /// angle is enough; only the tick's gate has to name both, or the angle would freeze
    /// under a still-visible spinner the moment the other consumer went idle.
    busy_spin: f32,
    /// The deferred flats grab hasn't landed yet. Drives the poll subscription that
    /// drains `frozen_slot`.
    frozen_pending: bool,
    /// Linux (DRAGON-600): the frozen-flats grab is HELD until the tray dropdown that
    /// launched this child is gone. `None` on every launch with no menu on screen and on
    /// every other platform, so nothing but a tray launch pays for it.
    menu_hold: Option<MenuFlatsHold>,
    /// DRAGON-663: the configured delay for a COLOUR PICKER launch, waiting for somewhere to
    /// draw its digits. `None` on every launch that is not a picker with a delay set, which
    /// leaves every other launch shape byte-identical.
    ///
    /// It is a two-step arm rather than one because [`App::enter_picker_countdown`] has to
    /// call `recreate_active_overlays`, and at `init` there are no overlays yet: on Linux
    /// they arrive per output from the Wayland registry, on macOS and Windows from
    /// `seed_outputs_mac`. Arming before they exist would mint them fully interactive and
    /// input-blocking, which is the one thing a countdown must not be: the delay exists so
    /// the user can rearrange the screen. `sub_picker_countdown_arm` waits for
    /// `self.outputs` and then spends this, so it is also the latch that keeps the countdown
    /// from being armed twice.
    picker_countdown_pending: Option<u8>,
    /// DRAGON-663: a colour picker's countdown has FIRED and the overlay has gone blank, so
    /// the held flats grab runs on the next settle tick (`sub_picker_reveal`).
    ///
    /// The settle is the same one `sub_pixel_capture` and the DRAGON-456 scan re-read take,
    /// and for the same reason: the countdown's dim and timer chip were on screen a frame
    /// ago, and a grab that starts before the compositor has presented the blank surface
    /// photographs them. For a capture that would put our own chrome in the shot; for the
    /// picker it is worse, because the picker REPORTS the pixel it reads, so a dim baked
    /// into the snapshot is returned to the user as the colour they picked.
    picker_revealing: bool,
    /// DRAGON-606: how far the capture overlay's dim has faded in. Starts `Waiting`, which
    /// paints NO dim, becomes `Armed` when the frozen-flats grab has landed
    /// (`overlay::dim_fade_may_start`), and only starts its clock on the first painted
    /// frame. A `Cell` because that last step happens inside the view, where there is no
    /// `&mut self`, exactly like `OutputState::placed`. See `src/app/overlay/mod.rs` for
    /// why the later of those two events is the only correct start.
    dim_fade: std::cell::Cell<overlay::DimFade>,
    /// The directional key currently held down, if any (DRAGON-601). `None` between holds.
    ///
    /// ONE field for BOTH nudge consumers, the colour picker's sample and the drawn region,
    /// because they are never on screen together (`region_nudge_fires` has `!color_picking`
    /// precisely so) and because two copies of a cadence are two cadences that can drift.
    nudge_hold: Option<keyboard::NudgeHold>,
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
    /// Whether the ScreenCast portal is reachable with usable source types
    /// (probed once at startup). Drives the indicator + whether the path is tried.
    pipewire_available: bool,
    /// Source types the portal advertises (bitflags: 1=monitor, 2=window, 4=virtual),
    /// for the indicator label. 0 until probed / when unavailable.
    pipewire_source_types: u32,
    /// Transient overlay message — e.g. "selected region not found in selected
    /// output" after a wrong-monitor portal pick. Auto-dismissed by a timer.
    toast: Option<String>,
    /// DRAGON-612: an accept key has been pressed on the colour picker and there is no pixel
    /// to take yet, held since this instant.
    ///
    /// The instant IS the state, saying both that a request is outstanding (which drives
    /// `sub_accept_pending`) and how long it has waited (against
    /// `keyboard::ACCEPT_WAIT_BUDGET_MS`, because nothing here waits unboundedly). Set on the
    /// FIRST ask only, so a re-ask cannot quietly renew the budget.
    ///
    /// Only the PICKER can hold one. A region accept is answered immediately either way: the
    /// rectangle is drawn or it is not, and no amount of waiting draws one.
    accept_pending: Option<std::time::Instant>,
    /// ScreenCast restore tokens, ONE SLOT PER SOURCE TYPE (monitor / window,
    /// DRAGON-570), replayed to skip the portal dialog. Per-source slots make the
    /// DRAGON-544 cross-type mis-replay (cosmic's portal silently restoring the
    /// WRONG source, the "window mode captured the whole monitor" bug) impossible
    /// by construction, and granting one kind no longer discards the other's
    /// token. The field keeps its single-token-era name because the capture
    /// settings page reads `pw_restore_token.is_some()` for its "Saved screen
    /// permission" row; see `portal::RestoreTokens`.
    pw_restore_token: portal::RestoreTokens,
    /// In-flight portal recording: context kept while the async ScreenCast request
    /// runs (its result lands in `pw_slot`, which the handler then consumes).
    pw_pending: Option<PwPending>,
    /// Hand-off slot for the async portal result (a `CastSession` holds a non-Clone
    /// fd, so it can't ride in a `Msg`; the task drops it here and signals readiness).
    pw_slot: std::sync::Arc<std::sync::Mutex<Option<Result<crate::platform::screencast::CastSession, crate::platform::screencast::CastError>>>>,
    /// A granted portal stream awaiting the start of recording (held across the
    /// countdown). When set, the recorder uses it instead of direct screencopy.
    pw_held: Option<HeldStream>,
    /// `lab/flatpak` (Linux): whether the fallback overlay path's ONE seed-time portal
    /// request has been kicked. Set by `on_output`'s first qualifying event so later
    /// output events only register geometry. The `immediate_kicked` shape.
    #[cfg(target_os = "linux")]
    fallback_seed_kicked: bool,
    /// `lab/flatpak` (Linux), DRAGON-604: the cursor answer the CURRENT seed frame was
    /// grabbed with, or `None` before any seed request. The portal bakes the pointer in
    /// at grab time, so those pixels cannot be un-decided later; this is what lets a
    /// kind change notice that the frame on screen no longer matches what the new kind
    /// needs (see `fallback_reseed_needed`).
    #[cfg(target_os = "linux")]
    fallback_seed_cursor: Option<bool>,
    /// `lab/flatpak` (Linux), DRAGON-604: a REPLACEMENT seed request is in flight. The
    /// grant handlers read it to keep a live session alive: the launch seed's endings
    /// are rightly fatal (there is no overlay yet, which is the whole premise), but a
    /// re-seed already has one on screen, so a declined or unreachable replacement must
    /// leave the existing frame standing instead of killing the session under the user.
    #[cfg(target_os = "linux")]
    fallback_reseeding: bool,
    /// `lab/flatpak` (Linux): the fallback overlay toplevel's winit id, once minted.
    /// This is what tells the close paths that ONE `self.outputs` entry is backed by a
    /// plain window (closed via `window::close`) while the rest are placeholder ids with
    /// no surface at all, so a layer-surface destroy for either would be wrong. `None` on
    /// every layer-shell session, and cleared by `destroy_surfaces` before the close is
    /// issued so the window's own `Closed` echo reads as ours.
    #[cfg(target_os = "linux")]
    fallback_window: Option<window::Id>,
    /// `lab/flatpak` (Linux): the seed grant's monitor (name + logical geometry). Set at
    /// `FallbackCastReady` and kept for the session: `FallbackFrozenReady` builds the
    /// `FrozenOutput` and mints the window from it, and the yield/restore dance around a
    /// portal dialog re-mints from it too.
    #[cfg(target_os = "linux")]
    fallback_grant: Option<FallbackGrant>,
    /// `lab/flatpak` (Linux): hand-off slot for the seed-time single-frame portal grab
    /// (an `RgbaImage` is ~30 MB, so it rides the house slot idiom, never a `Msg`).
    /// Outer `Option` = posted; inner = whether a frame arrived before the 5s watchdog.
    #[cfg(target_os = "linux")]
    fallback_frame_slot: FallbackFrameSlot,
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
    ///
    /// On a build with NO update channel (a Flatpak, DRAGON-605) neither of those two
    /// fills it, because neither runs; `ReleaseNotesFetched` does instead, from the
    /// notes-only fetch the About page starts. Same field, same rendering, so the
    /// changelog looks identical whoever fetched it.
    update_notes: Option<(String, cosmic::widget::markdown::Content)>,
    /// Whether the notes-only fetch has already run this session (DRAGON-605). Set on a
    /// build with no update channel (a Flatpak), where the About page fetches its own
    /// "What's new" text because no update check will ever supply it. Set BEFORE the
    /// fetch starts, and never cleared, so repeatedly navigating to About costs exactly
    /// one request, and a fetch that comes back empty is not retried on every visit.
    /// Always false on a build with a channel, which never sends the message at all.
    release_notes_fetched: bool,
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
    /// The request's source-type key ("monitor" / "window"), recorded so the grant
    /// stores it beside the restore token and a cancel clears the right pair.
    source_key: &'static str,
}

/// The monitor a region was clamped to, for validating the portal pick + cropping.
struct RegionTarget {
    out_pos: (i32, i32),
    out_size: (u32, u32),
    rect: (i32, i32, u32, u32),
}

/// `lab/flatpak` (Linux): the monitor the seed-time portal grant resolved to: the
/// output the fallback overlay freezes, names its capture after, and fullscreens onto
/// (as far as the compositor allows; see `OutputState::fallback_win_size` for the
/// mismatch guard when it lands elsewhere). Kept for the WHOLE session, because the
/// window is closed while a portal dialog is up (`yield_overlays`) and re-minted from
/// this on cancel/countdown (`mint_fallback_window`).
#[cfg(target_os = "linux")]
#[derive(Clone)]
struct FallbackGrant {
    /// The granted output's wl_output name (`OutputState::name`), the `self.frozen` key.
    name: String,
    pos: (i32, i32),
    size: (u32, u32),
}

/// `lab/flatpak` (Linux): the seed-frame hand-off slot (see the field doc on `App`).
#[cfg(target_os = "linux")]
type FallbackFrameSlot = std::sync::Arc<std::sync::Mutex<Option<Option<image::RgbaImage>>>>;

/// DRAGON-562 (Linux): the grant-time facts a portal WINDOW still needs to run
/// the native single-window aesthetics over the finished portal frame. Snapshotted
/// in `on_pipewire_cast_ready`, the only moment that has them: `self.outputs` is
/// torn down with the overlays before `do_pixel_capture` runs, and the stream's
/// position is not retrievable later. Rides [`HeldStream`], so a countdown keeps
/// it alive for free and a video launch simply never reads it.
#[cfg(target_os = "linux")]
#[derive(Clone)]
struct PortalWindowGrant {
    /// The window's global logical position (`StreamInfo.position`). `None` is
    /// the NORM, not an edge case: COSMIC's portal constructs every window
    /// stream with an explicit `position: None` (measured, fifth Flatpak live
    /// test; its screencast source only positions monitor streams). The
    /// fullscreen gate degrades honestly on `None` (decorate), while the
    /// wallpaper backdrop falls back to a SYNTHETIC anchor
    /// (`capture_flow::synthetic_window_anchor`) — waiting for a real position
    /// meant the backdrop never engaged at all.
    pos: Option<(i32, i32)>,
    /// The origin output's logical rect (x, y, w, h), resolved at grant time by
    /// the SAME containment (`output_for_grant_position`) that names
    /// `portal_origin_output`. The fullscreen gate's "out" input.
    origin_rect: Option<(i32, i32, i32, i32)>,
    /// The origin output's buffer scale (physical px per logical unit); `1.0`
    /// when no output matched.
    scale: f32,
    /// Every registered output's (name, logical_pos, logical_size): the window
    /// composite's wallpaper arm resolves the window's output, and its cosmic-bg
    /// entry, from these.
    outputs: Vec<crate::screenshot::OutputGeom>,
}

/// A granted portal stream held between the permission grant and the actual start
/// of recording (it survives the pre-capture countdown). Consumed by the recorder.
struct HeldStream {
    // DRAGON-229: the portal fd is a never-constructed TYPE off Linux (pipewire_available
    // is always false there). Unix keeps `std::os::fd::OwnedFd`; Windows has no
    // `std::os::fd`, so use its owned handle so the field type resolves.
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    fd: std::os::windows::io::OwnedHandle,
    node_id: u32,
    /// Region crop in stream pixels; `None` for whole monitor/window.
    crop: Option<(u32, u32, u32, u32)>,
    /// DRAGON-562: `Some` exactly when this grant is a WINDOW source — the facts
    /// the still path's aesthetics need (see [`PortalWindowGrant`]). The
    /// recording path ignores it.
    #[cfg(target_os = "linux")]
    window_grant: Option<PortalWindowGrant>,
}

/// DRAGON-659: a recording worker that is RUNNING but not yet the app's recording, the
/// early spawn a countdown covers. Held in `App::warming` until the countdown's zero-tick
/// promotes it, or [`App::abandon_warming`] throws it away.
///
/// The `out_path` is carried rather than recomputed because `record_output_path` embeds a
/// wall-clock timestamp (`capture_timestamp`): asking twice yields two different files, and
/// the second one would be bookkeeping for a file the worker never wrote to.
struct WarmSpawn {
    handle: crate::record::RecordHandle,
    out_path: std::path::PathBuf,
}

mod message;
pub use message::{
    BorderColorTarget, CaptureMsg, CloudSettingsMsg, ColorPickerMsg, RecordingMsg, DetectMsg,
    SettingsMsg, PermissionsMsg, WindowChromeMsg, PreviewMsg, VideoMeta,
};
// DRAGON-589: portable, because every platform's Global tab lists these seven actions. Only
// the two with a resident daemon can BIND them; the rest show the command that runs each.
pub use message::CaptureHotkeySlot;

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
    /// A colour-picker message (DRAGON-582): the dimmed picker overlay's pointer moves
    /// and pick, plus the result window's row edits, copies and recent-swatch loads.
    ColorPicker(ColorPickerMsg),
    /// A preview-editor message, ADDRESSED to the preview surface it belongs to
    /// (DRAGON-336 phase 2). The id lives on the wrapper rather than on each of
    /// `PreviewMsg`'s ~80 variants: view code always has its `PreviewState` (hence
    /// `p.window`) in scope, and async completions capture the owning id in their
    /// closure at spawn time, so routing is a single structural fact instead of a
    /// per-variant convention. `update_preview` looks the document up with
    /// [`App::preview_for`]; an id with no live preview is a silent no-op (the
    /// document closed while its task was in flight).
    Preview(window::Id, PreviewMsg),
}

#[cfg(test)]
mod pick_save_path_tests {
    use super::suggested_file_name;
    use std::path::Path;

    /// The native panels (macOS `NSSavePanel`, Windows `IFileSaveDialog`) take a NAME, while
    /// the suggestion `App::preview_save_target` builds is a full PATH. This is the reduction,
    /// and it is compiled on every host so the Linux gate covers it even though both callers
    /// are `cfg`-ed out here.
    #[test]
    fn the_name_half_of_a_suggested_path_is_taken() {
        assert_eq!(suggested_file_name(Path::new("/home/me/Capture/shot.png")), "shot.png");
        // Spaces and dots survive: the panels get exactly the name the picker would show.
        assert_eq!(
            suggested_file_name(Path::new("/rec/Recording 2026-07-29 at 10.30.mp4")),
            "Recording 2026-07-29 at 10.30.mp4"
        );
        // A bare name is already the answer.
        assert_eq!(suggested_file_name(Path::new("shot.png")), "shot.png");
    }

    /// A path with no file name falls back rather than handing the panel an empty box — the
    /// same `"capture"` fallback the picker has always used.
    #[test]
    fn a_path_with_no_file_name_falls_back() {
        for p in ["/", "..", ""] {
            assert_eq!(suggested_file_name(Path::new(p)), "capture", "{p:?}");
        }
    }
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

    /// DRAGON-574: the tray's "Countdown Timer" radio submenu writes `delay_idx`, the
    /// index into THIS table, so the two preset tables must stay equal entry for entry.
    /// A drift would make a tray pick of "05" count down some other number of seconds.
    #[test]
    fn tray_countdown_presets_match_the_delay_table() {
        assert_eq!(crate::recording_ui::COUNTDOWN_PRESET_SECS.len(), DELAYS.len());
        for (idx, &secs) in crate::recording_ui::COUNTDOWN_PRESET_SECS.iter().enumerate() {
            assert_eq!(DELAYS[idx].1, secs, "preset {idx} disagrees with the delay chips");
            assert_eq!(countdown_index(secs), idx, "countdown_index round-trip for {secs}s");
        }
    }

    /// DRAGON-243 (Windows): the transparent-overlay present-mode override forces `fifo`
    /// only when the user hasn't chosen one — a set value (even whitespace-trimmed
    /// non-empty) always wins, an unset-or-empty env is forced to the DWM-composited mode.
    #[cfg(windows)]
    #[test]
    fn present_mode_override_forces_fifo_only_when_unset() {
        assert_eq!(present_mode_env_override(None), Some("fifo"));
        assert_eq!(present_mode_env_override(Some("")), Some("fifo"));
        assert_eq!(present_mode_env_override(Some("   ")), Some("fifo"));
        assert_eq!(present_mode_env_override(Some("immediate")), None);
        assert_eq!(present_mode_env_override(Some("vsync")), None);
        assert_eq!(present_mode_env_override(Some("fifo")), None);
    }

    // ── DRAGON-427: the Windows 10 software-renderer decision ────────────────────
    // Pure and testable on ANY host, deliberately: the Windows arms these gate can only be
    // compiled on Windows, and nobody on this project has a Windows 10 machine to run them
    // on. `backend_env_override` is `cfg(windows)`-only code, so its test is too; the two
    // decisions that shape the app's own behaviour are portable and always run.

    /// The backend override forces the software rasterizer ONLY when this process wants it
    /// AND the user has not chosen a backend themselves. A user's `ICED_BACKEND` is never
    /// silently overridden — that is the whole reason this is a function and not an
    /// unconditional `set_var`.
    #[cfg(windows)]
    #[test]
    fn backend_override_forces_software_only_when_wanted_and_unset() {
        // Wanted + nothing set (or only whitespace): force it.
        assert_eq!(backend_env_override(None, true), Some(SOFTWARE_BACKEND));
        assert_eq!(backend_env_override(Some(""), true), Some(SOFTWARE_BACKEND));
        assert_eq!(backend_env_override(Some("   "), true), Some(SOFTWARE_BACKEND));
        // The user chose — including choosing the SAME value, or a comma list, or junk.
        // Every one of these is theirs to own; we must not write over any of them.
        for chosen in ["wgpu", "tiny-skia", "wgpu,tiny-skia", "nonsense"] {
            assert_eq!(backend_env_override(Some(chosen), true), None, "{chosen}");
        }
        // Not wanted (Windows 11, or a process that opens no overlays): never touched,
        // whatever the environment says.
        for existing in [None, Some(""), Some("wgpu"), Some("tiny-skia")] {
            assert_eq!(backend_env_override(existing, false), None, "{existing:?}");
        }
    }

    /// Which launches open an overlay — the ONE input the renderer decision keys off, so
    /// that every launch path (daemon-spawned child, global hotkey, Start Menu shortcut,
    /// a bare CLI run, the picker-free immediate captures) answers it from its own flags.
    #[test]
    fn only_overlay_launches_ask_for_the_software_renderer() {
        // A capture launch of ANY shape opens overlays: the picker, the countdown, the
        // window-pick cover, the fullscreen loader before an immediate capture resolves.
        assert!(Startup::default().opens_overlays(), "a bare capture launch");
        assert!(
            Startup { mode: Some(Mode::Region), ..Default::default() }.opens_overlays(),
            "--region"
        );
        assert!(
            Startup {
                immediate: Some(ImmediateCapture::ActiveWindow),
                ..Default::default()
            }
            .opens_overlays(),
            "--active-window still shows a fullscreen loader before its editor"
        );
        // The window-only launches keep the GPU renderer.
        assert!(!Startup { settings_only: true, ..Default::default() }.opens_overlays());
        assert!(!Startup { permissions_only: true, ..Default::default() }.opens_overlays());
        // DRAGON-680: the palette viewer is one of them. It is the picker's window with
        // no overlay phase, so it must not take the capture routing its `--color-picker`
        // sibling does.
        assert!(
            !Startup { palette_viewer: true, ..Default::default() }.opens_overlays(),
            "--palette-viewer opens the picker window and nothing else"
        );
        assert!(
            !Startup {
                preview: Some(std::path::PathBuf::from("/tmp/a.png")),
                ..Default::default()
            }
            .opens_overlays(),
            "--preview <file> opened cold"
        );
        assert!(
            !Startup {
                preview_handoff: Some(crate::preview_ipc::OpenRequest {
                    path: std::path::PathBuf::from("/tmp/a.png"),
                    video: false,
                    display_dims: None,
                    source_scale: 1.0,
                    external: false,
                    size: None,
                }),
                ..Default::default()
            }
            .opens_overlays(),
            "the spawned editor child"
        );
    }

    /// DRAGON-650: the colour picker keeps the GPU renderer even where the platform says
    /// overlays must be software-rendered. Its overlay is an OPAQUE frozen snapshot with
    /// the dim composited in-app, so it never needs the per-pixel window alpha the Windows
    /// 10 force works around — and its magnifier wants the shader-backed lens that force
    /// would bar. Portable and always run, like the other decisions in this block: the
    /// Windows arm it gates cannot be exercised here, so this table is its only net.
    #[test]
    fn the_colour_picker_never_asks_for_the_software_renderer() {
        // THE new case: a Windows 10 picker launch keeps wgpu.
        assert!(!wants_software_backend(true, true, true, false));
        // A Windows 10 capture launch is forced exactly as before — the exemption must
        // reach nothing but the picker.
        assert!(wants_software_backend(true, false, true, false));
        // Windows 11 / Linux / macOS (`platform_software` false): nobody is forced, picker
        // or not.
        assert!(!wants_software_backend(true, false, false, false));
        assert!(!wants_software_backend(true, true, false, false));
        // A launch with no overlays never was, whatever the other flags say.
        for (picker, platform) in [(false, false), (false, true), (true, false), (true, true)] {
            assert!(!wants_software_backend(false, picker, platform, false));
        }
        // And the picker still OPENS overlays (`Startup::opens_overlays` is untouched):
        // the narrowing lives at the renderer decision only, because the flats grab, the
        // boot policy and the surface routing all still need the true answer.
        assert!(Startup { color_picker: true, ..Default::default() }.opens_overlays());
    }

    /// DRAGON-666: the DirectComposition experiment OVERRULES the Windows 10 software
    /// force, and that ordering is the whole reason the experiment can answer anything.
    ///
    /// A DComp surface reports real per-pixel alpha modes, so tiny-skia has nothing left to
    /// fix. If the force still won, a Windows 10 tester would land on the software
    /// rasterizer before wgpu ever saw the option — they would report "no change", and we
    /// would read that as "DirectComposition does not work on Windows 10" when in fact it
    /// was never asked.
    #[test]
    fn the_dcomp_experiment_stands_the_windows_10_force_down() {
        // THE case the Windows 10 tester runs: the force would fire, and does not.
        assert!(wants_software_backend(true, false, true, false));
        assert!(!wants_software_backend(true, false, true, true));
        // It changes nothing anywhere the force was never going to fire, so a Windows 11
        // tester's run differs from today in the presentation path alone.
        for (overlays, picker, platform) in [
            (true, true, true),
            (true, false, false),
            (false, false, true),
            (false, true, false),
        ] {
            assert!(!wants_software_backend(overlays, picker, platform, true));
            assert_eq!(
                wants_software_backend(overlays, picker, platform, false),
                wants_software_backend(overlays, picker, platform, true),
                "dcomp may only change the one case the force owns"
            );
        }
    }

    /// DRAGON-650: the forced-backend predicate answers false wherever the force never ran.
    /// This process's own test run IS such a process (nothing here calls the force), so the
    /// read is the honest baseline every non-Windows-10 launch sees — and what keeps the
    /// magnifier on the shader arm everywhere the exemption applies.
    #[test]
    fn an_unforced_process_reports_no_software_force() {
        assert!(!process_forced_software_backend());
    }

    // ── DRAGON-440: which launches boot the macOS REGULAR activation policy ───────
    // Pure and run on every host: the two gates that read this are macOS-only, and the
    // macOS build is not run here (see CLAUDE.md), so this table is the only net the
    // decision has.

    /// A launch whose UI is a real WINDOW boots Regular, so it gets a Dock icon and a
    /// Cmd+Tab entry. Routing is irrelevant to these three — they never route.
    #[test]
    fn the_window_launches_boot_regular_whatever_the_routing_says() {
        for routed in [false, true] {
            assert!(boots_regular_policy(true, false, false, routed, false), "--settings");
            assert!(boots_regular_policy(false, true, false, routed, false), "--permissions");
            assert!(boots_regular_policy(false, false, true, routed, false), "--preview");
        }
    }

    /// Either preview flavour counts, exactly as `opens_overlays` counts both. The
    /// `--preview-handoff` editor child is a pure WINDOW launch: keying this on the bare
    /// `preview` field would boot it Accessory and install the overlay chrome strip on it.
    #[test]
    fn both_preview_flavours_are_window_launches() {
        let handoff = crate::preview_ipc::OpenRequest {
            path: std::path::PathBuf::from("/tmp/a.png"),
            video: false,
            display_dims: None,
            source_scale: 1.0,
            external: false,
            size: None,
        };
        for startup in [
            Startup { preview: Some(std::path::PathBuf::from("/tmp/a.png")), ..Default::default() },
            Startup { preview_handoff: Some(handoff), ..Default::default() },
        ] {
            // The call sites feed exactly this expression; assert against it so a change to
            // one and not the other cannot slip past.
            let preview_launch =
                startup.preview.is_some() || startup.preview_handoff.is_some();
            assert!(!startup.opens_overlays(), "a preview launch opens no overlays");
            assert!(boots_regular_policy(
                startup.settings_only,
                startup.permissions_only,
                preview_launch,
                false,
                false,
            ));
        }
    }

    /// THE DRAGON-440 case: a capture launch that routes to the permission checker shows
    /// the checker window and no overlay, so it must boot Regular too. Without this the
    /// checker had no Dock icon and no Cmd+Tab entry and could sit behind everything —
    /// invisible, while the startup guard suspended its budget for it.
    #[test]
    fn a_routed_capture_launch_boots_regular() {
        assert!(boots_regular_policy(false, false, false, true, false));
    }

    /// THE DRAGON-443 case: the installer's swap helper relaunches the app with BARE argv,
    /// and `App::init` turns that into a settings launch on the About page. So it shows a
    /// real window and must boot Regular — otherwise the release notes for the version the
    /// user just installed are presented by an Accessory process with no Dock icon and no
    /// Cmd-Tab entry, and the overlay chrome strip is installed for a launch that mints no
    /// overlay.
    ///
    /// Note the first four arguments are all FALSE here: that IS the shape of the bug. From
    /// `startup` alone a post-update relaunch is indistinguishable from a plain capture
    /// launch, which is why the marker has to reach the boot decision.
    #[test]
    fn a_post_update_relaunch_boots_regular() {
        assert!(boots_regular_policy(false, false, false, false, true));
    }

    /// The DRAGON-151 pin, and the reason this predicate is not simply "always true": a
    /// HEALTHY capture launch must stay Accessory. Promoting it puts a Dock icon up and
    /// stamps the app name into the menu bar, which then shows up inside captures of the
    /// menu-bar area.
    ///
    /// DRAGON-443: "healthy" now includes "not a post-update relaunch" — the fifth argument
    /// is the only difference between this case and the one above it.
    #[test]
    fn a_healthy_capture_launch_never_boots_regular() {
        assert!(!boots_regular_policy(false, false, false, false, false));
    }

    /// The COLOUR PICKER pin. A `--color-picker` launch ends in a real window, so it reads
    /// like a sixth window-shaped reason and must NOT become one: it is overlay-first, so
    /// from `startup` alone it is shaped exactly like a capture launch and answers FALSE
    /// here, and its window takes Regular after the fact through
    /// `platform::mac::window::ensure_regular_policy`. Promoting the launch would tile the
    /// picker's own overlays under AeroSpace (DRAGON-154), stamp the menu bar into the
    /// frozen snapshot the picker samples (DRAGON-151), and mint a Dock icon for the picker
    /// launches that open no window at all (DRAGON-587 / DRAGON-613). The predicate's doc
    /// carries the full account.
    #[test]
    fn a_colour_picker_launch_does_not_boot_regular() {
        let picker = Startup { color_picker: true, ..Default::default() };
        assert!(picker.opens_overlays(), "the picker mints capture-shaped overlays");
        // `post_update` (like the two `boots_regular_policy` gates themselves) is
        // macOS-only, but this table runs on every host; a fresh `Startup` never sets
        // it, so the literal `false` this test wants is the same value the field
        // would read on the one platform where it exists.
        assert!(!boots_regular_policy(
            picker.settings_only,
            picker.permissions_only,
            picker.preview.is_some() || picker.preview_handoff.is_some(),
            false,
            false,
        ));
    }

    /// A colour-picker launch that ROUTES to the permission checker is the one exception,
    /// and it needs no special case: it is covered by the fourth argument exactly like any
    /// other routed capture launch. Worth pinning, because a picker with no Screen Recording
    /// grant has no pixels to sample and really does show only the checker window.
    #[test]
    fn a_routed_colour_picker_launch_still_boots_regular() {
        let picker = Startup { color_picker: true, ..Default::default() };
        // See the sibling test above for why this is a literal `false` rather than
        // `picker.post_update` (macOS-only field, this table runs on every host).
        assert!(boots_regular_policy(
            picker.settings_only,
            picker.permissions_only,
            false,
            true,
            false,
        ));
    }

    /// The whole table, so a future argument cannot be added without a decision about it:
    /// the predicate is an OR, and each input is on its own sufficient and on its own
    /// insufficient.
    #[test]
    fn every_window_shaped_reason_is_sufficient_on_its_own() {
        let inputs: [fn(bool) -> bool; 5] = [
            |b| boots_regular_policy(b, false, false, false, false),
            |b| boots_regular_policy(false, b, false, false, false),
            |b| boots_regular_policy(false, false, b, false, false),
            |b| boots_regular_policy(false, false, false, b, false),
            |b| boots_regular_policy(false, false, false, false, b),
        ];
        for (i, f) in inputs.iter().enumerate() {
            assert!(f(true), "argument {i} alone must boot Regular");
            assert!(!f(false), "argument {i} alone must not");
        }
    }

    /// The Windows 10 preview-appearance rule, and its total absence everywhere else.
    #[test]
    fn windows_10_forces_the_windowed_editor_without_touching_other_platforms() {
        // Overlay editor available (macOS, Windows 11, a layer-shell Linux session): the
        // chosen value is returned untouched for every combination — byte-identical.
        for chosen in [false, true] {
            for opens in [false, true] {
                for software in [false, true] {
                    assert_eq!(
                        effective_preview_windowed(chosen, opens, true, software),
                        chosen,
                        "chosen={chosen} opens_overlays={opens} software={software}"
                    );
                }
            }
        }
        // Windows 10, the EDITOR half (opens no overlays): always the window, even when the
        // persisted setting says overlay or `--preview --overlay` asked for one. Hiding the
        // setting while still honouring it would strand such a user on a broken editor.
        assert!(effective_preview_windowed(false, false, false, true));
        assert!(effective_preview_windowed(true, false, false, true));
        // Windows 10, the CAPTURE half (opens overlays): its preview surfaces are fullscreen
        // loaders and covers, which must stay translucent overlays in the software-rendered
        // process. The real editor is spawned as its own process instead.
        assert!(!effective_preview_windowed(false, true, false, true));
        assert!(!effective_preview_windowed(true, true, false, true));
    }

    /// `lab/flatpak`: a Linux session with no layer shell has no fullscreen preview surface
    /// of ANY kind, so both halves land on the window. The capture half especially: it must
    /// NOT come out `false` here, because `save_state` persists `preview_windowed` and a
    /// sandboxed capture would then quietly rewrite the user's chosen appearance.
    #[test]
    fn a_linux_session_without_layer_shell_is_windowed_in_both_halves() {
        for chosen in [false, true] {
            for opens in [false, true] {
                assert!(
                    effective_preview_windowed(chosen, opens, false, false),
                    "chosen={chosen} opens_overlays={opens}"
                );
            }
        }
    }

    /// A spawned editor child receives its request as ONE argv word-set with no trailing
    /// newline, while the socket transport sends the same line newline-terminated. Both
    /// must parse back to the identical request, or the two transports would open subtly
    /// different documents.
    #[test]
    fn the_spawn_argument_is_the_same_wire_line_the_socket_sends() {
        let req = crate::preview_ipc::OpenRequest {
            path: std::path::PathBuf::from("/tmp/capture 1.mp4"),
            video: true,
            display_dims: Some((2560, 1440)),
            source_scale: 1.5,
            external: false,
            size: Some(4_242_424),
        };
        let line = req.encode();
        assert!(line.ends_with('\n'), "the socket form is newline-terminated");
        let argv = line.trim_end();
        assert!(!argv.contains('\n'), "the argv form carries no terminator");
        assert_eq!(crate::preview_ipc::OpenRequest::parse(argv), Ok(req.clone()));
        assert_eq!(crate::preview_ipc::OpenRequest::parse(&line), Ok(req));
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

    // DRAGON-336: the launch-instant flats are ~30 MB PER OUTPUT held for the whole
    // session, and only freeze + the QR/OCR scanners can read them. Grab them only when
    // this launch can actually consume them.
    #[test]
    fn launch_flats_are_grabbed_only_when_freeze_or_the_scanner_can_read_them() {
        // Freeze on: every kind needs the flats (the backdrop AND the freeze capture).
        assert!(launch_flats_needed(true, true, Kind::Image, false));
        assert!(launch_flats_needed(true, true, Kind::Video, false));
        assert!(launch_flats_needed(true, true, Kind::Scanner, false));
        // Freeze off, scanner launch (`--scan`): the scan source IS the flats crop.
        assert!(launch_flats_needed(true, false, Kind::Scanner, false));
        // Freeze off, plain photo/video launch: nothing can read them — skip the grab.
        assert!(!launch_flats_needed(true, false, Kind::Image, false));
        assert!(!launch_flats_needed(true, false, Kind::Video, false));
    }

    // DRAGON-582: the colour picker is the third reader, and its need is unconditional —
    // it samples the flats for every pointer move, so freeze-off must not skip the grab.
    #[test]
    fn a_colour_picker_launch_always_grabs_the_flats() {
        assert!(launch_flats_needed(true, false, Kind::Image, true));
        assert!(launch_flats_needed(true, true, Kind::Image, true));
        // And it still needs an ACTIVE scene: a settings launch grabs nothing.
        assert!(!launch_flats_needed(false, false, Kind::Image, true));
    }

    // The picker wants the flats and NOTHING else of the capture scene: no window
    // pre-capture (it is never a window-mode launch) and no locked cursor sprite.
    #[test]
    fn a_colour_picker_launch_wants_only_the_flats() {
        assert!(!launch_cursor_needed(true, true), "no captured pointer to draw");
        assert!(launch_cursor_needed(true, false), "an ordinary capture is untouched");
        assert!(!launch_cursor_needed(false, false), "and the preference still rules");
        assert!(!launch_precapture_runs(true, Mode::Region));
    }

    // A non-capture launch (settings / preview / permissions -> active=false) never
    // grabs the flats, whatever the persisted freeze setting or kind says.
    #[test]
    fn launch_flats_are_never_grabbed_for_a_non_capture_launch() {
        assert!(!launch_flats_needed(false, true, Kind::Scanner, false));
        assert!(!launch_flats_needed(false, true, Kind::Image, false));
        assert!(!launch_flats_needed(false, false, Kind::Scanner, false));
    }

    // DRAGON-456: the scan kind button carries two meanings, and which one it carries
    // depends ONLY on whether the scanner is already the active kind.
    #[test]
    fn pressing_scan_refreshes_only_when_the_scanner_is_already_open() {
        // Already in the scanner: the press has no kind to change, so it re-reads.
        assert!(scan_press_refreshes(Kind::Scanner, Kind::Scanner));
        // Entering the scanner from another kind is a kind SWITCH, never a refresh —
        // that path kicks the lazy first grab instead (`kick_frozen_flats`).
        assert!(!scan_press_refreshes(Kind::Image, Kind::Scanner));
        assert!(!scan_press_refreshes(Kind::Video, Kind::Scanner));
        // Pressing any OTHER kind is a plain switch, including while the scanner is open.
        assert!(!scan_press_refreshes(Kind::Scanner, Kind::Image));
        assert!(!scan_press_refreshes(Kind::Scanner, Kind::Video));
        assert!(!scan_press_refreshes(Kind::Image, Kind::Video));
    }

    /// DRAGON-460: the scan-shot machine has exactly two in-flight states, and the tick that
    /// takes the picture must only fire in the FIRST of them.
    ///
    /// The gap between them is the whole safety property. `Clearing` means the marks have
    /// been dropped but the frame without them may not have reached the screen yet;
    /// `Shooting` means the picture is being taken. Firing the shot while still `Idle` would
    /// photograph the marks, and firing it again while `Shooting` would start a second
    /// capture racing the first into the same slot.
    #[test]
    fn the_shot_is_taken_once_and_only_after_the_marks_are_cleared() {
        assert_eq!(ScanShot::Idle, ScanShot::Idle);
        // The three states are distinct, so the guards in `run_scan_shot`
        // (`!= Clearing` -> return) and `begin_scan_shot` (`!= Idle` -> return) can tell
        // "not started", "waiting for the clean frame" and "in flight" apart.
        assert_ne!(ScanShot::Idle, ScanShot::Clearing);
        assert_ne!(ScanShot::Clearing, ScanShot::Shooting);
        assert_ne!(ScanShot::Idle, ScanShot::Shooting);
    }

    // DRAGON-456's `frozen_delivery_accepted` test lived here. It pinned that a FAILED
    // refresh could not destroy the snapshot it was meant to replace, back when a refresh
    // wrote through the same flats slot as the launch grab.
    //
    // DRAGON-460 removed the function with the shared slot: the scanner reads its own live
    // region shot, and the same guarantee is now made where that shot lands (`MarksPoll`
    // takes `Some(None)` — the platform returned nothing — as "leave the previous answer
    // standing", never as "the screen is empty"). The property survived; only the place it
    // is enforced moved.

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

// DRAGON-571: the settings picker's READ side, pinned as its own island. It only
// displays a resolution; the write side (a real click through SetPreferredEncoder)
// is the sole path that persists a concrete id. All cases use ids present in every
// platform's `encode::AUTO_LADDER` ("nvenc", "vaapi", "software"), so the module
// passes unchanged on Linux, macOS and Windows.
/// DRAGON-595: the ONE cursor rule, pinned in its own module because it is now the
/// single copy that two mechanisms translate. Its whole job is to answer WHETHER a
/// capture takes the pointer; HOW is `platform::backend::CursorDelivery`, and the
/// separation is what let the native and portal copies collapse into this.
#[cfg(test)]
mod cursor_wanted_tests {
    use super::cursor_wanted;

    // The plain preference, both ways. This is the user-facing feature.
    #[test]
    fn an_ordinary_capture_follows_the_preference() {
        assert!(cursor_wanted(true, false));
        assert!(!cursor_wanted(false, false));
    }

    // The colour picker overrides it OFF whatever the setting says. The ON case is
    // the one that matters: the preference defaults on, and a picker session with a
    // baked pointer has a permanent blind spot over the pixels it exists to read.
    #[test]
    fn the_colour_picker_always_declines_the_pointer() {
        assert!(!cursor_wanted(true, true));
        assert!(!cursor_wanted(false, true));
    }

    // The whole table, so a future term has to be placed deliberately rather than
    // defaulting into someone else's lane: exactly one of the four states wants it.
    #[test]
    fn only_a_non_picker_capture_with_the_preference_on_wants_the_pointer() {
        let wants: Vec<_> = [(true, false), (true, true), (false, true), (false, false)]
            .into_iter()
            .filter(|(want, picker)| cursor_wanted(*want, *picker))
            .collect();
        assert_eq!(wants, vec![(true, false)]);
    }

    // The launch gate is this rule with the LAUNCH inputs, not a second rule. Pinned
    // so a change here cannot quietly leave the startup sprite grab behind.
    #[test]
    fn the_launch_gate_is_the_same_rule() {
        for (want, picker) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(super::launch_cursor_needed(want, picker), cursor_wanted(want, picker));
        }
    }
}

/// DRAGON-604: the capture MODE's veto over the pointer. Its own module because it
/// pins one rule and one promise, that NO route into the scanner can photograph the
/// mouse. The tests are deliberately table-driven over every [`Kind`] rather than one
/// case per entry point: a new entry point is invisible to a per-route test, but a new
/// KIND, or a kind that quietly starts honouring the preference again, fails the table.
#[cfg(test)]
mod kind_cursor_veto_tests {
    use super::{Kind, capture_extras_for_kind, kind_keeps_pointer};
    use crate::platform::backend::CaptureExtras;

    /// Every `Kind` there is. The `match` in [`kind_keeps_pointer`] is exhaustive, so
    /// adding a variant breaks the build there; this list is what makes it break HERE
    /// too, where the promise is written down.
    const EVERY_KIND: [Kind; 3] = [Kind::Scanner, Kind::Image, Kind::Video];

    const ALL_ON: CaptureExtras = CaptureExtras {
        freeze: true,
        cursor: true,
        transparency: true,
        wallpaper: true,
        fullscreen_aware: true,
    };
    const ALL_OFF: CaptureExtras = CaptureExtras {
        freeze: false,
        cursor: false,
        transparency: false,
        wallpaper: false,
        fullscreen_aware: false,
    };

    // The headline promise. A fully capable backend plus the preference ON is the
    // strongest case the veto has to beat, and it is the DEFAULT state: the pref ships
    // on. Whatever route reached the scanner, the answer is no pointer.
    #[test]
    fn the_scanner_never_keeps_the_pointer_however_loudly_the_preference_says_yes() {
        assert!(!kind_keeps_pointer(Kind::Scanner));
        assert!(!capture_extras_for_kind(ALL_ON, ALL_ON, Kind::Scanner).cursor);
    }

    // The other half, so the veto is a veto and not a blanket off switch: a picture
    // still follows the preference in both directions.
    #[test]
    fn a_picture_still_follows_the_preference_in_both_directions() {
        for kind in [Kind::Image, Kind::Video] {
            assert!(kind_keeps_pointer(kind), "{kind:?} is a picture of the desktop");
            assert!(capture_extras_for_kind(ALL_ON, ALL_ON, kind).cursor, "{kind:?} pref on");
            let pref_off = CaptureExtras { cursor: false, ..ALL_ON };
            assert!(!capture_extras_for_kind(ALL_ON, pref_off, kind).cursor, "{kind:?} pref off");
        }
    }

    // The whole table in one place: for every kind, crossed with every combination of
    // capability and preference, the cursor extra is on in exactly the states where the
    // kind allows it AND both older terms agree. This is the test that catches an entry
    // point someone adds later, because it constrains the ANSWER rather than the route.
    #[test]
    fn the_cursor_extra_is_capability_and_preference_and_kind() {
        for kind in EVERY_KIND {
            for cap in [true, false] {
                for pref in [true, false] {
                    let caps = CaptureExtras { cursor: cap, ..ALL_ON };
                    let prefs = CaptureExtras { cursor: pref, ..ALL_ON };
                    let got = capture_extras_for_kind(caps, prefs, kind).cursor;
                    assert_eq!(
                        got,
                        cap && pref && kind_keeps_pointer(kind),
                        "kind={kind:?} capability={cap} preference={pref}"
                    );
                }
            }
        }
    }

    // A backend that cannot toggle the pointer still wins, for every kind: the veto is
    // additional to the DRAGON-186 gating, never a replacement that could talk an
    // incapable backend into trying.
    #[test]
    fn an_incapable_backend_still_forces_the_pointer_off_for_every_kind() {
        for kind in EVERY_KIND {
            assert!(!capture_extras_for_kind(ALL_OFF, ALL_ON, kind).cursor, "{kind:?}");
        }
    }

    // The veto touches the CURSOR bit and nothing else. Scanning still freezes, still
    // preserves transparency and still composites the wallpaper, because none of those
    // obscure what is being decoded. Pinned so a future edit cannot widen the veto into
    // a general "the scanner gets no extras" rule by accident.
    #[test]
    fn the_veto_takes_the_pointer_and_leaves_every_other_extra_alone() {
        let scan = capture_extras_for_kind(ALL_ON, ALL_ON, Kind::Scanner);
        assert_eq!(scan, CaptureExtras { cursor: false, ..ALL_ON });
        // And with the cursor already off, a scan is byte-identical to a picture, so
        // the veto adds nothing where nothing was asked for.
        let pref_off = CaptureExtras { cursor: false, ..ALL_ON };
        assert_eq!(
            capture_extras_for_kind(ALL_ON, pref_off, Kind::Scanner),
            capture_extras_for_kind(ALL_ON, pref_off, Kind::Image)
        );
    }

    // Switching kind must be enough on its own to change the answer, in BOTH
    // directions, with the capability and the preference held fixed. That is the
    // "cannot be a one-shot applied at launch and then lost" property: the readers call
    // this per capture with the CURRENT kind, so image -> scanner -> image recovers the
    // pointer instead of stranding the session with it off.
    #[test]
    fn a_mode_switch_alone_flips_the_answer_and_flips_it_back() {
        let seq = [Kind::Image, Kind::Scanner, Kind::Image, Kind::Scanner, Kind::Video];
        let got: Vec<bool> =
            seq.iter().map(|k| capture_extras_for_kind(ALL_ON, ALL_ON, *k).cursor).collect();
        assert_eq!(got, vec![true, false, true, false, true]);
    }

    // The end-to-end translation into the portal's own mechanism (a scan asks its
    // ScreenCast stream to OMIT the pointer, rather than merely declining to draw one)
    // is pinned next to `cursor_request` itself, in `portal::cursor_request_tests`,
    // where that private fn is in scope.
}

/// DRAGON-604: the one place the mode veto is not enough on its own, the `lab/flatpak`
/// fallback overlay's single seed frame. Its own module because it guards a different
/// promise from the veto's: not "what does this capture apply" but "do the pixels we
/// are holding still match it".
#[cfg(test)]
mod fallback_reseed_tests {
    use super::fallback_reseed_needed;

    // The reported shape: a session seeded WITH the pointer, then the user enters the
    // scanner, so the veto now wants it gone. Those pixels cannot be changed in place,
    // so the frame has to be grabbed again.
    #[test]
    fn entering_the_scanner_on_a_pointer_bearing_frame_re_seeds() {
        assert!(fallback_reseed_needed(true, Some(true), false));
    }

    // And the mirror, which is what stops the veto being a one-way door: leaving the
    // scanner has to bring the pointer back for the rest of the session.
    #[test]
    fn leaving_the_scanner_re_seeds_to_get_the_pointer_back() {
        assert!(fallback_reseed_needed(true, Some(false), true));
    }

    // An unchanged answer costs nothing. This is most kind switches: image to video
    // either way, and every switch at all with the preference already off.
    #[test]
    fn an_unchanged_answer_never_pays_for_a_portal_round_trip() {
        assert!(!fallback_reseed_needed(true, Some(true), true));
        assert!(!fallback_reseed_needed(true, Some(false), false));
    }

    // Before the launch seed there is no frame to disagree with, so there is nothing to
    // replace. The seed request itself picks up the right answer when it runs.
    #[test]
    fn nothing_to_replace_before_the_first_seed() {
        assert!(!fallback_reseed_needed(true, None, true));
        assert!(!fallback_reseed_needed(true, None, false));
    }

    // Every session WITH layer shell is untouched, whatever the bits say. Those
    // sessions re-read the rule per capture and never hold a baked-in frame, which is
    // why the veto alone is sufficient there.
    #[test]
    fn a_layer_shell_session_never_re_seeds() {
        for seeded in [None, Some(true), Some(false)] {
            for wants in [true, false] {
                assert!(!fallback_reseed_needed(false, seeded, wants), "{seeded:?} {wants}");
            }
        }
    }
}

#[cfg(test)]
mod display_encoder_choice_tests {
    use super::display_encoder_choice;

    #[test]
    fn a_concrete_pick_displays_itself_while_usable() {
        assert_eq!(display_encoder_choice("nvenc", None, &["nvenc", "software"]), "nvenc");
        // A software pick is a real choice too.
        assert_eq!(display_encoder_choice("software", None, &["nvenc", "software"]), "software");
    }

    #[test]
    fn an_unusable_pick_displays_the_ladder_fallback() {
        // The pick stays persisted (intent is not touched by display); the picker
        // just shows what a recording would actually land on.
        assert_eq!(display_encoder_choice("nvenc", None, &["software"]), "software");
    }

    #[test]
    fn auto_displays_the_ranked_best_available() {
        assert_eq!(display_encoder_choice("auto", None, &["nvenc", "software"]), "nvenc");
        assert_eq!(display_encoder_choice("auto", None, &["software"]), "software");
    }

    #[test]
    fn auto_displays_a_usable_hint_first() {
        // The hint leads the probe order, so it leads the display too: display and
        // recording cannot disagree.
        assert_eq!(
            display_encoder_choice("auto", Some("vaapi"), &["nvenc", "vaapi", "software"]),
            "vaapi"
        );
    }

    #[test]
    fn auto_ignores_a_software_or_failed_hint() {
        // A software hint must never pin the display on the CPU fallback (the
        // DRAGON-571 bug), and a hint whose encoder dropped out of the probed list
        // degrades to the ranked order.
        assert_eq!(
            display_encoder_choice("auto", Some("software"), &["nvenc", "software"]),
            "nvenc"
        );
        assert_eq!(
            display_encoder_choice("auto", Some("vaapi"), &["nvenc", "software"]),
            "nvenc"
        );
    }

    #[test]
    fn an_empty_probed_list_is_software() {
        assert_eq!(display_encoder_choice("auto", None, &[]), "software");
    }
}

