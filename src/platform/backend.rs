//! The capture-backend seam: one trait every platform implements, so the rest of
//! the app asks "what can this environment do?" (and, increasingly, "do it")
//! without knowing which compositor/OS it's on.
//!
//! This formalizes what `app::settings::deps` modelled informally as
//! `CaptureMethod {screenshot, record}` — the Health page and the required-
//! capability checks read backend [`Caps`], and (DRAGON-129) the settings
//! "Capture method" dropdowns enumerate [`method_choices`] while the persisted
//! selection stores the stable [`CaptureBackend::id`]. Teaching the app a new
//! platform means implementing this trait and adding it to [`backends`]:
//! dropdown, dispatch keying, and Health all pick it up from there.
//!
//! P0 honesty note (DRAGON-92): capability reporting is fully live; the pixel
//! methods delegate to today's code for the cosmic backend, while the portal
//! backend's capture is SESSION-DRIVEN (a held xdg-portal ScreenCast session in
//! `app::capture_flow` / `record::pipewire`) and can't be a stateless call yet —
//! its pixel methods return `None`, and the capture flow keys its portal
//! branches on the selected backend ID (`App::screenshot_uses_portal` /
//! `recording_uses_portal`) until the Linux expansion ticket (DRAGON-93) moves
//! the session itself behind the trait.

use crate::platform::compositor::Toplevel;
use image::RgbaImage;

/// Stable ids for the built-in backends. These are PERSISTED in config.toml
/// (`screenshot_backend` / `record_backend`), so they must never be renamed.
/// [`Caps::name`] is the display label; this is the storage key.
pub const SCREENCOPY_ID: &str = "screencopy";
pub const PORTAL_ID: &str = "portal";
pub const SCK_ID: &str = "sck";

/// The platform's native backend id — what a saved capture-method choice falls
/// back to when it doesn't exist in this environment, and the screenshot default.
pub fn native_backend_id() -> &'static str {
    if cfg!(target_os = "linux") {
        SCREENCOPY_ID
    } else {
        SCK_ID
    }
}

/// A backend-agnostic monitor description: name + logical geometry (global,
/// top-left origin — the coordinate model the whole app uses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDesc {
    pub name: String,
    pub logical_pos: (i32, i32),
    pub logical_size: (i32, i32),
}

/// A monitor as the encoder benchmark sees it (DRAGON-163): a friendly label plus the
/// TRUE capture pixel footprint the capture backend would deliver for that output — the
/// physical/backing pixels (mac: logical points x `pointPixelScale`, e.g. a scaled-mode
/// Studio Display's 6400x3600; Linux: the output's current mode's physical resolution).
/// The benchmark tests these dims (through the recording encode plan) so its verdict
/// predicts real recording on that monitor, closing the DRAGON-162 large-display gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchMonitor {
    /// Stable backend output name (e.g. `Display-<id>` on mac), for logs/diagnostics.
    pub name: String,
    /// Human label for the dropdown: the friendly name + the true pixel size.
    pub label: String,
    /// True capture pixel footprint (physical/backing pixels).
    pub px_w: u32,
    pub px_h: u32,
}

/// What a backend can do in the current environment. `false` never means
/// "broken" — features gate off it honestly (Health rows, hidden settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Backend name for the Health page / logs.
    pub name: &'static str,
    /// Can take a still screenshot right now.
    pub screenshot: bool,
    /// Can record right now (capture path AND an encoder — the ffmpeg binary).
    pub record: bool,
    /// Can enumerate toplevel windows (the picker grid).
    pub window_list: bool,
    /// Can capture a single window's pixels by id (occlusion-proof).
    pub window_capture: bool,
    /// Can capture the cursor as a real sprite + position + hotspot.
    pub cursor_session: bool,
    /// Can create layer-shell overlay surfaces (vs plain fullscreen windows).
    pub layer_overlay: bool,
    /// Can resolve the desktop wallpaper to an image file (for the freeze
    /// backdrop + wallpaper-behind-window composites).
    pub wallpaper_path: bool,
    /// Can reconstruct captures from the launch-instant frozen scene (the
    /// "Freeze pixels during selection" extra). DRAGON-186.
    pub freeze: bool,
    /// Can preserve per-window transparency in composites (the "Preserve
    /// window transparency" extra) — needs real per-window pixels. DRAGON-186.
    pub transparency: bool,
    /// Can composite the desktop wallpaper INTO captures (the "Preserve
    /// wallpaper" extra). Distinct from [`Self::wallpaper_path`]: resolving the
    /// wallpaper FILE is not the same as compositing it correctly. DRAGON-186.
    pub wallpaper_compose: bool,
    /// Can detect that a captured window is truly fullscreen (e.g. a fullscreen
    /// game), so window-aesthetic compositing (border / shadow / rounding /
    /// padding / wallpaper-behind) can be skipped for it. A behavior capability,
    /// never a settings toggle. DRAGON-186.
    pub fullscreen_aware: bool,
}

impl Caps {
    /// This backend's capture-extras capability set, ONE bit per extra. The
    /// cursor extra reads [`Self::cursor_session`] (a real sprite session is
    /// exactly what "Preserve mouse cursor" needs) and the wallpaper extra reads
    /// [`Self::wallpaper_compose`] — NOT `wallpaper_path` — so each bit keeps a
    /// single source of truth. DRAGON-186.
    pub fn capture_extras(&self) -> CaptureExtras {
        CaptureExtras {
            freeze: self.freeze,
            cursor: self.cursor_session,
            transparency: self.transparency,
            wallpaper: self.wallpaper_compose,
            fullscreen_aware: self.fullscreen_aware,
        }
    }
}

/// The capture "extras" as one set of bits (DRAGON-186): the four settings
/// toggles (freeze / cursor / transparency / wallpaper) plus the
/// fullscreen-awareness behavior bit. The same shape serves as a backend's
/// CAPABILITY set ([`Caps::capture_extras`]), the user's persisted PREFERENCES,
/// and the EFFECTIVE set actually applied to a capture ([`CaptureExtras::and`]).
/// A future compositor supports an extra by declaring its bit in its `caps()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureExtras {
    /// "Freeze pixels during selection".
    pub freeze: bool,
    /// "Preserve mouse cursor".
    pub cursor: bool,
    /// "Preserve window transparency".
    pub transparency: bool,
    /// "Preserve wallpaper".
    pub wallpaper: bool,
    /// Fullscreen-window awareness (skip window-aesthetic compositing for a
    /// truly-fullscreen window). Behavior capability only, no settings row;
    /// preference sets carry it as `true` so the capability alone decides.
    pub fullscreen_aware: bool,
}

impl CaptureExtras {
    /// Field-wise AND — the effective-extras rule: an extra applies only when
    /// the user asked for it AND the active backend can honor it, so a stale
    /// persisted "on" from a supporting backend can never make an unsupporting
    /// one try (and fail) to honor it. DRAGON-186.
    #[must_use]
    pub fn and(self, other: CaptureExtras) -> CaptureExtras {
        CaptureExtras {
            freeze: self.freeze && other.freeze,
            cursor: self.cursor && other.cursor,
            transparency: self.transparency && other.transparency,
            wallpaper: self.wallpaper && other.wallpaper,
            fullscreen_aware: self.fullscreen_aware && other.fullscreen_aware,
        }
    }
}

/// One capture implementation (compositor protocol family or OS API family).
pub trait CaptureBackend {
    /// The backend's stable identifier (see the `*_ID` constants) — the value the
    /// persisted capture-method settings store, so it must never change.
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;
    /// Every monitor, in the backend's global logical coordinates.
    fn outputs(&self) -> Vec<OutputDesc>;
    /// A full-monitor screenshot by output name.
    fn screenshot_output(&self, name: &str) -> Option<RgbaImage>;
    /// Toplevels on the active workspace (id, global rect, title, active flag).
    fn list_windows(&self) -> Vec<Toplevel>;
    /// One window's pixels by toplevel id (works while occluded).
    fn screenshot_window(&self, id: &str) -> Option<RgbaImage>;
    /// The cursor as (sprite with real alpha, global position, hotspot).
    fn cursor(&self) -> Option<crate::screenshot::CursorSprite>;
}

/// What the Wayland compositor actually advertises, by PROTOCOL — not by desktop
/// name. Today's capture stack speaks the upstream `ext-image-copy-capture-v1`
/// family, so any compositor implementing these globals (COSMIC, wlroots ≥0.18 —
/// Sway 1.10+, Hyprland, …) runs the native backend unchanged.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaylandProtocols {
    /// `ext_image_copy_capture_manager_v1` — frame + cursor capture sessions.
    pub image_copy_capture: bool,
    /// `ext_output_image_capture_source_manager_v1` — whole-monitor sources.
    pub output_source: bool,
    /// `ext_foreign_toplevel_image_capture_source_manager_v1` — per-window sources.
    pub toplevel_source: bool,
    /// `ext_foreign_toplevel_list_v1` (or the cosmic equivalent) — the window list.
    pub toplevel_list: bool,
    /// `zwlr_layer_shell_v1` — real overlay surfaces (vs plain windows).
    pub layer_shell: bool,
}

/// The compositor's advertised protocols, probed once per process (one throwaway
/// Wayland connection listing the registry) and cached. All-false when there is
/// no Wayland display (X11 session / headless).
#[cfg(target_os = "linux")]
pub fn wayland_protocols() -> WaylandProtocols {
    static PROBE: std::sync::OnceLock<WaylandProtocols> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| probe_globals().unwrap_or_default())
}

#[cfg(target_os = "linux")]
fn probe_globals() -> Option<WaylandProtocols> {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry::WlRegistry;

    struct Probe;
    impl wayland_client::Dispatch<WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &wayland_client::Connection,
            _: &wayland_client::QueueHandle<Self>,
        ) {
        }
    }

    let conn = wayland_client::Connection::connect_to_env().ok()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    let mut p = WaylandProtocols::default();
    globals.contents().with_list(|list| {
        for g in list {
            match g.interface.as_str() {
                "ext_image_copy_capture_manager_v1" => p.image_copy_capture = true,
                "ext_output_image_capture_source_manager_v1" => p.output_source = true,
                "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                    p.toplevel_source = true;
                }
                "ext_foreign_toplevel_list_v1" | "zcosmic_toplevel_info_v1" => {
                    p.toplevel_list = true;
                }
                "zwlr_layer_shell_v1" => p.layer_shell = true,
                _ => {}
            }
        }
    });
    Some(p)
}

/// The native ext-image-copy-capture backend — today's capture stack
/// (`crate::screencopy` + `crate::screenshot` + toplevel-info). Available on any
/// compositor advertising the protocols, COSMIC or not.
#[cfg(target_os = "linux")]
pub struct ScreencopyBackend {
    /// The compositor's probed protocol set.
    pub protocols: WaylandProtocols,
    /// Whether the ffmpeg binary resolved (recording needs an encoder).
    pub ffmpeg: bool,
}

#[cfg(target_os = "linux")]
impl CaptureBackend for ScreencopyBackend {
    fn id(&self) -> &'static str {
        SCREENCOPY_ID
    }

    fn caps(&self) -> Caps {
        let p = self.protocols;
        let screenshot = p.image_copy_capture && p.output_source;
        Caps {
            name: "Compositor screencopy",
            screenshot,
            record: screenshot && self.ffmpeg,
            window_list: p.toplevel_list,
            window_capture: p.image_copy_capture && p.toplevel_source && p.toplevel_list,
            // The cursor session hangs off the capture manager + an output source.
            cursor_session: screenshot,
            layer_overlay: p.layer_shell,
            // Desktop-specific, not protocol: whatever the wallpaper ladder finds.
            wallpaper_path: crate::wallpaper::detect().is_some(),
            // The capture extras ride the native capture path itself: whenever
            // this backend can screenshot, it reconstructs the frozen scene,
            // composites transparency, and places the wallpaper (a missing
            // wallpaper FILE degrades at compose time, exactly as today).
            freeze: screenshot,
            transparency: screenshot,
            wallpaper_compose: screenshot,
            // Window state (the future fullscreen probe) comes off the toplevel list.
            fullscreen_aware: p.toplevel_list,
        }
    }

    fn outputs(&self) -> Vec<OutputDesc> {
        crate::screenshot::output_descs()
    }

    fn screenshot_output(&self, name: &str) -> Option<RgbaImage> {
        crate::screenshot::output(name, None)
    }

    fn list_windows(&self) -> Vec<Toplevel> {
        crate::platform::compositor::list_toplevels()
            .into_values()
            .flatten()
            .collect()
    }

    fn screenshot_window(&self, id: &str) -> Option<RgbaImage> {
        crate::screenshot::windows(&[id.to_string()]).remove(id)
    }

    fn cursor(&self) -> Option<crate::screenshot::CursorSprite> {
        crate::screenshot::capture_cursor()
    }
}

/// The xdg-desktop-portal backend (ScreenCast + PipeWire). Universal across
/// Wayland desktops, at the cost of portal permission dialogs.
#[cfg(target_os = "linux")]
pub struct PortalBackend {
    /// Whether the portal + PipeWire probe succeeded (App::pipewire_available).
    pub available: bool,
    pub ffmpeg: bool,
}

#[cfg(target_os = "linux")]
impl CaptureBackend for PortalBackend {
    fn id(&self) -> &'static str {
        PORTAL_ID
    }

    fn caps(&self) -> Caps {
        Caps {
            name: "PipeWire portal",
            screenshot: self.available,
            record: self.available && self.ffmpeg,
            // The portal has its own window picker dialog; it can't enumerate
            // windows INTO our grid, and gives no standalone per-window grab.
            window_list: false,
            window_capture: false,
            // Cursor comes baked/metadata via stream modes, not a sprite session.
            cursor_session: false,
            layer_overlay: false,
            wallpaper_path: false,
            // The portal hands back finished frames: nothing to freeze, no
            // per-window pixels for transparency/wallpaper compositing, and no
            // window state — none of the capture extras can be honored.
            freeze: false,
            transparency: false,
            wallpaper_compose: false,
            fullscreen_aware: false,
        }
    }

    // Portal capture is session-driven (a held ScreenCast session with a user
    // permission grant, owned by the capture flow) — there is no stateless
    // "grab now" call to delegate to yet. Dispatch stays in `capture_flow` /
    // `record::pipewire`; DRAGON-93 moves it behind this trait.
    fn outputs(&self) -> Vec<OutputDesc> {
        Vec::new()
    }
    fn screenshot_output(&self, _name: &str) -> Option<RgbaImage> {
        None
    }
    fn list_windows(&self) -> Vec<Toplevel> {
        Vec::new()
    }
    fn screenshot_window(&self, _id: &str) -> Option<RgbaImage> {
        None
    }
    fn cursor(&self) -> Option<crate::screenshot::CursorSprite> {
        None
    }
}

/// The ScreenCaptureKit backend (macOS 13+). Phase 2 (DRAGON-94) wires the STILL
/// pixel methods through `crate::platform::mac` (objc2 + SCK): `SCScreenshotManager`
/// for stills, `SCShareableContent` for the display/window list, `NSCursor` for the
/// cursor sprite. Recording (`SCStream`) landed in DRAGON-130 phase 3; the wallpaper
/// file resolves through `NSWorkspace.desktopImageURLForScreen:` (DRAGON-130 —
/// per-DISPLAY, main screen; per-Space and `.heic`/rotating-set pictures degrade to
/// `None` honestly). A layer-shell overlay has no macOS analogue (the PlainWindows
/// overlay stands in), so that cap stays off.
#[cfg(target_os = "macos")]
pub struct MacBackend {
    /// Whether the (bundled) ffmpeg binary resolved — recording needs an encoder.
    pub ffmpeg: bool,
}

#[cfg(target_os = "macos")]
impl CaptureBackend for MacBackend {
    fn id(&self) -> &'static str {
        SCK_ID
    }

    fn caps(&self) -> Caps {
        // Recording (SCStream video + h264_videotoolbox) landed in DRAGON-130 phase 3
        // (`record::sck`); it needs an ffmpeg to mux/encode, so gate on it exactly as
        // the Linux backends gate `record` on their own ffmpeg.
        Caps {
            name: "ScreenCaptureKit",
            screenshot: true,
            record: self.ffmpeg,
            window_list: true,
            window_capture: true,
            cursor_session: true,
            // No layer-shell (the PlainWindows overlay is phase 2b).
            layer_overlay: false,
            // Live-probed like the Linux backends: the AppKit desktop-picture
            // lookup behind `detect()` (macOS arm), which is `None` for the
            // undecodable cases (.heic dynamic wallpapers, rotating-set folders).
            wallpaper_path: crate::wallpaper::detect().is_some(),
            freeze: true,
            transparency: true,
            // DRAGON-186 Phase 2: the mac wallpaper compositor landed — a
            // windows-excluded SCK grab of the window's display sources the true
            // rendered wallpaper (`platform::mac::capture_wallpaper`), composited
            // behind the window in `platform/mac/screenshot.rs`'s `composite_over_wallpaper`.
            wallpaper_compose: true,
            fullscreen_aware: true,
        }
    }
    fn outputs(&self) -> Vec<OutputDesc> {
        crate::screenshot::output_descs()
    }
    fn screenshot_output(&self, name: &str) -> Option<RgbaImage> {
        crate::screenshot::output(name, None)
    }
    fn list_windows(&self) -> Vec<Toplevel> {
        crate::platform::mac::list_windows()
    }
    fn screenshot_window(&self, id: &str) -> Option<RgbaImage> {
        crate::screenshot::window(id, false)
    }
    fn cursor(&self) -> Option<crate::screenshot::CursorSprite> {
        crate::screenshot::capture_cursor()
    }
}

/// Every backend for this environment, in preference order. `portal_available`
/// is the app's runtime portal probe; `ffmpeg` the resolved-binary check.
#[cfg(target_os = "linux")]
pub fn backends(portal_available: bool, ffmpeg: bool) -> Vec<Box<dyn CaptureBackend>> {
    vec![
        Box::new(ScreencopyBackend { protocols: wayland_protocols(), ffmpeg }),
        Box::new(PortalBackend { available: portal_available, ffmpeg }),
    ]
}

/// macOS: the single ScreenCaptureKit backend (`portal_available` is Linux-only).
#[cfg(target_os = "macos")]
pub fn backends(_portal_available: bool, ffmpeg: bool) -> Vec<Box<dyn CaptureBackend>> {
    vec![Box::new(MacBackend { ffmpeg })]
}

/// One "Capture method" dropdown's contents, derived from [`backends`]: the stable
/// ids and their display labels ([`Caps::name`]) as PARALLEL vectors, because the
/// dropdown widget borrows a plain label slice. Same order as [`backends`].
#[derive(Default)]
pub struct MethodChoices {
    pub ids: Vec<&'static str>,
    pub labels: Vec<&'static str>,
}

impl MethodChoices {
    /// The dropdown index of `id`, `None` when the saved backend isn't offered here
    /// (e.g. a portal choice while the portal is unreachable).
    pub fn position(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|i| *i == id)
    }
}

/// The environment's backends filtered to one capability (`cap` picks it off each
/// backend's [`Caps`]), in preference order — the settings "Capture method"
/// dropdowns for screenshots (`|c| c.screenshot`) and recordings (`|c| c.record`).
pub fn method_choices(
    portal_available: bool,
    ffmpeg: bool,
    cap: fn(&Caps) -> bool,
) -> MethodChoices {
    choices_from(&backends(portal_available, ffmpeg), cap)
}

/// [`method_choices`] over an explicit backend list (split out for tests).
fn choices_from(backends: &[Box<dyn CaptureBackend>], cap: fn(&Caps) -> bool) -> MethodChoices {
    let mut choices = MethodChoices::default();
    for b in backends {
        let caps = b.caps();
        if cap(&caps) {
            choices.ids.push(b.id());
            choices.labels.push(caps.name);
        }
    }
    choices
}

#[cfg(test)]
mod extras_tests {
    use super::*;

    /// A Caps literal for exercising the extras accessor, platform-free.
    fn caps(extras: CaptureExtras, wallpaper_path: bool) -> Caps {
        Caps {
            name: "test",
            screenshot: true,
            record: true,
            window_list: true,
            window_capture: true,
            cursor_session: extras.cursor,
            layer_overlay: false,
            wallpaper_path,
            freeze: extras.freeze,
            transparency: extras.transparency,
            wallpaper_compose: extras.wallpaper,
            fullscreen_aware: extras.fullscreen_aware,
        }
    }

    const ALL: CaptureExtras = CaptureExtras {
        freeze: true,
        cursor: true,
        transparency: true,
        wallpaper: true,
        fullscreen_aware: true,
    };
    const NONE: CaptureExtras = CaptureExtras {
        freeze: false,
        cursor: false,
        transparency: false,
        wallpaper: false,
        fullscreen_aware: false,
    };

    #[test]
    fn extras_accessor_reads_each_bit_from_its_one_source() {
        assert_eq!(caps(ALL, true).capture_extras(), ALL);
        assert_eq!(caps(NONE, false).capture_extras(), NONE);
        // The wallpaper extra is wallpaper_compose, NOT wallpaper_path: a backend
        // that resolves the wallpaper file but can't composite it (macOS today)
        // must not offer the extra.
        let mac_shaped = caps(CaptureExtras { wallpaper: false, ..ALL }, true);
        assert!(mac_shaped.wallpaper_path);
        assert!(!mac_shaped.capture_extras().wallpaper);
        // The cursor extra is cursor_session.
        let no_cursor = caps(CaptureExtras { cursor: false, ..ALL }, true);
        assert!(!no_cursor.capture_extras().cursor);
    }

    #[test]
    fn effective_extras_are_pref_and_capability() {
        // The DRAGON-186 gating rule: an extra applies only when the persisted
        // preference AND the active backend's capability agree — the pattern a
        // future compositor inherits for free.
        let prefs = CaptureExtras { transparency: false, ..ALL };
        // A full-capability backend honors exactly the preferences.
        assert_eq!(ALL.and(prefs), prefs);
        // A no-extras backend (the portal) forces everything off, however stale
        // the persisted toggles are.
        assert_eq!(NONE.and(ALL), NONE);
        // A partial backend (mac: no wallpaper) can't be talked into the missing
        // extra by a persisted "on".
        let mac = CaptureExtras { wallpaper: false, ..ALL };
        assert!(!mac.and(ALL).wallpaper);
        assert!(mac.and(ALL).freeze && mac.and(ALL).cursor && mac.and(ALL).fullscreen_aware);
        // Symmetric AND: preference sets carry fullscreen_aware as true, so the
        // capability alone decides the behavior bit.
        assert!(!CaptureExtras { fullscreen_aware: false, ..ALL }.and(ALL).fullscreen_aware);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A full ext-image-copy-capture protocol set (COSMIC, modern wlroots).
    fn full() -> WaylandProtocols {
        WaylandProtocols {
            image_copy_capture: true,
            output_source: true,
            toplevel_source: true,
            toplevel_list: true,
            layer_shell: true,
        }
    }

    #[test]
    fn caps_gate_record_on_ffmpeg() {
        // A capture path without an encoder must not claim it can record.
        let c = ScreencopyBackend { protocols: full(), ffmpeg: false };
        assert!(c.caps().screenshot);
        assert!(!c.caps().record);
        let p = PortalBackend { available: true, ffmpeg: false };
        assert!(p.caps().screenshot);
        assert!(!p.caps().record);
    }

    #[test]
    fn screencopy_caps_follow_the_protocol_probe() {
        // Nothing advertised (GNOME, X11, headless): everything gates off.
        let off = ScreencopyBackend { protocols: WaylandProtocols::default(), ffmpeg: true };
        let caps = off.caps();
        assert!(!caps.screenshot && !caps.record && !caps.window_list && !caps.layer_overlay);
        // The full set: full capability, regardless of the desktop's NAME.
        let on = ScreencopyBackend { protocols: full(), ffmpeg: true };
        let caps = on.caps();
        assert!(caps.screenshot && caps.record && caps.window_capture && caps.cursor_session);
        assert!(caps.layer_overlay);
        // A compositor with capture but NO layer shell (or vice versa) reports
        // each capability independently.
        let partial = ScreencopyBackend {
            protocols: WaylandProtocols { layer_shell: false, ..full() },
            ffmpeg: true,
        };
        assert!(partial.caps().screenshot && !partial.caps().layer_overlay);
        // KDE-shaped: layer shell without ext-image-copy-capture.
        let kde = ScreencopyBackend {
            protocols: WaylandProtocols {
                layer_shell: true,
                toplevel_list: true,
                ..Default::default()
            },
            ffmpeg: true,
        };
        assert!(!kde.caps().screenshot && kde.caps().layer_overlay && kde.caps().window_list);
        assert!(!kde.caps().window_capture);
    }

    #[test]
    fn portal_is_capture_only() {
        // The portal can screenshot/record but brings no window grid, cursor
        // sprite, layer shell, or wallpaper — features must gate off these.
        let caps = PortalBackend { available: true, ffmpeg: true }.caps();
        assert!(caps.screenshot && caps.record);
        assert!(!caps.window_list && !caps.window_capture);
        assert!(!caps.cursor_session && !caps.layer_overlay && !caps.wallpaper_path);
    }

    #[test]
    fn native_declares_every_capture_extra_and_portal_none() {
        // DRAGON-186: the capture-extra toggles exist only where the backend can
        // honor them. Native screencopy (full protocol set) supports the whole
        // set; the portal (finished frames only) supports none of it — its four
        // settings rows must simply not render.
        let native = ScreencopyBackend { protocols: full(), ffmpeg: true }.caps().capture_extras();
        assert!(native.freeze && native.cursor && native.transparency && native.wallpaper);
        assert!(native.fullscreen_aware);
        let portal = PortalBackend { available: true, ffmpeg: true }.caps().capture_extras();
        assert!(!portal.freeze && !portal.cursor && !portal.transparency && !portal.wallpaper);
        assert!(!portal.fullscreen_aware);
        // No protocols advertised (the session clamp would route to the portal
        // anyway): the native backend honestly declares nothing.
        let bare = ScreencopyBackend { protocols: WaylandProtocols::default(), ffmpeg: true }
            .caps()
            .capture_extras();
        assert!(!bare.freeze && !bare.cursor && !bare.transparency && !bare.wallpaper);
        assert!(!bare.fullscreen_aware);
    }

    #[test]
    fn screencopy_freeze_cap_equals_screenshot_in_every_shape() {
        // DRAGON-186 Phase 2 gate-migration equivalence: the migrated `freezing()`
        // / window-decoration gates key on the active backend's `freeze` capability
        // instead of `!screenshot_uses_portal()`. On Linux that must be a NO-OP,
        // which holds because `ScreencopyBackend`'s freeze bit is exactly its
        // `screenshot` bit (= `image_copy_capture && output_source` =
        // `native_capture_available()`), so the capability tracks the same
        // native-vs-portal condition the boolean did. Prove it across the protocol
        // shapes the app actually sees.
        for protocols in [
            full(),
            WaylandProtocols::default(),
            WaylandProtocols { output_source: false, ..full() },
            WaylandProtocols { image_copy_capture: false, ..full() },
            WaylandProtocols { layer_shell: false, ..full() },
        ] {
            let caps = ScreencopyBackend { protocols, ffmpeg: true }.caps();
            assert_eq!(caps.freeze, caps.screenshot);
            assert_eq!(caps.capture_extras().freeze, caps.screenshot);
        }
        // The portal backend reports freeze false regardless (finished frames), so
        // an active-and-reachable portal gates freeze off exactly as before.
        assert!(!PortalBackend { available: true, ffmpeg: true }.caps().freeze);
    }

    #[test]
    fn method_choices_derive_from_backend_caps() {
        // A healthy COSMIC session: both backends offered, native first, labels
        // straight from Caps::name — the settings dropdown's exact contents.
        let both: Vec<Box<dyn CaptureBackend>> = vec![
            Box::new(ScreencopyBackend { protocols: full(), ffmpeg: true }),
            Box::new(PortalBackend { available: true, ffmpeg: true }),
        ];
        let shots = choices_from(&both, |c| c.screenshot);
        assert_eq!(shots.ids, vec![SCREENCOPY_ID, PORTAL_ID]);
        assert_eq!(shots.labels, vec!["Compositor screencopy", "PipeWire portal"]);
        assert_eq!(shots.position(PORTAL_ID), Some(1));
        assert_eq!(shots.position("sck"), None);
        // Portal unreachable: it drops out of the list; a saved portal choice has
        // no dropdown position (the page shows its fallback note instead).
        let portal_down: Vec<Box<dyn CaptureBackend>> = vec![
            Box::new(ScreencopyBackend { protocols: full(), ffmpeg: true }),
            Box::new(PortalBackend { available: false, ffmpeg: true }),
        ];
        let shots = choices_from(&portal_down, |c| c.screenshot);
        assert_eq!(shots.ids, vec![SCREENCOPY_ID]);
        assert_eq!(shots.position(PORTAL_ID), None);
        // GNOME-shaped (no screencopy protocols): the portal is the only entry.
        let gnome: Vec<Box<dyn CaptureBackend>> = vec![
            Box::new(ScreencopyBackend { protocols: WaylandProtocols::default(), ffmpeg: true }),
            Box::new(PortalBackend { available: true, ffmpeg: true }),
        ];
        assert_eq!(choices_from(&gnome, |c| c.screenshot).ids, vec![PORTAL_ID]);
        // No ffmpeg: recording has no method anywhere (the section gates on the
        // Recording capability before the dropdown renders, so empty is fine).
        let no_ffmpeg: Vec<Box<dyn CaptureBackend>> = vec![
            Box::new(ScreencopyBackend { protocols: full(), ffmpeg: false }),
            Box::new(PortalBackend { available: true, ffmpeg: false }),
        ];
        assert!(choices_from(&no_ffmpeg, |c| c.record).ids.is_empty());
    }

    #[test]
    fn any_backend_satisfies_capabilities() {
        // The deps.rs "at least one method" checks, expressed over backends: a
        // portal-only environment can still screenshot + record.
        let list = |native: bool, portal: bool, ffmpeg: bool| -> Vec<Caps> {
            let protocols = if native { full() } else { WaylandProtocols::default() };
            vec![
                ScreencopyBackend { protocols, ffmpeg }.caps(),
                PortalBackend { available: portal, ffmpeg }.caps(),
            ]
        };
        let gnome_like = list(false, true, true);
        assert!(gnome_like.iter().any(|c| c.screenshot));
        assert!(gnome_like.iter().any(|c| c.record));
        assert!(!gnome_like.iter().any(|c| c.window_capture));
        let nothing = list(false, false, true);
        assert!(!nothing.iter().any(|c| c.screenshot));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;

    #[test]
    fn mac_offers_the_single_sck_method() {
        // The whole point of DRAGON-129 on macOS: the dropdown derives a single
        // ScreenCaptureKit entry from backends(), no hardcoded label array.
        let shots = method_choices(false, true, |c| c.screenshot);
        assert_eq!(shots.ids, vec![SCK_ID]);
        assert_eq!(shots.labels, vec!["ScreenCaptureKit"]);
        // Recording gates on ffmpeg exactly like the Caps it derives from.
        assert_eq!(method_choices(false, true, |c| c.record).ids, vec![SCK_ID]);
        assert!(method_choices(false, false, |c| c.record).ids.is_empty());
        assert_eq!(native_backend_id(), SCK_ID);
    }

    #[test]
    fn sck_declares_every_capture_extra_including_wallpaper() {
        // DRAGON-186 Phase 2: ScreenCaptureKit honors the WHOLE extras set —
        // freeze / cursor / transparency / wallpaper — and is fullscreen-aware. The
        // wallpaper composite now sources the true rendered desktop via a
        // windows-excluded SCK grab, so its settings row renders and the toggle is
        // live (was declared off in Phase 1 while the composite still rendered
        // black).
        let extras = MacBackend { ffmpeg: true }.caps().capture_extras();
        assert!(extras.freeze && extras.cursor && extras.transparency);
        assert!(extras.wallpaper);
        assert!(extras.fullscreen_aware);
    }

    #[test]
    fn mac_freeze_capability_drives_the_migrated_gate() {
        // DRAGON-186 Phase 2 gate migration: `App::freezing()` /
        // `await_frozen_flats` used to AND `!screenshot_uses_portal()`, which is
        // ALWAYS true on macOS (no Wayland screencopy -> `native_capture_available`
        // false), so freeze was DEAD on mac. Post-migration those gates key on the
        // active backend's freeze capability instead, which is `true` for SCK — so
        // the capability alone re-enables freeze (gated by the user's preference).
        let caps = MacBackend { ffmpeg: true }.caps();
        assert!(caps.freeze, "SCK must declare freeze so the migrated gate lets it run");
        // The window-decoration settings block migrated to `extras.freeze` too; the
        // same true value keeps the "Single Window Aesthetics" section visible on
        // mac (it was hidden while the block keyed on `!screenshot_uses_portal()`).
        assert!(caps.capture_extras().freeze);
    }
}
