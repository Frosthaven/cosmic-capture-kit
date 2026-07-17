//! `WindowChromeMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

use crate::app::ResetScope;
#[cfg(target_os = "linux")]
use cosmic::iced::core::event::wayland::OutputEvent;
use cosmic::iced::keyboard::{Key, Modifiers};
use cosmic::iced::window;
use cosmic::widget;
#[cfg(target_os = "linux")]
use wayland_client::protocol::wl_output::WlOutput;

#[derive(Debug, Clone)]
pub enum WindowChromeMsg {
    // `OutputEvent` is large; box it so it doesn't bloat every `Msg`. The Wayland
    // OutputEvent subscription is Linux-only (DRAGON-94); macOS derives outputs
    // from NSScreen/SCK, so this variant doesn't exist there.
    #[cfg(target_os = "linux")]
    Output(Box<OutputEvent>, WlOutput),
    /// macOS/Windows: seed the per-display capture overlays at startup — the
    /// PlainWindows equivalent of the Wayland `Output` hotplug path (there is no
    /// output-event subscription off Wayland, so init dispatches this once).
    #[cfg(not(target_os = "linux"))]
    SeedOutputs,
    /// macOS/Windows: a capture overlay window finished opening (its `window::open`
    /// task resolved with this id). Fired then — not batched with open — so the
    /// native NSWindow tweaks run only once the view is installed in its window
    /// (running them concurrently with creation races winit and panics). The `u8`
    /// is the placement attempt (starts at 0): the NSWindow title is set by an async
    /// task and may not have landed yet, so `configure_overlay` re-emits this with an
    /// incremented count to poll briefly until the overlay can be matched + placed.
    #[cfg(not(target_os = "linux"))]
    OverlayOpened(window::Id, u8),
    /// macOS (DRAGON-130 crash-dodge): the windowed preview finished opening. Fired
    /// then — not batched with open — so the native titlebar-button strip runs only
    /// once the view is installed (running it mid-creation races winit and panics).
    /// The `u8` is the poll attempt (starts at 0): the NSWindow title is set by an
    /// async task and may not have landed yet, so `finalize_preview_window` re-emits
    /// this with an incremented count to briefly poll until the window can be matched.
    #[cfg(target_os = "macos")]
    PreviewOpened(window::Id, u8),
    /// macOS: the fullscreen OVERLAY preview window finished opening. Carries the
    /// target display's global logical rect (pos, size) so the retry re-emits are
    /// self-contained, plus the poll attempt — the same title-lag poll shape as
    /// `PreviewOpened` (see `finalize_preview_overlay`).
    #[cfg(target_os = "macos")]
    PreviewOverlayOpened(window::Id, (i32, i32), (u32, u32), u8),
    /// macOS/Windows: mint the per-display capture overlays. Dispatched by
    /// `seed_outputs_mac` after the tiling-WM wait: a no-op by default (the DRAGON-154
    /// chrome strip opts the overlays out of AeroSpace entirely), or the completed
    /// off-thread pause when the CCK_AEROSPACE_PAUSE escape hatch is engaged.
    #[cfg(not(target_os = "linux"))]
    SeedOverlays,
    OpenGear,
    /// Open a URL in the default browser (e.g. the ground-loop help link).
    OpenUrl(&'static str),
    /// Open a dynamically-sourced URL (a markdown release-note link, DRAGON-177,
    /// whose target is only known at runtime — the `&'static` form can't carry it).
    OpenUrlOwned(String),
    /// Swallow an input event without doing anything (e.g. a click on a modal
    /// backdrop that must block the window behind it but not dismiss the modal).
    Ignore,
    /// Settings: switch the active settings page (nav rail).
    SetConfigTab(widget::segmented_button::Entity),
    /// Settings: a reset-to-defaults button was pressed → show the confirm dialog.
    RequestReset(ResetScope),
    /// Confirm the pending reset.
    ConfirmReset,
    /// Dismiss the pending reset.
    CancelReset,
    /// The settings toplevel window finished opening.
    ConfigWindowOpened(window::Id),
    /// A window was resized (logical w, h) — used to remember the settings size.
    ConfigWindowResized(window::Id, f32, f32),
    /// Close the settings window (header ✕ / Done). On macOS the native traffic
    /// lights own close/minimize (DRAGON-135), so these two CSD variants are only
    /// constructed on Linux; the handlers stay platform-uniform.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    CloseConfigWindow,
    /// Settings window titlebar: drag to move.
    ConfigWindowDrag,
    /// Settings window titlebar: toggle maximize (button + double-click).
    ConfigWindowMaximize,
    /// Settings window titlebar: minimize.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    ConfigWindowMinimize,
    /// The permission-checker toplevel window finished opening (macOS onboarding
    /// surface; the window is only ever minted on macOS, so this never fires on
    /// Linux — kept un-cfg'd so the enum + match arms stay platform-uniform).
    PermissionsWindowOpened(window::Id),
    /// Close the permission-checker window (header ✕). Currently constructed on NO
    /// platform: the only construction site is the `not(macos)` header-close branch
    /// inside the macOS-only view module (on mac the traffic lights carry close,
    /// DRAGON-135; on Linux the window is never minted). Kept for the day the
    /// checker grows a Linux surface — the `expect` self-expires then.
    #[expect(dead_code)]
    ClosePermissionsWindow,
    /// Permission-checker window titlebar: drag to move.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    PermissionsWindowDrag,
    /// macOS (DRAGON-135): apply the empty-toolbar tweak that vertically centres
    /// the native traffic lights over the CSD header. Title-matched (the async
    /// set-title task must land first), so it polls: (window title, attempt).
    #[cfg(target_os = "macos")]
    MacCenterTitlebar(&'static str, u8),
    /// Toggle the settings nav rail (header hamburger).
    ToggleConfigNav,
    /// Expand the header search field (and focus it).
    ConfigSearchActivate,
    /// Search query changed.
    ConfigSearchInput(String),
    /// Clear + collapse the header search field.
    ConfigSearchClear,
    /// macOS: the pointer entered a window — if it's a capture overlay, keyboard
    /// focus follows it (Escape / shortcuts work on whichever display the user is
    /// on). Never emitted on Linux (layer-shell keyboard focus is on-demand there).
    #[cfg(target_os = "macos")]
    CursorEnteredWindow(window::Id),
    /// macOS (DRAGON-153): a blocked second `--settings` launch touched the focus
    /// poke file — bring this (live) settings pane to the front. Fired by
    /// `sub_settings_poke` while the pane is open.
    #[cfg(target_os = "macos")]
    SettingsFocusPoke,
    /// A window asked to close (✕ / WM) — only acted on for the settings window.
    WindowCloseRequested(window::Id),
    /// A window finished closing.
    WindowClosed(window::Id),
    /// A raw key press, routed through the live keymap in `update` (so rebinds take
    /// effect immediately, unlike the `'static` event subscription).
    KeyPressed(Modifiers, Key),
    /// A raw key release — only push-to-talk cares (release = mute the mic again).
    KeyReleased(Modifiers, Key),
    /// Escape / contextual back: settings → mode → region → quit.
    Close,
    /// The toolbar ✕: always quit the tool outright.
    Quit,
}
