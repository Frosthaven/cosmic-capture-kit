# Architecture

A map of the tree as it exists today, for contributors. Working agreements
(build/test/lint commands, CAUTION areas, conventions) live in the repo-root
[`CLAUDE.md`](../CLAUDE.md) — read that first; this doc is the deeper map it
points to.

## Module tree

- `src/main.rs` — argv parsing (mode/kind/countdown/`--preview`/`--inspect`/
  `--settings`), the settings single-instance lock (`src/instance.rs` — a
  CAPTURE launch takes no lock at all since DRAGON-351), launches the
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
  - `windows/` (all `cfg(windows)`) — the Windows plugin (DRAGON-229),
    operational for stills, the interactive overlay, delivery, screen
    recording, and named-mutex single-instance. Facet files: `backend.rs`
    (the `CaptureBackend` impl), `monitors.rs`/`window_list.rs`/
    `window_capture.rs`/`cursor.rs`/`gdi.rs` (the Win32 still sources — BitBlt /
    PrintWindow ladder / GetIconInfo), `dpi.rs` (PMv2), `wallpaper.rs`
    (IDesktopWallpaper), `wm/window.rs` (overlay placement + the komorebi
    opt-out via `WS_EX_DLGMODALFRAME` before first show), `services.rs`
    (clipboard / WinRT toast / reveal / `netsh` Wi-Fi), `instance.rs`
    (named-mutex single-instance + the Toolhelp sibling sweep), and the
    recording bodies `record_worker.rs` (Windows.Graphics.Capture, mounted at
    `record::wgc`), `wasapi_loopback.rs`, `named_pipe.rs`, `audio.rs`,
    `process.rs`. `screenshot.rs` is mounted at `crate::screenshot` off-Linux
    alongside mac's. Residual work (hardware encoder tiers, residency,
    packaging) is DRAGON-231; the module map + traps live in `windows/README.md`.
    The whole `windows/` dir is stripped from the public GPL export (closed
    split), like `mac/`.
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
- `src/cloud/` — cloud accounts (DRAGON-482): connect a drive, upload a capture
  to it, optionally copy a share link back. Read `mod.rs` first, it holds the
  shape everything else reads.
  - `mod.rs` — the REGISTRY (`registry()`), a compile-time table of
    `ProviderSpec`s: id, display name, icon, `ProviderCaps`, `AuthKind`, and the
    host lists a provider's URLs are checked against. **Screens read the CAPS,
    never the provider id**: the five drives disagree about almost everything
    (two have no public API at all, one can expire a link, one takes a
    visibility choice), and stating that once is what keeps the disagreement out
    of every UI. Also the derived transport allowlist (`registry_hosts`), the two
    provider-URL validators (`device_url_allowed` / `web_url_allowed`), and the
    cross-process file lock the oauth gate takes.

    A cloud AUTO-DELETE feature lived here from DRAGON-482 until DRAGON-505: a
    per-account "delete uploads after N hours" window, a `ledger.rs` recording
    each upload's deadline, and a six-hourly sweep thread in all three resident
    daemons. It is gone because no provider offers server-side file TTL, so what
    shipped was a client-side substitute that only deleted while our own daemon
    happened to be running, which is too weak a guarantee to promise in settings
    copy. `providers::delete_file` deliberately stayed: the cancel-after-commit
    path (DRAGON-496) and the undo-during-hold path (DRAGON-507) both need it,
    and both delete with the user watching. See `cloud/mod.rs`'s tombstone.
  - `oauth.rs` — authorization-code-with-PKCE and device-code flows: the
    loopback redirect catcher, `TokenSet`, and `ensure_fresh`, the ONE place a
    live access token comes from. Single-flight per account across threads AND
    processes (this app's model is short-lived processes, so an editor and a
    detached upload child refreshing at once is the ordinary case, and a
    provider that ROTATES refresh tokens turns that race into a permanently
    disconnected account). The loopback listener answers and IGNORES anything
    that cannot echo the `state` we minted, so a local process cannot end a
    connect the user is still working through.
  - `providers/` — one file per drive (`gdrive`, `onedrive`, `dropbox`) behind
    the `ProviderOps` trait, plus `mod.rs`'s five public functions and the ONE
    dispatch point (`ops`) that matches on a provider id. Each public function
    calls `oauth::ensure_fresh` once and hands the token down, so no provider
    file touches `oauth` or `secrets`. Shared plumbing lives beside them:
    chunk spans, `Content-Range`, the size-scaled `upload_budget`, and
    `TempChunk` (a 0600 byte range on disk, removed on drop).
  - `http.rs` — the transport: `curl` with a stdin config. **Every header, every
    form field AND the URL ride that config**, because argv is world-readable on
    Linux and two of these URLs are capabilities (Google's `upload_id`,
    Microsoft's `tempauth`). https only, only to a host the registry names, no
    redirects, an explicit budget per request with no default, and a bounded reap
    so a wedged curl is killed rather than waited on.
  - `secrets.rs` — the token store seam: `store`/`load`/`delete` over the
    platform keyring, with a 0600 file fallback that is a real configuration (a
    Linux box with no Secret Service) rather than an error path. **Decision here,
    syscall in the plugin**: the item naming, the account-id validation and the
    file name are pure and unit-tested on every host; `platform/*/secrets.rs`
    only calls its OS.
  - `child.rs` — the `--cloud-upload` helper's whole life. An upload runs in a
    DETACHED re-exec (the same technique every other post-capture action uses),
    because a transfer can take minutes and this app's model is one-shot. One
    outer budget, `UPLOAD_BUDGET`, armed before anything else. It records no
    `diag::Failure`: this is not a capture session, the capture was delivered to
    the editor long before an upload was asked for, and adding an upload code to
    that closed vocabulary would be the second failure vocabulary CLAUDE.md
    forbids.
  - `upload.rs` — staging (a copy of the capture the child owns, so the editor
    can close) and the child spawn. `tray.rs` is mounted under it.
  - `tray.rs` — the upload progress counter's portable model: the `Face` a tray
    item shows (a bucketed percentage, then a tick or a cross held for
    `FINISH_HOLD`), the tooltip wording, and the seven-segment SVG the two
    rastering platforms draw. The three surfaces are
    `platform/{linux,mac,windows}/upload_tray.rs`; a platform with no tray gets
    an honest no-op and the upload is unaffected.
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
  (surface lifecycle + composed views), `share.rs` (the save destination picker,
  the background bake, and the copy/delete completion seam), `naming.rs` (what
  path the save picker opens on), `covermark.rs` (picker + overlay re-raster),
  plus the media modules `image.rs`, `video.rs`, `timeline.rs`, `playback.rs`,
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

**Failing out loud (DRAGON-415).** A session that ends WITHOUT delivering anything
calls `diag::note_failure(...)` (DRAGON-419, for us) and then `App::fail_session`
(`app/failure.rs`, for the user) rather than `finish_session` directly. On macOS
that presents a native app-modal `NSAlert` (`platform/mac/services/alert.rs`)
before the child exits; the user dismisses it and the session then ends through
the normal path. It exists because macOS is the one platform where a `log::warn!`
reaches nothing at all, so every failure read to the user as "it just closed
itself and saved nothing".

The two mechanisms share ONE classification, `diag::Failure` — the log names it
and the alert is built from the same record, so there is no second taxonomy to
drift. `fail_session` reads `diag::root_failure()` (the FIRST note of the
session), because failures are recorded in causal order and the last note is the
symptom, not the diagnosis. The message TABLE (`alert_message`) is pure and
unit-tested on every platform; presentation is macOS-only, and Linux/Windows
`fail_session` is byte-identically `finish_session`. The rules the table encodes:
never blame a permission we have not checked (`CGPreflightScreenCaptureAccess` is
read live at failure time), never name the Sonoma (macOS 14) `SCShareableContent`
hang on a build that does not have it — DRAGON-439 widened that from 14.0-14.3
to all of macOS 14, since Apple's 14.4 fix made the stall rarer but did not end
it, and the minor version now only picks whether to advise the update — and
never surface a `diag` detail string
except the recording worker's reason (the rest is telemetry, not user copy).

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
  DAEMON lock (`acquire_daemon_lock`; the one-shot capture children it spawns
  take no lock at all) and installs the SIGUSR1 handler first thing (no boot
  race). A second bare launch finds the daemon lock held → `signal_existing_capture`
  SIGUSR1s the running daemon → daemon spawns the default capture child → second
  process exits. `SetResident(true)` (settings UI) spawns the daemon detached (menu
  bar appears at once); `SetResident(false)` calls `signal_daemon_quit` (SIGTERM
  the daemon-lock holder → AppKit terminates the run loop → menu bar disappears)
  and unregisters the login item. The daemon's Quit menu item is `NSApp terminate:`.
- **Startup self-exit guard** (`src/startup_guard.rs`, DRAGON-413) — the flip side
  of "detached, nothing to reap": nothing noticed a child that never reached
  `finish_session`, so a startup stall silently piled up invisible processes (a
  customer accumulated six retries). Each CAPTURE child now arms a detached budget
  clock in `main` — BEFORE `app::run`, so a hang inside `App::init` is covered too —
  and quietly `process::exit(0)`s (after the same tiling-WM resume + marker drop
  `finish_session` does) if it never presents anything within `DEFAULT_BUDGET` (90s).
  "Presents" = a PLACED capture overlay, countdown, in-flight capture, live
  recording, preview editor or settings window, snapshotted into a
  `startup_guard::Surfaces` by `startup_presence()` on every `App::update` and
  classified by `startup_guard::classify`; the FIRST such report disarms the guard
  permanently. PLACED, not merely minted (DRAGON-439): `outputs` gains its entry at
  winit-window creation, but the macOS overlay draws a transparent `Space` until
  `configure_overlay` raises and reframes it, so counting the mint disarmed the guard
  while the user could still see nothing at all. The
  permission checker instead SUSPENDS the clock for as long as it is open — a child
  showing it is doing its job, and reading it must never accumulate toward a kill.
  macOS-only for now (Linux/Windows are byte-identical; both could opt in by calling
  `arm` from their launch paths). Decision logic is pure and unit-tested.
- **`acquire_scene`** (`app/mod.rs`) — the scene grab (precapture thread + frozen
  output snapshots) factored out of `init()`; every capture child runs it once at
  launch (`active = !settings_only && !preview_mode`), exactly as before.
- **`AerospaceGuard`** — armed at the `seed_overlays_mac` choke point, released in
  `finish_session`/`quit_now`; its death-pipe babysitter restores AeroSpace tiling
  even across a crash (see `platform::mac::window`). Only engaged when the
  `CCK_AEROSPACE_PAUSE=1` escape hatch actually paused the WM — the DRAGON-154
  default never disables AeroSpace at all.

## Where a capture lives, and what the editor's actions do (DRAGON-467)

Four rules, and every one of them is a pure decision with tests beside it.

1. **Where the file is written** — `capture_flow::capture_write_dir`. The
   "Automatically save originals" setting (per media kind) picks between the
   user's configured folder (`screenshot_dir` / `record_dir`) and a transient
   location. ON is the default and reproduces every earlier version. OFF means
   an untouched capture leaves no file behind.

   The transient location differs by MEDIUM (`capture_flow::transient_dir`).
   Stills go to the session runtime directory. RECORDINGS go to a disk-backed
   cache folder (`util::transient_recording_dir`,
   `~/.cache/cosmic-capture-kit/transient`), because `$XDG_RUNTIME_DIR` is a
   tmpfs sized at ~10% of RAM and a take buffers both its live `.recording`
   temp and its finished file there, so a long recording could ENOSPC
   mid-capture. `/tmp` is no answer either: it is tmpfs on Arch too.

   The transient file is deliberately never cleaned up on close: on Linux the
   clipboard worker is a detached process holding a `file://` URI for a
   recording, so removing it would break a paste the user can still perform.
   Accumulation is bounded by AGE instead, by
   `util::sweep_transient_recordings` at `util::TRANSIENT_MAX_AGE` (7 days),
   run detached from `App::init`.
2. **Where a Save writes** — `preview/naming.rs`'s `save_prefill`, reached through
   `App::preview_save_target`. Save IS Save As: it opens the destination picker
   pre-filled with the document's own file once it has saved, else the configured
   folder plus the capture's name. The native dialog's replace prompt is what
   guards an existing file. There is no `-edited` suffix and no collision walk;
   `naming.rs`'s module doc carries the survey of CleanShot X / ShareX / the
   Windows Snipping Tool / Preview / Flameshot / Greenshot / Snagit that retired
   them (none of them names a derived copy; a known path means overwrite).
3. **The clipboard** — every capture goes on it as it is taken:
   `finish_share` for an editor-less delivery, `auto_copy_preview_on_open`
   otherwise. There is one exception, and it is a size cap rather than a
   setting: an IMAGE over `share::AUTO_COPY_MAX_BYTES` is skipped with a toast
   naming the limit (a recording is never skipped, since it copies as a path).
   "Automatically copy changes on exit" then re-copies the EDITED result as the
   editor closes (`exit_copies_changes`), because the clipboard would otherwise
   still hold the untouched grab.
4. **The exit path** — `PreviewMsg::Cancel` runs two gates in order: the
   settings-driven ask (`close_needs_confirmation`), then the exit copy, which
   arms `close_after_share` around a plain copy so a FAILED copy leaves the
   editor up instead of closing over lost work.

   The ask card offers exactly THREE options: **Save** (the picker, then
   close), **Continue editing** (dismiss), and **Close without saving** (the
   discard, which runs the SAME exit copy, because "without saving" is about
   the disk and the setting is about the clipboard). Copy and Delete used to be
   on it and are not: a copy neither saves nor discards the edits the card is
   asking about, so offering it as a way out invited a close that quietly
   dropped work.

Two economies keep those four from doing redundant work, both pure decisions in
`preview/edit.rs`. `bake_need` serves the LAST bake's artifact when the scene
has not moved since (so "Save then Escape" does not encode twice), and
`clipboard_is_current` skips the exit copy when the clipboard already holds this
exact state. Both are keyed on the undo depth and both are invalidated by
`EditState::push_op` on an abandoned redo branch, exactly like `saved_depth`.

**The pristine-source invariant** (`edit::bake_prep`, `PreviewState::bake_src`):
every bake composites the live scene onto UNTOUCHED media, so a bake must never
read its own output. Saving in place is now the default gesture, so a still
saving over its own capture snapshots the pristine bytes into the runtime dir
and repoints `bake_src` at them, while a recording (too large to copy) bakes
through a temp, renames over the destination, and then COMMITS: the document is
repointed at the result, the scene and history are reset, the file is re-probed
and the user is told the edits are now part of it.

The top-right toolbar group (`chrome.rs`'s `share_group`) is Save / Copy /
Share / Upload. There is no Delete: the editor stopped deleting files at all
(user decision), which is coherent with rule 1 above, since with
"Automatically save originals" off an unwanted capture never reaches the
user's folder and closing the editor IS the discard. The trash button, its
`Ctrl+Shift+X` action, `PreviewState::delete_paths` and the whole unlink path
went together. Share only APPEARS where
`platform::services::share_available()` says the system has a share sheet:
Windows since DRAGON-474, macOS since DRAGON-480, and nowhere else
(`share/share_sheet.rs` documents where each platform stands; the Windows body
is in `platform/windows/services.rs`, the macOS body in
`platform/mac/services/share.rs`);
Upload is always shown but disabled, because it is a feature that does not exist
anywhere rather than a capability of the machine. Upload renders through
`Tb::tool_button_gated`, which keeps the tooltip so a permanently-off button can
explain itself.

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

### Known platform difference: the Windows-overlay covermark fold (DRAGON-235 / DRAGON-395)

On the **Windows OVERLAY** preview surface only, the still base (and the video
poster) is drawn through the `LayerStack` shader rather than `widget::image`,
with the covermark folded into that same stack. Everywhere else — Linux, macOS,
and the Windows *windowed* preview — the base is a `widget::image` and the
covermark is its own element stacked above.

**Why.** DRAGON-235 found that iced's raster-image pipeline does not composite
on the premultiplied transparent overlay surface: the identical opaque pixels
show through the opaque windowed surface but vanish on the overlay, while the
shader (same alpha blending) composites them correctly. The fold keeps the
covermark to a single `LayerStack` per window, because slots are keyed per
window rather than per widget and two stacks in one window would fight over
slot pruning.

**What it costs.** A z-order deviation. The real-time effects shader is a
distinct primitive stacked on top of the base element, so on the Windows
overlay the effects draw **over** the covermark, and **under** it on every
other platform and surface. Text annotations are unaffected — they are
canvas-drawn (DRAGON-373) and have always ridden above everything.

**Status.** The DRAGON-235 premise has never been verified by anyone who could
run both sides: it was found on Windows 10, and Linux builds cannot compile the
arm. The libcosmic/iced pin has not moved since, so it cannot have been fixed
upstream by accident either. DRAGON-427 then removed the overlay editor from
Windows 10 entirely, so this arm now fires on **Windows 11 only** — which is a
configuration that can finally be A/B'd.

**To A/B it.** The overlay editor is not the default (`preview_windowed`
defaults true), so first set Settings → Editor appearance → fullscreen overlay.
Then take a capture, apply a covermark *and* an effect, and compare the
stacking with and without `CCK_TEST_UNFOLD_COVERMARK=1`, which bypasses the
fold and takes the portable path (`app::preview::layers::unfold_covermark`).
Use the same machine's windowed preview as the reference rendering.

**If the A/B clears it.** If the unfolded overlay renders the base and the
covermark correctly and stacks them like the windowed preview, then DRAGON-235
no longer holds: delete the two `#[cfg(windows)]` fold arms
(`preview::image::…`, `preview::video::video_still_content`), the
`unfold_covermark` hook, `layers::rgba_handle_frame`, and this section. If the
unfolded rendering blanks or mis-composites, DRAGON-235 stands and this
deviation is the accepted answer — say so here and keep the hook.

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
