//! The surface SHELL seam: every compositor-specific way we create/destroy a
//! top-level surface lives here, and nothing else in the app touches layer-shell
//! types. Today there is one implementation — wlr-layer-shell via libcosmic
//! (COSMIC + any layer-shell compositor). The portable implementation for
//! platforms without layer shell (GNOME, macOS, Windows — DRAGON-93/94/95) is
//! per-monitor borderless always-on-top transparent winit windows with
//! `set_cursor_hittest` standing in for input zones; it branches INSIDE these
//! functions, so the app above deals only in `window::Id`s + views either way.

#[cfg(target_os = "linux")]
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, set_keyboard_interactivity,
};
#[cfg(target_os = "linux")]
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::{window, Task};
#[cfg(target_os = "linux")]
use cosmic::iced::Limits;
#[cfg(target_os = "linux")]
use cosmic_client_toolkit::sctk::shell::wlr_layer;

use super::Msg;

/// A fullscreen per-output capture overlay. `input_zone`: None = full input,
/// Some(rects) = only those rects are interactive (rest click-through).
/// `keyboard` enables ON-DEMAND keyboard input (focus on click); `false` takes no
/// keyboard interactivity at all. Never MINTED Exclusive: an Overlay-layer exclusive
/// grab holds keyboard focus hostage from every window for as long as the surface is
/// mapped (DRAGON-109) — recording hotkeys that must work unfocused go through the
/// portal GlobalShortcuts path instead. The ONE sanctioned exception is the live
/// [`picker_keyboard`] flip: an idle monitor/window PICKER holds Exclusive while
/// it's up (DRAGON-228 — it's modal UI, and without it Escape has no way in), and
/// releases it the moment picking ends.
#[cfg(target_os = "linux")]
pub(super) fn overlay_surface(
    output: super::OutputHandle,
    id: window::Id,
    input_zone: Option<Vec<cosmic::iced::Rectangle>>,
    keyboard: bool,
) -> Task<cosmic::Action<Msg>> {
    overlay_surface_with(
        output,
        id,
        input_zone,
        if keyboard {
            wlr_layer::KeyboardInteractivity::OnDemand
        } else {
            wlr_layer::KeyboardInteractivity::None
        },
    )
}

/// [`overlay_surface`] with the keyboard interactivity spelled out — the PICKING
/// phase mints Exclusive here (DRAGON-228). Minting with the right interactivity
/// (instead of flipping right after creation) matters: a flip issued in the same
/// batch races the surface's creation and is dropped.
#[cfg(target_os = "linux")]
pub(super) fn overlay_surface_with(
    output: super::OutputHandle,
    id: window::Id,
    input_zone: Option<Vec<cosmic::iced::Rectangle>>,
    keyboard_interactivity: wlr_layer::KeyboardInteractivity,
) -> Task<cosmic::Action<Msg>> {
    get_layer_surface(SctkLayerSurfaceSettings {
        id,
        layer: wlr_layer::Layer::Overlay,
        keyboard_interactivity,
        input_zone,
        anchor: wlr_layer::Anchor::all(),
        output: IcedOutput::Output(output),
        namespace: "cosmic-capture-kit".into(),
        margin: IcedMargin::default(),
        size: Some((None, None)),
        exclusive_zone: -1,
        size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
    })
}

/// macOS/Windows PlainWindows capture overlay (DRAGON-94 phase 2b): one
/// transparent, always-on-top winit window per display, positioned + sized to cover
/// it exactly. Winit mints the `window::Id` (there is no API to open with a
/// pre-chosen id), so — like [`preview_window`] — this RETURNS the id for the caller
/// to record in `OutputState.id`; `close_surface`/view keying then line up.
/// `logical_pos` is the display's top-left in global LOGICAL points, which is exactly
/// what iced/winit wants for the outer position on mac (X matches Cocoa; Y is
/// top-left in both iced and CoreGraphics, so the [`coords`] flip is NOT applied
/// here — that pair is for Cocoa/AppKit geometry, which this isn't).
///
/// `decorations: true` is a CRASH-DODGE, not a real titlebar: a borderless mac
/// window trips a winit abort ("view must be installed in a window") because
/// libcosmic polls `is_maximized` on every resize and winit's `is_zoomed` flips a
/// *borderless* window's style mask mid-reframe. Keeping `Titled | Resizable` in the
/// mask (decorations + resizable) makes `is_zoomed` skip that flip. The titlebar is
/// then made invisible — transparent, no title, no buttons, no shadow — natively
/// once the window has opened (the `OverlayOpened` message →
/// [`crate::platform::mac::window::finalize_overlay_windows`], which also sets the
/// full-screen-Space collection behavior). The native step runs only AFTER open
/// because reaching a window mid-creation races winit and aborts.
#[cfg(not(target_os = "linux"))]
pub(super) fn overlay_window(
    logical_pos: (i32, i32),
    logical_size: (u32, u32),
) -> (window::Id, Task<cosmic::Action<Msg>>) {
    let (w, h) = (logical_size.0.max(1) as f32, logical_size.1.max(1) as f32);
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(w, h),
        position: window::Position::Specific(cosmic::iced::Point::new(
            logical_pos.0 as f32,
            logical_pos.1 as f32,
        )),
        // decorations + resizable keep `Titled | Resizable` in the style mask (the
        // is_zoomed crash-dodge above); the titlebar is hidden natively after open.
        resizable: true,
        decorations: true,
        transparent: true,
        exit_on_close_request: false,
        level: window::Level::AlwaysOnTop,
        // Content fills behind the (soon-invisible) titlebar so the overlay is
        // full-bleed; the title is hidden up front too.
        #[cfg(target_os = "macos")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..Default::default()
    });
    // Carry the id out on open completion; the handler applies the native tweaks then
    // (see `App::configure_overlay`) — doing it before the view is installed aborts.
    let open = task.map(|id| {
        cosmic::Action::App(Msg::WindowChrome(super::WindowChromeMsg::OverlayOpened(id, 0)))
    });
    (id, open)
}

/// The windowed preview's NSWindow title. Shared so the open path and the macOS
/// native finalize (`finalize_preview_window`) match the SAME window by title.
pub(super) const PREVIEW_WINDOW_TITLE: &str = "Cosmic Capture Kit - Preview";

/// The macOS fullscreen OVERLAY preview's (hidden) NSWindow title — distinct from
/// both [`PREVIEW_WINDOW_TITLE`] and the capture overlays' display-name titles, so
/// the native finalize (`finalize_preview_overlay`) can never match a still-closing
/// capture overlay from the same session.
#[cfg(target_os = "macos")]
pub(super) const PREVIEW_OVERLAY_TITLE: &str = "Cosmic Capture Kit - Preview Overlay";

/// macOS: the fullscreen OVERLAY preview — the layer-shell `preview_surface`'s
/// PlainWindows counterpart. Same recipe as [`overlay_window`] (the capture
/// overlay): a transparent always-on-top winit window positioned + sized to the
/// target display, opened `decorations: true` (the borderless `is_zoomed`
/// crash-dodge) and natively finished AFTER open — `PreviewOverlayOpened` →
/// `App::finalize_preview_overlay` → `place_overlay`, which raises it to the
/// shielding level (menu bar covered, like the Linux overlay), strips the titlebar,
/// and sets the exact full-display frame. The `AlwaysOnTop` level set here (before
/// first order-front) is what routes it through the DRAGON-154 pre-order-front
/// chrome strip, keeping AeroSpace's popup classification (never tiled).
#[cfg(target_os = "macos")]
pub(super) fn preview_overlay_window(
    logical_pos: (i32, i32),
    logical_size: (u32, u32),
    visible: bool,
) -> (window::Id, Task<cosmic::Action<Msg>>) {
    let (w, h) = (logical_size.0.max(1) as f32, logical_size.1.max(1) as f32);
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(w, h),
        position: window::Position::Specific(cosmic::iced::Point::new(
            logical_pos.0 as f32,
            logical_pos.1 as f32,
        )),
        // decorations + resizable keep `Titled | Resizable` in the style mask (the
        // is_zoomed crash-dodge); the titlebar is hidden natively after open.
        resizable: true,
        decorations: true,
        transparent: true,
        exit_on_close_request: false,
        level: window::Level::AlwaysOnTop,
        // DRAGON-216 (overlay pre-open): open `visible:false` while covering the grab so
        // winit's create-time `makeKeyAndOrderFront` never keys/activates us (which would
        // flip the picked window off frontmost); `place_overlay` then orders it on screen
        // non-key. `true` (the default) for a normal deferred open, byte-identical to before.
        visible,
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..Default::default()
    });
    // Carry the id + target rect out on open completion; the handler applies the
    // native placement then (reaching the window mid-creation races winit and aborts).
    let open = task.map(move |id| {
        cosmic::Action::App(Msg::WindowChrome(super::WindowChromeMsg::PreviewOverlayOpened(
            id,
            logical_pos,
            logical_size,
            0,
        )))
    });
    (id, open)
}

/// The windowed preview's floor size — small enough to sit on a compact monitor, large
/// enough that every top/bottom toolbar control stays laid out without clipping. Shared by
/// the window `min_size` and the fit-to-media sizing so they agree.
pub(super) const PREVIEW_MIN_W: f32 = 792.0;
pub(super) const PREVIEW_MIN_H: f32 = 440.0;

/// The post-capture preview as a normal RESIZABLE WINDOW (the "Windowed" appearance)
/// instead of the fullscreen overlay — so it can be moved / resized / min / maximized.
/// Mints its own `window::Id` (returned so the caller stores it in `PreviewState`); the
/// open task is mapped to a no-op since the state is set up-front.
///
/// `output` (the target monitor's logical size) becomes a TRANSIENT `max_size` hint:
/// cosmic-comp (`FloatingLayout::map_internal`, floating/mod.rs:405-468 @ 9d52653d)
/// resets a NEW floating toplevel's size PER AXIS to 2/3 of the output's non-exclusive
/// zone — breaking the aspect — but ONLY when the client set no `xdg_toplevel.set_max_size`
/// hint; with one, the request is honoured up to the compositor's own output-size clamp.
/// The hint is cleared again on the window's first configure (`App::preview_resized`)
/// so interactive resizing stays unconstrained (DRAGON-108).
pub(super) fn preview_window(
    size: (f32, f32),
    output: (f32, f32),
) -> (window::Id, Task<cosmic::Action<Msg>>) {
    // Frosted glass (DRAGON-217): enroll the windowed preview's surface in the
    // compositor's backdrop blur when frosted windows are on. Gated at the seam —
    // `glass_windows_enabled` is `false` off COSMIC / macOS, so the window opens
    // un-enrolled and opaque exactly as before. `preview_view`'s window chrome
    // paints translucent (`theme::frost_color`).
    let blur = crate::app::theme::glass_windows_enabled();
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(size.0, size.1),
        blur,
        max_size: Some(cosmic::iced::Size::new(
            output.0.max(size.0),
            output.1.max(size.1),
        )),
        // Floor sized so the top/bottom toolbars (incl. the covermark sliders + zoom scale)
        // never clamp and clip over each other — see DRAGON-106.
        min_size: Some(cosmic::iced::Size::new(PREVIEW_MIN_W, PREVIEW_MIN_H)),
        resizable: true,
        resize_border: 8,
        // CLIENT-side decorations (like the settings window): we draw our own header bar
        // into the surface. Server-side decorations are drawn by the compositor OUTSIDE the
        // surface, so a window capture of the preview would miss its title bar (DRAGON-105);
        // a CSD header bar is part of the surface and captures correctly.
        //
        // macOS EXCEPTION (DRAGON-130 crash-dodge): `decorations: false` mints a truly
        // BORDERLESS NSWindow, which trips the same winit abort the overlays dodge —
        // libcosmic polls `is_maximized` on every resize, winit's `is_zoomed` flips a
        // borderless window's style mask mid-reframe, and the frame-change notification
        // hits the momentarily-detached content view → "view must be installed in a
        // window" (the stop→preview crash). Keeping `Titled | Resizable` in the mask
        // (decorations + resizable) makes `is_zoomed` skip that flip. The native
        // titlebar is then hidden — transparent, no title, no traffic-light buttons,
        // content behind it — so it still reads as a clean CSD window; the buttons are
        // pulled natively once the window has opened (`finalize_preview_window`, kicked
        // by the `PreviewOpened` follow-up below), same as the overlay path.
        #[cfg(target_os = "macos")]
        decorations: true,
        #[cfg(not(target_os = "macos"))]
        decorations: false,
        // macOS (DRAGON-146): opaque for the native masked corner (see
        // `settings::open_config_window`). The FULLSCREEN overlay preview
        // (`preview_surface`) stays transparent — it's edge-to-edge, no visible corner.
        #[cfg(target_os = "macos")]
        transparent: false,
        #[cfg(not(target_os = "macos"))]
        transparent: true,
        exit_on_close_request: false,
        // The app id matches our installed `.desktop`, so the compositor shows the
        // app icon on the preview window's titlebar / task switcher. `application_id`
        // is a Wayland-only field of iced's PlatformSpecific; macOS uses its defaults.
        #[cfg(target_os = "linux")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            application_id: "dev.frosthaven.CosmicCaptureKit".to_string(),
            ..Default::default()
        },
        // macOS: fill content behind the (soon-invisible) transparent titlebar so the
        // CSD header bar is full-bleed and the window looks borderless.
        #[cfg(target_os = "macos")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific::default(),
        ..Default::default()
    });
    // macOS: after the view is installed, natively strip the titlebar buttons (doing it
    // mid-creation races winit and aborts). Elsewhere the open is a no-op.
    #[cfg(target_os = "macos")]
    let done = task.map(|id| {
        cosmic::Action::App(Msg::WindowChrome(super::WindowChromeMsg::PreviewOpened(id, 0)))
    });
    #[cfg(not(target_os = "macos"))]
    let done = task.map(|_| cosmic::Action::App(Msg::WindowChrome(super::WindowChromeMsg::Ignore)));
    (id, done)
}

/// A fullscreen overlay for the post-capture preview. Exclusive keyboard so its
/// hotkeys (Save / Copy / Cancel) work immediately without a click first; full
/// input so the action buttons are clickable. `output`: the capture's monitor,
/// or the compositor's active one (`--preview`, which has no capture anchor).
#[cfg(target_os = "linux")]
pub(super) fn preview_surface(output: IcedOutput, id: window::Id) -> Task<cosmic::Action<Msg>> {
    preview_surface_with(output, id, wlr_layer::KeyboardInteractivity::Exclusive)
}

/// DRAGON-216: the same overlay opened FOCUS-NEUTRAL (`KeyboardInteractivity::None`) —
/// it takes no keyboard focus, so pre-opening it during a window pick's focus-then-grab
/// can't steal the picked toplevel's focus (the DRAGON-194 invariant). Promoted to
/// `Exclusive` on `WindowGrabbed` via [`set_keyboard_interactivity`]; the spinner view
/// is identical, so the swap is invisible (no surface re-create, no flicker).
#[cfg(target_os = "linux")]
pub(super) fn preview_surface_neutral(
    output: IcedOutput,
    id: window::Id,
) -> Task<cosmic::Action<Msg>> {
    preview_surface_with(output, id, wlr_layer::KeyboardInteractivity::None)
}

/// DRAGON-216: promote a pre-opened focus-neutral preview overlay to `Exclusive`
/// keyboard once the window grab is done, so the loading spinner's hotkeys work.
/// Same surface (no re-create) — the view is unchanged, so nothing repaints.
#[cfg(target_os = "linux")]
pub(super) fn promote_preview_surface(id: window::Id) -> Task<cosmic::Action<Msg>> {
    set_keyboard_interactivity(id, wlr_layer::KeyboardInteractivity::Exclusive)
}

#[cfg(target_os = "linux")]
fn preview_surface_with(
    output: IcedOutput,
    id: window::Id,
    keyboard_interactivity: wlr_layer::KeyboardInteractivity,
) -> Task<cosmic::Action<Msg>> {
    get_layer_surface(SctkLayerSurfaceSettings {
        id,
        layer: wlr_layer::Layer::Overlay,
        keyboard_interactivity,
        input_zone: None,
        anchor: wlr_layer::Anchor::all(),
        output,
        namespace: "cosmic-capture-kit-preview".into(),
        margin: IcedMargin::default(),
        size: Some((None, None)),
        exclusive_zone: -1,
        size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
    })
}

/// The 1x1 bootstrap surface created at startup: libcosmic with
/// `no_main_window` needs one surface to exist before tasks run. Bottom layer,
/// no input, invisible in practice.
#[cfg(target_os = "linux")]
pub(super) fn bootstrap_surface(id: window::Id) -> Task<cosmic::Action<Msg>> {
    get_layer_surface(SctkLayerSurfaceSettings {
        id,
        layer: wlr_layer::Layer::Bottom,
        keyboard_interactivity: wlr_layer::KeyboardInteractivity::None,
        input_zone: Some(Vec::new()),
        anchor: wlr_layer::Anchor::empty(),
        output: IcedOutput::Active,
        namespace: "cosmic-capture-kit-dummy".into(),
        margin: IcedMargin::default(),
        size: Some((Some(1), Some(1))),
        exclusive_zone: -1,
        size_limits: Limits::NONE,
    })
}

/// macOS/Windows: no layer shell, so the bootstrap is a real (tiny, undecorated)
/// winit window — libcosmic's `no_main_window(true)` needs one surface to exist
/// before tasks run, and winit needs a window to drive its event loop. DRAGON-94
/// phase 2 replaces this with the resident menu-bar shell + real capture overlays.
#[cfg(not(target_os = "linux"))]
pub(super) fn bootstrap_surface(id: window::Id) -> Task<cosmic::Action<Msg>> {
    let (_id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(1.0, 1.0),
        decorations: false,
        transparent: true,
        exit_on_close_request: false,
        // Never ordered on screen (DRAGON-154): the anchor is a pure event-loop
        // bootstrap, but a VISIBLE one enters the AX window list, where an enabled
        // tiling WM (AeroSpace, no-pause mode) manages and natively FOCUSES it —
        // activating this app, stamping its name into the menu bar, and pulling
        // input focus into an invisible 1x1 window.
        visible: false,
        ..Default::default()
    });
    // `window::open` mints its own id; the caller's `id` is unused on this path
    // (the bootstrap window is never addressed again). Map the open to a no-op.
    let _ = id;
    task.map(|_| cosmic::Action::App(Msg::WindowChrome(super::WindowChromeMsg::Ignore)))
}

/// Destroy any surface created by this module.
#[cfg(target_os = "linux")]
pub(super) fn close_surface(id: window::Id) -> Task<cosmic::Action<Msg>> {
    destroy_layer_surface(id)
}

/// macOS/Windows: plain windows close via winit.
#[cfg(not(target_os = "linux"))]
pub(super) fn close_surface(id: window::Id) -> Task<cosmic::Action<Msg>> {
    window::close(id)
}
