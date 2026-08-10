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
//! `app::capture_flow` / `record::pipewire`) and its stateless pixel methods
//! answer empty.
//!
//! **That is the state today, not a step on the way to something** (DRAGON-594
//! correction, worth recording rather than just deleting). This note used to say
//! the capture flow kept those branches only "until the Linux expansion ticket
//! (DRAGON-93) moves the session itself behind the trait". DRAGON-93 is DONE and
//! never carried that work: what it shipped is protocol-keyed backend selection
//! and the environment-tier model, "judge compositors by protocol, not name",
//! which is why the OTHER DRAGON-93 citations under `platform/` are accurate and
//! stay exactly as they are.
//!
//! # The open question DRAGON-594 left, and how DRAGON-595 answered it
//!
//! DRAGON-594 measured that the pixel methods have exactly ONE caller in the whole
//! tree (`cli::diagnostics::backend_test`), so this trait is a CAPABILITY
//! DECLARATION plus a diagnostic probe rather than the dispatch seam, and it handed
//! on the real question: should dispatch move behind it at all?
//!
//! **No, and the reasons are structural rather than a matter of effort.**
//! DRAGON-595 mapped every shared-tree branch that asks which backend is running
//! before touching anything. Read this before trying it a third time:
//!
//! * **The pixel branches do not key on backend identity, and must not.**
//!   `capture_flow::do_pixel_capture` and `app::recording::start_recording` both fork
//!   on `App::pw_held` (a HELD ScreenCast stream), never on the id. That is
//!   load-bearing: a grant failing with `CastError::Unavailable` proceeds with no held
//!   stream so the native path serves the capture. Keying the fork on "is the portal
//!   selected" would delete that fallback.
//! * **The portal plugin cannot serve stateless pixel calls**, for two independent
//!   reasons recorded on [`PortalBackend`]'s stubs: the pixels live in a session `App`
//!   owns across iced messages, and every native read funnels through a
//!   `connect_raw` that returns `None` on exactly the sandboxed sessions this backend
//!   exists for. It now DECLARES this as [`Acquisition::Session`] instead of leaving
//!   a caller to infer it from an empty answer.
//! * **The remaining "portal or not" branches are SESSION SHAPE, not capture**: which
//!   surface to mint, whose picker presents the target choice, which chrome and
//!   settings rows render, which metadata label is written. Those legitimately read
//!   the choice, and [`crate::app::App::active_screenshot_backend`] is now the ONE
//!   place that resolves it into an object.
//! * **The frozen-reconstruction paths are not captures.** `region_windows_frozen`,
//!   `crop_frozen` and `stitch_region` are pure `RgbaImage` math over a scene captured
//!   at launch and held by `App`. They stay in the app layer on purpose.
//!
//! What DID move behind the trait is the cursor contract ([`CursorDelivery`]), which
//! was the one place a real decision had leaked into shared code twice over.

use crate::platform::compositor::Toplevel;
use image::RgbaImage;

/// Stable ids for the built-in backends. These are PERSISTED in config.toml
/// (`screenshot_backend` / `record_backend`), so they must never be renamed.
/// [`Caps::name`] is the display label; this is the storage key.
pub const SCREENCOPY_ID: &str = "screencopy";
pub const PORTAL_ID: &str = "portal";
pub const SCK_ID: &str = "sck";
/// Windows Graphics Capture (DRAGON-229). The stable persisted id for the Windows
/// native backend, named for its planned capture API (Windows.Graphics.Capture /
/// DXGI Desktop Duplication) — never rename it (it is stored in config.toml).
pub const WGC_ID: &str = "wgc";

/// The platform's native backend id — what a saved capture-method choice falls
/// back to when it doesn't exist in this environment, and the screenshot default.
pub fn native_backend_id() -> &'static str {
    if cfg!(target_os = "linux") {
        SCREENCOPY_ID
    } else if cfg!(target_os = "windows") {
        // DRAGON-229: the `else` below assumed macOS (SCK); Windows gets its own
        // native id. Linux + macOS selection is unchanged (Linux takes the first
        // arm, macOS falls through to the `else`).
        WGC_ID
    } else {
        SCK_ID
    }
}

/// A backend-agnostic monitor description: name + geometry in the GLOBAL, top-left-origin
/// space that platform's whole app coordinate model uses (selections, window rects,
/// pointer positions, capture crops).
///
/// # The units contract — READ THIS BEFORE ADDING A PLATFORM (DRAGON-447)
///
/// The fields are named `logical_*` for their ORIGINAL home, Linux/mac, where they hold
/// points. **They are not points on every platform, and they cannot be**: what they hold
/// is whatever the platform's global coordinate space is actually defined in.
///
/// | Platform | What `output_descs()` returns | Why |
/// |---|---|---|
/// | **Linux (COSMIC)** | The compositor's LOGICAL output size + position (`wl_output`), with the buffer scale kept separately on `OutputState.scale`. | Wayland defines a global logical space; the layer surface is sized by the compositor, so nothing is seeded from these. |
/// | **macOS** | `CGDisplayBounds` POINTS. | AppKit defines a single global point space across mixed-backing-scale displays; a window seed IS points, and `platform::mac::scale_for` recovers the backing scale where pixels are needed. |
/// | **Windows** | `rcMonitor` PHYSICAL virtual-screen pixels. | Under Per-Monitor-Aware-V2 the virtual screen — `GetWindowRect`, `GetCursorPos`, `SetWindowPos`, `BitBlt`, the WGC monitor rects — is DEFINED in physical pixels, and each monitor has its OWN DPI. There is no OS-defined global point space to convert into, so physical is the only globally coherent choice. `platform::windows::scale_for` carries the per-monitor DPI for the places that need it. |
///
/// **The rule that follows from this**: anything comparing against a pointer position or a
/// window rect, or indexing captured PIXELS, consumes these values AS-IS — that is what
/// they are for, on every platform. Anything handed to iced/winit as a window SIZE or
/// POSITION is LOGICAL POINTS by definition, so on Windows it must be converted first, and
/// `platform::windows::overlay_seed_rect` is the ONE sanctioned place that does it.
///
/// The same rule holds INSIDE the overlay once it is open, where it bites much more often
/// (DRAGON-448): the overlay's iced viewport is points, so every layout, hit-test and
/// widget placement that meets this geometry has to cross. That crossing has exactly one
/// implementation — [`crate::geometry::OverlayUnits`], fed per output by
/// [`crate::platform::overlay_point_scale`]. Read its doc before adding anything that
/// mixes an `OutputDesc`/`OutputState` rect with an iced coordinate, and never open-code
/// a `* scale` next to one.
///
/// Skipping that conversion is not a cosmetic error. Seeding winit with a physical monitor
/// rect asks for a surface `dpi/96`× too large in each axis; on a customer's 3840x2160
/// display at 300% that was an 11520x6480 request against an 8192 GPU limit, and wgpu
/// ABORTS the process on an oversized `Surface::configure` rather than returning an error —
/// so every capture child died ~430ms after minting its overlays and every capture silently
/// "did nothing". A 96-DPI dev machine cannot reproduce it, because there physical and
/// logical are the same number.
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
    /// Can INCLUDE OR OMIT the pointer in what it captures, which is exactly what
    /// the "Preserve mouse cursor" toggle asks for. HOW a backend does that is its
    /// own business and deliberately invisible here: the native Linux path opens an
    /// ext-image-copy-capture cursor session for stills and passes
    /// `CaptureOptions::PaintCursors` for recordings, Windows composites
    /// `GetCursorInfo` + `GetIconInfo`, macOS asks ScreenCaptureKit, and the portal
    /// sets its stream's cursor mode (`Embedded` vs `Hidden`).
    ///
    /// This bit was called `cursor_session` until DRAGON-592, and that name was the
    /// NATIVE IMPLEMENTATION rather than the capability. It cost real behaviour: the
    /// portal backend has no sprite session, so it answered false, so
    /// [`Self::capture_extras`] reported the cursor extra off, so the settings row
    /// never rendered on a portal (Flatpak) session. Meanwhile the portal request
    /// asked for `CursorMode::Embedded` unconditionally, so a portal user got the
    /// pointer baked into EVERY capture with no way to turn it off. The colour picker
    /// paid worst: the portal's own permission dialog parks the pointer over its OK
    /// button, so that sprite landed in the picker's frozen snapshot and permanently
    /// obscured real pixels for the whole picker session.
    ///
    /// Do not re-narrow it to one backend's mechanism. If some future feature needs a
    /// REPOSITIONABLE cursor LAYER (a sprite handed to us to place where we like),
    /// that is a different fact and earns its own bit; nothing in the tree asks for
    /// one today, and `CaptureBackend::cursor` already answers `None` where no sprite
    /// exists.
    pub cursor_toggle: bool,
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
    /// Can decorate a captured SINGLE WINDOW with the user's window aesthetics:
    /// padding, the two configured borders, the drop shadow, our corner rounding,
    /// and the wallpaper-or-black backdrop. Gates the settings "Single Window
    /// Aesthetics" section. A capability only, never a toggle (each aesthetic has
    /// its own persisted knob).
    ///
    /// Its own bit since DRAGON-562: [`Self::freeze`] stood in as the section's
    /// gate while the two always coincided, and the portal backend breaks the
    /// coincidence — it decorates the finished window frame it is handed (pure
    /// `RgbaImage` math over the grabbed pixels) while still having nothing to
    /// freeze. Every native backend declares this equal to its freeze bit, so
    /// their settings gating is byte-identical to the freeze-keyed era.
    pub window_aesthetics: bool,
}

impl Caps {
    /// This backend's capture-extras capability set, ONE bit per extra. The
    /// cursor extra reads [`Self::cursor_toggle`] (can this backend include or omit
    /// the pointer at all, by whatever means) and the wallpaper extra reads
    /// [`Self::wallpaper_compose`] — NOT `wallpaper_path` — so each bit keeps a
    /// single source of truth. DRAGON-186.
    ///
    /// DRAGON-592 corrected the cursor mapping's JUSTIFICATION, not its shape. It
    /// used to read "a real sprite session is exactly what Preserve mouse cursor
    /// needs", and that sentence is what kept the toggle off every portal session.
    /// The toggle needs the pointer to be includable or omittable; a sprite session
    /// is one way to achieve that, not the requirement.
    pub fn capture_extras(&self) -> CaptureExtras {
        CaptureExtras {
            freeze: self.freeze,
            cursor: self.cursor_toggle,
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

/// HOW a backend puts the pointer into a capture (DRAGON-595). [`Caps::cursor_toggle`]
/// says a backend can include or omit the pointer at all; this says by what means, and
/// the two answers are not interchangeable.
///
/// It is an enum rather than a flag on purpose, and that is the whole point of the type.
/// The two mechanisms differ in WHEN the pointer is decided and WHERE it comes from, so
/// no boolean can carry both without one backend lying:
///
/// * [`Self::Sprite`] decides at COMPOSE time, from a sprite the backend itself hands
///   over ([`CaptureBackend::cursor`]). The native paths lock that sprite at LAUNCH
///   deliberately (DRAGON-214): the compositor would otherwise stamp the pointer where
///   it sits at capture time, which after an overlay teardown is usually over our own
///   toolbar, and the launch-locked sprite is what matches the on-overlay indicator.
///   `screenshot::output`'s doc says the same thing from the other side.
/// * [`Self::InStream`] decides at REQUEST time, before any pixels exist. The portal's
///   ScreenCast stream is asked for `CursorMode::Embedded` or `Hidden`
///   (`platform::screencast::CursorRequest`, DRAGON-592) and the frames arrive already
///   made. There is no sprite to reposition and never will be, so
///   [`CaptureBackend::cursor`] answers `None` there honestly.
///
/// What the shared tree gets from this: the "does this capture want the pointer at all"
/// rule is ONE predicate ([`crate::app::cursor_wanted`]) instead of one copy per
/// mechanism. Before DRAGON-595 those were two separate pure fns,
/// `app::launch_cursor_needed` and `app::portal::cursor_request`, computing the
/// identical `want && !picker` rule in two files and held together only by a test
/// asserting they agreed. Naming the MECHANISM as its own type is what made the shared
/// rule extractable: what differed between the two copies was never the rule.
///
/// # Why this does not gate the cursor stamp, though it looks like it should
///
/// DRAGON-595 tried keying `do_pixel_capture`'s sprite stamp on this, to make the
/// portal fallback's double-pointer hazard unrepresentable (that path crops a seed
/// frame which already carries the stream's pointer). It is the wrong predicate and the
/// attempt is recorded so it is not retried: **the backend SELECTED is not always the
/// backend SERVING.** With layer shell present, the portal chosen, and the grant failing
/// `CastError::Unavailable`, the capture degrades to native screencopy while
/// `App::active_screenshot_backend` still answers Portal, so gating on it drops the
/// cursor from a capture that had one. "Did the frozen scene come from the portal" is
/// `App::overlay_fallback_active`, and that is what the frozen-source rule already uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorDelivery {
    /// The backend hands back a sprite ([`CaptureBackend::cursor`]) and the capture
    /// pipeline stamps it onto the finished pixels.
    Sprite,
    /// The capture session bakes the pointer in; the REQUEST carries the choice and
    /// the backend has no sprite to give.
    // Constructed only by `PortalBackend`, which is Linux-only, so this variant is
    // honestly dead on macOS and Windows. It stays in the shared enum because the
    // TYPE is what makes the two mechanisms non-substitutable, and a platform that
    // grows a session-driven capture path (a future portal-shaped backend) needs the
    // variant to already mean something rather than inventing a second vocabulary.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    InStream,
}

/// WHERE a backend's pixels come from when something asks it for a capture
/// (DRAGON-595). This is what decides whether the stateless pixel methods on
/// [`CaptureBackend`] can mean anything, and it is declared rather than left for a
/// caller to infer from `None`.
///
/// It is not a capability and it does not gate a feature. [`Caps::screenshot`] still
/// says truthfully that a [`Self::Session`] backend can screenshot, because it can:
/// the app drives it through a granted session every day. The two answers are about
/// different questions, and conflating them is what made `--test backend` print an
/// empty monitor list for a working portal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acquisition {
    /// A grab can be issued at any moment with nothing held: the backend opens its
    /// own connection, takes the pixels and returns them. Every compositor-direct and
    /// OS-API backend (screencopy, ScreenCaptureKit, Windows Graphics Capture).
    OnDemand,
    /// Pixels exist only inside a session the APP holds: a user permission grant plus
    /// a live file descriptor, negotiated over several messages and consumed once.
    /// The xdg-desktop-portal ScreenCast path. Its stateless pixel methods answer
    /// empty, and that is the honest answer rather than a missing implementation, for
    /// the reasons on [`PortalBackend`]'s stubs.
    // Dead off Linux for the same reason as `CursorDelivery::InStream`: the only
    // session-driven backend in the tree is the portal. Kept for the same reason too,
    // so `--test backend` on a future session-driven platform reports it rather than
    // printing a capable backend with nothing behind it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Session,
}

/// One capture implementation (compositor protocol family or OS API family).
pub trait CaptureBackend {
    /// The backend's stable identifier (see the `*_ID` constants) — the value the
    /// persisted capture-method settings store, so it must never change.
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;
    /// How this backend honours a capture that WANTS the pointer (DRAGON-595). Only
    /// meaningful where [`Caps::cursor_toggle`] is true; a backend that cannot include
    /// or omit the pointer still has to name a mechanism, and names the one it would
    /// use. See [`CursorDelivery`].
    fn cursor_delivery(&self) -> CursorDelivery;
    /// Whether the stateless pixel methods below can serve, or this backend only
    /// captures through a session the app holds (DRAGON-595). See [`Acquisition`].
    /// A [`Acquisition::Session`] backend answers empty from `outputs`,
    /// `screenshot_output`, `list_windows` and `screenshot_window`, and this is how a
    /// caller tells that apart from a failure.
    fn acquisition(&self) -> Acquisition;
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
/// family, so any compositor implementing these globals (COSMIC, wlroots ≥0.19 —
/// Sway 1.11+, Hyprland 0.52+, niri, KWin 6.6+, Mutter 49.2+, …) runs the native
/// backend unchanged. The protocols landed in wlroots 0.19, not 0.18: Sway 1.11
/// is the first release carrying them.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaylandProtocols {
    /// `ext_image_copy_capture_manager_v1` — frame + cursor capture sessions.
    pub image_copy_capture: bool,
    /// `ext_output_image_capture_source_manager_v1` — whole-monitor sources.
    pub output_source: bool,
    /// `ext_foreign_toplevel_image_capture_source_manager_v1` — per-window sources.
    pub toplevel_source: bool,
    /// `ext_foreign_toplevel_list_v1` — the upstream toplevel LIST, and nothing more:
    /// a handle, a title, an app id, a stable identifier. No geometry, no state.
    ///
    /// This used to be true for `zcosmic_toplevel_info_v1` as well, and that OR is
    /// exactly what made [`Caps::window_list`] lie on wlroots (DRAGON-620). Keep the
    /// three toplevel flags separate: they are three different protocols answering
    /// three different questions, and the window picker needs all three.
    pub toplevel_list: bool,
    /// `zcosmic_toplevel_info_v1` — the COSMIC EXTENSION to the list above, and the
    /// only source of a toplevel's geometry, state, output and workspace. cctk marks
    /// all four "Requires zcosmic_toplevel_info_v1 version 2" (version 3 for the
    /// workspace), and leaves them empty otherwise.
    ///
    /// The window picker is built entirely from geometry, so without this the list
    /// enumerates fine and yields nothing placeable.
    pub cosmic_toplevel_info: bool,
    /// `zcosmic_toplevel_manager_v1` — COSMIC's toplevel ACTIVATION and move manager,
    /// behind `compositor::activate`, `activate_until` and `move_toplevel_to_output`.
    ///
    /// It has no upstream equivalent we speak. cctk's `ToplevelManagerState::new`
    /// UNWRAPS its bind, so a missing global is an abort rather than a degrade, which
    /// is why this earns a probe flag of its own rather than riding on the list.
    pub cosmic_toplevel_manager: bool,
    /// `zwlr_layer_shell_v1` — real overlay surfaces (vs plain windows).
    pub layer_shell: bool,
    /// `zwlr_data_control_manager_v1` or `ext_data_control_manager_v1` — the clipboard write
    /// that OUTLIVES the writing process, which is the only kind our copy-then-exit model can
    /// use (`wl-clipboard-rs` binds one of these).
    ///
    /// Either name counts: they are the same capability, the `ext` one being the standardised
    /// successor, and a compositor may advertise one, the other, or both.
    pub data_control: bool,
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
                "ext_foreign_toplevel_list_v1" => p.toplevel_list = true,
                "zcosmic_toplevel_info_v1" => p.cosmic_toplevel_info = true,
                "zcosmic_toplevel_manager_v1" => p.cosmic_toplevel_manager = true,
                "zwlr_layer_shell_v1" => p.layer_shell = true,
                "zwlr_data_control_manager_v1" | "ext_data_control_manager_v1" => {
                    p.data_control = true;
                }
                _ => {}
            }
        }
    });
    Some(p)
}

/// Whether this protocol set can produce a USABLE window list: one that names windows
/// AND places them, which is the only kind the picker grid, the per-window grab and
/// `--active-window` can do anything with.
///
/// Pure; unit-tested in `window_list_honesty_tests`.
///
/// # Why all three flags, when "list" sounds like one
///
/// DRAGON-620. This answered `toplevel_list` alone, and the probe set that flag from
/// `ext_foreign_toplevel_list_v1` OR `zcosmic_toplevel_info_v1`. wlroots >= 0.18
/// advertises the first, so every wlroots session (Sway, Hyprland, niri, KWin) was told
/// it had a window list. It does not. cctk fills geometry, state, output and workspace
/// only from `zcosmic_toplevel_info_v1` v2+, so `compositor::list_toplevels` enumerated
/// the windows, found every `geometry` map empty, and returned nothing. The capability
/// said yes and the feature returned an empty grid, which is the exact shape of lie this
/// predicate exists to prevent: a capability that lies is worse than one that is false.
///
/// The MANAGER is in here for a blunter reason. Our four toplevel entry points share one
/// `WaylandState`, and cctk's `ToplevelManagerHandler` demands a non-optional
/// `&mut ToplevelManagerState`, so all four construct a manager whether or not they use
/// it. Requiring it keeps ONE predicate behind both the capability and the plugin guard,
/// which is what stops the two drifting apart again. It costs nothing real: cosmic-comp
/// ships the info and manager globals together, so no session advertises one without the
/// other, and a session that somehow did would be under-reported rather than lied to.
///
/// Naming three protocols is still PROTOCOL-keyed, never desktop-keyed. Nothing here asks
/// what the desktop calls itself, and a non-COSMIC compositor that implemented these
/// globals tomorrow would get the window picker with no change here.
#[cfg(target_os = "linux")]
pub fn window_list_supported(p: &WaylandProtocols) -> bool {
    p.toplevel_list && p.cosmic_toplevel_info && p.cosmic_toplevel_manager
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
            window_list: window_list_supported(&p),
            // The per-window PIXEL grab is pure upstream (an `ext` toplevel handle fed to
            // the `ext` capture-source manager), but a window nobody can pick is a window
            // nobody can capture, so this rides the same honest list answer.
            window_capture: p.image_copy_capture && p.toplevel_source && window_list_supported(&p),
            // Both halves hang off the capture manager + an output source: the
            // still path's cursor session, and the recorder's PaintCursors option.
            cursor_toggle: screenshot,
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
            // Window state comes off `ToplevelInfo::state`, which cctk fills only from
            // `zcosmic_toplevel_info_v1` v2+, so this asks the same question as the list.
            fullscreen_aware: window_list_supported(&p),
            // The aesthetics ride the native window compose, available whenever this
            // backend can screenshot — the same expression as `freeze`, so the
            // settings section's gating is byte-identical to the freeze-keyed era.
            window_aesthetics: screenshot,
        }
    }

    /// The ext-image-copy-capture cursor session hands back a real sprite with real
    /// alpha, which the still paths stamp themselves; the recorder's `PaintCursors`
    /// option is the same include-or-omit choice expressed to the compositor. Both are
    /// the SPRITE mechanism from the shared tree's point of view: the pointer is
    /// decided against pixels we already hold, not asked for before they exist.
    fn cursor_delivery(&self) -> CursorDelivery {
        CursorDelivery::Sprite
    }

    /// A screencopy grab opens its own Wayland connection and takes pixels there and
    /// then; nothing has to be held between calls.
    fn acquisition(&self) -> Acquisition {
        Acquisition::OnDemand
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

    /// The per-extra capability table (DRAGON-562). The portal hands back a
    /// FINISHED frame, but most of the single-window pipeline is pure `RgbaImage`
    /// math that runs on any frame — so the bits are decided one by one, not
    /// declared all-false as a family:
    ///
    /// - `freeze` FALSE: a live portal frame arrives at capture time; there is no
    ///   launch-instant scene to reconstruct from.
    /// - `cursor_toggle` TRUE since DRAGON-592: the ScreenCast stream's cursor
    ///   mode IS this capability. `CursorMode::Embedded` bakes the pointer into
    ///   the frames, `CursorMode::Hidden` leaves it out, and the request picks
    ///   between them from the user's preference
    ///   (`platform::screencast::cursor_mode`). The bit read FALSE while it was
    ///   named `cursor_session` and meant "has a repositionable sprite SESSION",
    ///   which the portal genuinely does not have and never will: its modes are
    ///   include-or-omit, never reposition. That was the wrong question to ask on
    ///   behalf of a user-facing toggle, and the wrong answer cost every portal
    ///   user a pointer baked into every capture with no way to turn it off. See
    ///   [`Caps::cursor_toggle`] for the whole account.
    /// - `transparency` FALSE pending the DRAGON-562 alpha probe: `convert_crop`
    ///   forces every pixel opaque today, and whether the compositor would even
    ///   negotiate an alpha format (BGRA vs BGRx) for a Window stream is
    ///   unmeasured. Flip only on a measured yes
    ///   (`CCK_ALPHA_PROBE=1 --test pw window`).
    /// - `wallpaper_compose` TRUE: the SAME wallpaper compose the native path
    ///   uses (`wallpaper_crop` via the native window composite) puts the
    ///   desktop behind the decorated window. The original premise — a Window
    ///   grant's `StreamInfo.position` is the window's global position — is
    ///   FALSE on COSMIC's portal (it sends `position: None` for every window
    ///   stream; only monitor streams are positioned), so the crop anchors at
    ///   `capture_flow::synthetic_window_anchor` (centered on the largest
    ///   registered output) instead of a real position. For region/monitor
    ///   frames the wallpaper is baked into the pixels either way; the toggle's
    ///   OFF state is honored exactly where this backend composites at all
    ///   (single-window stills — it has no per-window pixels to subtract a
    ///   wallpaper from anywhere else).
    /// - `fullscreen_aware` TRUE: the grant position + the stream size against
    ///   the registered output's geometry feed the same `is_fullscreen` rule the
    ///   native path uses (`capture_flow::portal_window_fullscreen`), so a
    ///   truly-fullscreen window keeps its bare frame.
    /// - `window_aesthetics` TRUE: padding, both user-configured borders, the
    ///   drop shadow and our corner rounding are pure image math over the
    ///   finished frame (the native `WindowCaptureJob` runs on it unchanged).
    ///   Frosted glass stays structurally absent: reproducing it needs the scene
    ///   BEHIND the window, which the portal cannot provide.
    fn caps(&self) -> Caps {
        Caps {
            name: "PipeWire portal",
            screenshot: self.available,
            record: self.available && self.ffmpeg,
            // The portal has its own window picker dialog; it can't enumerate
            // windows INTO our grid, and gives no standalone per-window grab.
            window_list: false,
            window_capture: false,
            // The stream's cursor mode is include-or-omit, which is the whole bit
            // (DRAGON-592). Gated on `available` like every other portal bit: an
            // unreachable portal honours no preference at all.
            cursor_toggle: self.available,
            layer_overlay: false,
            // The wallpaper FILE ladder reads desktop config, not a capture
            // protocol, so it resolves the same whichever backend grabs pixels
            // (measured in-sandbox: the host config is bind-mounted readable).
            wallpaper_path: crate::wallpaper::detect().is_some(),
            freeze: false,
            transparency: false,
            wallpaper_compose: self.available,
            fullscreen_aware: self.available,
            window_aesthetics: self.available,
        }
    }

    /// The ScreenCast stream's cursor mode IS this backend's mechanism, and it is
    /// chosen when the session is REQUESTED, before a single frame exists
    /// (`platform::screencast::request` takes a `CursorRequest`). There is no sprite
    /// to hand back, which is why [`Self::cursor`] answers `None` rather than
    /// pretending, and why a flag could never have carried both backends' contract.
    fn cursor_delivery(&self) -> CursorDelivery {
        CursorDelivery::InStream
    }

    /// The whole reason the stubs below are empty. See [`Acquisition::Session`].
    fn acquisition(&self) -> Acquisition {
        Acquisition::Session
    }

    // Portal capture is SESSION-DRIVEN, and there is no stateless "grab now" call to
    // delegate to. Dispatch stays in `capture_flow` / `record::pipewire`, and that is
    // the shipped design rather than a waypoint: this comment used to name DRAGON-93
    // as the ticket that would move it behind this trait, and DRAGON-93 is done and
    // never carried that work (DRAGON-594 correction). DRAGON-595 then measured that
    // it is structural rather than unfinished, so the stubs below are the honest
    // answer and not a placeholder. Two independent reasons, either one sufficient:
    //
    // 1. The pixels only exist inside a granted ScreenCast session: an `OwnedFd` plus
    //    a PipeWire node id, obtained through a user permission dialog over several
    //    iced messages and owned by `App` (`HeldStream`) across a countdown. A
    //    `&self` method on a value type minted per call cannot reach it, and moving
    //    the session onto the backend would mean the backend outliving the messages
    //    that build it.
    // 2. This plugin cannot enumerate anything on its own either. Every native read
    //    funnels through `screencopy::connect_raw`, which returns `None` unless the
    //    compositor advertises `ext_image_copy_capture` + the toplevel list. On the
    //    session this backend exists to serve (a Flatpak, where cosmic-comp hides
    //    those globals from a security-context client) that is exactly the case, so
    //    delegating `outputs()` to `crate::screenshot::output_descs()` would return
    //    an empty vec while LOOKING implemented. The App's own output list comes from
    //    libcosmic's Wayland connection, which is app state, not a plugin capability.
    //
    // So the dispatch keys on the HELD STREAM (`App::pw_held`), never on the backend
    // id, in `capture_flow::do_pixel_capture` and `app::recording::start_recording`.
    // That is deliberate and load-bearing, not an oversight: a grant that fails with
    // `CastError::Unavailable` proceeds with no held stream and the native path
    // serves the capture (`app::portal::on_pipewire_cast_ready`). Re-keying either
    // branch on "is the portal selected" would delete that fallback.
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
            // SCK decides whether the pointer is in the shot; unchanged by the
            // DRAGON-592 rename (it was true as `cursor_session` too).
            cursor_toggle: true,
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
            // Same value as `freeze`, so the settings section's gating is
            // byte-identical to the freeze-keyed era (DRAGON-562).
            window_aesthetics: true,
        }
    }
    /// `NSCursor` hands back a real sprite ([`Self::cursor`] via
    /// `crate::screenshot::capture_cursor`), stamped at compose time like every other
    /// native backend. SCK's own "include the pointer" stream option is the recorder's
    /// half of the same include-or-omit choice.
    fn cursor_delivery(&self) -> CursorDelivery {
        CursorDelivery::Sprite
    }
    /// `SCScreenshotManager` grabs on demand; nothing is held between calls.
    fn acquisition(&self) -> Acquisition {
        Acquisition::OnDemand
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

/// Windows: the single Windows-Graphics-Capture backend (`portal_available` is
/// Linux-only). `WindowsBackend`'s impl lives under `platform/windows/` per the strict
/// closed split; this is the one-line dispatch that registers it. DRAGON-229.
#[cfg(target_os = "windows")]
pub fn backends(_portal_available: bool, ffmpeg: bool) -> Vec<Box<dyn CaptureBackend>> {
    vec![Box::new(crate::platform::windows::backend::WindowsBackend { ffmpeg })]
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

    /// The dropdown index the method picker DISPLAYS for the persisted `selected`
    /// id (DRAGON-575): its own position when this session offers it, otherwise the
    /// method that will actually serve, which is the FIRST offered method. "First"
    /// is not a guess: [`backends`] lists methods in preference order (native
    /// first, then the portal), and the dispatch clamp
    /// (`App::screenshot_uses_portal` / `recording_uses_portal`) sends a session
    /// whose saved method can't apply to exactly that survivor: a session without
    /// the native protocols offers only the portal, and a session whose portal is
    /// unreachable offers only native. Two rules fall out: a persisted id naming an
    /// unavailable method shows the serving method instead of an empty dropdown,
    /// and a single-entry list is selected by definition. `None` only for an empty
    /// list (the settings section gates on the capability before rendering).
    ///
    /// DISPLAY-only, mirroring DRAGON-571's encoder rule: the persisted intent is
    /// never rewritten by what this shows. The Flatpak sandbox is the motivating
    /// case: its config carries the native default "screencopy" while the
    /// compositor's security context hides the screencopy protocols from sandboxed
    /// clients, so the old strict `position` match rendered a one-entry dropdown
    /// with nothing selected.
    ///
    /// Pure and unit-tested (`display_resolution_tests`).
    pub fn display_position(&self, selected: &str) -> Option<usize> {
        self.position(selected).or_else(|| (!self.ids.is_empty()).then_some(0))
    }

    /// The backend id [`Self::display_position`] lands on, the method the picker
    /// shows and the session actually serves. `None` only for an empty list.
    ///
    /// Pure and unit-tested (`display_resolution_tests`).
    pub fn display_id(&self, selected: &str) -> Option<&'static str> {
        self.display_position(selected).map(|i| self.ids[i])
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
            cursor_toggle: extras.cursor,
            layer_overlay: false,
            wallpaper_path,
            freeze: extras.freeze,
            transparency: extras.transparency,
            wallpaper_compose: extras.wallpaper,
            fullscreen_aware: extras.fullscreen_aware,
            // Mirrors freeze, as every native backend declares it (DRAGON-562).
            window_aesthetics: extras.freeze,
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
        // The cursor extra is cursor_toggle (DRAGON-592: can this backend include
        // or omit the pointer, whatever mechanism it uses to do it).
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

/// DRAGON-575: the method picker's display-resolution rule, pinned platform-free
/// over hand-built lists (the rule reads only `MethodChoices`, never a live probe).
/// Two shapes matter: the SANDBOX shape (a portal-only list, because the
/// compositor's security context hides the screencopy protocols from Flatpak
/// clients) and the NATIVE shape (both methods offered).
#[cfg(test)]
mod display_resolution_tests {
    use super::*;

    /// The Flatpak sandbox's list: the portal is the only offered method.
    fn portal_only() -> MethodChoices {
        MethodChoices { ids: vec![PORTAL_ID], labels: vec!["PipeWire portal"] }
    }

    /// A healthy native session's list: native screencopy first, then the portal.
    fn both() -> MethodChoices {
        MethodChoices {
            ids: vec![SCREENCOPY_ID, PORTAL_ID],
            labels: vec!["Compositor screencopy", "PipeWire portal"],
        }
    }

    #[test]
    fn sandbox_shape_resolves_the_native_default_to_the_portal() {
        // THE DRAGON-575 bug: the sandbox config persists the schema default
        // "screencopy" while only the portal is offered. The strict match rendered
        // no selection; the display rule shows the method that will really serve.
        let m = portal_only();
        assert_eq!(m.position(SCREENCOPY_ID), None, "the strict match really is empty");
        assert_eq!(m.display_position(SCREENCOPY_ID), Some(0));
        assert_eq!(m.display_id(SCREENCOPY_ID), Some(PORTAL_ID));
    }

    #[test]
    fn a_single_option_list_is_selected_by_definition() {
        // Whatever the persisted intent says (the offered id itself, an id this
        // session lacks, or garbage), a one-entry list has exactly one method that
        // can serve, so it is always the selection.
        let m = portal_only();
        for intent in [PORTAL_ID, SCREENCOPY_ID, SCK_ID, WGC_ID, "bogus", ""] {
            assert_eq!(m.display_position(intent), Some(0), "intent {intent:?}");
            assert_eq!(m.display_id(intent), Some(PORTAL_ID), "intent {intent:?}");
        }
    }

    #[test]
    fn native_shape_shows_an_offered_intent_verbatim() {
        // Both methods offered: every offered intent resolves to itself, so the
        // native multi-method dropdown is byte-identical to the strict-match era.
        let m = both();
        assert_eq!(m.display_position(SCREENCOPY_ID), Some(0));
        assert_eq!(m.display_id(SCREENCOPY_ID), Some(SCREENCOPY_ID));
        assert_eq!(m.display_position(PORTAL_ID), Some(1));
        assert_eq!(m.display_id(PORTAL_ID), Some(PORTAL_ID));
    }

    #[test]
    fn native_shape_resolves_an_unoffered_intent_to_the_first_offered() {
        // An intent this session can't serve (a mac config's "sck" read on Linux,
        // or a hand-edited id) falls to the FIRST offered method, the same
        // native-first preference order `backends()` declares and the dispatch
        // clamp falls back to.
        let m = both();
        assert_eq!(m.display_id(SCK_ID), Some(SCREENCOPY_ID));
        assert_eq!(m.display_id("bogus"), Some(SCREENCOPY_ID));
    }

    #[test]
    fn portal_down_shape_resolves_a_portal_intent_to_native() {
        // The portal-unreachable session offers only native; a saved portal choice
        // used to render no selection here too. The dropdown now shows the
        // compositor method that actually serves (the section's warn note still
        // names the fallback, keyed on the INTENT).
        let m = MethodChoices {
            ids: vec![SCREENCOPY_ID],
            labels: vec!["Compositor screencopy"],
        };
        assert_eq!(m.display_position(PORTAL_ID), Some(0));
        assert_eq!(m.display_id(PORTAL_ID), Some(SCREENCOPY_ID));
    }

    #[test]
    fn an_empty_list_resolves_to_nothing() {
        // No method at all (the section gates on the capability before rendering
        // the dropdown, so this is honesty, not a reachable UI state).
        let m = MethodChoices::default();
        assert_eq!(m.display_position(SCREENCOPY_ID), None);
        assert_eq!(m.display_id(PORTAL_ID), None);
    }

    /// The sandbox shape derived from the REAL backends rather than a literal:
    /// no advertised screencopy protocols + a reachable portal is exactly what a
    /// Flatpak session's `choices_from` produces, and the persisted native
    /// default resolves to the portal on it.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_sandbox_backend_list_resolves_like_the_literal() {
        let sandbox: Vec<Box<dyn CaptureBackend>> = vec![
            Box::new(ScreencopyBackend {
                protocols: WaylandProtocols::default(),
                ffmpeg: true,
            }),
            Box::new(PortalBackend { available: true, ffmpeg: true }),
        ];
        let shots = choices_from(&sandbox, |c| c.screenshot);
        assert_eq!(shots.ids, vec![PORTAL_ID]);
        assert_eq!(shots.display_id(SCREENCOPY_ID), Some(PORTAL_ID));
        assert_eq!(shots.display_position(SCREENCOPY_ID), Some(0));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A full protocol set: ext-image-copy-capture PLUS the two COSMIC toplevel
    /// extensions, i.e. what cosmic-comp advertises. Modern wlroots matches the `ext`
    /// half of this and none of the cosmic half; see [`wlroots_shaped`].
    pub(super) fn full() -> WaylandProtocols {
        WaylandProtocols {
            image_copy_capture: true,
            output_source: true,
            toplevel_source: true,
            toplevel_list: true,
            cosmic_toplevel_info: true,
            cosmic_toplevel_manager: true,
            layer_shell: true,
            data_control: true,
        }
    }

    /// A modern wlroots session (Sway 1.11+, Hyprland, niri): the whole upstream `ext`
    /// capture family and the upstream toplevel LIST, but neither COSMIC extension.
    pub(super) fn wlroots_shaped() -> WaylandProtocols {
        WaylandProtocols {
            cosmic_toplevel_info: false,
            cosmic_toplevel_manager: false,
            ..full()
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
        assert!(caps.screenshot && caps.record && caps.window_capture && caps.cursor_toggle);
        assert!(caps.layer_overlay);
        // A compositor with capture but NO layer shell (or vice versa) reports
        // each capability independently.
        let partial = ScreencopyBackend {
            protocols: WaylandProtocols { layer_shell: false, ..full() },
            ffmpeg: true,
        };
        assert!(partial.caps().screenshot && !partial.caps().layer_overlay);
        // KDE-shaped: layer shell and the upstream toplevel list, without
        // ext-image-copy-capture. The window list is honestly FALSE here (DRAGON-620):
        // KWin advertises `ext_foreign_toplevel_list_v1` but neither COSMIC extension, so
        // it can name windows and place none of them. This assertion read `window_list`
        // TRUE until the protocol flags were split apart.
        let kde = ScreencopyBackend {
            protocols: WaylandProtocols {
                layer_shell: true,
                toplevel_list: true,
                ..Default::default()
            },
            ffmpeg: true,
        };
        assert!(!kde.caps().screenshot && kde.caps().layer_overlay && !kde.caps().window_list);
        assert!(!kde.caps().window_capture);
    }

    #[test]
    fn portal_is_capture_only() {
        // The portal can screenshot/record but brings no window grid and no layer
        // shell, so features must gate off these. It DOES honour the cursor toggle
        // (its stream cursor mode; DRAGON-592), which is pinned in
        // `portal_extras_tests`. The wallpaper FILE resolution is desktop-config,
        // not capture-protocol, so it matches the ladder's live answer
        // (DRAGON-562), same as the native backend.
        let caps = PortalBackend { available: true, ffmpeg: true }.caps();
        assert!(caps.screenshot && caps.record);
        assert!(!caps.window_list && !caps.window_capture);
        assert!(!caps.layer_overlay);
        assert_eq!(caps.wallpaper_path, crate::wallpaper::detect().is_some());
    }

    #[test]
    fn native_declares_every_capture_extra() {
        // DRAGON-186: the capture-extra toggles exist only where the backend can
        // honor them. Native screencopy (full protocol set) supports the whole set.
        let native = ScreencopyBackend { protocols: full(), ffmpeg: true }.caps().capture_extras();
        assert!(native.freeze && native.cursor && native.transparency && native.wallpaper);
        assert!(native.fullscreen_aware);
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

/// DRAGON-562: the portal backend's per-extra capability table, one pin per bit.
/// This is what replaced the all-false family verdict; each assertion names the
/// reason the bit holds its value (see `PortalBackend::caps`'s rustdoc).
#[cfg(all(test, target_os = "linux"))]
mod portal_extras_tests {
    use super::*;

    fn portal() -> Caps {
        PortalBackend { available: true, ffmpeg: true }.caps()
    }

    // The two FALSE bits, each false for its own documented reason, never
    // again as "the portal supports no extras" in one stroke.
    #[test]
    fn freeze_and_transparency_stay_off() {
        let caps = portal();
        // A live portal frame can't be "frozen": there is no launch scene.
        assert!(!caps.freeze);
        // Pending the alpha probe: convert_crop forces alpha opaque today.
        assert!(!caps.transparency);
        let extras = caps.capture_extras();
        assert!(!extras.freeze && !extras.transparency);
    }

    // DRAGON-592: the cursor bit is ON. The stream's cursor mode is exactly the
    // capability ("include or omit the pointer"), so the "Preserve mouse cursor"
    // row renders on a portal session and the request honours it. It read FALSE
    // while the bit was named `cursor_session` and asked the wrong question (does
    // this backend hand back a repositionable SPRITE), which is why a portal user
    // got the pointer baked into every capture with no way to turn it off.
    #[test]
    fn the_cursor_toggle_is_honoured_through_the_stream_cursor_mode() {
        let caps = portal();
        assert!(caps.cursor_toggle);
        assert!(caps.capture_extras().cursor, "the settings row renders on a portal session");
        // And it is not a blanket true: an unreachable portal honours nothing.
        assert!(!PortalBackend { available: false, ffmpeg: true }.caps().cursor_toggle);
    }

    // The TRUE bits: the window grant carries a global position, so the
    // wallpaper backdrop lands with the correct region, the fullscreen rule has
    // its geometry inputs, and the aesthetics are pure image math on the frame.
    #[test]
    fn wallpaper_fullscreen_and_aesthetics_are_on() {
        let caps = portal();
        assert!(caps.wallpaper_compose);
        assert!(caps.fullscreen_aware);
        assert!(caps.window_aesthetics);
        let extras = caps.capture_extras();
        assert!(extras.wallpaper && extras.fullscreen_aware);
    }

    // The aesthetics bit genuinely DIVERGES from freeze here — the whole reason
    // it exists as its own capability (the native backends keep them equal).
    #[test]
    fn aesthetics_no_longer_ride_the_freeze_bit() {
        let caps = portal();
        assert!(caps.window_aesthetics && !caps.freeze);
        let native = ScreencopyBackend {
            protocols: WaylandProtocols {
                image_copy_capture: true,
                output_source: true,
                toplevel_source: true,
                toplevel_list: true,
                cosmic_toplevel_info: true,
                cosmic_toplevel_manager: true,
                layer_shell: true,
                data_control: true,
            },
            ffmpeg: true,
        }
        .caps();
        assert_eq!(native.window_aesthetics, native.freeze);
    }

    // An unreachable portal declares nothing, exactly like its screenshot bit.
    #[test]
    fn an_unavailable_portal_declares_nothing() {
        let caps = PortalBackend { available: false, ffmpeg: true }.caps();
        assert!(!caps.screenshot);
        assert!(!caps.wallpaper_compose && !caps.fullscreen_aware && !caps.window_aesthetics);
        assert!(!caps.cursor_toggle);
    }
}

/// DRAGON-595: the cursor CONTRACT, one pin per backend. The point of the enum is
/// that the two mechanisms are not substitutable, so the pins are about which
/// backend answers which, and about the invariant tying the answer to the sprite
/// method.
#[cfg(all(test, target_os = "linux"))]
mod cursor_delivery_tests {
    use super::*;

    /// A full ext-image-copy-capture protocol set, as `tests::full()` builds it.
    fn native() -> ScreencopyBackend {
        ScreencopyBackend {
            protocols: WaylandProtocols {
                image_copy_capture: true,
                output_source: true,
                toplevel_source: true,
                toplevel_list: true,
                cosmic_toplevel_info: true,
                cosmic_toplevel_manager: true,
                layer_shell: true,
                data_control: true,
            },
            ffmpeg: true,
        }
    }

    // The headline split: the two Linux backends genuinely differ, which is why a
    // bool could not carry both. Native decides against pixels it holds; the portal
    // decides before any pixel exists.
    #[test]
    fn the_two_linux_backends_deliver_the_pointer_differently() {
        assert_eq!(native().cursor_delivery(), CursorDelivery::Sprite);
        assert_eq!(
            PortalBackend { available: true, ffmpeg: true }.cursor_delivery(),
            CursorDelivery::InStream,
        );
    }

    // The invariant that keeps the enum honest: only a Sprite backend may claim to
    // hand a sprite over. An `InStream` backend answering `Some` from `cursor()`
    // would mean the shared tree could stamp a pointer the stream ALSO baked in,
    // which is the double-pointer bug this typing exists to make unrepresentable.
    #[test]
    fn only_a_sprite_backend_offers_a_sprite() {
        let portal = PortalBackend { available: true, ffmpeg: true };
        assert_eq!(portal.cursor_delivery(), CursorDelivery::InStream);
        assert!(portal.cursor().is_none(), "an in-stream backend has no sprite to give");
    }

    // The mechanism is a property of the BACKEND, not of whether this session can
    // use it. An unreachable portal still delivers in-stream (it has no other way);
    // a protocol-less screencopy backend still delivers by sprite. The
    // can-it-at-all question is `Caps::cursor_toggle`, and it is separate.
    #[test]
    fn the_mechanism_does_not_track_the_capability() {
        let dead_portal = PortalBackend { available: false, ffmpeg: true };
        assert!(!dead_portal.caps().cursor_toggle);
        assert_eq!(dead_portal.cursor_delivery(), CursorDelivery::InStream);
        let bare = ScreencopyBackend { protocols: WaylandProtocols::default(), ffmpeg: true };
        assert!(!bare.caps().cursor_toggle);
        assert_eq!(bare.cursor_delivery(), CursorDelivery::Sprite);
    }
}

/// DRAGON-595: the acquisition model, and the ONE invariant that makes the empty
/// stateless answers honest rather than unfinished.
#[cfg(all(test, target_os = "linux"))]
mod acquisition_tests {
    use super::*;

    fn native() -> ScreencopyBackend {
        ScreencopyBackend {
            protocols: WaylandProtocols {
                image_copy_capture: true,
                output_source: true,
                toplevel_source: true,
                toplevel_list: true,
                cosmic_toplevel_info: true,
                cosmic_toplevel_manager: true,
                layer_shell: true,
                data_control: true,
            },
            ffmpeg: true,
        }
    }

    // The split the whole ticket turned on: one Linux backend grabs on demand, the
    // other only inside a grant the app holds.
    #[test]
    fn only_the_portal_is_session_driven() {
        assert_eq!(native().acquisition(), Acquisition::OnDemand);
        assert_eq!(
            PortalBackend { available: true, ffmpeg: true }.acquisition(),
            Acquisition::Session,
        );
    }

    // The invariant: a Session backend answers EMPTY from every stateless pixel
    // method, and that is a contract, not an accident. `--test backend` reads
    // `acquisition()` to report it as such instead of printing a working portal with
    // no monitors, which is exactly how it read before.
    #[test]
    fn a_session_backend_serves_nothing_statelessly() {
        let portal = PortalBackend { available: true, ffmpeg: true };
        assert_eq!(portal.acquisition(), Acquisition::Session);
        assert!(portal.outputs().is_empty());
        assert!(portal.list_windows().is_empty());
        assert!(portal.screenshot_output("any").is_none());
        assert!(portal.screenshot_window("any").is_none());
        assert!(portal.cursor().is_none());
    }

    // And it is NOT the screenshot capability wearing another hat. The portal really
    // can screenshot (the app does it through a grant); the two answers speak to
    // different questions and conflating them is the bug this type prevents.
    #[test]
    fn session_driven_does_not_mean_incapable() {
        let portal = PortalBackend { available: true, ffmpeg: true };
        assert!(portal.caps().screenshot, "the portal screenshots, through its session");
        assert!(portal.caps().record);
        assert_eq!(portal.acquisition(), Acquisition::Session);
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

/// DRAGON-620: the window list must not advertise itself where it cannot answer.
///
/// These pin [`window_list_supported`] against the three real-world protocol shapes,
/// because the bug being prevented was invisible on the dev box: every one of these
/// answers correctly on COSMIC, and only the wlroots shape ever disagreed.
#[cfg(all(test, target_os = "linux"))]
mod window_list_honesty_tests {
    use super::tests::{full, wlroots_shaped};
    use super::*;

    #[test]
    fn cosmic_advertises_a_usable_window_list() {
        assert!(window_list_supported(&full()));
        let caps = ScreencopyBackend { protocols: full(), ffmpeg: true }.caps();
        assert!(caps.window_list && caps.window_capture && caps.fullscreen_aware);
    }

    #[test]
    fn wlroots_names_windows_but_cannot_place_them() {
        // The whole point of the ticket. The upstream list is present, so the OLD
        // `toplevel_list`-only rule said yes; cctk fills geometry only from
        // `zcosmic_toplevel_info_v1` v2+, so `list_toplevels` would return an empty map.
        let p = wlroots_shaped();
        assert!(p.toplevel_list, "wlroots really does advertise the upstream list");
        assert!(!window_list_supported(&p), "but it cannot place a single window");
        let caps = ScreencopyBackend { protocols: p, ffmpeg: true }.caps();
        assert!(!caps.window_list && !caps.window_capture && !caps.fullscreen_aware);
        // Everything that does NOT depend on toplevel geometry must survive intact:
        // a wlroots session still screenshots, records and overlays normally.
        assert!(caps.screenshot && caps.record && caps.layer_overlay && caps.cursor_toggle);
    }

    #[test]
    fn every_missing_cosmic_toplevel_global_turns_the_list_off() {
        // Neither COSMIC global is sufficient alone, and neither is the upstream list.
        for p in [
            WaylandProtocols { toplevel_list: false, ..full() },
            WaylandProtocols { cosmic_toplevel_info: false, ..full() },
            WaylandProtocols { cosmic_toplevel_manager: false, ..full() },
            WaylandProtocols::default(),
        ] {
            assert!(!window_list_supported(&p), "{p:?} must not claim a window list");
        }
    }

    #[test]
    fn the_probe_keeps_the_three_toplevel_globals_apart() {
        // A regression guard on the PROBE rather than the predicate: these three flags
        // were one ORed flag, and merging any two of them back together silently
        // restores the lie. Mirrors what `probe_globals` does per registry entry.
        let list = WaylandProtocols { toplevel_list: true, ..Default::default() };
        assert!(!list.cosmic_toplevel_info && !list.cosmic_toplevel_manager);
        let info = WaylandProtocols { cosmic_toplevel_info: true, ..Default::default() };
        assert!(!info.toplevel_list && !info.cosmic_toplevel_manager);
        let mgr = WaylandProtocols { cosmic_toplevel_manager: true, ..Default::default() };
        assert!(!mgr.toplevel_list && !mgr.cosmic_toplevel_info);
    }
}
