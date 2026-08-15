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
//! | `platform::mac::secrets` | `services/secrets.rs` | `mac/mod.rs` | macos | closed-split (DRAGON-482) |
//! | `platform::mac::upload_tray` | `services/upload_tray.rs` | `mac/mod.rs` | macos | closed-split (DRAGON-482) |
//! | `record::sck` | `mac/screencapturekit/record_worker.rs` | `record/mod.rs` | macos | closed-split |
//! | `record::sck_live_tests` | `mac/screencapturekit/record_worker_live_tests.rs` | `record/mod.rs` | test+macos | closed-split |
//! | `audio::ducking::duck_mac` | `mac/services/duck_mac/mod.rs` | `audio/ducking.rs` | macos | closed-split |
//! | `audio::ducking::media_control` | `windows/media_control.rs` | `audio/ducking.rs` | windows | closed-split (DRAGON-283) |
//!
//! `closed-split` (DRAGON-226): whole mac-native files homed under `platform/mac/` so
//! `scripts/publish-public.sh` can strip the closed platform plugins from the public
//! Linux tree in one directory cut. Shared-core `#[cfg]` glue stays public by design.
//!
//! Not every plugin file is a MOUNT. `linux::upload_tray` and `windows::upload_tray`
//! (DRAGON-482's cloud upload counter, the third tray-family surface after the recording
//! tray and the resident) are plain `pub mod` declarations in their plugin's own `mod.rs`,
//! because they are NEW paths rather than legacy ones being preserved; only the mac file
//! needs a `#[path]`, and only because the mac plugin sorts its bodies into facet folders.
//! The seam all three fill is `cloud::upload::tray`, which owns the number, the glyph and
//! the wording, so those three files paint and nothing else.
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
// Portable Windows cursor-capture geometry (DRAGON-567): the pure decision that a captured
// cursor draws at its bitmap's own physical size (the GetIconInfo bitmap is already
// display-scaled, so no dpi/96 resample), compiled on EVERY platform so the Linux gate
// pins it. The GDI reads stay in `windows/cursor.rs`; see the module doc for the
// double-scale dead end this replaces.
pub mod win_cursor;

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
///
/// **Windows 10 was tried with a composited surface (DRAGON-666) and it is REFUSED, on
/// measurement.** The two mechanisms above are one mechanism, and it is not "the buttons do
/// not paint": they paint exactly where our chrome is TRANSPARENT, and on Windows 10 the
/// strip they paint into has no backdrop material behind it, so that is where the window
/// shows solid BLACK. The A/B, same build, same machine (Windows 10 19045, settings window):
///
/// | chrome | caption strip | buttons |
/// | --- | --- | --- |
/// | glass (transparent) | a black band across the titlebar | visible |
/// | `CCK_NO_GLASS=1` (opaque) | no band, chrome is uniform | GONE |
///
/// So on Windows 10 native buttons and a black titlebar are the same fact, and the trade is
/// not worth taking. Windows 11 is unaffected because Mica fills that strip.
///
/// What would change the answer is giving the strip a material on Windows 10 — the
/// blur-behind accent policy we already apply for the client area does NOT reach the
/// extended frame region, which is what this measured. Until something does, Windows 10
/// keeps the CSD buttons, and it now keeps them over chrome that is finally translucent.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_paints_native_caption_buttons(build: u32, _dcomp: bool) -> bool {
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
///
/// `dcomp` (DRAGON-666) is the way OUT of this: a DirectComposition surface has real
/// per-pixel alpha on Windows 10 too — measured, translucent, GPU-rendered — so the CPU
/// rasterizer has nothing left to fix and this returns false. The force survives only as the
/// fallback for a run that turned DComp off.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_software_overlays(build: u32, dcomp: bool) -> bool {
    win_build_is_windows_10(build) && !dcomp
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
///
/// `dcomp` (DRAGON-666) lifts it on Windows 10: the ban exists only because the software
/// rasterizer cannot draw the editor's shader layers, and with a DirectComposition surface
/// that process keeps wgpu. So the editor's overlay shape, its settings row and its toolbar
/// toggle all come back on Windows 10, which is the whole reason this parameter exists.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_build_has_overlay_preview(build: u32, dcomp: bool) -> bool {
    !win_build_software_overlays(build, dcomp)
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
        let dcomp = dcomp_enabled();
        crate::platform::windows::window::os_build()
            .is_some_and(|b| win_build_software_overlays(b, dcomp))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Runtime seam: may the preview editor open as the fullscreen overlay on THIS machine?
///
/// Two independent reasons it can be `false`, and every caller wants the same answer for
/// both — the button that offers the appearance, the settings row behind it, the toggle
/// handler, and the mint decision itself:
///
/// * **Windows 10** (see [`win_build_has_overlay_preview`]): the overlay editor would
///   inherit the software rasterizer that cannot draw its shader layers.
/// * **Linux with no `zwlr_layer_shell_v1`** (`lab/flatpak`): the Linux overlay preview IS a
///   layer surface (`app::shell::preview_surface_on` → `get_layer_surface`), so a session
///   that cannot see the global has no overlay editor to offer at all. Protocol-keyed for
///   the same reason [`layer_overlay_available`] is: sandboxed under cosmic-comp's
///   security-context filter and "mutter never implemented it" are different causes with
///   one right answer. A session that CAN see the global is byte-identical to before.
///
/// macOS, Windows 11 and every layer-shell Linux session answer `true`, as they always have.
pub fn overlay_preview_available() -> bool {
    #[cfg(windows)]
    {
        // An unreadable build keeps the overlay editor — the same safe side as
        // `software_overlays` (the two must agree, or a process could end up software-rendered
        // WITH an overlay editor, which is the one combination that cannot draw).
        let dcomp = dcomp_enabled();
        crate::platform::windows::window::os_build()
            .is_none_or(|b| win_build_has_overlay_preview(b, dcomp))
    }
    #[cfg(target_os = "linux")]
    {
        layer_overlay_available()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        true
    }
}

/// Runtime seam: may the capture overlay be a LAYER-SHELL surface on this machine, or must it
/// fall back to a plain fullscreen toplevel (`lab/flatpak`)?
///
/// Linux answers from the live protocol probe, so this is protocol-keyed rather than
/// desktop-keyed or sandbox-keyed, exactly like the capture backend selection next to it. That
/// matters because there are two quite different reasons `zwlr_layer_shell_v1` can be missing
/// and the overlay code should not care which it is hitting:
///
/// * **Sandboxed.** cosmic-comp hides the layer-shell global from clients carrying a
///   `wp_security_context_v1`, which is every Flatpak. The protocol is there, we are just not
///   allowed to see it.
/// * **Never implemented.** mutter has never shipped `wlr-layer-shell` at all, so a GNOME
///   session has no layer shell for anyone, sandboxed or not.
///
/// Either way the registry simply does not advertise it, `probe_globals` records `false`, and
/// the overlay takes the plain-toplevel path. A launch that CAN see the global is byte-identical
/// to before this existed, which is what keeps the normal Linux build unaffected.
///
/// macOS and Windows return false and always have: they never had layer shell and their
/// overlays are the PlainWindows path already.
// Its only caller is `app::shell::overlay_surface_with`, which is `cfg(target_os = "linux")`,
// so this is honestly dead off Linux. The body stays portable rather than being gated to Linux
// because the false arm IS the correct answer for mac and Windows, and a caller that ever asks
// there should get it rather than fail to compile.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn layer_overlay_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::platform::backend::wayland_protocols().layer_shell
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// **Pure**, unit-tested: can an overlay surface of this kind HIDE the pointer sprite
/// (DRAGON-587, then DRAGON-597)?
///
/// **Both kinds can, since DRAGON-597. This answers `true` for every input.** The parameter
/// stays because it names the axis that USED to decide the answer, and because the tests below
/// are what pin that the axis is gone.
///
/// The colour picker wants the cursor gone while you aim: its magnifier already marks the
/// sample point, and an arrow would sit on top of the very pixel being read. iced expresses
/// that as [`cosmic::iced::core::mouse::Interaction::Hidden`].
///
/// A **winit toplevel** (every macOS and Windows overlay, and on Linux the whole
/// portal-fallback path) has always reached `Window::set_cursor_visible(false)`, which is a
/// real hide. A **layer surface** did not: libcosmic's Wayland backend left
/// `set_cursor_visible` an unimplemented `// TODO` for the surfaces it manages itself (layer,
/// popup, lock, subsurface), so asking there hid nothing. That hole is now filled by our iced
/// fork (see the iced `[patch]` block in `Cargo.toml`), which implements the method as the
/// canonical `wl_pointer.set_cursor(serial, NULL, 0, 0)`.
///
/// # Tombstone: what this predicate was for, and why it stops here (DRAGON-587)
///
/// The gap was never COSMIC-specific and this signature said so: it was keyed on the surface
/// KIND, so it opened wherever our overlay was a layer surface (COSMIC, sway, hyprland, river
/// alike) and closed wherever it was a plain toplevel (GNOME, since mutter has never shipped
/// `wlr-layer-shell`; every Flatpak launch, since cosmic-comp hides the global from a
/// sandboxed client; and the portal-fallback path anywhere). A `false` was not "give up": it
/// selected the picker's OTHER design, a default ARROW whose hotspot is its tip with the
/// sample read one point up and left of it, the nearest pixel the arrow did not cover.
///
/// Three routes to hiding a layer surface's pointer were searched from inside this app. Two
/// are still closed and are recorded so nobody repeats the search:
///
/// 1. **A CUSTOM (transparent) cursor image.** Blocked twice over. iced's public vocabulary is
///    `mouse::Interaction`, a closed enum of NAMED shapes, and `conversion::mouse_interaction`
///    returns `Option<CursorIcon>`, the `cursor-icon` named set: no variant is transparent and
///    none carries a buffer. Below that, the layer-surface `set_cursor` still handles only the
///    named arm, with `Cursor::Custom(_) => { /* TODO */ }`. Our fork did not touch this one.
/// 2. **This repo's own cursor plumbing** (`widgets::cursor_reassert`, DRAGON-331). It does
///    not reach lower than the enum. It solved a TIMING problem (cosmic-comp drops a
///    `set_cursor` issued too soon after `wl_pointer.enter`, so the widget re-asserts past
///    that window) by returning different `Interaction` VALUES at different moments. It never
///    touches a `wl_pointer`.
///
/// The third route is the one taken. `wl_pointer.set_cursor(serial, NULL, 0, 0)` was always
/// one method call away: iced holds an sctk `ThemedPointer` (`SctkSeat.ptr`) and already calls
/// `ptr.set_cursor(conn, icon)` on it, and the next method on that object is `hide_cursor()`.
/// What blocked it from OUR side was purely visibility: the pointer is `pub(crate)` to
/// iced_winit, the layer backend's only action was `SetCursor(CursorIcon)`, and the public
/// `iced_runtime::platform_specific::wayland::Action` enum that `send_wayland_action_direct`
/// forwards has no cursor variant at all. Minting our own `wl_pointer` could never work
/// either: `set_cursor` needs the serial of a `wl_pointer.enter` for one of the CALLER'S own
/// surfaces, and a second Wayland connection owns none, which is exactly the
/// `PointerThemeError::MissingEnterSerial` sctk returns. So the fix had to be a fork, and
/// DRAGON-597 made it one.
///
/// # When to delete this seam
///
/// Once the owner has confirmed the hide on a live native COSMIC session (the ONE place a
/// layer-shell overlay exists, and the one thing no test here can prove), this function and
/// [`overlay_pointer_hideable`] are both constants and should GO rather than linger as a `fn`
/// returning `true`. `overlay_pointer_tests` goes with them, and
/// `app::color_picker::view`'s `.hide_pointer(…)` becomes `.hide_pointer(true)`. Keeping them
/// until then is deliberate: it is what makes reverting to the arrow fallback a clean revert
/// if the fork turns out not to work on a real compositor.
///
/// Pure so the answer is provable on any host; [`overlay_pointer_hideable`] is the reader.
pub fn overlay_hides_pointer(_layer_surface: bool) -> bool {
    true
}

/// Runtime seam: can THIS session's capture-shaped overlay hide the pointer sprite?
///
/// Since DRAGON-597 the answer is yes everywhere, but the shape is kept: Linux still asks
/// whether the overlay is a layer surface at all, from the same protocol probe
/// `app::shell::overlay_surface_with` mints them with, so if the fork ever has to be dropped
/// the answer goes back to tracking what was actually created rather than which desktop is
/// running. macOS and Windows are PlainWindows toplevels and always could.
pub fn overlay_pointer_hideable() -> bool {
    #[cfg(target_os = "linux")]
    {
        overlay_hides_pointer(layer_overlay_available())
    }
    #[cfg(not(target_os = "linux"))]
    {
        overlay_hides_pointer(false)
    }
}

/// **Pure**, unit-tested: can a shortcut bound in THIS app's own keymap reach a recording
/// that is ALREADY IN PROGRESS (DRAGON-583)?
///
/// There are exactly two ways a keypress gets from the user to `App::handle_key` while a
/// recording runs, and a session needs only one of them:
///
/// * `focus_free_hotkeys`: something binds the chord process-wide, so it arrives whatever
///   has focus. On Linux that is the xdg-desktop-portal `GlobalShortcuts` interface, and
///   the input is a REAL probe of the live session
///   ([`global_shortcuts::interface_available`]), never a cfg and never a Flatpak test: the
///   interface can be absent because the desktop never shipped it (COSMIC today) or because
///   a sandbox hid it, and the answer we need is the same either way. On macOS and Windows
///   it is a constant `false`, and that is a fact about our own code rather than an
///   assumption: both resident daemons register exactly the seven CAPTURE hotkey slots
///   (`CaptureHotkeySlot::ALL`) and nothing recording-related, `global_shortcuts::start` is
///   a do-nothing stub off Linux, and there is no event tap or keyboard hook anywhere in
///   the tree.
/// * `keeps_keyboard_focus`: one of OUR surfaces still owns the keyboard while the
///   recording runs, so ordinary key events reach us. macOS and Windows keep their overlay
///   windows up at record start (they only turn click-through), and nothing on those
///   platforms takes the focus away, so the chord can still land. LINUX is the opposite on
///   BOTH of its paths, deliberately: a native session hands focus straight back to the
///   window being recorded (`App::start_recording` calls `compositor::activate`, so you can
///   type into the app you are recording), and the portal-fallback session destroys its one
///   toplevel outright at record start. Either way nothing of ours is left to press a key
///   into.
///
/// Where this answers `false`, the three Recording rows in Settings → Keyboard Shortcuts
/// are DEAD controls, and the settled rule for a control that cannot apply is to hide it
/// and say what does work instead (the DRAGON-551 / 569 / 577 hide-where-dead line). What
/// works there is the CLI: `--toggle-mic`, `--toggle-system-audio`, `--pause-recording`,
/// `--finish-recording` and `--cancel-recording` each reach the live recording through the
/// resident relay's own command words (`crate::daemon_ipc`), so a desktop-level global
/// hotkey can drive a recording exactly the way one already launches a capture.
pub fn in_app_recording_shortcut_reachable(
    focus_free_hotkeys: bool,
    keeps_keyboard_focus: bool,
) -> bool {
    focus_free_hotkeys || keeps_keyboard_focus
}

/// Runtime seam: [`in_app_recording_shortcut_reachable`] for the session we are actually
/// running in, so a caller needs no `cfg` of its own.
///
/// Linux probes the portal once per process and pins `keeps_keyboard_focus` to `false`;
/// every other platform is the constant pair the doc above justifies. Read by the settings
/// page, which must not advertise a chord it knows cannot fire.
pub fn in_app_recording_shortcuts_work() -> bool {
    #[cfg(target_os = "linux")]
    let (focus_free, keeps_focus) = (global_shortcuts::interface_available(), false);
    #[cfg(not(target_os = "linux"))]
    let (focus_free, keeps_focus) = (false, true);
    in_app_recording_shortcut_reachable(focus_free, keeps_focus)
}

/// Does THIS BUILD register the global CAPTURE hotkeys itself (DRAGON-589)?
///
/// Honestly a compile-time fact, and named here once so no caller has to write the `cfg`.
/// The registration lives in the two resident daemons and nowhere else: macOS
/// (`platform::mac::daemon`, a Carbon hotkey per `CaptureHotkeySlot`) and Windows
/// (`platform::windows::daemon`, `RegisterHotKey` per slot). Neither exists in a Linux build.
/// Linux's capture keys are the DESKTOP's own custom shortcuts, pointing at this binary with
/// a flag, which is a different mechanism owned by a different program.
///
/// This decides which shape a Global Capture row takes: a chord editor where we can bind the
/// key, otherwise the command a user pastes into their desktop's shortcut settings. It is NOT
/// the question of whether the action exists at all; that one is
/// `capture_flow::immediate_capture_available`, and an action missing from the build gets no
/// row of either shape.
pub const fn app_registers_capture_hotkeys() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

// `toplevel_clamped_to_work_area` lived here (DRAGON-549): a desktop-profile seam saying
// whether the window system clamps an over-large new toplevel to the work area, so the
// windowed preview could ask COSMIC for the whole output height. DRAGON-579 deleted it
// with the full-height ask it served: the README-recommended floating exception routes
// our windows through the placement path that SKIPS cosmic-comp's map-time clamp, native
// sessions included, so "the compositor source says it clamps" was never a guarantee on
// the machines that follow our own docs. The height budget is the DRAGON-221 guess again
// (`sizing::USABLE_H_FRAC`, where the full story lives).

/// Pure, unit-tested: should a LINUX interactive capture launch seed the PORTAL-FROZEN
/// fallback overlay (`lab/flatpak`) instead of per-output layer surfaces?
///
/// The fallback exists for the session where the capture overlay cannot be a layer
/// surface (`layer_overlay` false: sandboxed under cosmic-comp's security-context filter,
/// or a compositor that never implemented `wlr-layer-shell`) but the session-clamped
/// capture choice lands on the portal anyway (`uses_portal`), so a full-monitor frame CAN
/// be grabbed through ScreenCast and region selection can run over that frozen frame in
/// one ordinary fullscreen toplevel. Both terms are protocol/caps seams, never a sandbox
/// probe: a global can be missing because we are refused it OR because the compositor
/// never shipped it, and the fallback wants the same answer for both.
///
/// Deliberately NOT keyed on the async portal reachability probe: at output-seed time
/// that probe may not have resolved yet, and the seed-time ScreenCast request is its own
/// proof: an unreachable portal answers `Unavailable`, which the handler turns into a
/// loud `fail_session`, never a silent exit. A layer-shell session (`layer_overlay`
/// true) answers `false` unconditionally, which is what keeps a normal COSMIC launch
/// byte-identical.
///
/// This is a LINUX decision only. Its caller (`App::overlay_fallback_active`) returns a
/// constant `false` on macOS/Windows, whose overlays are the PlainWindows path and where
/// `uses_portal` is spuriously true (no Wayland screencopy exists to prefer).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn overlay_fallback_seeding(layer_overlay: bool, uses_portal: bool) -> bool {
    !layer_overlay && uses_portal
}

/// **Pure**, unit-tested: does a saved capture-method choice, CLAMPED to this session, land
/// on the portal?
///
/// A preference for native screencopy cannot apply on a compositor that does not offer it, so
/// a session with no native capture is on the portal whatever the config says. This is the
/// rule `App::screenshot_uses_portal` / `App::recording_uses_portal` apply; it lives out here
/// as well because the tray DAEMON has to reach the same answer, and it is a separate process
/// with no `App` at all (DRAGON-555).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn capture_choice_uses_portal(saved_backend_id: &str, native_capture: bool) -> bool {
    saved_backend_id == backend::PORTAL_ID || !native_capture
}

/// Runtime seam: is THIS session's interactive capture overlay the PORTAL-FROZEN fallback
/// rather than a real layer-shell overlay (`lab/flatpak`, DRAGON-555)?
///
/// The same question `App::overlay_fallback_active` answers inside a capture process, asked
/// from OUTSIDE one. The tray daemon needs it to decide which capture entries its menu can
/// honestly offer, and the daemon is a separate process: no `App`, no GUI stack, nothing to
/// ask. It has the same two ingredients available anyway, because it is the same binary:
///
/// * the Wayland protocol probe (one throwaway registry connection, cached per process), and
/// * the persisted capture-method choices, which it already re-reads for the tray accent.
///
/// Protocol-keyed and preference-keyed, never sandbox-keyed, so a normal COSMIC session
/// answers false and the menu it renders is byte-identical to before this existed.
///
/// macOS and Windows return false and always will: their overlays are the PlainWindows path
/// and the portal this fallback grabs through does not exist there. Its DRAGON-555 callers
/// were the two Linux trays, which used it to slim their capture menus; DRAGON-558 reverted
/// that slimming (the owner moved the audio pre-arm into the tray's persistent Enable
/// items, so a portal-picker launch no longer skips anything the user cannot set), which
/// left this with no caller at all. It stays: the QUESTION — "is this session's capture
/// overlay the portal-frozen fallback, asked from outside a capture process" — remains
/// real, the ingredients and caching are non-obvious, and the next out-of-process
/// fallback-shaped decision should find the answer here rather than rebuild it.
#[allow(dead_code)]
pub fn portal_fallback_session() -> bool {
    #[cfg(target_os = "linux")]
    {
        let protocols = backend::wayland_protocols();
        let native = protocols.image_copy_capture && protocols.output_source;
        let saved = crate::state::load();
        // Either capture kind landing on the portal is enough, matching the app's own OR: in
        // the session this fallback exists for, both are true anyway.
        let uses_portal = capture_choice_uses_portal(&saved.screenshot_backend, native)
            || capture_choice_uses_portal(&saved.record_backend, native);
        overlay_fallback_seeding(protocols.layer_shell, uses_portal)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// The env var that flips the Windows 10 overlay between the two candidate window shapes
/// (DRAGON-408). Temporary — see [`win_overlay_is_layered`].
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN10_LAYERED_ENV: &str = "CCK_WIN10_LAYERED";

// ── DRAGON-666: the DirectComposition presentation experiment ────────────────
//
// The question every Windows overlay bug since DRAGON-403 has actually been asking, asked
// properly for once.
//
// What we do today: wgpu presents the overlay from a `CreateSwapChainForHwnd` surface,
// which reports `composite_alpha_modes = [Opaque]` and nothing else, so iced's alpha
// selection falls through to `Auto`. The window is translucent anyway ONLY because winit
// calls `DwmEnableBlurBehindWindow` with an empty region at creation
// (`winit-win32/src/window.rs`), which makes DWM honour the alpha channel of the window's
// REDIRECTION SURFACE. That holds exactly as long as DWM composites the window through
// that surface — and a fullscreen, topmost, monitor-sized window (ours, precisely) is the
// prime candidate for the driver to promote onto its own hardware plane, where nothing
// applies the alpha and the raw RGB shows: transparent black, i.e. the black overlay.
//
// That is one mechanism for FOUR reports we treated as three different bugs: Windows 10
// hardware (DRAGON-403/427), the WARP adapter in a VM (DRAGON-408's void measurement), and
// now a Windows 11 NVIDIA desktop. The variable was never the OS version. It was whether
// this machine's composition path shows per-pixel alpha at all.
//
// The supported alternative is a DirectComposition swapchain. `wgpu-hal`'s DX12 backend
// already builds one from a plain HWND (`Dx12SwapchainKind::DxgiFromVisual`), and that
// surface reports `PostMultiplied` + `PreMultiplied`, which iced's selection already
// prefers over `Auto`. So the whole change is one environment variable — plus the one-line
// iced-fork commit that makes iced read the environment at all (see `FORKED_CHANGES.md`).
//
// It is an EXPERIMENT and it is opt-in, because it must be measured on machines nobody
// here owns: a Windows 11 box that is black today, and a Windows 10 box (which currently
// takes the tiny-skia force instead, and would never reach a swapchain to configure).
// Delete it the moment the answer is known — an experiment left in the tree becomes a
// configuration surface nobody can remove later.

/// The env var that turns the DRAGON-666 DirectComposition experiment on.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN_DCOMP_ENV: &str = "CCK_WIN_DCOMP";

/// The value `WGPU_DX12_PRESENTATION_SYSTEM` takes when the experiment is on: wgpu's own
/// spelling for "present through a DirectComposition visual built from the HWND".
#[cfg_attr(not(windows), allow(dead_code))]
pub const WGPU_DCOMP_VALUE: &str = "DxgiFromVisual";

/// The wgpu env var that selects the DX12 presentation system.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WGPU_PRESENTATION_ENV: &str = "WGPU_DX12_PRESENTATION_SYSTEM";

/// The wgpu env var that picks the BACKEND, and the value that pins it to DX12.
///
/// **Setting the presentation system without this does nothing on most real hardware**,
/// which is how the first build reached a customer and rendered every window solid: wgpu
/// picked VULKAN on her GTX 1080 Ti, `WGPU_DX12_PRESENTATION_SYSTEM` is a DX12-only option,
/// and the Vulkan surface reports `composite_alpha_modes = [Opaque]` exactly as the HWND one
/// does. Her log named it in one line — `backend: Vulkan` — which is the whole reason that
/// line is now captured (see `diag::DEP_INFO_TARGET_PREFIX`).
///
/// The two VMs never showed it because WARP has no Vulkan at all (their logs carry
/// `Returned GL context is 1.1` beside it), so they fell to DX12 and the option applied.
/// A VM is not a GPU; this is the second time that has cost a wrong conclusion here.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WGPU_BACKEND_ENV: &str = "WGPU_BACKEND";
/// wgpu's own spelling for the DX12 backend (`Backends::from_comma_list`).
#[cfg_attr(not(windows), allow(dead_code))]
pub const WGPU_BACKEND_DX12: &str = "dx12";

// **Tombstone: `WGPU_LATENCY_ENV` / `WGPU_LATENCY_DONTWAIT`** (DRAGON-685, same day it
// added them). The palette viewer's panel toggle froze on a stale frame for 1 to 3
// seconds after every resize, because wgpu's DX12 swapchain waits on DXGI's frame-latency
// waitable before each acquire and `ResizeBuffers` leaves that waitable unsignaled until
// a later `Present`: the first post-resize acquires each blocked for wgpu-core's full 1s
// frame timeout, UI thread and all. Setting the waitable mode to `DontWait` (via wgpu's
// `WGPU_DX12_USE_FRAME_LATENCY_WAITABLE_OBJECT`, which iced patch 3 reads from the
// environment) removed the stall — and introduced a WORSE defect: that wait is the only
// backpressure pacing presents to the display's rate, so a drag's per-event redraw storm
// queued frames faster than the compositor drains them and the drag ghost trailed the
// pointer by up to a second. `None` measured the same. So the variable is not an answer
// at either value, the constants are gone, and the stall is fixed where it lives: the
// wgpu-hal fork bounds the waitable wait once after an in-place `ResizeBuffers`
// (FORKED_CHANGES.md carries the patch and all three measurements — the first cut of
// that patch SKIPPED the wait instead, which leaked pacing credits on every interactive
// resize tick and un-paced the settings window the same way).

/// Pure, unit-tested: does this run present through a DirectComposition visual, given the
/// raw value of [`WIN_DCOMP_ENV`]?
///
/// **DEFAULT ON**, and only the exact value `"0"` turns it off. It shipped as an opt-in
/// experiment and became the default the moment it was measured, on both Windows versions:
///
/// * Windows 10 19045 and Windows 11 26200, region overlay, control arm: 97.8% and 99.1% of
///   the frame PURE BLACK, the app's own UI drawn correctly over it. The customer's report,
///   reproduced with no NVIDIA hardware anywhere near it.
/// * The same two machines with this on: translucent, the desktop visible through the dim.
/// * The same two machines' logs: an HWND surface offers `[Opaque]` and NOTHING else, so the
///   alpha selection falls through to `Auto`; a composition surface offers `PostMultiplied`
///   and `PreMultiplied`, and the renderer takes one.
///
/// The escape hatch is the exact `"0"` and it exists because this is the presentation path
/// for every Windows window we own: if some driver hates it, a customer can get the old
/// behaviour back in one environment variable while we fix it, rather than downgrading.
/// Anything else — unset, `"1"`, empty, junk — is on.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_dcomp_enabled(env: Option<&str>) -> bool {
    env != Some("0")
}

/// Runtime seam: is THIS process presenting through a DirectComposition visual? The env read
/// behind [`win_dcomp_enabled`], for the gates below that have to ask.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn dcomp_enabled() -> bool {
    #[cfg(windows)]
    {
        win_dcomp_enabled(std::env::var(WIN_DCOMP_ENV).ok().as_deref())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

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

// ──────────────── macOS: a stray capture overlay must not land in a capture ────────────────
//
// DRAGON-TBD (no ticket filed yet; replace this marker when one is). Reported as: with
// "freeze pixels" on, a region capture SOMETIMES comes back with a ghost of a PREVIOUS
// capture's selection area in the frozen pixels. With freeze on that is not a preview
// artefact: the saved file IS a crop of the launch-time flats (`capture_flow::crop_frozen`),
// so whatever the flats grab photographed is what the user sends to someone.
//
// WHERE IT COMES FROM, verified by direct experiment rather than by reading. Launch a region
// capture, leave its overlay up, and run a second process through the same grab
// (`--test mac-shot`, which calls `platform::mac::capture_output`): the PNG that comes back
// contains the first instance's dim wash, its "Begin drawing a capture region" hint and its
// whole toolbar, as ordinary desktop pixels. `capture_output` built its `SCContentFilter`
// with an unconditionally EMPTY exclusion list, so nothing of ours was ever kept out of it.
// Capture instances are deliberately concurrent (DRAGON-351 deleted the single-instance
// capture lock outright) and nothing tears a sibling's overlay down until that sibling
// COMMITS (`instance::close_other_instances`, called from `do_pixel_capture`), so a second
// hotkey press while the first overlay is still on screen is an ordinary thing to do.
//
// WHY PID MATCHING WOULD NOT HAVE FIXED IT, and this is the crux. The nearest existing
// exclusion, `mac::recording_display_target_excluding_own_ui`, matches
// `owningApplication.processID == std::process::id()`. The ghost is a DIFFERENT PROCESS:
// same app, same binary, another one-shot capture child. Identity here has to be the
// APPLICATION, which is what [`MacAppIdentity`] and [`mac_same_app`] express.
//
// WHY A LEVEL BAND rather than "every window we own". Our ordinary toplevels (the settings
// window, the windowed preview editor, the colour picker's result window) sit at the normal
// window level and are legitimate capture subjects: somebody documenting this app must still
// be able to photograph its own UI. Capture chrome does not, and is identifiable without a
// title match, which matters because `place_overlay` titles every overlay with its DISPLAY
// name and the colour picker mints the same windows with the same titles. Measured on this
// machine: a placed overlay reports `windowLayer == CGShieldingWindowLevel == 2147483628`,
// and winit parks one at `kCGFloatingWindowLevel == 3` between creation and placement, so
// the whole set sits strictly above the ordinary level of 0.
// The menu-bar item is not a concern either way: macOS hosts `NSStatusItem` windows in
// Control Center's process (verified, the item reports `owner=Control Center`), so it is
// never in the exclusion set and a full-display capture keeps a gap-free menu bar.
//
// THE COLOUR PICKER IS CARVED OUT, and this is not a detail. DRAGON-608 shipped, on the
// owner's explicit correction, the behaviour that "an ordinary region capture, started over a
// live overlay with the existing keybinds, must produce a correct image of that overlay" —
// photographing the picker's loupe is the whole of what that ticket delivered, and
// `capture_flow::begin_capture`'s doc records that it works precisely BECAUSE the exclusion
// "reaches exactly ONE process, our own". A filter keyed on app identity alone would delete
// that feature. The picker's overlay is indistinguishable from a capture overlay by every
// window property there is (same process, same level, same `Display-<id>` title), so the
// carve-out cannot be made from the window: it is made from the OWNER PROCESS's argv
// (`instance::is_color_picker_instance`), the same source `is_settings_instance` and
// `is_resident_instance` already read.
//
// Our OWN process is excluded unconditionally, with no picker carve-out, because a picker
// launch grabs the flats it will later sample from: `app::color_picker`'s `PixelSource` doc
// says a live read would return our own dimming layer instead of the desktop.

/// The CoreGraphics window level ordinary application windows sit at
/// (`kCGNormalWindowLevel`, measured 0). Anything of ours strictly above it is capture
/// chrome rather than content. See the note above.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MAC_APP_WINDOW_LAYER: isize = 0;

/// macOS: the handles the system offers for "which application owns this window".
///
/// Three of them, because no single one answers on every build. `pid` is free and exact but
/// only ever recognises the current process. `exe` is the identity
/// `instance::close_other_instances` already uses to recognise a macOS sibling, and it is the
/// one that works for an unbundled `cargo run` dev binary, which has no bundle identifier at
/// all. `bundle_id` is what recognises two instances launched from DIFFERENT copies of the
/// app, a dev build beside the one in `/Applications`.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default)]
pub struct MacAppIdentity<'a> {
    /// The owning process id, or `0` when ScreenCaptureKit named no owning application.
    pub pid: i32,
    /// The owning process's executable image (`proc_pidpath`), or `None` when it could not
    /// be read (a process that exited between the snapshot and this call).
    pub exe: Option<&'a std::path::Path>,
    /// The owning application's bundle identifier, or `None` for an unbundled binary.
    pub bundle_id: Option<&'a str>,
}

/// **Pure**, unit-tested: do two identities name the same APPLICATION, i.e. this process or
/// any other running instance of the same app?
///
/// Any ONE of the three handles agreeing is enough, because each covers a case the others
/// cannot. Deliberately NOT an all-three match: a dev binary has no bundle identifier, and a
/// sibling's exe path is unreadable once it has exited.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_same_app(a: MacAppIdentity<'_>, b: MacAppIdentity<'_>) -> bool {
    // Same process. Cheapest, and the only handle that needs no syscall. `0` is SCK's "no
    // owning application", so it never matches, not even another 0.
    if a.pid != 0 && a.pid == b.pid {
        return true;
    }
    if let (Some(x), Some(y)) = (a.exe, b.exe)
        && x == y
    {
        return true;
    }
    // Empty never matches empty: SCK reports an unbundled process's identifier as an empty
    // string, and treating that as an identity would make every unbundled process on the
    // system "us".
    match (a.bundle_id, b.bundle_id) {
        (Some(x), Some(y)) => !x.is_empty() && x == y,
        _ => false,
    }
}

/// **Pure**, unit-tested: must this window be kept out of a whole-display grab because it is
/// a stray piece of THIS APP's capture chrome?
///
/// `owner_is_color_picker` is the DRAGON-608 carve-out and applies to SIBLINGS only: a live
/// colour picker is something the user may deliberately be photographing, and no window
/// property can tell its overlay apart from a capture overlay. See the note above this
/// function for why the carve-out has to come from the owner's argv.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_window_is_stray_chrome(
    owner: MacAppIdentity<'_>,
    me: MacAppIdentity<'_>,
    window_layer: isize,
    owner_is_color_picker: bool,
) -> bool {
    if window_layer <= MAC_APP_WINDOW_LAYER {
        return false; // ordinary application window: content, not chrome
    }
    // Our own chrome, always. Nothing this process draws belongs in a scene this process is
    // reading, and that holds for a picker launch too (its own dim would otherwise become
    // the pixels it later samples).
    if owner.pid != 0 && owner.pid == me.pid {
        return true;
    }
    !owner_is_color_picker && mac_same_app(owner, me)
}

// ───────────────────────── Launch at login: what the row may claim ─────────────────────────
//
// DRAGON-628. The "Automatically start on login" row rendered the PERSISTED PREFERENCE, which
// is what the user asked for and not what the system is doing. On the owner's machine the two
// had been apart for a day: the config said on, the autostart entry named an AppImage that
// DRAGON-590 relocated, and the session refused it at every login
// (`systemd-xdg-autostart-generator: not generating unit, executable specified in Exec= does
// not exist`) while the row kept claiming the feature was on. DRAGON-625 made
// `linux_autostart::is_enabled` honest about exactly that entry, but nothing on the display
// path ever asked it.
//
// The usual split. The DECISION (what may the row claim, given a preference and whatever
// reality we could observe) is pure and lives here, so `cargo test` proves it on any host. The
// READ is per-platform and lives in the plugins.
//
// **Reading and repairing are separate occasions, on purpose.** Opening the settings window
// only READS: it probes, displays reality and logs a disagreement, and writes nothing at all,
// so a window opening can never issue a portal request, re-create an entry the user removed,
// or (on an unbundled mac dev build, where `set` always fails) correct a stored preference
// nobody withdrew. The REPAIR lives in the resident daemon's startup instead, which is the
// better home for it: the daemon is the thing autostart exists to launch, so if it is running
// then that is the moment its own registration should be made to match, with nobody opening
// anything. A login that worked keeps working, and one that did not is fixed the first time
// the daemon runs by any other route.
//
// The rule that is not obvious: reality is not always observable, and a build that cannot
// observe it must not guess. Two builds cannot:
//
//   * A **Flatpak** registers through the Background portal, which writes the entry on the
//     HOST, where a sandbox with no `--filesystem=home` cannot read it. Nor does the portal
//     offer a read: `RequestBackground` is a REQUEST, asynchronous and possibly interactive.
//     Probing by asking would put a dialog on screen every time Settings opened.
//   * An **unbundled macOS dev binary** has no `SMAppService` to query at all
//     (`login_item::Availability::Unbundled`), so its `is_enabled()` answers `false` for a
//     process that could never have registered anything, which is not the same as "off".
//
// Both answer `None`, and `None` means "show the stored preference". On the Flatpak that is
// honest for a second reason: `linux_autostart::settled_preference` already writes the
// portal's real answer into the preference, so there it IS the last known truth.

/// What the "Automatically start on login" row may claim, given the stored preference and
/// whatever the OS could be asked.
///
/// Three states rather than a bare `bool` because the two ways of arriving at the same
/// rendered value are not the same event: one is a build that cannot know, the other is a
/// build that looked and found a contradiction, and only the second is worth telling the
/// debug log about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartRow {
    /// Reality could not be read (see the note above), so the stored preference is all
    /// anyone has and the row shows it. This is what every platform used to do
    /// unconditionally.
    Unobservable(bool),
    /// Reality was read and it agrees with the preference. Nothing to say.
    Agrees(bool),
    /// Reality was read and it CONTRADICTS the preference. The row shows reality, because
    /// the row's job is to say what the machine will do at the next login. The preference is
    /// left exactly as it is: the user asked for it, we simply could not deliver it yet.
    Disagrees { shown: bool, preference: bool },
}

impl AutostartRow {
    /// What the toggle renders.
    pub fn shown(self) -> bool {
        match self {
            AutostartRow::Unobservable(v) | AutostartRow::Agrees(v) => v,
            AutostartRow::Disagrees { shown, .. } => shown,
        }
    }
}

/// **Pure**, unit-tested: classify the autostart row from the stored preference and the
/// observed registration (`None` when this build has no read-only way to find out).
///
/// Reality wins wherever there is any, in BOTH directions. A preference of "on" against a
/// dead entry shows off, which is the case that produced this ticket. A preference of "off"
/// against a live entry shows on, which is the same rule and matters just as much: the row
/// would otherwise promise the app will stay put while the session is going to launch it.
pub fn autostart_row(preference: bool, registered: Option<bool>) -> AutostartRow {
    match registered {
        None => AutostartRow::Unobservable(preference),
        Some(actual) if actual == preference => AutostartRow::Agrees(actual),
        Some(actual) => AutostartRow::Disagrees {
            shown: actual,
            preference,
        },
    }
}

/// What the OS has on file for launch-at-login.
///
/// Three states rather than a bool because **absent and stale are different events with
/// different owners**, and only one of them is ours to fix:
///
/// * [`Stale`](Self::Stale) is OUR bug. The registration is right there, and it names an
///   artifact that has moved: the owner's entry pointed at an AppImage DRAGON-590 relocated.
///   Nobody chose that, and rewriting it with this build's current path is a repair.
/// * [`Absent`](Self::Absent) is somebody's DECISION. Either the user never turned it on, or
///   they removed it, possibly through their desktop's own autostart editor. Putting it back
///   because a window opened would override a choice we have no evidence they withdrew.
///
/// Reality is what the row shows in every case ([`is_live`](Self::is_live)); the distinction
/// governs only whether opening Settings may WRITE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartRegistration {
    /// Nothing is registered.
    Absent,
    /// A registration exists but names something that no longer resolves, so the session
    /// will skip it. Not reachable on macOS: see [`autostart_registration`].
    Stale,
    /// A registration exists and will run.
    Live,
}

impl AutostartRegistration {
    /// Will the app actually start at the next login? This is what the row shows.
    pub fn is_live(self) -> bool {
        matches!(self, AutostartRegistration::Live)
    }
}

/// **Pure**, unit-tested: the login item this build should have.
///
/// The ONE expression of the rule, so the settings handler and all three resident daemons
/// cannot drift: registered iff there is a resident to launch AND the user opted in. It was
/// inlined in `App::reconcile_login_item`, which was fine while that was the only caller and
/// stopped being fine when the daemons started asking the same question (DRAGON-628).
pub fn autostart_wanted(resident: bool, autostart_on_login: bool) -> bool {
    resident && autostart_on_login
}

/// **Pure**, unit-tested: may the resident daemon REPAIR the login item at startup?
///
/// Exactly one situation earns an unprompted write: a registration that is present and no
/// longer resolves, on a build that wants one. Everything else is left alone.
///
/// * **Absent** never repairs, whatever the preference says. An absent entry is
///   indistinguishable from one the user deleted, in their desktop's own autostart editor or
///   by hand, so nothing may re-create it behind them. The row shows off, the preference is
///   kept, and the click that turns it back on still does exactly what it always did.
/// * **Live** needs nothing.
/// * `want == false` never repairs either, so a stale entry is never REMOVED here. Its
///   absence-in-effect already agrees with the preference, and deleting a file from the
///   user's home to tidy a setting is the thing `linux_autostart::registration` promises not
///   to do.
///
/// So the only write this can cause is the one that fixes a path we broke.
pub fn autostart_daemon_repair(registration: Option<AutostartRegistration>, want: bool) -> bool {
    want && registration == Some(AutostartRegistration::Stale)
}

/// **Pure**, unit-tested: should the resident daemon SEED the login item at startup, on
/// its first Linux run (DRAGON-683)?
///
/// [`autostart_daemon_repair`] deliberately leaves an ABSENT registration alone: absent
/// is somebody's decision, and only the settings toggle may create one. A fresh Linux
/// install breaks on that rule alone. `resident` and `autostart_on_login` both default
/// on, but no toggle was ever flipped, so the XDG entry never comes to exist and the
/// tray daemon never comes back at login. The rule is: the Linux resident tray must
/// come back at login by default on a fresh install, on every session kind (the
/// in-progress X11 line carries this same seed; a session kind is deliberately not a
/// parameter here). macOS and Windows solved the same problem with one-time seeds
/// (`mac_login_item_seeded` / `win_login_item_seeded`, each in its own daemon); this is
/// the Linux arm of the same rule, kept here beside the repair predicate it is the ONE
/// exception to.
///
/// True in exactly one situation: an ABSENT registration, both settings on (`want`,
/// [`autostart_wanted`]), and the seed never having run. `already_seeded` is what keeps
/// the never-override rule intact afterwards: once the seed has run, an entry the user
/// removed stays removed. `Stale` never seeds (that is the repair's case), `Live` needs
/// nothing, and `None` (no readable registration, e.g. the Flatpak portal mechanism)
/// never seeds either, because writing blind is exactly what the absent-is-a-decision
/// rule forbids.
//
// Compiled everywhere so the Linux gate is not the only prover; wired only into the
// Linux seed branch below, so it is honestly dead elsewhere outside tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn linux_autostart_should_seed(
    registration: Option<AutostartRegistration>,
    want: bool,
    already_seeded: bool,
) -> bool {
    registration == Some(AutostartRegistration::Absent) && want && !already_seeded
}

/// What the OS has on file for launch-at-login, or `None` when this build has no read-only
/// way to find out (see the note above this function's neighbours).
///
/// **Read only.** It never registers, never unregisters, never writes a file and never issues
/// a portal request, which is what lets it run every time the settings window opens.
///
/// `Some(_)` answers a SECOND question too, and deliberately the same way: it is exactly the
/// set of builds whose login item can also be WRITTEN here and now, synchronously, without
/// asking the user anything. A Flatpak's registration is a portal request; an unbundled mac
/// binary's `set` returns an honest error. So the row's source and the repair's gate are one
/// value rather than two predicates that could drift.
///
/// **macOS can never answer [`AutostartRegistration::Stale`], and that is the honest answer
/// rather than a gap.** `SMAppService` registers the app's IDENTITY, not a command line, so
/// there is no recorded path to go stale and nothing for a settings-open repair to fix. It
/// reports `Live` or `Absent`, both of which leave the login item untouched on open. Linux
/// and Windows record a path, so both can be stale and both can be repaired.
pub fn autostart_registration() -> Option<AutostartRegistration> {
    #[cfg(target_os = "macos")]
    {
        // Unbundled reports `false` for a process that never could have registered, so it is
        // "unknown", not "off". Bundled, `SMAppService.status` is the OS's own answer.
        use crate::platform::mac::login_item;
        (login_item::availability() == login_item::Availability::Available).then(|| {
            if login_item::is_enabled() {
                AutostartRegistration::Live
            } else {
                AutostartRegistration::Absent
            }
        })
    }
    #[cfg(target_os = "linux")]
    {
        use crate::platform::linux_autostart as autostart;
        match autostart::autostart_mechanism(crate::util::package_kind()) {
            autostart::Mechanism::DesktopFile => Some(autostart::registration()),
            autostart::Mechanism::BackgroundPortal => None,
        }
    }
    #[cfg(target_os = "windows")]
    {
        Some(crate::platform::windows_autostart::registration())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Repair a STALE login item, once, at resident-daemon startup (DRAGON-628).
///
/// Called by all three residents (`platform/{linux,mac,windows}/daemon.rs`) early in their
/// startup, after the single-instance lock and before the tray goes up, so exactly one process
/// per session does it and a duplicate launch that exits does nothing.
///
/// Portable on the outside: the body branches by `cfg`, so each daemon's call site is one line
/// with no `cfg` of its own and the three cannot drift apart.
///
/// It writes in exactly ONE case, [`autostart_daemon_repair`]: the registration is present, no
/// longer resolves, and this build wants one. That is the case that is unambiguously our bug
/// rather than somebody's decision. Every other case is logged and left alone, with ONE
/// exception, the first-run Linux SEED ([`linux_autostart_should_seed`], DRAGON-683): on a
/// Linux daemon whose registration is ABSENT, with `resident` + `autostart_on_login` both on
/// and the one-time `linux_login_item_seeded` marker unset, the entry is CREATED once and
/// the marker persisted, because the Linux resident tray must come back at login by default
/// on a fresh install, on every session kind, and no toggle was ever flipped to create the
/// entry. After the seed the absent-is-a-decision rule holds again in full: a removal is
/// never overridden twice. Non-Linux platforms are untouched by construction (the seed
/// branch is `cfg(target_os = "linux")`; macOS and Windows run their own one-time seeds in
/// their daemons).
///
/// **macOS reaches this and correctly does nothing, which is the honest answer rather than a
/// gap.** `SMAppService` registers the app's bundle IDENTITY, not a command line, so there is
/// no recorded path that can go stale: [`autostart_registration`] can only answer `Live` or
/// `Absent` there and the repair predicate is false for both. The call site exists anyway so
/// the three daemons read identically, and so that a future mac mechanism which DID record a
/// path would be covered without anyone remembering to add a hook. Linux and Windows both
/// record a path, and both really can be stale: Linux in the `.desktop` `Exec=` line, Windows
/// in the `Run` value's command.
///
/// Best effort throughout. A failure is logged and the daemon carries on; launch-at-login is
/// not worth refusing to start a tray over.
pub fn autostart_repair_at_daemon_start() {
    // One config read at daemon startup, on the process that is about to sit resident for the
    // whole session. The settings handler reads its own copy; there is no shared state to
    // thread through three separate processes.
    let p = crate::state::load();
    let want = autostart_wanted(p.resident, p.autostart_on_login);
    let registration = autostart_registration();
    // The one-time Linux SEED (DRAGON-683): see the doc above and
    // `linux_autostart_should_seed` for the full reasoning. The cfg keeps every other
    // platform byte-identical.
    #[cfg(target_os = "linux")]
    {
        if linux_autostart_should_seed(registration, want, p.linux_login_item_seeded) {
            match crate::platform::linux_autostart::set(true) {
                Ok(()) => {
                    // Load-modify-save through the normal settings write path, the same
                    // shape the settings handlers use, so nothing else in the config is
                    // disturbed.
                    let mut seeded = crate::state::load();
                    seeded.linux_login_item_seeded = true;
                    crate::state::save(&seeded);
                    log::info!(
                        "autostart: first daemon start with resident + autostart on; \
                         registering the login item (one-time seed)"
                    );
                    // The entry is Live now; there is nothing left to repair or report.
                    return;
                }
                // The marker is deliberately NOT set on failure, so a later start
                // retries; the ordinary no-op logging below still says the entry is
                // absent.
                Err(e) => log::warn!(
                    "autostart: the one-time seed could not register the login item: {e}"
                ),
            }
        }
    }
    if !autostart_daemon_repair(registration, want) {
        // Say WHY nothing happened. A silent no-op here is indistinguishable from the hook
        // never having run, and "autostart is still not working" is a question this log has
        // to be able to answer.
        match registration {
            None => log::debug!(
                "autostart: this build cannot read its login item, so there is nothing to \
                 repair at startup"
            ),
            Some(AutostartRegistration::Absent) if want => log::info!(
                "autostart: no login item is registered although the setting asks for one. \
                 Leaving it alone: an absent registration may have been removed on purpose, \
                 and only the settings toggle may create one (the one exception is the \
                 first Linux daemon start, which seeds the entry once; see \
                 linux_autostart_should_seed)"
            ),
            Some(AutostartRegistration::Stale) => log::debug!(
                "autostart: the login item is stale but this build does not want one, so it \
                 is left as it is rather than deleted"
            ),
            Some(_) => {}
        }
        return;
    }
    log::info!(
        "autostart: the login item no longer resolves; rewriting it for this build (the \
         artifact it named has moved)"
    );
    #[cfg(target_os = "macos")]
    let result = crate::platform::mac::login_item::set(true);
    #[cfg(target_os = "linux")]
    let result = crate::platform::linux_autostart::set(true);
    #[cfg(target_os = "windows")]
    let result = crate::platform::windows_autostart::set(true);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: Result<(), String> = Ok(());
    match result {
        Ok(()) => log::info!("autostart: the login item was rewritten"),
        Err(e) => log::warn!("autostart: could not rewrite the login item: {e}"),
    }
}

/// **Pure**, unit-tested: whether a Windows `Run` value can still LAUNCH anything, given its
/// data.
///
/// The Windows half of the DRAGON-625 lesson, ported by DRAGON-628 because the Windows toggle
/// could lie in exactly the same way the Linux one did. `is_enabled` there asked only whether
/// the registry VALUE exists, and the value carries an absolute exe path recorded when the
/// toggle was flipped. Move the exe (a dev install rebuilt elsewhere, an MSI upgraded into a
/// different directory) and Windows silently skips the entry at login while the row keeps
/// saying the feature is on.
///
/// Kept in the shared tree rather than in `platform/windows/`, for the same reason as the
/// `win_build_*` gates above: nobody on the project runs Windows day to day, so the reasoning
/// has to be provable from the Linux gate. The registry READ stays native.
///
/// The rules mirror `linux_autostart::entry_is_live_with` deliberately, so the two platforms
/// cannot answer the same question differently:
///
/// * Our own writer emits `"<exe>" resident`, so a QUOTED first field is the path, spaces and
///   all. That is the normal case and it is tried first.
/// * Failing that, the whole command minus a trailing bare `resident`, then the first
///   whitespace-separated token, which covers an unquoted value someone else wrote.
/// * A candidate that is not an ABSOLUTE Windows path is a `PATH` lookup we cannot honestly
///   resolve, so it reads as LIVE. Guessing there would disable a working entry.
///
/// So it only ever calls a value dead when it names an absolute path that is not there, which
/// is precisely the case Windows itself skips. `is_absolute()` is not used, and must not be:
/// it answers for the HOST target, so `C:\…` is not absolute to a Linux test run and every
/// case would collapse to "live".
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win_run_value_is_live_with(data: &str, exists: impl Fn(&std::path::Path) -> bool) -> bool {
    /// `X:\…`, `X:/…` or a `\\server\share` UNC. Spelled out rather than asked of
    /// `Path::is_absolute`, which answers for whatever target the test happens to run on.
    fn is_windows_absolute(s: &str) -> bool {
        if s.starts_with(r"\\") {
            return true;
        }
        let mut c = s.chars();
        matches!(
            (c.next(), c.next(), c.next()),
            (Some(d), Some(':'), Some('\\' | '/')) if d.is_ascii_alphabetic()
        )
    }

    let data = data.trim();
    if data.is_empty() {
        return false;
    }
    // A candidate that is not a path at all is a PATH lookup we cannot resolve, so it is
    // never called dead.
    let live = |cand: &str| {
        !cand.is_empty() && (!is_windows_absolute(cand) || exists(std::path::Path::new(cand)))
    };
    // The quoted form our own writer produces. When it parses it is the WHOLE answer and the
    // unquoted candidates below are not tried at all: they would both still carry the quote
    // characters, which no absolute-path test can match, so a dead exe would read as a PATH
    // lookup and so as live. An unterminated quote falls through instead.
    if let Some(exe) = data
        .strip_prefix('"')
        .and_then(|rest| rest.split_once('"').map(|(exe, _)| exe))
    {
        return live(exe);
    }
    // `instance::RESIDENT_ARG`, the token every launcher agrees on, rather than a fourth
    // private copy of the literal (see its doc). `windows_autostart` keeps its own for the
    // command it WRITES, pinned to the same string by its own test.
    let whole = data
        .strip_suffix(crate::instance::RESIDENT_ARG)
        .map(str::trim_end)
        .unwrap_or(data);
    let first = data.split_whitespace().next().unwrap_or(data);
    live(whole) || live(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_caption_buttons_need_windows_11_or_a_composited_windows_10() {
        // Win11 21H2 and everything after it: unchanged, and unchanged WHATEVER dcomp says.
        for dcomp in [true, false] {
            assert!(win_build_paints_native_caption_buttons(22000, dcomp)); // 21H2
            assert!(win_build_paints_native_caption_buttons(22621, dcomp)); // 22H2 (Mica floor)
            assert!(win_build_paints_native_caption_buttons(22631, dcomp)); // 23H2
            assert!(win_build_paints_native_caption_buttons(26100, dcomp)); // 24H2
            assert!(win_build_paints_native_caption_buttons(u32::MAX, dcomp));
            // Windows 8.1 and older never had the recipe, dcomp or not.
            assert!(!win_build_paints_native_caption_buttons(9600, dcomp));
            // An unreadable build reads as 0: the SAFE side is our own buttons, which
            // always render, over DWM's, which may not.
            assert!(!win_build_paints_native_caption_buttons(0, dcomp));
        }
        // WINDOWS 10 stays on its own buttons at EITHER setting, and that is a measurement,
        // not caution (DRAGON-666): a composited Windows 10 does paint the DWM cluster, but
        // only into a strip that renders solid BLACK there for want of a backdrop material,
        // so the buttons and a black titlebar arrive together. The A/B is in the doc above.
        for build in [10240, 19041, 19045, 21999] {
            for dcomp in [true, false] {
                assert!(
                    !win_build_paints_native_caption_buttons(build, dcomp),
                    "{build} dcomp={dcomp}: Windows 10 keeps its CSD buttons"
                );
            }
        }
    }

    // DRAGON-408 deleted `layered_per_pixel_alpha_is_windows_11_only` along with the gate it
    // pinned. `WS_EX_LAYERED` is unconditional on every Windows build again (DRAGON-280's
    // shape), so there is no build-keyed decision left to test — see the note above
    // `WIN10_MIN_BUILD` for why the gate's premise was wrong.

    #[test]
    fn fallback_seeding_needs_no_layer_shell_and_a_portal_landing() {
        // The Flatpak / GNOME shape: no layer shell, capture clamped to the portal.
        assert!(overlay_fallback_seeding(false, true));
        // A normal COSMIC session keeps its layer surfaces whatever the backend choice
        // says. This is the term that keeps the unsandboxed build byte-identical.
        assert!(!overlay_fallback_seeding(true, true));
        assert!(!overlay_fallback_seeding(true, false));
        // No layer shell AND no portal landing (native screencopy present, e.g. a
        // layer-shell-less compositor with ext-image-copy-capture, screencopy chosen):
        // the fallback has no frame source, so the session keeps today's loud
        // OverlayNeverShown ending instead of half-seeding.
        assert!(!overlay_fallback_seeding(false, false));
    }

    /// The clamp the tray daemon applies to reach the same answer a capture process does
    /// (DRAGON-555). A saved preference only decides anything where BOTH backends exist.
    #[test]
    fn a_capture_choice_lands_on_the_portal_when_it_is_asked_for_or_is_all_there_is() {
        use backend::{PORTAL_ID, SCREENCOPY_ID};
        // Asked for, on a session that has both.
        assert!(capture_choice_uses_portal(PORTAL_ID, true));
        // Native asked for and native present: the preference stands.
        assert!(!capture_choice_uses_portal(SCREENCOPY_ID, true));
        // No native capture at all (the sandbox / GNOME shape): the saved preference cannot
        // apply, and the session is on the portal whatever the config says. The persisted
        // value is never rewritten for it, which is what keeps a COSMIC+GNOME dual login
        // working.
        assert!(capture_choice_uses_portal(SCREENCOPY_ID, false));
        assert!(capture_choice_uses_portal(PORTAL_ID, false));
    }

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

    /// DRAGON-666. The DirectComposition experiment is OFF unless asked for by the exact
    /// value, and it is deliberately NOT build-keyed: the whole point is that the thing it
    /// fixes was never the OS version, so a Windows 10 tester and a Windows 11 tester run
    /// the same switch and their answers are comparable.
    #[test]
    fn the_dcomp_experiment_needs_an_exact_one_and_asks_nothing_about_the_os() {
        // DEFAULT ON: unset is the composited path, because that is the supported one and
        // the HWND path is measured broken on both Windows versions.
        assert!(win_dcomp_enabled(None));
        assert!(win_dcomp_enabled(Some("1")));
        // The escape hatch is the EXACT "0" and nothing else -- a customer with a driver
        // that hates this gets the old behaviour back in one variable.
        assert!(!win_dcomp_enabled(Some("0")));
        for junk in ["", " ", "0 ", "00", "false", "no", "off", "O"] {
            assert!(win_dcomp_enabled(Some(junk)), "{junk:?} must not read as off");
        }
        // The value we hand wgpu is its own spelling, not ours — a typo here is a silent
        // no-op inside wgpu (it falls back to the default), which is the one failure mode
        // a tester could not distinguish from "the experiment did not help".
        assert_eq!(WGPU_DCOMP_VALUE, "DxgiFromVisual");
        assert_eq!(WGPU_PRESENTATION_ENV, "WGPU_DX12_PRESENTATION_SYSTEM");
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
            // WITHOUT a composited surface the old rule stands, exactly.
            assert_eq!(
                win_build_software_overlays(build, false),
                win_build_is_windows_10(build),
                "build {build}: software overlays must be exactly the Win10 band"
            );
            // The overlay EDITOR is the complement, at either setting: available everywhere
            // the software rasterizer is not forced.
            for dcomp in [true, false] {
                assert_eq!(
                    win_build_has_overlay_preview(build, dcomp),
                    !win_build_software_overlays(build, dcomp),
                    "build {build} dcomp={dcomp}: the two gates must stay exact complements"
                );
            }
            // WITH one, nothing anywhere is forced to the CPU rasterizer -- that is the
            // point of DRAGON-666, and it is what gives Windows 10 its overlay editor back.
            assert!(!win_build_software_overlays(build, true), "build {build} composited");
            assert!(win_build_has_overlay_preview(build, true), "build {build} composited");
        }
        // Spelled out at the edges, so a future band edit has to break a named assertion.
        assert!(!win_build_software_overlays(10239, false)); // Windows 8.1 and older
        assert!(win_build_software_overlays(10240, false)); // Win10 RTM
        assert!(win_build_software_overlays(19045, false)); // Win10 22H2, the last one
        assert!(win_build_software_overlays(21999, false)); // just under the Win11 floor
        assert!(!win_build_software_overlays(22000, false)); // Win11 21H2 — wgpu, as today
        assert!(!win_build_software_overlays(26100, false)); // Win11 24H2 — wgpu, as today
        // An unreadable build reads as "not Windows 10", i.e. keep the GPU renderer and the
        // overlay editor. That is the SAFE side: a machine we cannot identify keeps the
        // path that works on every OS we ship for, rather than being silently degraded.
        assert!(!win_build_software_overlays(0, false));
        assert!(win_build_has_overlay_preview(0, false));
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
        // At either dcomp setting: the Win11 term does not depend on it (DRAGON-666 only
        // ADDED a Windows 10 term).
        for dcomp in [true, false] {
            for build in [22621_u32, 22631, 26100] {
                assert!(win_build_paints_native_caption_buttons(build, dcomp));
            }
            assert!(win_build_paints_native_caption_buttons(22000, dcomp));
        }
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

/// DRAGON-583: whether an in-app recording shortcut can reach a live recording, and so
/// whether the three Recording rows in Settings → Keyboard Shortcuts are honest.
#[cfg(test)]
mod recording_shortcut_reach_tests {
    use super::*;

    /// Either route on its own is enough, and neither is required.
    #[test]
    fn either_route_alone_delivers_the_chord() {
        // A focus-free binding (the portal GlobalShortcuts interface) reaches us whatever
        // has focus, so our own surfaces are irrelevant.
        assert!(in_app_recording_shortcut_reachable(true, false));
        // Our surface still owns the keyboard, so ordinary key events arrive with no
        // global binding at all.
        assert!(in_app_recording_shortcut_reachable(false, true));
        // Both, which nothing ships today but is not a contradiction.
        assert!(in_app_recording_shortcut_reachable(true, true));
    }

    /// The whole point of the ticket: with no focus-free binding AND no surface of ours
    /// holding the keyboard, the chord can never arrive. That is exactly the shape of a
    /// COSMIC recording, native or portal-fallback, and it is why those rows stop being
    /// advertised there.
    #[test]
    fn no_binding_and_no_focus_can_never_deliver() {
        assert!(!in_app_recording_shortcut_reachable(false, false));
    }

    /// The predicate is a plain OR and must stay one: neither term may start implying the
    /// other. Stated exhaustively so a third term added later cannot quietly widen it.
    #[test]
    fn the_matrix_is_exactly_an_or() {
        for focus_free in [false, true] {
            for keeps_focus in [false, true] {
                assert_eq!(
                    in_app_recording_shortcut_reachable(focus_free, keeps_focus),
                    focus_free || keeps_focus,
                    "focus_free={focus_free} keeps_focus={keeps_focus}"
                );
            }
        }
    }

    /// The LIVE reader's platform inputs, pinned so a cfg edit cannot silently change what
    /// a session is told.
    ///
    /// Linux answers from a real probe of the running desktop, so this cannot assert the
    /// result; what it CAN assert is that the answer is that probe and nothing else, which
    /// is the same as pinning the second term to `false` (a native session hands focus to
    /// the recorded window at record start, and the fallback session destroys its
    /// toplevel). macOS and Windows answer `true` and must keep doing so: their overlays
    /// stay up and nothing takes the keyboard away, so their Recording rows are
    /// byte-identically unchanged by DRAGON-583.
    #[test]
    fn the_live_reader_keeps_each_platforms_answer() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            in_app_recording_shortcuts_work(),
            global_shortcuts::interface_available(),
            "on Linux the answer must be the portal probe and nothing else"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            in_app_recording_shortcuts_work(),
            "mac and Windows keep their overlay windows, so their rows stay advertised"
        );
    }
}

/// DRAGON-587, then DRAGON-597: whether an overlay can hide the pointer sprite. The surface
/// kind used to decide it; the iced fork closed the gap, so now nothing does.
#[cfg(test)]
mod overlay_pointer_tests {
    use super::*;

    /// The axis is GONE. A layer surface used to be the one kind that could not hide, because
    /// libcosmic's Wayland backend no-oped `set_cursor_visible`; our iced fork implements it
    /// (the iced `[patch]` block in `Cargo.toml`), so both kinds hide now. This test is the
    /// thing
    /// that fails if the `[patch]` is ever dropped without restoring the arrow fallback.
    #[test]
    fn every_overlay_kind_can_now_hide_the_pointer() {
        assert!(overlay_hides_pointer(false), "a plain toplevel reaches winit and hides");
        assert!(
            overlay_hides_pointer(true),
            "a layer surface hides too, via the iced fork's set_cursor_visible"
        );
    }

    /// The live reader agrees with the pure predicate on every platform, layer shell or not.
    #[test]
    fn the_live_reader_agrees_on_every_platform() {
        assert!(overlay_pointer_hideable(), "every overlay this app mints can hide");
        #[cfg(target_os = "linux")]
        assert_eq!(
            overlay_pointer_hideable(),
            overlay_hides_pointer(layer_overlay_available()),
            "the reader is the pure predicate fed this session's real surface kind"
        );
    }
}

/// DRAGON-628: what the "Automatically start on login" row is allowed to claim.
#[cfg(test)]
mod autostart_row_tests {
    use super::*;

    /// THE regression. The owner's config said on, the autostart entry named an AppImage
    /// DRAGON-590 had moved, and the row went on claiming the feature was on. Reality wins.
    #[test]
    fn a_dead_registration_reads_off_however_the_preference_was_left() {
        let row = autostart_row(true, Some(false));
        assert_eq!(
            row,
            AutostartRow::Disagrees {
                shown: false,
                preference: true
            }
        );
        assert!(!row.shown(), "the row must say what the next login will do");
    }

    /// The same rule pointing the other way, which is not symmetry for its own sake: an entry
    /// that exists while the preference says off WILL launch the app, and a row reading off
    /// would be exactly as untrue as the case above.
    #[test]
    fn a_live_registration_reads_on_even_when_the_preference_says_off() {
        let row = autostart_row(false, Some(true));
        assert_eq!(
            row,
            AutostartRow::Disagrees {
                shown: true,
                preference: false
            }
        );
        assert!(row.shown());
    }

    /// A build that cannot observe reality (a Flatpak, an unbundled mac dev binary) shows the
    /// stored preference, which is what every platform did before this ticket. Getting this
    /// wrong would put a NEW lie in the Flatpak: the portal grants autostart on the host,
    /// where the sandbox cannot read the entry, so a probe there would answer a confident
    /// "off" for an app that really does start at login.
    #[test]
    fn an_unobservable_build_falls_back_to_the_preference() {
        for pref in [true, false] {
            let row = autostart_row(pref, None);
            assert_eq!(row, AutostartRow::Unobservable(pref));
            assert_eq!(row.shown(), pref);
        }
    }

    /// The ordinary case: reality and the preference agree, so nothing is logged and nothing
    /// on screen changes from what shipped before.
    #[test]
    fn agreement_is_its_own_state_so_the_log_stays_quiet() {
        for v in [true, false] {
            let row = autostart_row(v, Some(v));
            assert_eq!(row, AutostartRow::Agrees(v));
            assert_eq!(row.shown(), v);
        }
    }

    /// Whatever the inputs, the row renders the observation when there is one. Pinned as one
    /// statement so a future variant cannot quietly start rendering the preference instead.
    #[test]
    fn the_row_always_renders_the_observation_when_there_is_one() {
        for pref in [true, false] {
            for actual in [true, false] {
                assert_eq!(
                    autostart_row(pref, Some(actual)).shown(),
                    actual,
                    "preference {pref}, observed {actual}"
                );
            }
        }
    }

    /// Only a registration that RUNS is live. `Stale` is the state that exists purely to
    /// separate our bug from the user's choice, and it must never read as on.
    #[test]
    fn only_a_runnable_registration_is_live() {
        assert!(AutostartRegistration::Live.is_live());
        assert!(!AutostartRegistration::Stale.is_live());
        assert!(!AutostartRegistration::Absent.is_live());
    }
}

/// DRAGON-628: when the resident daemon may write the login item at startup.
#[cfg(test)]
mod autostart_daemon_repair_tests {
    use super::*;

    /// The ONE case that earns an unprompted write: the registration is there and no longer
    /// resolves, and this build wants one. That is the owner's case, where DRAGON-590 moved
    /// the AppImage the entry named.
    #[test]
    fn only_a_stale_registration_that_is_wanted_is_repaired() {
        assert!(autostart_daemon_repair(
            Some(AutostartRegistration::Stale),
            true
        ));
    }

    /// THE refinement. An absent registration is indistinguishable from one the user deleted
    /// in their desktop's autostart editor, so no daemon may put it back, however loudly the
    /// stored preference asks for it. The row shows off and the click still works.
    #[test]
    fn an_absent_registration_is_never_recreated() {
        assert!(!autostart_daemon_repair(
            Some(AutostartRegistration::Absent),
            true
        ));
    }

    /// A live one needs nothing, so the `Exec=` path is never rewritten out from under a
    /// working entry by whichever build happened to start the daemon.
    #[test]
    fn a_live_registration_is_left_exactly_as_it_is() {
        assert!(!autostart_daemon_repair(
            Some(AutostartRegistration::Live),
            true
        ));
    }

    /// With no login item wanted, nothing is written at all. In particular a stale entry is
    /// never DELETED here: it already fails to run, so it agrees with the preference, and
    /// removing a file from the user's home to tidy a setting is not this hook's job.
    #[test]
    fn nothing_is_written_when_no_login_item_is_wanted() {
        for r in [
            AutostartRegistration::Absent,
            AutostartRegistration::Stale,
            AutostartRegistration::Live,
        ] {
            assert!(!autostart_daemon_repair(Some(r), false), "{r:?}");
        }
    }

    /// A build that cannot READ its registration cannot repair it either, and must not try.
    /// On a Flatpak "trying" would mean a Background-portal request from a daemon nobody is
    /// looking at; on an unbundled mac dev binary it would fail every time.
    #[test]
    fn an_unobservable_build_never_writes() {
        assert!(!autostart_daemon_repair(None, true));
        assert!(!autostart_daemon_repair(None, false));
    }

    /// The desired state is ONE expression, shared by the settings handler and all three
    /// daemons. A login item with no resident to launch is pointless, and one the user opted
    /// out of is unwanted.
    #[test]
    fn a_login_item_is_wanted_only_with_a_resident_and_an_opt_in() {
        assert!(autostart_wanted(true, true));
        assert!(!autostart_wanted(true, false));
        assert!(!autostart_wanted(false, true));
        assert!(!autostart_wanted(false, false));
    }
}

/// DRAGON-683: the ONE exception to "an absent registration is never recreated", the
/// first-run Linux seed.
#[cfg(test)]
mod linux_autostart_seed_tests {
    use super::*;

    /// The one true combination: an absent entry, both settings on, and a seed that has
    /// never run.
    #[test]
    fn the_first_run_with_everything_on_seeds() {
        assert!(linux_autostart_should_seed(
            Some(AutostartRegistration::Absent),
            true,
            false
        ));
    }

    /// Each condition alone kills the seed: a build that does not want a login item, and
    /// a seed that has already run (a removal after the seed stays removed, forever).
    #[test]
    fn each_condition_alone_kills_the_seed() {
        assert!(!linux_autostart_should_seed(
            Some(AutostartRegistration::Absent),
            false,
            false
        ));
        assert!(!linux_autostart_should_seed(
            Some(AutostartRegistration::Absent),
            true,
            true
        ));
    }

    /// Only an ABSENT registration seeds. `Stale` is the repair predicate's case, `Live`
    /// needs nothing, and `None` (no readable registration, the Flatpak portal mechanism)
    /// must never provoke a blind write.
    #[test]
    fn only_an_absent_registration_seeds() {
        assert!(!linux_autostart_should_seed(
            Some(AutostartRegistration::Stale),
            true,
            false
        ));
        assert!(!linux_autostart_should_seed(
            Some(AutostartRegistration::Live),
            true,
            false
        ));
        assert!(!linux_autostart_should_seed(None, true, false));
    }
}

/// DRAGON-628: the Windows `Run` value's liveness, the port of DRAGON-625's Linux rule.
#[cfg(test)]
mod win_run_value_tests {
    use super::*;
    use std::path::Path;

    /// What our own writer produces, and the lie it could tell. The value exists either way;
    /// only the exe's presence separates "starts at login" from "silently skipped".
    #[test]
    fn a_quoted_exe_decides_the_answer_by_itself() {
        let v = r#""C:\Program Files\CCK\cosmic-capture-kit.exe" resident"#;
        assert!(
            win_run_value_is_live_with(v, |p| p
                == Path::new(r"C:\Program Files\CCK\cosmic-capture-kit.exe")),
            "a path with spaces must survive the quotes"
        );
        assert!(
            !win_run_value_is_live_with(v, |_| false),
            "a value naming a missing exe must read as not registered"
        );
    }

    /// The trap this function is written around. Falling back to the unquoted candidates
    /// after a quoted path misses would hand the absolute-path test a string that still
    /// begins with `"`, which reads as a PATH lookup, which reads as LIVE. A dead entry
    /// would then answer true and the row would be lying again, one layer down.
    #[test]
    fn a_dead_quoted_path_never_falls_through_to_the_raw_string() {
        for v in [
            r#""C:\gone\cosmic-capture-kit.exe" resident"#,
            r#""C:\gone\cosmic-capture-kit.exe""#,
            r#""D:/gone/cck.exe" resident"#,
        ] {
            assert!(!win_run_value_is_live_with(v, |_| false), "{v}");
        }
    }

    /// An unquoted absolute path, which is what a hand-edited value or another writer looks
    /// like. The trailing resident token is stripped first, then the first token is tried.
    #[test]
    fn an_unquoted_value_resolves_through_its_path_or_its_first_token() {
        let v = r"C:\cck\cosmic-capture-kit.exe resident";
        assert!(win_run_value_is_live_with(v, |p| p
            == Path::new(r"C:\cck\cosmic-capture-kit.exe")));
        assert!(!win_run_value_is_live_with(v, |_| false));
        // A launcher plus arguments: the whole string is not a path, so the first token is.
        let w = r"C:\Windows\System32\cmd.exe /c start cck.exe resident";
        assert!(win_run_value_is_live_with(w, |p| p
            == Path::new(r"C:\Windows\System32\cmd.exe")));
    }

    /// A bare command is a `PATH` lookup we cannot honestly reproduce, so it is never called
    /// dead. Guessing there would disable a working entry, exactly as it would on Linux.
    #[test]
    fn a_path_lookup_is_never_called_dead() {
        assert!(win_run_value_is_live_with("cosmic-capture-kit.exe resident", |_| false));
        assert!(win_run_value_is_live_with(r#""cck.exe" resident"#, |_| false));
    }

    /// A UNC path is absolute too, so it is checked rather than waved through.
    #[test]
    fn a_unc_path_is_absolute_and_gets_checked() {
        let v = r#""\\nas\apps\cck.exe" resident"#;
        assert!(win_run_value_is_live_with(v, |p| p == Path::new(r"\\nas\apps\cck.exe")));
        assert!(!win_run_value_is_live_with(v, |_| false));
    }

    /// Nothing to launch is not registered.
    #[test]
    fn an_empty_value_is_not_live() {
        assert!(!win_run_value_is_live_with("", |_| true));
        assert!(!win_run_value_is_live_with("   ", |_| true));
        assert!(!win_run_value_is_live_with(r#""" resident"#, |_| true));
    }

    /// The Linux and Windows readers must answer the same QUESTION, or one platform's toggle
    /// tells the truth and the other does not. The token they both strip is the one every
    /// launcher agrees on.
    #[test]
    fn both_platforms_strip_the_same_resident_token() {
        assert_eq!(crate::instance::RESIDENT_ARG, "resident");
    }
}

/// DRAGON-TBD: which windows a macOS whole-display grab must keep out of the picture. The
/// SCK and `libproc` reads are in `platform::mac`; the RULE is here so `cargo test` proves it
/// on Linux and Windows too.
#[cfg(test)]
mod mac_stray_chrome_tests {
    use super::*;
    use std::path::Path;

    /// The app under test: pid 100, the installed bundle, bundled.
    fn me() -> MacAppIdentity<'static> {
        MacAppIdentity {
            pid: 100,
            exe: Some(Path::new("/Applications/Cosmic Capture Kit.app/Contents/MacOS/cck")),
            bundle_id: Some("dev.thedragon.CosmicCaptureKit"),
        }
    }

    /// A placed capture overlay, measured on a real machine.
    const SHIELDING: isize = 2147483628;
    /// Where winit parks an `AlwaysOnTop` window before `place_overlay` raises it.
    const FLOATING: isize = 3;

    /// Each handle recognises a case the other two cannot, so any one of them agreeing is
    /// enough. Requiring all three would fail on a dev binary (no bundle id) and on a
    /// sibling that has already exited (no readable exe path).
    #[test]
    fn any_single_handle_agreeing_names_the_same_app() {
        // Same process.
        assert!(mac_same_app(MacAppIdentity { pid: 100, ..Default::default() }, me()));
        // A DIFFERENT process running the same binary: the whole point of this predicate.
        assert!(mac_same_app(
            MacAppIdentity { pid: 200, exe: me().exe, bundle_id: None },
            me()
        ));
        // A dev build beside the installed one: exe paths differ, bundle ids agree.
        assert!(mac_same_app(
            MacAppIdentity {
                pid: 200,
                exe: Some(Path::new("/repo/target/release/cck")),
                bundle_id: Some("dev.thedragon.CosmicCaptureKit"),
            },
            me()
        ));
    }

    /// Nothing about another application may ever look like us.
    #[test]
    fn a_foreign_app_is_never_us() {
        assert!(!mac_same_app(
            MacAppIdentity {
                pid: 200,
                exe: Some(Path::new("/Applications/Zen.app/Contents/MacOS/zen")),
                bundle_id: Some("app.zen-browser.zen"),
            },
            me()
        ));
    }

    /// SCK reports "no owning application" as pid 0 and an unbundled process's identifier as
    /// an empty string. Either one matching itself would make half the system "us".
    #[test]
    fn unknown_owners_never_match() {
        assert!(!mac_same_app(MacAppIdentity::default(), MacAppIdentity::default()));
        assert!(!mac_same_app(
            MacAppIdentity { pid: 0, exe: None, bundle_id: Some("") },
            MacAppIdentity { pid: 100, exe: None, bundle_id: Some("") }
        ));
    }

    /// The reported bug: a SIBLING capture instance's overlay, which a pid-only rule leaves
    /// in the picture. Both of the levels an overlay is ever seen at must be caught, because
    /// `place_overlay` raises it from one to the other a frame or two after creation.
    #[test]
    fn a_sibling_capture_overlay_is_stray_at_both_of_its_levels() {
        let sibling = MacAppIdentity { pid: 200, exe: me().exe, bundle_id: me().bundle_id };
        assert!(mac_window_is_stray_chrome(sibling, me(), SHIELDING, false));
        assert!(mac_window_is_stray_chrome(sibling, me(), FLOATING, false));
    }

    /// DRAGON-608, and it is a shipped feature rather than a nicety: a capture started over a
    /// live colour picker must still photograph the picker. Its overlay is the SAME window
    /// kind at the SAME level, so only the owner's argv can separate the two.
    #[test]
    fn a_sibling_color_picker_overlay_survives_the_filter() {
        let picker = MacAppIdentity { pid: 200, exe: me().exe, bundle_id: me().bundle_id };
        assert!(!mac_window_is_stray_chrome(picker, me(), SHIELDING, true));
    }

    /// The carve-out is for SIBLINGS. A picker grabbing its own flats must still drop its own
    /// dim, or the pixels it later samples are its own dimming layer.
    #[test]
    fn our_own_chrome_is_stray_even_on_a_picker_launch() {
        let mine = MacAppIdentity { pid: 100, exe: me().exe, bundle_id: me().bundle_id };
        assert!(mac_window_is_stray_chrome(mine, me(), SHIELDING, true));
    }

    /// Our ordinary toplevels are legitimate capture subjects: documenting this app means
    /// photographing its settings window and its preview editor.
    #[test]
    fn our_own_ordinary_windows_are_content_and_stay() {
        let mine = MacAppIdentity { pid: 100, exe: me().exe, bundle_id: me().bundle_id };
        assert!(!mac_window_is_stray_chrome(mine, me(), MAC_APP_WINDOW_LAYER, false));
        // And the negative-layer desktop band (wallpaper, desktop icons) is never ours.
        assert!(!mac_window_is_stray_chrome(mine, me(), -2147483624, false));
    }

    /// A foreign app's always-on-top panel is somebody else's content. The level band alone
    /// must never decide.
    #[test]
    fn a_foreign_floating_panel_is_never_stray() {
        let other = MacAppIdentity {
            pid: 300,
            exe: Some(Path::new("/Applications/Ice.app/Contents/MacOS/Ice")),
            bundle_id: Some("com.jordanbaird.Ice"),
        };
        assert!(!mac_window_is_stray_chrome(other, me(), SHIELDING, false));
        assert!(!mac_window_is_stray_chrome(other, me(), 25, false));
    }
}
