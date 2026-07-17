# Architecture

A map of the tree as it exists today, for contributors. Working agreements
(build/test/lint commands, CAUTION areas, conventions) live in the repo-root
[`CLAUDE.md`](../CLAUDE.md) — read that first; this doc is the deeper map it
points to.

## Module tree

- `src/main.rs` — argv parsing (mode/kind/countdown/`--preview`/`--inspect`/
  `--settings`), single-instance lock (`src/instance.rs`), launches the
  `cosmic::Application`.
- `src/cli/` — `diagnostics.rs` (the `--test` harness) and `inspect.rs`
  (metadata dump). See [`CLI.md`](../CLI.md) for the user-facing flag list.
- `src/app/` — the application; see below.
- `src/record/`, `src/encode/` — recording + encoding; see "Pipeline" below.
- `src/audio/` — mic capture/cleanup chain (`input.rs` composes the stages,
  `clean_mic.rs` chain orchestration, `filters/` the DSP stage implementations —
  gate/AGC/AEC+WebRTC-NS/RNNoise/VAD, plus `duck.rs`, the system-track sidechain
  ducker `record::pump` runs (DRAGON-128) — behind a minimal seam, `ducking.rs`
  (the unrelated MPRIS pause-other-players guard), `devices.rs`, `meter(s).rs`).
  CAUTION area — see `CLAUDE.md`. On macOS the same chain is fed by different
  sources: `clean_mic.rs`'s ffmpeg mic tap grows an `-f avfoundation -i ":<idx>"`
  arm (same 48k mono f32le contract), and system audio comes from an audio-only
  SCK stream in `capture.rs`'s `MonitorCapture` (planar/interleaved f32 → stereo
  48k, `StreamAnchor`-stamped) instead of a Pulse monitor — the DSP filters
  themselves are byte-identical across platforms.
- `src/detect/` — in-region scanners: `codes/` (QR/barcode via `rxing`) and
  `text/` (OCR via the `tesseract` binary).
- `src/platform/` — the per-platform PLUGIN layer (DRAGON-220). The seam map,
  the `#[path]` mount registry, and the add-a-plugin recipes live in
  `platform/mod.rs`'s module doc; start there. Portable spine: `backend.rs`
  (the `CaptureBackend` capability trait + `Caps` + the Wayland protocol
  probe), `compositor.rs` (facade: portable `Toplevel`/`WinRect` plus the
  mac/fallback arms; Linux re-exports the cosmic plugin's enumeration),
  `services.rs`, `tray_stub.rs`, `daemon_ipc.rs`, `screencast_stub.rs`.
  Physical files sort into plugin folders while every module path stays
  stable via `#[path]` mounts:
  - `linux/native/` — the compositor-direct capture stack: `screencopy.rs`
    (cctk ext-image-copy-capture client, incl. the cursor session) and
    `screenshot.rs` (high-level grabs + the frozen scene), mounted at
    `crate::screencopy` / `crate::screenshot`.
  - `linux/portal/` — `screencast.rs` (xdg ScreenCast session), `pipewire.rs`
    (in-process frame consumption), `pixfmt.rs`; mounted at
    `platform::{screencast,pipewire,pixfmt}`.
  - `linux/{cosmic,gnome,kde,wlroots}/` — the `DesktopProfile` axis:
    per-desktop config readers + quirks (capture stays protocol-keyed via the
    probe). `cosmic/` owns the cctk toplevel enumeration/activation
    (`compositor.rs`), the com.system76 theme + glass readers (`theme.rs`),
    the cosmic-bg wallpaper arm, `is_cosmic`, and the preview-float tiling
    exception (`quirks.rs`); `gnome/`/`kde/`/`wlroots/` own their wallpaper
    arms and document the future native tiers (DRAGON-100 / DRAGON-97).
  - `linux/tray.rs`, `linux/daemon.rs`, `linux/autostart.rs` — ksni recording
    tray, the Linux resident, XDG autostart; mounted at `crate::tray`,
    `crate::daemon_linux`, `platform::linux_autostart`.
  - `mac/` (all `cfg(target_os = "macos")`) — the ScreenCaptureKit plugin in
    facet folders (paths stable; see the Facet index in `mac/mod.rs`):
    `screencapturekit/sck_stream.rs` (the reusable `SckSession` +
    `SCStreamOutput`/`SCStreamDelegate` seam used by stills and the recording
    worker); `wm/` — `window.rs` (per-`NSScreen` overlay NSWindow tweaks + the
    DRAGON-154 pre-order-front chrome strip that opts the overlays out of
    AeroSpace's window detection, with the legacy pause/resume +
    `AerospaceGuard` death-pipe babysitter behind `CCK_AEROSPACE_PAUSE=1`),
    `focus.rs`, `spaces.rs`, `active_window.rs`, `coords.rs` (the AppKit
    to app-coordinate mapper); `services/` — `file_panel.rs`,
    `login_item.rs`, `appearance.rs`, `env.rs`; root files `tcc.rs`
    (prompt-free permission probes + one-shot requests + System-Settings deep
    links; the pure `map_*_status` reducers are unit-tested), `wallpaper.rs`
    (NSWorkspace desktop-picture resolution with an honest HEIC/missing
    degrade), `pinch.rs`, `screenshot.rs` (mounted at `crate::screenshot`
    off-Linux), `tray.rs`, `daemon.rs`. Read each module's doc comment before
    touching overlay placement or the WM dance.
  - `windows/README.md` — the honest fill-in scaffold for a future Windows
    plugin (not compiled).
- `src/state/` — `schema.rs` (the persisted `Persisted` struct) + `store.rs`
  (TOML load/save/migrate, legacy RON read).
- `src/widgets/` — reusable `iced::Widget`s: `region_selection.rs`,
  `output_selection.rs`, `zoom_pan.rs`, `drag_area.rs`, `spinner.rs`.
- `crate::screencopy` (low-level Wayland screencopy client) and
  `crate::screenshot` (high-level grabs built on it: stitch/composite/
  decorate) live under `src/platform/linux/native/` (off-Linux the
  `crate::screenshot` mount points at `src/platform/mac/screenshot.rs`);
  `src/compose.rs` (pure `RgbaImage` compositing: corners/shadow/border);
  `src/wallpaper.rs` (decode + memoized placement; `detect()` walks the
  Linux desktop profiles in fixed ladder order).
- `src/share/` — post-capture actions, run via a re-exec of this binary
  (`reexec.rs`): `clipboard.rs`, `notify.rs`, `open.rs`, `wifi.rs`.
- `src/media/` — PNG `tEXt` metadata chunk read/write.
- `src/geometry.rs`, `src/selection.rs`, `src/shortcuts.rs`,
  `src/platform/linux/tray.rs`, `src/platform/tray_stub.rs`, `src/util.rs` —
  pure rectangle/quad math, the resolved capture-target type, the keybinding
  model (`Keymap`/`Action`/`Shortcut`), the Linux system-tray recording
  controls (`ksni`, mounted at `crate::tray`), and the fallback `TraySession`
  stub (macOS mounts `platform/mac/tray.rs` instead — the resident menu bar
  lives in `src/platform/mac/daemon.rs`, see "Resident mode"). `util.rs` also holds
  `locate_tool` — the ffmpeg/ffprobe locator (env override → `.app` `Resources/`
  sidecar → dev `vendor/ffmpeg/macos-arm64/` → `PATH`).

### `src/app/`

- `mod.rs` — the `App` struct + the top-level `Msg` enum.
- `application.rs` — the `cosmic::Application` impl (init/view/update/
  subscription); `update` is a thin dispatch to per-domain `update_*` methods.
- `update/` — the `update_*` bodies, one file per message domain, mirroring
  `message/` (`capture.rs`, `recording.rs`, `detect.rs`, `settings.rs`,
  `window_chrome.rs`). `PreviewMsg` is the exception: `update_preview` lives
  with the module it drives, `preview/mod.rs`.
- `subscriptions.rs` — every timer/poll, one named `sub_*` fn per trigger
  condition, batched by `subscriptions()`.
- `keyboard.rs` — `handle_key` resolves a raw key press through
  `shortcuts::Keymap` to an `Action`, then to a `Msg`.
- `shell.rs`, `surfaces.rs` — the surface story; see below.
- `layout.rs`, `theme.rs` — toolbar geometry constants; COSMIC theme readers.
- `overlay/` — the capture UI (`toolbar/`, `marks.rs` for QR/OCR overlays,
  `menus.rs`).
- `settings/` — the settings window (`mod.rs` CSD shell + nav, `deps.rs`
  capability/dependency model, `row.rs` declarative row helpers, `pages/*` one
  file per tab).
- `permissions/` — the **macOS permission-checker window** (DRAGON-130): the
  CleanShot/Rectangle-style onboarding surface. `mod.rs` holds the pure model
  (`PermStatus`/`CardAction`/`card_action` — unit-tested, the `login_item`
  `row_state` pattern), the `Probe` snapshot + `probe_now` (off-view live
  probes), and `open_permissions_window`; `view.rs` (cfg macOS) the card view.
  Mirrors the `--settings` window plumbing exactly: `PermissionsState` field on
  `App`, a `view_window` branch, `sub_permission_poll` (1s live refresh while
  open), a `PermissionsMsg` domain (`update/permissions.rs`). Entry points: the
  `--permissions` CLI flag; a capture launch missing the Screen Recording grant
  routes here instead of an empty capture (`application.rs`, superseding the old
  bare `request_screen_capture`, keeping the `mac_first_run_seen` once-guard as
  the card's Request-vs-Open-Settings input); and the resident daemon spawns a
  `--permissions` child ONCE at startup when the grant is missing. The Screen
  Recording card carries a **Relaunch** button — macOS only applies that grant
  to a fresh launch, so the button spawns `current_exe --permissions` detached
  and exits.
- `preview/` — the post-capture editor. `mod.rs` holds the shared types
  (`PreviewState`/`PreviewKind`) + the `update_preview` dispatch; around it
  (DRAGON-115 split): `surface.rs` (overlay-vs-window + all sizing math),
  `chrome.rs` (the `Tb` toolbar builders + bars + edit-toolbar views),
  `viewport.rs` (zoom/pan state + math + the zoom control), `open.rs`
  (surface lifecycle + composed views), `share.rs` (Save As / reload / the
  background bake), `covermark.rs` (picker + overlay re-raster), plus the
  media modules `image.rs`, `video.rs`, `timeline.rs`, `playback.rs`,
  `layers.rs`, `edit.rs`.
- `message/` — `Msg`'s per-domain sub-enums.
- `num_field.rs`, `persist.rs`, `portal.rs`, `audio_ui.rs`, `capture_flow.rs`,
  `recording.rs` — numeric-input widget pairing, settings persistence glue,
  folder-picker portal call, audio-settings view helpers, capture-flow and
  recording-lifecycle orchestration.

## `Msg` dispatch

`Msg` (`app/mod.rs`) is a thin wrapper over per-domain sub-enums —
`CaptureMsg`, `RecordingMsg`, `DetectMsg`, `SettingsMsg`, `WindowChromeMsg`,
`PreviewMsg` (all defined under `app/message/`, re-exported from there). Each
variant is unwrapped once, in `application.rs`, into a matching `update_*`
method (bodies under `app/update/`, one file per domain). Keep new messages in
their domain's sub-enum; view code should not hand-handle another domain's
message.

## The surface story

`app/shell.rs` is the ONLY place that creates/destroys a compositor surface
(today: wlr-layer-shell via libcosmic; the non-layer-shell backend for
GNOME/macOS/Windows branches inside these same functions per DRAGON-93/94/95).
`app/surfaces.rs` builds on it: per-output capture overlays, and
`finish_session` — THE lifecycle seam for ending a one-shot session (capture
shared, preview closed, or unrecoverable error all route through it, so the
resident-app platforms only need to change this one function).

The post-capture preview is either an `Overlay` (fullscreen layer-shell, like
the capture UI) or a `Window` (resizable CSD toplevel) — `PreviewSurface` in
`app/preview/surface.rs`. The persisted setting `preview_windowed` decides what to
mint for the NEXT preview; `PreviewState.surface` records what is actually
open and drives behavior/chrome/close paths — never resurrect a close path
that consults the setting instead of the open surface's real kind.

## Resident mode (macOS) — the daemon

The app is ALWAYS one-shot: `finish_session` (`app/surfaces.rs`) always calls
`iced::exit()` (on macOS it first resumes the tiling WM + releases the AeroSpace
babysitter). macOS residency is an OPT-IN setting (`resident`; Settings → General
→ Behavior) implemented by a SEPARATE menu-bar **daemon** — NOT by keeping the
GUI process alive. The in-app resident idle cost ~440MB (the whole iced/wgpu app
idling just to listen for a hotkey); the daemon idles at ~14MB phys_footprint.

- **`src/platform/mac/daemon.rs`** (mounted at `crate::daemon`,
  `cfg(macos)`) — a tiny AppKit-only process: an
  `NSApplication` with the Accessory activation policy (LSUIElement in the bundle
  plist; set programmatically so the dev binary also stays out of the Dock), an
  `NSStatusItem` with the six-item menu (Scanner / Capture Region / Window /
  Monitor / — / Settings… / Quit), and the process-wide PrintScreen (+ F13)
  `global-hotkey`. It NEVER touches `app::run`, so the iced/cosmic/wgpu graph is
  never initialized — that is what buys the memory number. `NSApp.run()` blocks
  the main thread for the daemon's life; menu callbacks act DIRECTLY (spawn a
  child / terminate) — no drain queue. A background thread drains the hotkey
  receiver + a SIGUSR1 flag, spawning detached one-shot capture children.
- **Early branch** (`main.rs`, `cfg(macos)`) — BEFORE any GUI init: a BARE launch
  (no capture-mode / `--settings` / `--preview` / worker flag) with `resident`
  on runs `daemon::run()` and never returns. Every other launch (capture flags,
  `--settings`, `--preview`, or non-resident) falls through to `app::run` exactly
  as on Linux.
- **Menu/hotkey/signal actions** — each spawns the full app as a DETACHED
  (`setsid`) one-shot child with the matching CLI flag (Scanner→`--scan`,
  Region→`--region`, …, Settings→`--settings`; hotkey/SIGUSR1→`--region`, the
  bare default). Detached so a child crash never touches the daemon and there's
  no SIGCHLD to reap. Each child captures and EXITS at finish — same as Linux.
- **Lifecycle** (`src/instance.rs`) — the daemon takes its own single-instance
  DAEMON lock (`acquire_daemon_lock`, separate from the capture lock so children
  can still take THAT) and installs the SIGUSR1 handler first thing (no boot
  race). A second bare launch finds the daemon lock held → `signal_existing_capture`
  SIGUSR1s the running daemon → daemon spawns the default capture child → second
  process exits. `SetResident(true)` (settings UI) spawns the daemon detached (menu
  bar appears at once); `SetResident(false)` calls `signal_daemon_quit` (SIGTERM
  the daemon-lock holder → AppKit terminates the run loop → menu bar disappears)
  and unregisters the login item. The daemon's Quit menu item is `NSApp terminate:`.
- **`acquire_scene`** (`app/mod.rs`) — the scene grab (precapture thread + frozen
  output snapshots) factored out of `init()`; every capture child runs it once at
  launch (`active = !settings_only && !preview_mode`), exactly as before.
- **`AerospaceGuard`** — armed at the `seed_overlays_mac` choke point, released in
  `finish_session`/`quit_now`; its death-pipe babysitter restores AeroSpace tiling
  even across a crash (see `platform::mac::window`). Only engaged when the
  `CCK_AEROSPACE_PAUSE=1` escape hatch actually paused the WM — the DRAGON-154
  default never disables AeroSpace at all.

## The preview layer stack

`app/preview/layers.rs` is a custom wgpu shader primitive (`LayerStack`) that
draws a stack of pixel layers, each keyed by a stable `LayerKey` to its OWN
persistent GPU texture, re-uploaded in place every frame instead of minted
fresh (which churned iced's texture atlas and flickered). This is what lets a
playing video frame and a covermark overlay coexist without fighting over one
texture slot — a real defect in the previous single-texture design. `RasterSlot`
(same file) is the reusable coalescing-producer state (invalidate/begin/finish)
behind each editable layer's off-thread raster job — never hand-roll a
generation counter for a new layer. To add one: see the 3-step recipe in
`layers.rs`'s module doc.

## Capture → record → encode pipeline

`crate::screencopy` (`src/platform/linux/native/screencopy.rs`) is the shared
low-level Wayland screencopy client; single
grabs go through `crate::screenshot` (stitch/composite/decorate) for
screenshots. Recording has two capture sources — owned screencopy
(`src/record/screencopy.rs`) or the PipeWire portal (`src/record/pipewire.rs`,
fed by `platform::screencast` + `platform::pipewire`, physically under
`src/platform/linux/portal/`) — each with a
CPU readback path piping raw frames to the `ffmpeg` binary
(`src/encode/command.rs`). `src/record/zero_copy.rs` (feature `zero-copy`,
default on) is a GPU alternative for both sources: DMA-BUF frames go straight
into an in-process hardware encoder (`src/encode/gpu.rs`), no CPU readback.
`src/encode/device.rs` + `plan.rs` + `preset.rs` pick and configure the
encoder (NVENC/VAAPI/software, and — on macOS — VideoToolbox, tried ahead of
the software fallback via `videotoolbox_plan`); `src/encode/resolution.rs` and
`pixfmt.rs` handle size-fitting and RGBA→NV12 conversion. `src/record/finalize.rs`
bakes the live mic/system-audio mute timeline into the recorded file at the end.

On **macOS** the fourth worker path is `src/record/sck.rs` — an `SCStream`
(built on the `platform::mac::sck_stream` seam) pushes screen frames to a
delegate on SCK's serial queue; the delegate copies each `CVPixelBuffer` out as
tightly-packed top-left RGBA (the BGRA→RGBA swizzle rides that copy) and hands
it to the same media-clock loop the Linux workers run. `MacRecordTarget`
(`record::mod`, a `cfg(macos)` field on `RegionRecordParams`) selects the SCK
filter: `Region` (overlap + `sourceRect` crop), `Window(id)`
(`initWithDesktopIndependentWindow` — occlusion-independent, so window recording
survives being covered), or `Display(name)` (full bounds, no crop). The
media-clock plumbing shared by every OWNED path — the frame-writer closure, the
audio pre-flight (`try_start_owned_audio`), the FIFO/smoke-check helpers — lives
in `src/record/owned.rs`, relocated verbatim out of the Linux-only
`record::pipewire` so the SCK worker reuses it without pulling in PipeWire.

Pausing a recording freezes the OWNED media clock (DRAGON-125/127): zero video
ticks, the mixer frozen, in ONE continuous file — no segments, no re-spawn; the
capture connection stays alive and nothing is captured while paused. See
"Recording pipeline invariants" in `CLAUDE.md` for the full sync/pause model
(and the pause-gated liveness budgets it requires).

## Tests

Unit tests live at the bottom of the file they test (`#[cfg(test)] mod
tests`), close to pure-logic islands: geometry, parsing, validators, state
machines, encoder preset/resolution policy, shortcut matching, zoom/pan
clamping, and so on — anything exercisable without a compositor, D-Bus, or
`ffmpeg`. `rstest` is available for table-driven cases. The 4 CLI-level tests
in `tests/cli.rs` drive the compiled binary (via `assert_cmd`) for
`--help`/unknown-flag/`--inspect` behavior; `tests/ocr/` holds a small labeled
image corpus used by the `--ocr-bench` harness, not `cargo test`.

## Historical record

`docs/archive/` holds finished tickets' working logs (see its own README) —
useful for "why" archaeology, not current behavior.
