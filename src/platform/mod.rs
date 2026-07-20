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
