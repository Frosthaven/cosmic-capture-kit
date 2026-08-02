//! Platform-glue layer: Wayland compositor client, xdg-portal ScreenCast session, and PipeWire frame consumers.
//!
//! # Platform seam map v2 (the "adding Windows" contract, DRAGON-161 / DRAGON-220)
//!
//! Cosmic Capture Kit runs on Wayland/COSMIC-family Linux and macOS today. Every place
//! where behavior forks by OS is behind ONE of the named seams below, so teaching the
//! app a NEW platform (Windows, a non-COSMIC Linux flavor) means IMPLEMENTING these
//! seams, not grepping for `cfg(target_os = …)` and adding branches. The portable core
//! (`app` state machine, `record::pump`/`finalize`/`owned`, `encode::command`, the audio
//! DSP `filters/`) carries NO platform knowledge; it composes these seams.
//!
//! Each seam is a trait, a per-platform module behind a stable module path, or a
//! `cfg`-selected impl of a shared type. The style is uniform: **portable seam + cfg-gated
//! platform module** (never a second competing abstraction). Where a seam is still a set
//! of parallel per-platform `fn`s rather than a `trait` (recording worker, some
//! services), that is noted so it is a KNOWN shape, not an accident.
//!
//! DRAGON-220 sorted the platform bodies into PLUGIN FOLDERS (`platform/linux/*`,
//! `platform/mac/*`, `platform/windows/`) without moving any MODULE. The impl column
//! below names the folder a body physically lives in; the LOGICAL path a caller uses is
//! in the boundary column and is unchanged (the folders are `#[path]`-mounted at the old
//! names, see "Mount registry"). So "where does the mac window code live" answers with a
//! folder, "how do I call it" answers with `platform::mac::window` exactly as before.
//!
//! | Seam | Boundary | Linux impl | macOS impl | New platform plugs in by |
//! |------|----------|------------|------------|--------------------------|
//! | **Capture backend** | [`backend::CaptureBackend`] trait (stills, window list, cursor, caps); logical `crate::screenshot` / `crate::screencopy` | `ScreencopyBackend` / `PortalBackend` (in `backend.rs`, driving `platform/linux/native/` + `linux/portal/`) | `MacBackend` (in `backend.rs`, driving `platform/mac/`) | impl `CaptureBackend`, add to [`backend::backends`] |
//! | **Recording worker** | `record::start_region_recording` / `start_pipewire_recording` → a worker owning its capture connection, posting ONE `Result` via `DoneGuard`, running `record::owned`'s shared media-clock stop tail | `record::screencopy` / `record::pipewire` (+ `zero_copy`) | `record::sck` | add a `cfg`-gated `start_region_recording`, run the `owned::run_video_stop_tail` contract |
//! | **Capture connection** | frame source feeding the media-clock loop; logical `platform::screencast` + `platform::pipewire` / `platform::mac::sck_stream` | `platform/linux/portal/` (Wayland screencopy client / PipeWire portal) | `platform/mac/screencapturekit/` (`SCStream`) | provide a frame source; reuse `record::owned` verbatim |
//! | **Audio capture** | `audio::capture::MonitorCapture` (system) + `audio::clean_mic` mic tap; the DSP `filters/` are byte-identical everywhere | Pulse monitor + ffmpeg pulse mic | SCK audio-only stream + ffmpeg avfoundation mic | give both a 48k f32 source behind the same `StreamTap`/`CaptureChunk` contract |
//! | **Encoder** | `encode::plan` / `encode::device` backend tiers | NVENC / VAAPI / x264 | VideoToolbox | add a tier in `encode::plan`; `encode::command` stays shared |
//! | **Overlay / window placement** | `app::shell` (creates/destroys surfaces) + `app::surfaces` (`finish_session` is THE lifecycle seam); logical `platform::mac::window` | wlr-layer-shell | `platform/mac/wm/` (per-`NSScreen` winit windows) | branch inside `shell`/`surfaces` (per DRAGON-93/94/95) |
//! | **Tray / resident mode** | logical `crate::tray` (Linux `ksni`) vs `platform/tray_stub.rs` vs the macOS menu-bar `crate::daemon`; IPC via `crate::daemon_ipc` | `platform/linux/tray.rs` + `platform/linux/daemon.rs` (`crate::daemon_linux`) | `platform/mac/daemon.rs` + `platform/mac/tray.rs` | a `#[path]` module mount in `main.rs` (see "Mount registry") |
//! | **Permissions** | `app::permissions` model (`PermStatus`/`card_action`) | no-op (Wayland has no TCC) | `platform/mac/tcc.rs` probes | fill the platform arm of the permission probes |
//! | **Paths / services / portals** | `util::locate_tool`, `platform::services` (notify/open/file-manager), `instance` (locks/signals), `share/` (clipboard/open/notify); logical `platform::mac::{file_panel, login_item, appearance, env}` | xdg / D-Bus | `platform/mac/services/` (`.app` sidecar / NSWorkspace / launchd) | fill each service's platform arm |
//! | **Desktop profile** (Linux) | [`linux::DesktopProfile`] trait + [`linux::PROFILES`] registry (config readers + quirks, keyed by DESKTOP, never capture) | `platform/linux/{cosmic,gnome,kde,wlroots}/` | n/a (macOS is one desktop) | copy a profile folder, add its unit struct to `PROFILES` |
//! | **Wallpaper** | `wallpaper::detect` | desktop-config ladder over `platform/linux/{cosmic,gnome,kde,wlroots}/` (cosmic-bg/gnome/kde/sway) | `platform/mac/wallpaper.rs` (`NSWorkspace` desktop picture) | add a `detect()` arm |
//!
//! Inline `cfg(target_os = …)` sites OUTSIDE this module are, by policy, only:
//! module/import gates, per-platform `trait`/`fn` impls of a seam above, message-domain
//! enum variants + their `update_*` arms, brief per-platform UI text/routing, and
//! `#[cfg(test)]` gates, none of which a portable-core reader must cross to follow the
//! non-platform logic.
//!
//! ## Plugin folders
//!
//! The platform bodies sort into folder families. Each names a LOGICAL mount point (the
//! path callers use); the folder is only where the file sits on disk.
//!
//! - **`platform/linux/native/`**: the compositor-DIRECT capture stack, the
//!   `ext-image-copy-capture` (cctk) client + the scene composition around it. Mounted at
//!   `crate::screencopy` (the frame/cursor client) and `crate::screenshot` (the Linux arm
//!   of the still-grab layer). Linux-only; macOS mounts its own `screenshot` from
//!   `platform/mac/`.
//! - **`platform/linux/portal/`**: the xdg-desktop-portal ScreenCast + PipeWire capture
//!   path and its pixel-format helpers. Mounted at `platform::screencast`,
//!   `platform::pipewire`, and `platform::pixfmt`. The portal backend's recording
//!   connection; `screencast` also keeps a tiny off-Linux TYPE stub (`screencast_stub.rs`)
//!   because its data types leak into platform-free app state.
//! - **`platform/linux/{cosmic,gnome,kde,wlroots}/`**: the DesktopProfile axis, one
//!   per-desktop CONFIG reader + quirk owner (wallpaper path, theme readers, tiling
//!   tweaks) behind [`linux::DesktopProfile`], walked in fixed order by [`linux::PROFILES`].
//!   This axis is deliberately SEPARATE from capture: capture stays PROTOCOL-keyed through
//!   [`backend`] (DRAGON-93 "judge compositors by protocol, not name"), so a wlroots
//!   compositor advertising `ext-image-copy-capture` gets the native backend regardless of
//!   which profile matches. `cosmic/compositor.rs` also holds the cctk toplevel enumeration
//!   the [`compositor`] facade re-exports.
//! - **`platform/mac/{wm,services,screencapturekit}/`**: FACET folders under the macOS
//!   plugin. `wm/` is window-manager interaction (overlay placement, the focus dance,
//!   Spaces/Stage-Manager filtering, the AppKit↔app coordinate mapper); `services/` is
//!   user-facing OS services (file panels, login item, appearance, PATH repair);
//!   `screencapturekit/` is the SCK recording stream. Every file is `#[path]`-mounted so
//!   `platform::mac::window`, `platform::mac::file_panel`, `platform::mac::sck_stream`, etc.
//!   all keep their paths (see [`mac`]'s own facet index). `tcc.rs`, `wallpaper.rs`,
//!   `pinch.rs` stay at the `mac/` root.
//! - **`platform/windows/`** (DRAGON-229): the Windows plugin, `#[cfg(windows)] pub mod
//!   windows;`. Holds `backend.rs` (the `CaptureBackend` impl) + `services.rs` (clipboard
//!   / open / reveal bodies) behind the strict dispatch-only split; `screenshot.rs` is
//!   `#[path]`-mounted at `crate::screenshot` from `main.rs`. M0 is compile-and-open
//!   only (honest stubs); M1 (capture) / M2 (delivery) / M3 (recording) fill the bodies.
//!   The remaining fill-in list is in `platform/windows/README.md`.
//!
//! ## Mount registry
//!
//! Every `#[path]` mount, why it exists, and where a future flatten would edit. The RULE:
//! files move physically, the module tree stays stable: each mount pins a LEGACY logical
//! path onto a file that now lives deeper, so no call site changed when the bodies moved
//! (DRAGON-220's PRIME RULE). A future flatten to canonical deep paths would delete these
//! mounts and update the call sites DELIBERATELY (out of scope now).
//!
//! | Logical path | Physical file | Declared in | cfg | Reason |
//! |--------------|---------------|-------------|-----|--------|
//! | `crate::screencopy` | `platform/linux/native/screencopy.rs` | `main.rs` | linux | folder-sort |
//! | `crate::screenshot` | `platform/linux/native/screenshot.rs` | `main.rs` | linux | folder-sort |
//! | `crate::screenshot` | `platform/mac/screenshot.rs` | `main.rs` | macos | folder-sort |
//! | `crate::screenshot` | `platform/windows/screenshot.rs` | `main.rs` | windows | closed-split (DRAGON-229) |
//! | `crate::tray` | `platform/linux/tray.rs` | `main.rs` | linux | folder-sort |
//! | `crate::tray` | `platform/mac/tray.rs` | `main.rs` | macos | folder-sort |
//! | `crate::tray` | `platform/tray_stub.rs` | `main.rs` | not(linux/macos) | folder-sort |
//! | `crate::daemon` | `platform/mac/daemon.rs` | `main.rs` | macos | folder-sort |
//! | `crate::daemon` | `platform/windows/daemon.rs` | `main.rs` | windows | closed-split (DRAGON-237) |
//! | `crate::daemon_linux` | `platform/linux/daemon.rs` | `main.rs` | linux | folder-sort |
//! | `crate::tray` | `platform/windows/tray.rs` | `main.rs` | windows | closed-split (DRAGON-237) |
//! | `crate::daemon_ipc` | `platform/daemon_ipc.rs` | `main.rs` | any(macos,linux,windows) | folder-sort |
//! | `platform::windows_autostart` | `windows/autostart.rs` | `platform/mod.rs` | windows | closed-split (DRAGON-237) |
//! | `platform::screencast` | `linux/portal/screencast.rs` | `platform/mod.rs` | linux | folder-sort |
//! | `platform::screencast` | `screencast_stub.rs` | `platform/mod.rs` | not(linux) | type-stub |
//! | `platform::pipewire` | `linux/portal/pipewire.rs` | `platform/mod.rs` | linux | folder-sort |
//! | `platform::pixfmt` | `linux/portal/pixfmt.rs` | `platform/mod.rs` | linux | folder-sort |
//! | `platform::linux_autostart` | `linux/autostart.rs` | `platform/mod.rs` | linux | folder-sort |
//! | `platform::mac::active_window` | `wm/active_window.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::coords` | `wm/coords.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::focus` | `wm/focus.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::spaces` | `wm/spaces.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::window` | `wm/window.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::appearance` | `services/appearance.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::env` | `services/env.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::file_panel` | `services/file_panel.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::login_item` | `services/login_item.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `platform::mac::clipboard` | `services/clipboard.rs` | `mac/mod.rs` | macos | closed-split (DRAGON-230) |
//! | `platform::mac::notify` | `services/notify.rs` | `mac/mod.rs` | macos | closed-split (DRAGON-230) |
//! | `platform::mac::open` | `services/open.rs` | `mac/mod.rs` | macos | closed-split (DRAGON-230) |
//! | `platform::mac::sck_stream` | `screencapturekit/sck_stream.rs` | `mac/mod.rs` | macos | facet-sort |
//! | `record::sck` | `mac/screencapturekit/record_worker.rs` | `record/mod.rs` | macos | closed-split |
//! | `record::sck_live_tests` | `mac/screencapturekit/record_worker_live_tests.rs` | `record/mod.rs` | test+macos | closed-split |
//! | `audio::ducking::duck_mac` | `mac/services/duck_mac/mod.rs` | `audio/ducking.rs` | macos | closed-split |
//! | `audio::ducking::media_control` | `windows/media_control.rs` | `audio/ducking.rs` | windows | closed-split (DRAGON-283) |
//!
//! `closed-split` (DRAGON-226): whole mac-native files homed under `platform/mac/` so
//! `scripts/publish-public.sh` can strip the closed platform plugins from the public
//! Linux tree in one directory cut. Shared-core `#[cfg]` glue stays public by design.
//!
//! ## Recipes
//!
//! ### Adding a platform (OS)
//! 1. Create `platform/<os>/` for the new plugin's bodies.
//! 2. Implement [`backend::CaptureBackend`] for the OS's capture API and add it to
//!    [`backend::backends`] (its cfg arm) with a stable `*_ID` const.
//! 3. Fill the service / tray / daemon mounts `main.rs` expects for the OS (a `screenshot`
//!    module, the `tray` arm, a `daemon` arm if residency is wanted), or stub them.
//! 4. Add an `encode::plan` tier if the OS has a hardware encoder; `encode::command` and
//!    `record::owned` stay shared.
//! 5. Follow the honest fill-in list in `platform/windows/README.md` (it maps every
//!    not(linux) arm that today resolves to a mac stub).
//!
//! ### Adding a Linux desktop profile
//! 1. Copy an existing profile folder (e.g. `platform/linux/gnome/`) as the template.
//! 2. Implement [`linux::DesktopProfile`] for the new desktop (its `id` + `wallpaper_path`).
//! 3. Add the unit struct to [`linux::PROFILES`] PRESERVING ladder order (the fixed order
//!    IS the wallpaper precedence; a reorder silently changes which desktop wins).
//! 4. Put the desktop-specific config readers + quirks in the folder, not in shared code.
//! 5. Declare honest capabilities through the existing probe / caps paths ([`backend`]),
//!    never by desktop name (capture is protocol-keyed).
//!
//! ### Adding a capability
//! 1. Add the bit to [`backend::Caps`] with a doc comment on what it gates.
//! 2. Derive it HONESTLY in each backend's `caps()` (a real live probe, never a blanket
//!    `true`); `false` means "feature gated off", never "broken".
//! 3. Gate the feature (a Health row, a hidden settings toggle, a skipped compositing step)
//!    off the bit.
//! 4. Extend [`backend::CaptureExtras`] ONLY if the capability is a user-facing capture
//!    EXTRA (freeze / cursor / transparency / wallpaper): those flow through the
//!    capability x preference x effective AND (`CaptureExtras::and`); a behavior-only
//!    capability stays a `Caps` bit alone.

/// Fixed settle after a window's focus state is DRIVEN (activated or defocused) before its
/// pixels are grabbed, shared by every platform's focus-then-capture path (DRAGON-189/194).
/// Confirming the OS changed focus (frontmost app on macOS, `activated` toplevel state on
/// Wayland) does NOT mean the window server / client has REPAINTED the window's active vs
/// inactive chrome yet, so grabbing immediately can catch the wrong (e.g. still-gray) state.
/// One flat wait is simpler and more predictable than re-grabbing and measuring pixels.
// The focus-then-capture settle runs on Linux (`capture_window_with_focus`), macOS, and
// Windows (DRAGON-278 `wm/focus.rs` drives a picked window's focus before the grab); only an
// exotic other target leaves it dead.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
pub const WINDOW_ACTIVATION_SETTLE: std::time::Duration = std::time::Duration::from_millis(200);

pub mod backend;
pub mod services;
pub mod compositor;
pub mod global_shortcuts;
// The per-desktop PROFILE layer (DRAGON-220): COSMIC / GNOME / KDE / wlroots
// config + quirk owners, plus the `DesktopProfile` registry the wallpaper ladder
// walks. Capture stays PROTOCOL-keyed via `platform::backend` (DRAGON-93 "judge
// compositors by protocol, not name"); this axis is only for the per-desktop
// config readers + behavior tweaks. Linux-only.
#[cfg(target_os = "linux")]
pub mod linux;
// The macOS ScreenCaptureKit capture stack (DRAGON-94 phase 2): the
// coordinate-space mapper + SCK stills/window-list/cursor. Linux uses the
// Wayland screencopy client instead, so this only compiles on macOS.
#[cfg(target_os = "macos")]
pub mod mac;
// The Windows platform plugin (DRAGON-229): the capture backend + desktop-service
// bodies the shared tree dispatches into (strict closed split — see windows/mod.rs).
// Windows uses Windows.Graphics.Capture / DXGI, not Wayland or SCK, so this only
// compiles on Windows. `platform/windows/screenshot.rs` is `#[path]`-mounted at
// `crate::screenshot` from `main.rs` and is deliberately NOT a submodule here.
#[cfg(target_os = "windows")]
pub mod windows;
// Portal ScreenCast + PipeWire consumers are the Linux capture stack (ashpd /
// libpipewire). macOS captures via ScreenCaptureKit through the mac backend
// (DRAGON-94), so the real modules don't compile off-Linux. `screencast` keeps a
// tiny TYPE-stub elsewhere (its data types leak into platform-free app state); the
// session `request()` lives behind Linux-gated methods. `pipewire`/`pixfmt` have no
// off-Linux caller (their call sites are Linux-gated), so they're Linux-only.
#[cfg(target_os = "linux")]
#[path = "linux/portal/screencast.rs"]
pub mod screencast;
#[cfg(not(target_os = "linux"))]
#[path = "screencast_stub.rs"]
pub mod screencast;
#[cfg(target_os = "linux")]
#[path = "linux/portal/pipewire.rs"]
pub mod pipewire;
#[cfg(target_os = "linux")]
#[path = "linux/portal/pixfmt.rs"]
pub(crate) mod pixfmt;
// Launch-at-login on Linux (DRAGON-173): an XDG autostart `.desktop` entry, the
// counterpart of the macOS `mac::login_item` (SMAppService). Drives the resident tray
// back after a login; wired to the same `resident` setting. Linux-only.
#[cfg(target_os = "linux")]
#[path = "linux/autostart.rs"]
pub mod linux_autostart;
// Launch-at-login on Windows (DRAGON-237): an `HKCU\...\Run` registry value, the
// counterpart of `mac::login_item` (SMAppService) and `linux_autostart` (XDG). Drives the
// resident tray daemon back after a login; wired to the same `resident` setting. Windows-only.
#[cfg(target_os = "windows")]
#[path = "windows/autostart.rs"]
pub mod windows_autostart;
// Portable Windows chrome helpers: ex-style bit names, `HRESULT` formatting, and the
// Windows 10-vs-11 build classification. Compiled on EVERY platform on purpose — it contains
// no Win32, and living in the shared tree is what lets the Linux gate unit-test it. What is
// left is the residue of DRAGON-406's diagnostics report after DRAGON-407 deleted that
// instrument: see the module doc for which three helpers survived and why.
pub mod win_diag;

/// Opt OUR-app window titled `title` OUT of automatic tiling by the user's tiling window
/// manager — AeroSpace on macOS, komorebi on Windows — where possible. This is the single
/// portable entry point for per-window tiling opt-out: each platform implements its own
/// `opt_out_of_tiling` arm, and every arm is scoped to OUR OWN windows (matched by our
/// process AND the exact title), so it can never grab another app's window. Idempotent; a
/// no-op where there is no tiling WM. To keep a window un-tiled, call this instead of
/// reaching into a platform module; the capture overlays are the first consumers.
///
/// macOS timing: the AeroSpace opt-out must land before the window's first AX exposure, so
/// the mac arm keeps a pre-order-front `orderFront:` swizzle that strips our above-normal
/// overlays automatically; calling this additionally installs that swizzle and strips a
/// matching already-visible window as a backstop. On Windows the komorebi bit likewise
/// wants to be set before first show — the overlays' `place_overlay` does that directly;
/// this seam is the general entry point.
///
/// No Linux/COSMIC arm: the capture overlays are layer-shell surfaces COSMIC never tiles,
/// and a real toplevel that wants to float opts out via a user-managed COSMIC WindowRules
/// tiling exception (documented in the README, alongside the AeroSpace/komorebi rules — not
/// an app-written config). Hence the seam is scoped to the two platforms where opting a live
/// window out of tiling is a per-window op.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn opt_out_of_tiling(title: &str) {
    #[cfg(target_os = "macos")]
    crate::platform::mac::window::opt_out_of_tiling(title);
    #[cfg(target_os = "windows")]
    crate::platform::windows::window::opt_out_of_tiling(title);
}

/// Env var carrying the TRIGGER display's name from a pickerless daemon spawn to the
/// capture child (DRAGON-309). A hotkey / tray daemon (macOS + Windows) has no picker
/// overlay to read an active output from, so it resolves the CURSOR's monitor AT PRESS
/// TIME — while the pointer is still where the user pressed — and hands its name here,
/// the same env-handoff mechanism `CCK_ACTIVE_WIN_ID` uses for the active window. The
/// child seeds its trigger display (where the post-capture preview opens) from it. Off
/// Linux the value is a display NAME string (the `OutputHandle`); Linux has no global
/// pointer and no capture-hotkey daemon, so it never sets or reads this. Cross-platform
/// const so the daemon producer and the child consumer agree by one name.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub const ENV_TRIGGER_DISPLAY: &str = "CCK_ACTIVE_DISPLAY";

/// The display name the CURSOR sits on RIGHT NOW, for a pickerless daemon spawn to hand
/// the capture child via [`ENV_TRIGGER_DISPLAY`] (DRAGON-309). Resolves the pointer's
/// monitor from the live display list (`monitor_for_pointer` semantics: the display under
/// the pointer, else the primary, else the first). `None` only when there are no displays.
/// macOS reads the global pointer (`NSEvent.mouseLocation`); Windows `GetCursorPos`. Linux
/// has no global pointer and no capture-hotkey daemon, so this seam is not built there.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn cursor_display_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    let (pointer, descs) = (
        Some(crate::platform::mac::global_pointer_position()),
        crate::platform::mac::output_descs(),
    );
    #[cfg(target_os = "windows")]
    let (pointer, descs) = (
        crate::platform::windows::cursor_position(),
        crate::platform::windows::output_descs(),
    );
    crate::app::monitor_for_pointer(pointer, &descs).map(|d| d.name)
}

/// Snapshot the TRIGGER display's NAME at LAUNCH (DRAGON-309), BEFORE the picker overlay is
/// shown / before the user moves the cursor to draw a region or pick a window and before our
/// layer-shell overlay grabs focus. This is THE authoritative "monitor active when the
/// capture was initiated" value: sampling it at capture COMMIT is wrong, because by then the
/// cursor sits on the TARGET monitor and the focused window is our own overlay. Stored on the
/// `App` and returned by `active_trigger_display()` at commit (resolved to a rect then).
///
/// - **macOS / Windows**: the daemon press-time handoff [`ENV_TRIGGER_DISPLAY`] wins (a hotkey
///   / tray launch), else the CURSOR's monitor sampled RIGHT NOW at init (a direct launch — the
///   cursor is still on the trigger monitor before any picker UI appears).
/// - **Linux (COSMIC)**: Wayland has no global pointer and no capture-hotkey daemon, so the
///   active display is the FOCUSED toplevel's output (`list_toplevels`, the `active` toplevel)
///   sampled at init, before our overlay takes focus. Just the name; the rect resolves later.
///
/// `None` when nothing resolves (no displays / no focused toplevel) — the caller then falls
/// back to the selection's output, keeping the DRAGON-304 immediate-capture behavior.
pub fn snapshot_trigger_display_name() -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        std::env::var(ENV_TRIGGER_DISPLAY)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(cursor_display_name)
    }
    #[cfg(target_os = "linux")]
    {
        crate::platform::compositor::list_toplevels()
            .into_iter()
            .find_map(|(output, tops)| tops.iter().any(|t| t.active).then_some(output))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

/// The CURRENT display of OUR OWN window titled `title`, as an `OutputHandle` NAME
/// (DRAGON-309). Used when the windowed preview editor is toggled to the fullscreen
/// overlay: the user may have DRAGGED the window to another monitor since it opened, so
/// the overlay must spawn where the window ACTUALLY is now, not on the stored capture-time
/// display. Each platform reads the window's live monitor natively: macOS matches the
/// `NSWindow`'s `screen` to its `Display-<id>`; Windows `MonitorFromWindow` to the device
/// name. `None` when the window isn't found (fall back to the capture-time display).
///
/// Off Linux only: on Linux the preview is a layer-shell / CSD toplevel whose output the
/// compositor owns; the toggle keeps its existing (capture-time) anchor there (see the
/// caller's Linux note), so this seam is scoped to the two String-`OutputHandle` platforms.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn window_current_display(title: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::window::window_current_display(title)
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::window::window_current_display(title)
    }
}

/// CAPTURE units per POINT for the output named `name` — the ONE per-platform input to
/// [`crate::geometry::OverlayUnits`], the capture-overlay units bridge (DRAGON-448).
///
/// Read [`OverlayUnits`](crate::geometry::OverlayUnits) for what the two spaces are and why
/// they differ; this only answers "by how much, on this output, on this OS":
///
/// - **Windows**: that monitor's `dpi / 96` (`GetDpiForMonitor`, per-monitor under
///   Per-Monitor-Aware-V2). Its `OutputDesc`s are PHYSICAL virtual-screen pixels while the
///   overlay's iced viewport is logical points, so this is the whole gap. A 100% monitor
///   answers `1.0`, which is why a 96-DPI box is byte-identical.
/// - **macOS**: `1.0`. Its `OutputDesc`s are CoreGraphics POINTS and the overlay's app space
///   is points too, so the two spaces already coincide. A Retina display is `1.0` HERE even
///   though its backing scale is 2.0 — that factor belongs to captured MEDIA
///   (`scale_for_selection` / `source_scale`), not to overlay layout, and conflating them
///   would halve every mac overlay.
/// - **Linux**: `1.0`. The layer surface is sized by the compositor in its own logical
///   space, which is what `output_descs()` reports; app space IS point space.
///
/// An unknown name (a display that vanished between enumeration and here) answers `1.0`:
/// the unscaled identity, i.e. exactly the pre-DRAGON-448 behaviour, never a guess.
// Dead on LINUX, honestly: both callers are compiled out there — `seed_overlays_mac` is
// `cfg(not(linux))`, and `preview::surface::monitor_point_scale` asks only on its
// `cfg(not(linux))` arm. Linux never needs to ask, because its answer is the identity by
// construction (the compositor sizes the layer surface in the same logical space
// `output_descs()` reports). The body stays portable rather than Windows-only so the
// Linux/macOS answers remain stated here, where the doc above explains all three.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn overlay_point_scale(name: &str) -> f32 {
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::scale_for(name)
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = name;
        1.0
    }
}

// ── Windows OS-build gates (DRAGON-403) ───────────────────────────────────────
// PURE build-number predicates, deliberately kept in the SHARED tree (not under
// `platform/windows/`) so `cargo test` proves them on ANY host — the Windows arms they
// gate can only be compiled on Windows, and DRAGON-403 is a Windows-10-only bug nobody
// on this project can run. The version READ (`platform::windows::window::os_build`,
// `RtlGetVersion` with a registry fallback) stays Windows-native; only the
// "build number in → is this feature permitted" decision lives here, where it is testable.

/// The first Windows build that is **Windows 11** (21H2 = build 22000). Everything below
/// it is Windows 10 or older, where the Win11 DWM window model our chrome assumes does
/// not exist. Distinct from the Mica floor (`platform::windows::window::MICA_MIN_BUILD`
/// = 22621, Win11 22H2): Mica is a strictly LATER, narrower gate.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN11_MIN_BUILD: u32 = 22000;

/// Pure: may this OS `build` be asked to paint the **native DWM caption buttons** over our
/// frameless settings / preview toplevels (the DRAGON-284 recipe —
/// `DwmExtendFrameIntoClientArea` top margin + a `DwmDefWindowProc`-first subclass)?
///
/// Win11 (22000+) only. On Windows 10 the buttons hit-test (DwmDefWindowProc computes the
/// cluster from window metrics regardless of build) but are never SEEN — DRAGON-403's
/// "clickable but invisible" report. Two mechanisms can produce that, and this gate covers
/// BOTH because it stops depending on DWM to paint the buttons at all:
///
/// 1. **The client covers them.** DWM composites the caption cluster in the extended-frame
///    layer BEHIND the window's own content, so it is only visible where that content is
///    transparent. Our chrome paints transparent ONLY when `app::theme::glass_config` says
///    frosted (Windows arm: Mica, i.e. build ≥ 22621); below that `frost_color` leaves the
///    window base fully OPAQUE — so on Windows 10 the header paints over the strip.
/// 2. **Win10's DWM does not composite a caption cluster there at all** for a window whose
///    caption region is CLIENT (our `WM_NCCALCSIZE` leaves the top flush).
///
/// Below the gate the toplevels keep their own CSD min/max/close instead (the pre-DRAGON-284
/// Windows path, still wired: those messages route to the native `toggle_maximize` /
/// `minimize` helpers). Win11 is unaffected — it takes the exact path it ships today.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_paints_native_caption_buttons(build: u32) -> bool {
    build >= WIN11_MIN_BUILD
}

// DRAGON-408 DELETED the second DRAGON-403 gate that lived here,
// `win_build_layered_keeps_per_pixel_alpha` — "below Win11, withhold `WS_EX_LAYERED` from the
// capture overlays, because a layered top-level driven by `SetLayeredWindowAttributes`
// composites at a CONSTANT alpha and flattens the swapchain's per-pixel alpha to solid black".
//
// Its premise is DISPROVEN from both directions:
//
// * Windows 11 renders the overlay CORRECTLY while carrying the exact configuration the gate
//   blamed — `WS_EX_LAYERED` set and `SetLayeredWindowAttributes(key=0, alpha=255, LWA_ALPHA)`
//   called. If LWA_ALPHA destroyed per-pixel alpha, Win11 would be black too.
// * The gate WORKED and changed nothing. A real Windows 10 run with the DRAGON-406 diagnostics
//   read back `layered_tier=false ex_actual=0x00000199 slwa=not-called` — the bit withheld
//   exactly as designed, the OS agreeing — and the overlay was still 40/40 pure black.
//
// The real constraint sits a level BELOW the window style, and it is not OS-keyed: wgpu's DX12
// backend hardcodes `composite_alpha_modes = [Opaque]` for every HWND-target surface
// (`wgpu-hal/src/dx12/adapter.rs`, a plain match on the surface target with no OS check),
// because DXGI does not offer premultiplied alpha on `CreateSwapChainForHwnd`. Windows 11 gets
// that same `[Opaque]` swapchain and is translucent anyway — so its translucency rides on
// undefined behaviour that some GPU/driver/DWM composition paths honour and others do not
// (the WARP software adapter does not, which is why a Win11 VM is black too). The supported
// fix is a DirectComposition swapchain; see `place_overlay`'s doc for why the app cannot
// currently ask for one.
//
// So the gate could never have helped, and it COST DRAGON-280's protection against a
// fullscreen hardware-overlay video flipping the capture overlay away on Windows 10. The
// overlay is layered again on every Windows build, exactly as DRAGON-280 shipped it.
//
// The CAPTION half of DRAGON-403 (`win_build_paints_native_caption_buttons`, above) is
// untouched: that one is confirmed working on Windows 10 and stays.

/// The first Windows 10 build (RTM 1507 = 10240) — the floor for the undocumented
/// `SetWindowCompositionAttribute` accent policy, which arrived with Windows 10 and does not
/// exist on 8.1 or earlier.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN10_MIN_BUILD: u32 = 10240;

/// Pure: does this OS `build` take the **blur-behind** accent policy as its frosted-windows
/// material (DRAGON-405)? Windows 10 ONLY — `[10240, 22000)`.
///
/// This is the Win10 analog of Win11's Mica: `ACCENT_ENABLE_BLURBEHIND` via the undocumented
/// `SetWindowCompositionAttribute`. Deliberately NOT acrylic
/// (`ACCENT_ENABLE_ACRYLICBLURBEHIND`), which carries a documented, never-fixed drag/resize
/// input lag on Windows 10 1903+ (the OS is EOL, so it will not be fixed); blur-behind is the
/// standard workaround and is the performant one.
///
/// The window is CLOSED at both ends on purpose:
/// * below 10240 the accent policy does not exist (8.1 and older);
/// * at/above [`WIN11_MIN_BUILD`] every Windows 11 build keeps EXACTLY today's behavior —
///   Mica at ≥ 22621 (`platform::windows::window::mica_supported`) and, for the 22000..22620
///   band (Win11 21H2, which has no Mica), the same "no glass, fully opaque" look it has now.
///   Win11 must not gain a new material from this change.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_has_blur_behind_glass(build: u32) -> bool {
    win_build_is_windows_10(build)
}

/// Pure: is this OS `build` **Windows 10** — the closed band `[10240, 22000)`?
///
/// Extracted (DRAGON-406) from [`win_build_has_blur_behind_glass`], which had carried the
/// band inline since DRAGON-405 and now delegates here — ONE definition of "this is Windows
/// 10" for every caller that needs one.
///
/// KEPT by DRAGON-407 when the diagnostics instrument that shared it was deleted:
/// `win_build_has_blur_behind_glass` is a shipped Win10 feature and is defined in terms of
/// this.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_is_windows_10(build: u32) -> bool {
    (WIN10_MIN_BUILD..WIN11_MIN_BUILD).contains(&build)
}

// ── DRAGON-427: Windows 10 renders its overlays in SOFTWARE ───────────────────
//
// wgpu cannot make a Windows 10 window translucent on ANY backend, and this is settled
// rather than suspected:
//
// * DX12 — `wgpu-hal/src/dx12/adapter.rs` returns a constant keyed on the surface target,
//   and an HWND surface is ALWAYS `[Opaque]`. There is no OS check to differ on.
// * GLES — a hardcoded `vec![Opaque]` carrying a literal `//TODO`.
// * Vulkan — queries the driver, but WezTerm (Rust, same stack) tested it directly and
//   found window transparency broken on Windows; they left wgpu for their own renderer.
//
// Windows 11 is translucent anyway, riding composition behaviour that Windows 10's DWM
// does not honour (see the DRAGON-408 note above). So the split is real and not ours to
// fix inside wgpu.
//
// What a customer PROVED on real Windows 10 hardware: running with iced's SOFTWARE
// rasterizer (tiny-skia) makes the capture overlay actually translucent. That is the fix,
// and it is why these two predicates exist. They are separate names, not one, because they
// answer different questions and only happen to share a band today.

/// Pure: must this OS `build` render our OVERLAY surfaces with iced's software rasterizer
/// (DRAGON-427)? Windows 10 ONLY — the closed band [`win_build_is_windows_10`].
///
/// Windows 11 stays on wgpu, byte-identical: it is translucent there today and moving it to
/// a CPU rasterizer would cost frame rate for nothing.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_software_overlays(build: u32) -> bool {
    win_build_is_windows_10(build)
}

/// Pure: may the preview EDITOR be the fullscreen OVERLAY on this OS `build` (DRAGON-427)?
///
/// Everywhere except Windows 10. On Windows 10 the overlay-shaped editor would inherit the
/// software rasterizer that makes overlays translucent there — and the editor draws its
/// media through `cosmic::iced::widget::shader` (`app/preview/layers.rs`), which tiny-skia
/// cannot render at all. So the editor is ALWAYS the windowed variant on Windows 10, and
/// the setting that offers the choice is disabled there rather than merely hidden: a user
/// whose config already says `preview_windowed = false` must land on the window, not on a
/// broken overlay.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_has_overlay_preview(build: u32) -> bool {
    !win_build_is_windows_10(build)
}

/// Runtime seam: must THIS machine software-render its overlays? Windows 10 only; `false`
/// everywhere else, so Linux and macOS keep their exact behaviour with no `cfg` at the
/// call site. The build number itself comes from the Windows-native reader.
pub fn software_overlays() -> bool {
    #[cfg(windows)]
    {
        // An UNREADABLE build number reads as "not Windows 10" — the safe side: we keep the
        // GPU renderer that works on every OS we ship for rather than silently degrading a
        // machine we could not identify. Same convention as `native_caption_buttons_supported`.
        crate::platform::windows::window::os_build().is_some_and(win_build_software_overlays)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Runtime seam: may the preview editor open as the fullscreen overlay on THIS machine?
/// `true` everywhere except Windows 10 (see [`win_build_has_overlay_preview`]).
pub fn overlay_preview_available() -> bool {
    #[cfg(windows)]
    {
        // An unreadable build keeps the overlay editor — the same safe side as
        // `software_overlays` (the two must agree, or a process could end up software-rendered
        // WITH an overlay editor, which is the one combination that cannot draw).
        crate::platform::windows::window::os_build()
            .is_none_or(win_build_has_overlay_preview)
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// The env var that flips the Windows 10 overlay between the two candidate window shapes
/// (DRAGON-408). Temporary — see [`win_overlay_is_layered`].
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN10_LAYERED_ENV: &str = "CCK_WIN10_LAYERED";

/// Pure: should the capture overlay carry `WS_EX_LAYERED` on this OS `build`, given the
/// raw value of [`WIN10_LAYERED_ENV`]?
///
/// **This exists to settle an argument with one measurement, not to be a setting.** The
/// Windows 10 overlay renders solid black where it should be translucent. The surviving
/// hypothesis is that Windows 10's DWM honours `SetLayeredWindowAttributes(LWA_ALPHA, 255)`
/// literally — constant alpha, per-pixel alpha discarded — while Windows 11's ignores it in
/// favour of the swapchain's own alpha. Every piece of REAL evidence fits that:
///
/// * real Windows 10 hardware — overlay opaque, with the bit set;
/// * real Windows 11 hardware — overlay correct, with the same bit and the same call.
///
/// DRAGON-403 tested it by withholding the bit below [`WIN11_MIN_BUILD`], and DRAGON-408
/// reverted that on a measurement showing the withholding made no difference. **That
/// measurement is void**: it was taken on the WARP software adapter (`device_type=Cpu`),
/// which renders this overlay black on *any* Windows version and any window style, so it
/// could not have shown a difference either way. The hypothesis was never actually tested.
///
/// Nobody on the team has Windows 10 hardware, so the only route is a customer running both
/// shapes and reporting which one is translucent. Hence a runtime flip rather than a build:
/// one binary, two runs, no guessing.
///
/// Contract:
/// * **Windows 11 and newer is untouched, whatever the env says.** The bit is always set
///   there. It works today and must stay byte-identical.
/// * Windows 10 defaults to LAYERED — today's behaviour, so a customer who never sets the
///   variable sees no change.
/// * Only the exact value `"0"` withholds it, and only on Windows 10. Anything else
///   (unset, `"1"`, empty, junk) means layered, because a typo must not silently give up
///   DRAGON-280's protection against a fullscreen hardware-overlay video flipping the
///   overlay away.
///
/// Delete this the moment the answer is known — it is an experiment, and an experiment left
/// in the tree becomes a configuration surface nobody can remove later.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_overlay_is_layered(build: u32, env: Option<&str>) -> bool {
    if !win_build_is_windows_10(build) {
        return true;
    }
    env != Some("0")
}

/// Pure, unit-tested: may the OVERLAY PREVIEW take the foreground after a successful native
/// placement (DRAGON-469)?
///
/// Two independent terms, and BOTH have to hold:
///
/// * `pre_open` — this surface is the DRAGON-305 pre-open BLOCKER cover, not the editor. That
///   cover exists to hide a single-window grab that is running RIGHT NOW on another thread,
///   and `place_overlay` is already handed `activate = !pre_open` for exactly that reason:
///   taking the foreground would flip the window being grabbed to its INACTIVE chrome
///   (the DRAGON-278 bug) and can make `foreground_and_verify` fail, which silently skips the
///   DRAGON-308 glass region grab. So a pre-open cover may NEVER be raised, no matter who
///   holds the foreground. This term is the whole reason the rule is a predicate and not an
///   `if` at the call site: the first version had only the second term, and it fired straight
///   into the middle of a live grab.
/// * `foreground_is_other` — somebody ELSE owns the foreground, so our activating show was
///   refused by the foreground lock. When we already hold it there is nothing to take, and
///   asking anyway would be a needless `AttachThreadInput` dance.
///
/// Kept in the shared tree rather than in `platform/windows/` for the reason the whole
/// `win_build_*` family is: nobody here can run the Windows path, so the reasoning has to be
/// provable on Linux. The two READS (`win_preview_preopen` off the app, the live foreground
/// owner off Win32) stay native.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn overlay_preview_should_refocus(pre_open: bool, foreground_is_other: bool) -> bool {
    !pre_open && foreground_is_other
}

// ── DRAGON-426: proving nothing of the user's is behind a captured window ─────
//
// A Windows single-window capture may preserve the window's transparency (DRAGON-275). The
// moment it does, whatever the DWM composited BEHIND that window is visible through it — so a
// capture of one window can contain another window entirely, and the person sharing it has no
// way to notice. That is a confidentiality failure, not a cosmetic one.
//
// DRAGON-308's answer is to float our own opaque wallpaper backdrop directly beneath the
// target, so the only thing behind it is something we drew. The answer is sound; what was
// missing is that nothing ever CHECKED it landed. These predicates are that check, kept pure
// and in the shared tree so the Linux gate covers the reasoning even though the Win32 that
// feeds them can only run on Windows.

/// One rung of the z-order chain beneath a captured window: `(hwnd bits, frame rect)`, where
/// the rect is physical `(x, y, w, h)`. The handle travels as `isize` rather than an `HWND` so
/// the whole decision stays portable and testable off Win32.
#[cfg_attr(not(windows), allow(dead_code))]
pub type WinZOrderRung = (isize, (i32, i32, i32, i32));

/// Whether two `(x, y, w, h)` rects share any area (half-open edges).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

/// Whether `outer` fully contains `inner` (both `(x, y, w, h)`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_rect_contains(outer: (i32, i32, i32, i32), inner: (i32, i32, i32, i32)) -> bool {
    let (ox, oy, ow, oh) = outer;
    let (ix, iy, iw, ih) = inner;
    ix >= ox && iy >= oy && ix + iw <= ox + ow && iy + ih <= oy + oh
}

/// Pure: is our floated backdrop genuinely the ONLY thing behind the target, across the whole
/// of the target's rect (DRAGON-426)?
///
/// `below` is the z-order chain beneath the target, TOP-FIRST, as `(hwnd bits, frame rect)`
/// (`platform::windows::window_list::windows_below`). The answer is yes only when the FIRST
/// window in that chain that touches the target's rect is our backdrop AND that backdrop covers
/// the rect completely.
///
/// Every other shape is a leak waiting to happen, and each one has a real cause:
///
/// * **A foreign window reached first** — `SetWindowPos` seated the backdrop somewhere other
///   than immediately below the target (a topmost band mismatch, an owner relationship, a
///   window raised between the float and the grab).
/// * **The desktop reached first** — `Progman` / a `WorkerW`. That is where Wallpaper Engine,
///   Lively Wallpaper and every other animated-desktop tool reparent themselves: below normal
///   windows, above the wallpaper bitmap. A hide-other-windows sweep written against "normal
///   top-level application windows" misses them by construction, which is why this check is
///   about Z-ORDER and coverage rather than about what kind of window anything is.
/// * **A partly-covering backdrop** — a strip of the real desktop is still behind the target.
/// * **An empty chain** — the float did not take at all.
///
/// Fails CLOSED: anything it cannot prove is treated as unsafe, because the cost of a false
/// "safe" is the user mailing someone else's window to a colleague, and the cost of a false
/// "unsafe" is a capture that keeps its glass a little less faithfully.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_backdrop_seated_below(
    below: &[WinZOrderRung],
    backdrop: isize,
    target: (i32, i32, i32, i32),
) -> bool {
    for &(hwnd, rect) in below {
        if !win_rects_overlap(target, rect) {
            continue; // does not touch the target — cannot show through it
        }
        // The first window that DOES touch it decides: ours and covering, or unsafe.
        return hwnd == backdrop && win_rect_contains(rect, target);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_caption_buttons_are_windows_11_only() {
        // Win11 21H2 (the first 22000 build) and everything after it: the DRAGON-284
        // native DWM caption buttons stay exactly as they ship today.
        assert!(win_build_paints_native_caption_buttons(22000)); // 21H2
        assert!(win_build_paints_native_caption_buttons(22621)); // 22H2 (the Mica floor)
        assert!(win_build_paints_native_caption_buttons(22631)); // 23H2
        assert!(win_build_paints_native_caption_buttons(26100)); // 24H2
        assert!(win_build_paints_native_caption_buttons(u32::MAX)); // future builds
        // Windows 10 (any servicing level) and older: CSD buttons instead (DRAGON-403).
        assert!(!win_build_paints_native_caption_buttons(19045)); // 22H2, the last Win10
        assert!(!win_build_paints_native_caption_buttons(19041)); // 2004
        assert!(!win_build_paints_native_caption_buttons(10240)); // Win10 RTM
        assert!(!win_build_paints_native_caption_buttons(9600)); // Win8.1
        // A build number that could not be read at all reads as 0 → treated as Win10,
        // i.e. the SAFE side: we draw our own buttons rather than trust DWM to.
        assert!(!win_build_paints_native_caption_buttons(0));
    }

    // DRAGON-408 deleted `layered_per_pixel_alpha_is_windows_11_only` along with the gate it
    // pinned. `WS_EX_LAYERED` is unconditional on every Windows build again (DRAGON-280's
    // shape), so there is no build-keyed decision left to test — see the note above
    // `WIN10_MIN_BUILD` for why the gate's premise was wrong.

    #[test]
    fn win11_is_always_layered_whatever_the_env_says() {
        // DRAGON-408: Windows 11 works today. The experiment must not be able to reach it,
        // including by a customer who sets the variable globally and then runs on Win11.
        for build in [WIN11_MIN_BUILD, 22621, 26100, u32::MAX] {
            for env in [None, Some("0"), Some("1"), Some("")] {
                assert!(
                    win_overlay_is_layered(build, env),
                    "build {build} env {env:?} must stay layered"
                );
            }
        }
    }

    #[test]
    fn win10_defaults_to_layered_and_only_an_exact_zero_withholds() {
        // Default = today's shipped behaviour, so an untouched install changes nothing.
        assert!(win_overlay_is_layered(19045, None));
        assert!(win_overlay_is_layered(19045, Some("1")));
        // The experiment.
        assert!(!win_overlay_is_layered(19045, Some("0")));
        // Anything ambiguous keeps the bit: a typo must not silently drop DRAGON-280's
        // MPO protection, which is the failure a customer would never think to report.
        for junk in ["", " ", "0 ", "00", "false", "no", "O"] {
            assert!(
                win_overlay_is_layered(19045, Some(junk)),
                "{junk:?} must not be read as off"
            );
        }
    }

    #[test]
    fn the_layered_experiment_covers_exactly_the_windows_10_band() {
        // Same band as every other Win10 predicate — one definition of "this is Windows 10",
        // so the experiment cannot drift from the thing it is experimenting on.
        for build in [WIN10_MIN_BUILD, 19041, 19045, WIN11_MIN_BUILD - 1] {
            assert!(!win_overlay_is_layered(build, Some("0")), "build {build} is in-band");
        }
        for build in [0, 7600, WIN10_MIN_BUILD - 1, WIN11_MIN_BUILD] {
            assert!(win_overlay_is_layered(build, Some("0")), "build {build} is out of band");
        }
    }

    // Every caller that asks "is this Windows 10" must get the SAME answer, so they can never
    // drift into disagreeing about which builds they apply to.
    // `win_build_has_blur_behind_glass` delegates to `win_build_is_windows_10`; this pins that
    // they stay one band, band-for-band. (Introduced by DRAGON-406, when its diagnostics log
    // was the second caller; kept by DRAGON-407, which deleted that log.)
    #[test]
    fn windows_10_band_is_one_definition() {
        for build in [0, 9600, 10240, 19041, 19045, 21999, 22000, 22621, 26100, u32::MAX] {
            assert_eq!(
                win_build_is_windows_10(build),
                win_build_has_blur_behind_glass(build),
                "build {build} must classify identically for both"
            );
        }
        // And the band itself is the closed [10240, 22000) DRAGON-405 established.
        assert!(!win_build_is_windows_10(10239));
        assert!(win_build_is_windows_10(10240));
        assert!(win_build_is_windows_10(21999));
        assert!(!win_build_is_windows_10(22000));
        // An unreadable build reads as "not Windows 10" -> diagnostics stay off, which is
        // the safe side: a Win11 user must never be able to tell the feature exists.
        assert!(!win_build_is_windows_10(0));
    }

    /// DRAGON-427: the software-renderer gate and the overlay-editor gate are BOTH the
    /// Windows 10 band and nothing else, band-for-band with `win_build_is_windows_10`. Two
    /// names for one band on purpose (they answer different questions), so this is what
    /// stops them drifting apart — and what pins that **Windows 11 is untouched**, which is
    /// the ticket's hard constraint: it renders correctly on wgpu today and must not be
    /// moved onto a CPU rasterizer or lose its overlay editor.
    #[test]
    fn the_software_renderer_gate_is_exactly_the_windows_10_band() {
        for build in [0, 9600, 10240, 19041, 19045, 21999, 22000, 22621, 26100, u32::MAX] {
            assert_eq!(
                win_build_software_overlays(build),
                win_build_is_windows_10(build),
                "build {build}: software overlays must be exactly the Win10 band"
            );
            // The overlay EDITOR is the complement: available everywhere the software
            // rasterizer is not forced.
            assert_eq!(
                win_build_has_overlay_preview(build),
                !win_build_software_overlays(build),
                "build {build}: the two gates must stay exact complements"
            );
        }
        // Spelled out at the edges, so a future band edit has to break a named assertion.
        assert!(!win_build_software_overlays(10239)); // Windows 8.1 and older
        assert!(win_build_software_overlays(10240)); // Win10 RTM
        assert!(win_build_software_overlays(19045)); // Win10 22H2, the last one
        assert!(win_build_software_overlays(21999)); // just under the Win11 floor
        assert!(!win_build_software_overlays(22000)); // Win11 21H2 — wgpu, as today
        assert!(!win_build_software_overlays(26100)); // Win11 24H2 — wgpu, as today
        // An unreadable build reads as "not Windows 10", i.e. keep the GPU renderer and the
        // overlay editor. That is the SAFE side: a machine we cannot identify keeps the
        // path that works on every OS we ship for, rather than being silently degraded.
        assert!(!win_build_software_overlays(0));
        assert!(win_build_has_overlay_preview(0));
    }

    #[test]
    fn blur_behind_glass_is_windows_10_only() {
        // Windows 10, RTM through the last servicing build: the blur-behind accent policy is
        // the frosted-windows material (DRAGON-405).
        assert!(win_build_has_blur_behind_glass(10240)); // 1507 RTM
        assert!(win_build_has_blur_behind_glass(19041)); // 2004
        assert!(win_build_has_blur_behind_glass(19045)); // 22H2, the last Win10
        assert!(win_build_has_blur_behind_glass(21996)); // just under the Win11 floor
        // Windows 11, EVERY build: unchanged. 22000..22620 keeps its current no-glass look
        // (Mica starts at 22H2), 22621+ keeps Mica — neither may pick up blur-behind.
        assert!(!win_build_has_blur_behind_glass(22000)); // 21H2 — no glass, as today
        assert!(!win_build_has_blur_behind_glass(22621)); // 22H2 — Mica, as today
        assert!(!win_build_has_blur_behind_glass(26100)); // 24H2 — Mica, as today
        // Windows 8.1 and older have no accent policy at all; an unreadable build reads as 0.
        assert!(!win_build_has_blur_behind_glass(9600)); // 8.1
        assert!(!win_build_has_blur_behind_glass(0));
    }

    #[test]
    fn exactly_one_windows_glass_material_per_build() {
        // The two materials must never both claim a build (they would fight over the same
        // window) and Win10 must never be left materialless. `mica_supported` (≥ 22621) lives
        // in the Windows plugin, so it is modelled here by its floor.
        let mica = |b: u32| b >= 22621;
        for build in [0, 9600, 10240, 19041, 19045, 21999, 22000, 22620, 22621, 26100, u32::MAX] {
            assert!(
                !(mica(build) && win_build_has_blur_behind_glass(build)),
                "build {build} claimed by both materials"
            );
        }
        // Every real Windows 10 build gets exactly the blur-behind material.
        for build in [10240_u32, 14393, 17763, 19041, 19045] {
            assert!(win_build_has_blur_behind_glass(build));
            assert!(!mica(build));
        }
    }

    #[test]
    fn every_mica_build_also_clears_the_win11_gates() {
        // The Windows gates are ordered: the Mica floor (22H2 = 22621, checked in
        // `platform::windows::window`) sits ABOVE the Win11 floor, so everything that gets
        // Mica also gets the native caption buttons — never the reverse (22000..22621 is
        // Win11 without Mica: buttons yes, Mica no). DRAGON-408: the layered-alpha gate that
        // used to be asserted alongside is gone; the overlay is layered on every build.
        for build in [22621_u32, 22631, 26100] {
            assert!(win_build_paints_native_caption_buttons(build));
        }
        assert!(win_build_paints_native_caption_buttons(22000));
    }

    // ── DRAGON-426: the backdrop seating predicate ────────────────────────────
    //
    // Each case below is a distinct way a capture of ONE window could come back containing
    // another. They are written as such, not as geometry trivia.

    /// The window being captured, for the seating cases.
    const TARGET: (i32, i32, i32, i32) = (100, 100, 400, 300);

    #[test]
    fn seated_when_our_backdrop_is_first_below_and_covers_the_target() {
        // The good case: our backdrop is exactly the target's footprint and first in the chain,
        // so the only thing the glass can show is what we drew.
        let below = [(7, TARGET), (9, (0, 0, 1920, 1080))];
        assert!(win_backdrop_seated_below(&below, 7, TARGET));
    }

    #[test]
    fn a_larger_backdrop_still_counts_as_covering() {
        // Covering MORE than the target is fine — every pixel of the target is backed.
        assert!(win_backdrop_seated_below(&[(7, (0, 0, 1920, 1080))], 7, TARGET));
    }

    #[test]
    fn windows_that_miss_the_target_never_veto_the_grab() {
        // A window below the target but nowhere near it cannot show through the glass, so it
        // must not block the good path — the backdrop behind it still decides.
        let below = [(3, (1400, 700, 200, 200)), (7, TARGET)];
        assert!(win_backdrop_seated_below(&below, 7, TARGET));
    }

    #[test]
    fn a_foreign_window_reached_first_is_not_seated() {
        // THE reported bug: something of the user's sits between the target and our backdrop,
        // so its pixels are behind the target's translucency and land in the saved capture.
        let below = [(3, (200, 150, 600, 400)), (7, TARGET)];
        assert!(!win_backdrop_seated_below(&below, 7, TARGET));
    }

    #[test]
    fn the_desktop_reached_first_is_not_seated() {
        // Progman / WorkerW — where Wallpaper Engine and Lively Wallpaper reparent themselves.
        // Reaching the DESKTOP before our backdrop means a live wallpaper is what shows through
        // the glass, which is exactly the second half of the customer's report. Note the check
        // never asks what KIND of window this is: keying on a product or a window class is what
        // makes such a sweep miss the next tool that uses the same trick.
        let below = [(11, (0, 0, 1920, 1080)), (7, TARGET)];
        assert!(!win_backdrop_seated_below(&below, 7, TARGET));
    }

    #[test]
    fn a_backdrop_that_only_partly_covers_the_target_is_not_seated() {
        // A short backdrop leaves a strip of the real desktop behind the target's lower edge…
        assert!(!win_backdrop_seated_below(&[(7, (100, 100, 400, 200))], 7, TARGET));
        // …and an offset one leaves a strip along the top.
        assert!(!win_backdrop_seated_below(&[(7, (100, 150, 400, 300))], 7, TARGET));
    }

    #[test]
    fn an_empty_chain_is_not_seated() {
        // `SetWindowPos` silently did not take, or the backdrop never became visible: nothing
        // of ours is behind the target, so there is no guarantee at all. Fail CLOSED.
        assert!(!win_backdrop_seated_below(&[], 7, TARGET));
    }

    // ── DRAGON-469: raising the overlay preview after its native placement ──────

    /// The rule, row by row. The pre-open BLOCKER cover is NEVER raised, whoever holds the
    /// foreground: it is covering a single-window grab that is running on another thread, and
    /// stealing the foreground there flips the target to its inactive chrome and can fail
    /// `foreground_and_verify` (which silently skips the DRAGON-308 glass grab). Only a real
    /// editor surface that has actually LOST the foreground is raised.
    #[rstest::rstest]
    #[case::editor_backgrounded(false, true, true)]
    #[case::editor_already_foreground(false, false, false)]
    #[case::pre_open_cover_backgrounded(true, true, false)]
    #[case::pre_open_cover_foreground(true, false, false)]
    fn the_overlay_preview_is_raised_only_when_it_is_the_editor_and_lost_the_foreground(
        #[case] pre_open: bool,
        #[case] foreground_is_other: bool,
        #[case] want: bool,
    ) {
        assert_eq!(overlay_preview_should_refocus(pre_open, foreground_is_other), want);
    }

    /// The pre-open term is the DOMINANT one, and it must stay that way: `place_overlay` is
    /// handed `activate = !pre_open` three lines from the call site, so a raise during a
    /// pre-open would contradict the show that produced the window.
    #[test]
    fn a_pre_open_cover_is_never_raised_whatever_the_foreground_says() {
        for foreground_is_other in [false, true] {
            assert!(!overlay_preview_should_refocus(true, foreground_is_other));
        }
    }

    #[test]
    fn rect_helpers_agree_with_their_names() {
        // Half-open edges: touching is not overlapping.
        assert!(win_rects_overlap((0, 0, 100, 100), (50, 50, 100, 100)));
        assert!(!win_rects_overlap((0, 0, 100, 100), (100, 0, 100, 100)));
        // Containment is inclusive of coincident edges.
        assert!(win_rect_contains((0, 0, 100, 100), (0, 0, 100, 100)));
        assert!(win_rect_contains((0, 0, 100, 100), (10, 10, 10, 10)));
        assert!(!win_rect_contains((0, 0, 100, 100), (10, 10, 100, 10)));
    }
}
