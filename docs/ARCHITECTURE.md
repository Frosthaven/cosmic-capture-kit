# Architecture

A map of the tree as it exists today, for contributors. Working agreements
(build/test/lint commands, CAUTION areas, conventions) live in the repo-root
[`CLAUDE.md`](../CLAUDE.md) — read that first; this doc is the deeper map it
points to.

## Module tree

- `src/main.rs`: argv parsing (mode/kind/countdown/`--audio`/`--preview`/
  `--inspect`/`--settings`/`--permissions`), the settings single-instance lock
  (`src/instance.rs`; a CAPTURE launch takes no lock at all since DRAGON-351),
  launches the `cosmic::Application`.
- `src/cli/`: `diagnostics.rs` (the `--test` harness), `inspect.rs`
  (metadata dump) and `sync.rs` (the A/V-sync reference clip + calibration).
  See [`CLI.md`](../CLI.md) for the user-facing flag list.
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
  (the `CaptureBackend` capability trait + `Caps` + `CursorDelivery` +
  `Acquisition` + the Wayland protocol probe; see "How the capture backend is
  chosen" below), `compositor.rs` (facade: portable `Toplevel`/`WinRect` plus the
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
  - `clipboard.rs` also holds **`CopyRoute`** (`copy_route`, DRAGON-553): the
    ONE copy decision every copy site consults. The detached worker where the
    compositor offers data-control (that write outlives the process); the
    focused-window `iced` clipboard write everywhere else (GNOME, sandboxes),
    deferred to the window's focus event, whose serial the selection needs.
  - **Every re-exec goes through `util::self_exe`, never `current_exe`**
    (DRAGON-510). It answers `$APPIMAGE` when the app is running from one, and
    `current_exe()` otherwise. Inside an AppImage, `current_exe()` is a FUSE
    mount that dies with the process holding it, and these children are detached
    on purpose and outlive their parent, so the mount path is exactly the wrong
    thing to hand them. `util::locate_tool` is the deliberate exception: it wants
    the mount path, because the bundled ffmpeg/tesseract sidecars live there.
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
- `src/color.rs`: the colour picker's pure colour model (DRAGON-582). `Srgb`, the
  seven notations (`ColorFormat`: HEX / RGB / HSL / HSV / OKLCH / CMYK / LAB), their
  formatters and their deliberately TOLERANT parsers, over ONE sRGB / linear /
  XYZ-D65 stack so no two rows can disagree about the same colour. Three things the
  module doc states outright and the tests pin: CMYK is the naive device-agnostic
  separation, not an ICC one; an out-of-gamut LAB / OKLCh value clamps per channel
  rather than wrapping; and a hue wraps because 400 degrees is unambiguously 40. What
  the parser refuses is the wrong NUMBER of components, since guessing there is the
  "reports the wrong colour" failure the whole tool exists to avoid. No `App`, no iced,
  no platform, so the Linux gate proves all of it.
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
- `color_picker/` — the **colour picker tool** (DRAGON-582): a dimmed overlay with a
  magnifier that follows the pointer, and the result window a pick opens. `mod.rs`
  holds the state, the `PixelSource` seam and `open_color_picker_window`; `geom.rs`
  the pure decisions (which source pixel, the magnifier raster, the label-placement
  ladder, the window size, the recents write rule); `view.rs` both views. A
  `--color-picker` launch mints the SAME per-output overlays a capture does, through
  the same `shell` seam, so it inherits layer-shell / portal-fallback / PlainWindows /
  Windows-10-software routing without a second copy of any of it.

  **Read the module doc before changing where the picked pixel comes from.** It reads
  the LAUNCH-TIME FROZEN SNAPSHOT on every platform, because the picker's own dimming
  layer is on screen while it samples, so a live read of the composite would come back
  dimmed. The doc records what each platform COULD do later (Windows
  `WDA_EXCLUDEFROMCAPTURE`, an SCK content filter on macOS, nothing on Wayland) and why
  the xdg portal's own `PickColor` is the honest Linux fallback rather than the default.

  DRAGON-587 added the rest of the behaviour, and each piece has its WHY in the module
  doc: the pick's hex copy goes through the app's one copy ladder (`share::copy_step`,
  so it survives a session with no data-control, where it used to be dropped); the
  pointer sprite is hidden wherever the surface can hide it
  (`platform::overlay_hides_pointer`, keyed on the SURFACE KIND rather than the OS), which
  since DRAGON-597 is EVERY surface: a layer surface could not, because libcosmic's Wayland
  backend left `set_cursor_visible` unimplemented, and our iced fork fills that hole
  (the iced `[patch]` block in `Cargo.toml`). The arrow fallback that went with it, a
  default arrow plus a
  one-point sample shift out from under its tip, is deleted, with tombstones in
  `color_picker`'s module doc, `color_picker::geom` and `widgets::color_pick`; the
  magnifier CLIPS at a screen edge instead of being clamped or
  contain-fitted (`geom::disc_view`, which is also why the accent ring is baked into the
  raster); it ZOOMS from the trackpad, the wheel and the numpad `+`/`-` through one
  clamp (`geom::zoom_after_step`, the range being `MAGNIFIER_ZOOM_MIN` 3 to
  `MAGNIFIER_ZOOM_MAX` 26, opening at `MAGNIFIER_ZOOM_DEFAULT`, which is
  `MAGNIFIER_CELL` = 12; both ends are stated as the SOURCE PIXELS the fixed-size disc
  holds edge to edge, `156 / zoom`, so 52 at the floor, 13 at the default and 6 at the
  ceiling. DRAGON-598 raised the floor off 1:1 and DRAGON-601 raised the ceiling above
  the default, which used to BE the maximum, and two compile-time asserts now pin the
  default strictly between the two ends);
  the result window is a fixed, non-resizable size; and a pick launched from a preview
  editor's pipette is DELIVERED to that editor over the preview-handoff socket's second
  verb (`preview_ipc::Request::Color`, addressed by pid from `CCK_COLOR_TO_PID`) and
  opens no window at all.

  **Four entry points, one argv.** The tray / menu-bar entry
  (`recording_ui::CaptureAction::ColorPicker`, first in the idle menu, before
  Scanner), the preview editor's toolbar pipette (`PreviewMsg::OpenColorPicker`,
  which adds `CCK_COLOR_TO_PID`), the `--color-picker` flag, and the
  `CaptureHotkeySlot::ColorPicker` global-shortcut slot (index 6, LAST so the six
  capture slots keep their indices; ships UNBOUND like every slot, and on Linux the
  Global tab shows the command instead of a chord). Every one of them spawns
  `--color-picker`; the result window's own pick-again pipette
  (`ColorPickerMsg::PickAgain`) does the same rather than re-entering the overlay in
  this process, because a re-entry would have to re-grab the frozen scene and, on the
  portal fallback, re-request a ScreenCast mid-session.

  **One window, and any new pick updates it** (DRAGON-613). A CROSS-PROCESS rule, not
  a state update, because every pick is its own one-shot process. A pick about to show
  a window first looks for a live one (`instance::live_color_picker_windows`) and hands
  its colour there (`preview_ipc::send_color_to_picker`), becoming that window's current
  colour and the newest entry in its recents; only if nothing takes it does this process
  open a window. The fresh LAUNCH stays: it mints overlays for every output, so the next
  pick can come off a different monitor, and only the DELIVERY is redirected. This is the
  same shape as the editor handoff above, one verb along, which is why both live on the
  preview-handoff socket rather than growing a transport of their own.

  **The keyboard lanes** (`app::keyboard.rs`, not this module, because both are shared
  with the region overlay). The four arrows and the vim letters `h` `j` `k` `l` become a
  `shortcuts::Direction` and move the SAMPLE while a pick is up, the drawn REGION
  otherwise (DRAGON-599); `color_picking()` keeps the two exclusive by construction rather
  than by ordering. Hold-to-move runs on OUR cadence, never the desktop's
  (`shortcuts::nudge_step_allowed` decides, `nudge_step_due` owns the clock): a press is
  always exactly one pixel and repeats are ignored until the key clears
  `NUDGE_HOLD_DELAY`, because the system's repeat cadence is tuned for text entry and
  would make one deliberate tap cost two or three pixels, differently per machine. Bare
  Enter and bare Space then ACCEPT (DRAGON-612), routing the same `ColorPickerMsg::Pick`
  a left click sends, with the same raw pointer point; `picker_sample_state` is the pure
  three-way classification of whether there is a pixel to hand over yet, since the overlay
  maps before any pointer enter and the first moments of every launch have none. Both
  lanes sit in the FALL-THROUGH, after the keymap, so a binding the user configured always
  wins; neither key collides with any default, which is pinned by tests. A right press on
  a per-output overlay cancels (`update::window_chrome::right_click_cancels`), routing into
  the Escape path.

  **What it reads out of settings**, none of it its own: `color_picker_overlay_opacity`
  (the dim, its own `Persisted` field with its own Settings row, defaulting to the same
  33% as the active overlay; DRAGON-588 took it to zero and the owner took it back, and
  the field's doc says why so it is not re-argued), `selection_box_thickness` (the
  magnifier's accent ring, the same width the region box uses, which is what DRAGON-582
  asked for), and the theme's rounding token (every swatch in the window, one lookup, so
  they cannot drift). `recent_colors` is the one field it WRITES: `#RRGGBB` strings,
  newest first, capped at `geom::RECENTS_CAP` (10). It is persisted because the app is
  one-shot, so an in-memory list would be empty at every window open and the feature
  would do nothing. It is also user CONTENT, so it goes to the config and never to the
  debug log; nothing in the picker logs a colour value, only a `ColorFormat::id`.

  **On the Linux fallback overlay the picker covers ONE output**, the granted one, for
  the same reason a capture does: only that `OutputState` is backed by a real surface
  (see "The Linux fallback overlay" below). Nothing in the picker knows that; it is what
  minting the same overlays a capture mints buys.
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

### Photographing our own overlay (DRAGON-608, DRAGON-611)

**A capture excludes our own overlay by TIMING, not by any filter**, and that
one fact is why photographing the colour picker needs no feature of its own.

Nothing marks our surfaces as un-capturable. What keeps them out of an ordinary
shot is that `begin_capture` tears the overlay down and waits for the
`sub_pixel_capture` tick, so a live grab reads a screen we have already left
(and a frozen grab reads a snapshot taken before we painted, DRAGON-600 and
DRAGON-606). That exclusion reaches exactly one process: **our own**.

So a region capture started over a live colour picker is a live composite that
CONTAINS the picker, because the picker is a separate process whose overlay
nothing tore down. The ordinary keybinds already do the right thing, and no
second entry point is needed. DRAGON-608 originally shipped one, a
`primary+Shift+P` chord in its own `self_capture.rs`; it was removed once the
requirement was corrected, because an undiscoverable binding for a case the
ordinary path already covers is a maintenance cost with no user.

What DID need fixing is that the teardown DISTURBS the picker underneath. The
compositor hands the pointer to the newly topmost surface as a
`wl_pointer.enter`, our iced fork turns that into a `CursorMoved`, and the
picker's loupe jumps to it just in time to be photographed. The rule that
separates a revealed pointer from a moved one is
`widgets::color_pick::pointer_report_moves_sample`; read its doc before
touching pointer handling in the picker. The same disturbance applies to any of
our surfaces mapping over a live picker, not just the region path.

## How the capture backend is chosen (DRAGON-595)

Linux is the only platform with TWO runtime backends, native screencopy and the
xdg-portal ScreenCast path, so it is the only place a compile-time module mount
cannot answer "who is capturing". `App::active_screenshot_backend` and
`active_record_backend` (`app/portal.rs`) are the ONE place that resolves it,
returning a `Box<dyn CaptureBackend>`. Ask them, then ask the backend: that is how
a capability, a cursor mechanism or a metadata label can never disagree with the
plugin that implements it.

`App::screenshot_uses_portal` / `recording_uses_portal` survive underneath, and
their remaining readers are legitimate. They answer SESSION SHAPE questions, not
capture ones: which selection surface to mint, whose picker presents the target
choice, which chrome and settings rows render, which preview anchor survives
teardown. Those really are about the choice rather than about pixels.

**What deliberately did NOT move behind the trait**, so it is not attempted a
third time (DRAGON-93 promised it; DRAGON-595 measured it and stopped):

- **The pixel branches key on the HELD STREAM, not on identity, and must.**
  `capture_flow::do_pixel_capture` and `app::recording::start_recording` both fork
  on `App::pw_held`. A portal grant that fails with `CastError::Unavailable`
  proceeds with no held stream so the native path serves the capture. Re-keying
  either fork on "is the portal selected" deletes that fallback.
- **The portal plugin cannot serve stateless pixel calls.** Its pixels live in a
  session (an `OwnedFd` plus a node id) negotiated across several iced messages
  through a permission dialog and owned by `App`; and every native read funnels
  through `screencopy::connect_raw`, which returns `None` unless the compositor
  advertises protocols a sandboxed session does not get. Both reasons are
  independent and either alone is sufficient. It declares this as
  `Acquisition::Session` rather than leaving callers to infer it from `None`.
- **The frozen-reconstruction paths are not captures.** `region_windows_frozen`,
  `crop_frozen` and `stitch_region` are pure `RgbaImage` math over a scene
  captured at launch and held by `App`.

What DID move is the cursor contract, `CaptureBackend::cursor_delivery`. It is an
enum (`Sprite` vs `InStream`) because the two mechanisms differ in when the
pointer is decided and where it comes from: a native backend stamps a sprite it
locked at launch (DRAGON-214, so the pointer is where it was when the tool opened
rather than over our own toolbar after teardown), while the portal picks a stream
cursor mode before any frame exists. No boolean carries both without one backend
lying. Naming the mechanism separately is what let the "does this capture take the
pointer at all" rule collapse into one predicate, `app::cursor_wanted`; it used to
exist twice, once per mechanism, kept in step only by a test.

**One trap worth knowing before you use `cursor_delivery` for anything: the backend
SELECTED is not always the backend SERVING.** With layer shell present, the portal
chosen, and the grant failing `CastError::Unavailable`, the capture degrades to
native screencopy while `active_screenshot_backend` still answers Portal. So it must
not gate the cursor stamp, and DRAGON-595 backed that change out after finding it
dropped the pointer from exactly that capture. The question "did the frozen scene
come from the portal" is `overlay_fallback_active`, not a backend identity.

## `Msg` dispatch

`Msg` (`app/mod.rs`) is a thin wrapper over per-domain sub-enums —
`CaptureMsg`, `RecordingMsg`, `DetectMsg`, `SettingsMsg`, `WindowChromeMsg`,
`ColorPickerMsg`, `PreviewMsg` (all defined under `app/message/`, re-exported from
there). Each
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

### The Linux fallback overlay: no layer shell, one frozen toplevel (`lab/flatpak`)

cosmic-comp hides `zwlr_layer_shell_v1` from any client carrying a
`wp_security_context_v1` (every Flatpak), and mutter never implemented it at all,
so on those sessions the per-output capture overlay cannot exist. There the app
takes a FALLBACK path: grab a full-monitor frame through the ScreenCast portal
FIRST, then run region selection over that frozen frame in ONE ordinary
fullscreen xdg toplevel. Freeze stops being an extra and becomes the only mode.

The gate is `platform::overlay_fallback_seeding(layer_overlay, uses_portal)`, pure
and unit-tested, wrapped by `App::overlay_fallback_active()`. It is
PROTOCOL-keyed, never sandbox-keyed: a global can be missing because we are
refused it or because the compositor never shipped it, and the overlay wants the
same answer either way. A layer-shell session answers `false` and is
byte-identical to before the path existed.

The flow, all in `app/portal.rs` unless noted:

1. `on_output` (`app/surfaces.rs`) registers each output's GEOMETRY but mints no
   layer surface and notes no failure, then kicks ONE seed request
   (`fallback_seed_kicked`, the `immediate_kicked` shape). The kick honors the
   LAUNCH MODE: a Window or Monitor launch (the daemon's `--window` /
   `--monitor`) goes straight to the portal picker of the matching source and
   delivers through the normal held-stream path, no region overlay at all; only
   a Region launch takes the frozen-overlay seed below. A dismissed launch
   dialog ends the session as Cancelled (there is no selector to return to).
2. `request_fallback_cast` asks the portal for a monitor with the persisted
   restore token. The portal's own picker IS the monitor selection. Tokens live
   in one persisted SLOT per source type (`pw_restore_token_monitor` /
   `pw_restore_token_window`, the `RestoreTokens` pair, DRAGON-570; the legacy
   `pw_restore_token` + `pw_restore_source` pair migrates once, config v11),
   and the pure `replayable_restore_token` reads only the requested source's
   own slot: cosmic's portal restores the stored source ACROSS types, so a
   window request replaying a monitor token silently re-granted the monitor.
   Grants PERSIST across one-shot capture children because
   `platform::screencast` requests `PersistMode::ExplicitlyRevoked` (mode 2),
   which writes the portal's on-disk permission store; mode-1 grants live in a
   per-connection table and die with the requesting process, which re-prompted
   every capture (the DRAGON-552 finding, recorded in `screencast.rs`'s module
   doc).
3. `on_fallback_cast_ready` resolves the grant against the registered outputs,
   keeps it in `fallback_grant`, and pulls ONE frame off-thread
   (`pipewire::grab_frame`, its own 5s watchdog) into `fallback_frame_slot`.
4. `on_fallback_frozen_ready` builds the `FrozenOutput` for that monitor and
   `mint_fallback_window` opens the toplevel (`shell::overlay_fallback_window`),
   writing the winit id into that output's `OutputState`. The view chain
   (`view_window` → `overlay_view` → `with_frozen_bg`) is UNCHANGED: it already
   renders selection over a frozen backdrop.

Three things this path has to get right, and where they live:

* **Failing out loud.** A dismissed portal dialog is the ordinary `Cancelled`
  ending (the dialog stands in for the overlay). An unreachable portal or an
  empty grant is `OverlayNeverShown`, a frameless grab `SceneGrabTimeout`, and
  both go through `fail_session`. No new `diag::Failure` variant.
* **The monitor mismatch.** Wayland gives a client no say in which monitor a
  fullscreen toplevel maps on, so the window can land on a different one than the
  portal granted. The frozen frame keeps its ASPECT there:
  `geometry::OverlayUnits::letterbox` (pure, unit-tested) computes the one uniform
  scale that fits the whole frame, capped at 1 so a smaller monitor's capture is
  never blown up (it renders at native size, bars on all four sides, the owner's
  call from the third live test), centres it over opaque black bars, and maps
  window points back onto the frame by subtracting the offsets and dividing by the
  scale. Bar points clamp to the frame's edge and the selection walls confine to
  the visible image (`visible_capture_size`), so selection can only ever cover
  pixels the frame actually has. The backdrop draws the still at the SAME bridge's
  `letterbox_dest`, one math source for pixels and mapping. Round 1 stretched the
  still per axis (`ContentFit::Fill` plus a `stretched()` bridge) instead; it
  mapped correctly but distorted the image, and the owner requires the aspect
  kept, so the per-axis form is gone. Every uniform bridge is bit-identical to
  before the fallback existed.
* **Closing.** `App::close_overlay_surface` routes by id: `window::close` for the
  fallback toplevel, nothing for the placeholder ids of the other outputs (they
  never got a surface), the layer destroy otherwise. A WM close or an out-of-band
  destroy of that window ends the session as a cancel.

Delivery is WYSIWYG: a non-delayed region STILL crops the seed-frozen frame
(`capture_flow::fallback_still_from_frozen`, pure), and so does the in-overlay
scanner. A DELAYED still and region VIDEO keep the per-capture portal request,
because the delay exists to change the screen; the restore token means no second
prompt. The backdrop itself serves region selection of EVERY kind, video
included (`capture_flow::fallback_backdrop`, pure, deliberately without a kind
input): a fullscreen toplevel has no live desktop composited behind it, so the
seed still is the only honest backdrop there is. Known costs, all consequences
of a plain toplevel having no input zones: recording tears the window down and
the tray becomes the control, and a COUNTDOWN does the same (DRAGON-563): the
fallback window closes at countdown start and the remaining seconds render in
the tray icon (the upload counter's pixel digits, tinted the recording glyph's
red, with one "Cancel countdown" menu entry routing to the ordinary Cancelled
ending). The tray digits themselves are UNGATED, owner's call: every session on
every platform with a tray/menu-bar presence shows them during a countdown
(`tray::CountdownTraySession` per platform, decisions in `recording_ui`);
normal sessions keep their on-screen countdown and get the digits in addition,
and only the fallback path loses the window. The fallback path never mints the
window countdown at all, tray or NO tray (DRAGON-563 reopened): the historical
gray-but-visible failure-safe was removed after the fourth sandbox test, where
child tray items failed to register while the resident's succeeded, so the
owner hit the gray sheet on every delayed capture. With no tray host the
fallback countdown is invisible (a warn names it) and still fires and cancels
on schedule.

One recording gotcha this path surfaced lives OUTSIDE the overlay: ffmpeg 7.x
blocks a raw-PCM FIFO input's stream analysis on real audio data before it will
open the NEXT input, which deadlocked against the pump's own FIFO rendezvous
order and killed every sandbox recording at the muxer watchdog. The fix is
two-sided and platform-wide: `-probesize 32 -analyzeduration 0` on the FIFO
inputs (`encode::command::fifo_input_args`) and the POSIX sys-FIFO rendezvous
riding the pump's WRITER thread as a pending sink (`record::pump::SysSink`), so
mic audio flows while ffmpeg is still probing. ffmpeg 8 never needed either,
and the media-clock E2E content tests pin that its behavior is unchanged.

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
  `NSStatusItem` with the capture menu (Scanner / Capture Region / Window /
  Monitor / — / Record Region / Record Window / Record Monitor / the "Audio
  Recording: <state>" radio submenu, DRAGON-558, closing the record block since
  DRAGON-559 / — / Settings… / Manage Permissions / — / Quit), and the
  process-wide PrintScreen
  (+ F13) `global-hotkey`. The record entries (DRAGON-559) spawn the capture
  twins' children plus `--video` (`CaptureAction::spawn_args`), show wherever
  the capture trio shows, and are idle-only (the recording-time Capture Menu
  submenu drops them: one recording at a time). The audio submenu is on EVERY
  tray/menu-bar surface: its title carries the current arm state, one radio pick
  (Both / Microphone only / System only / None) sets the complete pair, and the
  pick routes by ONE portable decision (`recording_ui::audio_toggles_are_live`):
  the live toggle diff to a recording child, the persisted
  `record_mic`/`record_system_audio` while idle. The in-recording menus are the
  three-entry group (Pause/Resume, Finish & Save, Cancel & Delete) since the
  audio toggles moved here. It NEVER touches `app::run`, so the iced/cosmic/wgpu graph is
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

## Controlling a live recording from another process (DRAGON-583)

Five verbs act on a recording that is already running: pause, finish, cancel,
toggle mic, toggle system audio. They arrive from three places and converge
immediately: a resident menu click, a recording-tray click, and the CLI flags
(`--pause-recording`, `--finish-recording`, `--cancel-recording`,
`--toggle-mic`, `--toggle-system-audio`). All three become the SAME `TrayEvent`,
drained by the same `RecordingMsg::TrayPoll`, so there is one code path to
reason about and no verb can work on one surface but not another.

The transport is `platform/daemon_ipc.rs`'s existing `Command` vocabulary,
unchanged. What DRAGON-583 added was an ADDRESS, not a protocol: normally the
resident listens on the one well-known socket and the recording child connects
to it, so with no resident there is no socket at all. The recording child now
also listens on its own per-pid socket beside the recording marker
`instance.rs` already writes, the shape `platform/preview_ipc.rs` established
for reaching a live preview host. Fire-and-forget, bounded, no reply word, no
new tokens. The resident's own socket, connection and menu are untouched, which
is why macOS and Windows saw no behavior change.

The inlet is deliberately INDEPENDENT of the tray. A sandboxed capture child
often fails to register a ksni item at all (the DRAGON-563 finding), and that is
precisely the session where these commands are the only control the user has
left, so `sub_tray_poll` gates on either being alive.

**Why the CLI exists at all on Linux**: `in_app_recording_shortcut_reachable`
(`platform/mod.rs`, pure, unit-tested) says an in-app recording chord can only
fire if the session has focus-free hotkeys OR our surface keeps the keyboard.
Linux has neither: COSMIC ships no `GlobalShortcuts` portal interface (a real
bounded, memoized probe reads its `version`, so a desktop that DOES ship it
keeps the ordinary key bindings), and record start hands focus back to the
recorded window while the fallback path destroys its toplevel outright. macOS
and Windows pass the second term today, though only incidentally: see
DRAGON-585 for making that deliberate.

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
the software fallback via `videotoolbox_plan`). An AUTO encoder preference
stays "auto" (DRAGON-571): the persisted `encoder_auto_hint` caches the
last-known-good winner, `EncodePlan::resolve_hinted` hoists it to the front of
the auto ladder (so the happy path pays one probe), and the winner updates only
the HINT (`state::note_encoder_auto_hint`), never the preference itself.
`src/encode/resolution.rs` and
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
`ffmpeg`. `rstest` is available for table-driven cases. The 5 CLI-level tests
in `tests/cli.rs` drive the compiled binary (via `assert_cmd`) for
`--help`/unknown-flag/`--inspect` behavior and for a recording-control flag
with no recording running; `tests/ocr/` holds a small labeled image corpus used
by the `--ocr-bench` harness, not `cargo test`.

**Never run the two feature configs at once.** `cargo test` and `cargo test
--no-default-features` must go SEQUENTIALLY even in separate
`CARGO_TARGET_DIR`s: separate dirs stop the builds colliding but not the runs,
and `record::media_clock_e2e_tests` plus `record::wedge_live_tests` drive real
`ffmpeg` against the live pulse server, so two suites contend for the audio
server and manufacture failures. A parallel run invented a "2 failed" on
2026-08-08 that passed clean re-run alone. Clippy legs and the Windows
cross-check are pure compilation and may run alongside anything.

## Historical record

`docs/archive/` holds finished tickets' working logs (see its own README) —
useful for "why" archaeology, not current behavior.
