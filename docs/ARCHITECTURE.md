# Architecture

A map of the tree as it exists today, for contributors. Working agreements
(build/test/lint commands, CAUTION areas, conventions) live in the repo-root
[`CLAUDE.md`](../CLAUDE.md) — read that first; this doc is the deeper map it
points to.

## Module tree

- `src/main.rs`: argv parsing (the subcommand grammar: `capture` / `record` /
  `scan` / `color` / `editor` / `timer` / `sound` / `settings` / `permissions` /
  `tray`, each with its own options), the settings single-instance lock
  (`src/instance.rs`; a CAPTURE launch takes no lock at all since DRAGON-351),
  launches the `cosmic::Application`.
- `src/cli/`: `diagnostics.rs` (the diagnostics harness), `inspect.rs`
  (metadata dump) and `sync.rs` (the A/V-sync reference clip + calibration).
  See [`CLI.md`](../CLI.md) for the user-facing command list.
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
    `activation.rs` (the capture activation gate: a capture child never becomes
    the active app until it has an editor, an alert or a panel to show, which is
    what keeps a fullscreen game from minimizing),
    `return_focus.rs` (the editor hands the keyboard back to the app the capture
    started from when it closes),
    `picker_keys.rs` (the picker's keys as Carbon hotkeys while the capture child
    is inactive, since an inactive app's window gets no key events; the full set
    while picking, Close only through a countdown, with held keys repeated here),
    `focus.rs`, `spaces.rs`, `active_window.rs`, `coords.rs` (the AppKit
    to app-coordinate mapper); `services/` — `file_panel.rs`,
    `login_item.rs`, `appearance.rs`, `env.rs`; root files `tcc.rs`
    (prompt-free permission probes + one-shot requests + System-Settings deep
    links; the pure `map_*_status` reducers are unit-tested), `wallpaper.rs`
    (NSWorkspace desktop-picture resolution with an honest HEIC/missing
    degrade), `pinch.rs` (the recognizer, plus a global magnify monitor while
    the capture child is inactive), `screenshot.rs` (mounted at `crate::screenshot`
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
  - `child.rs` — the cloud-upload helper's whole life. It is internal plumbing,
    not a command anyone types, and it is not in `CLI.md`. An upload runs in a
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
  sidecar → dev `vendor/ffmpeg/macos-<arch>/` → `PATH`; the dev directory is
  per-architecture since DRAGON-700, `arm64` or `x86_64`).

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
  Mirrors the `settings` window plumbing exactly: `PermissionsState` field on
  `App`, a `view_window` branch, `sub_permission_poll` (1s live refresh while
  open), a `PermissionsMsg` domain (`update/permissions.rs`). Entry points: the
  `permissions` command; a capture launch missing the Screen Recording grant
  routes here instead of an empty capture (`application.rs`, superseding the old
  bare `request_screen_capture`, keeping the `mac_first_run_seen` once-guard as
  the card's Request-vs-Open-Settings input); and the resident daemon spawns a
  `permissions` child ONCE at startup when the grant is missing. The Screen
  Recording card carries a **Relaunch** button — macOS only applies that grant
  to a fresh launch, so the button spawns `current_exe permissions` detached
  and exits.
- `color_picker/` — the **colour picker tool** (DRAGON-582): a dimmed overlay with a
  magnifier that follows the pointer, and the result window a pick opens. `mod.rs`
  holds the state, the `PixelSource` seam and `open_color_picker_window`; `geom.rs`
  the pure decisions (which source pixel, the magnifier raster, the label-placement
  ladder, the window size, the recents write rule); `view.rs` both views. A
  `color --picker` launch mints the SAME per-output overlays a capture does, through
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

  **The result window's own layout is DRAGON-630's, as revised by DRAGON-680** (the
  owner's polish pass; every part of it is a fixed size, so `geom::color_window_size`
  derives the whole window, 366 x 542): the saturation/value square, a controls row
  (pipette, copy button, round current-colour swatch, hue and alpha strips), one value
  row (the boxes with the mode stepper at their right end, then their caption band), a
  divider carrying the "Add color" button, and two rows of history swatches. What
  DRAGON-680 changed, all of it owner-driven, with the reasons kept as tombstones in
  `geom` and `view`: the value row's split-vs-unified LAYOUT is a property of the mode (hex
  is one unified box, everything else per-component) rather than a persisted toggle, so
  `color_picker_split_inputs` left the schema; the mode dropdown's CHIP became a bare
  up/down chevron unit beside the boxes (its MENU is unchanged and still opens upward, now
  right-aligned through `chrome::FlyoutDir::UpRight`), which deleted the whole mode ROW and
  40pt of window height; changing the mode no longer copies to the clipboard (only the copy
  button and the pick do); the copy button ended up at the HEAD of the value input row at
  the pipette's 48pt size, which is what widened the content to 388 (so CMYK's five boxes
  keep their 55pt) and made the box band 48 tall; and the slider thumbs went 90% opaque.
  The history keeps a right-click menu with one entry, "Remove from recents", and
  Backspace or Delete does the same to the hovered swatch, or to the selected one while the
  history holds the focus ring, and NEVER while a value box has the caret
  (`geom::remove_target`).

  **The window owns its own Tab ring** (`geom::PickerFocus` / `geom::next_focus`): the
  value boxes in order, then the mode activator, then the whole colour history as ONE
  stop, then round again, forwards and backwards. It has to, because libcosmic's
  `keyboard_nav` drives iced's `focus_next` over every `operation.focusable` widget and a
  cosmic BUTTON is one, so the toolkit's cycle walked the pipette, the copy button, Add
  color and eighteen swatches; the picker turns that off for its own process
  (`Core::set_keyboard_nav(false)`) and answers Tab in `keyboard.rs`. Only the boxes take
  real toolkit focus; the other two stops draw the same accent frame from app state, with
  the toolkit's focus parked on an id no widget carries. Arrows then belong to whichever
  stop holds focus: up/down step the notation on the activator, and all four move the
  selected swatch through the history, loading each as a click does. The pure list and
  grid rules live in `src/keynav.rs`, shared with the preview editor's flyouts.

  **The side PANEL** (DRAGON-682): a toggle in the header's end region doubles the window's
  width, and the second half is the settings window's own `tab_bar` strip (Harmonies, Saved
  Palettes, each with its icon) over a scrollable. The Harmonies tab lists five relationship
  cards, recalculated from the current colour on every redraw: monochromatic, analogous,
  complementary (180 degrees), triadic (120/240) and tetradic (90/180/270), all pure in
  `color::Harmony`. The rotations keep the base's saturation and value and lead with the base;
  MONOCHROMATIC is deliberately different, one ordered dark-to-light ramp of the base's hue
  with the base at the slot its own value earns and the step sized to the room on both sides
  (`color::monochrome_ramp`). That ramp holds NINE segments since DRAGON-705
  (`color::MONOCHROME_STEPS`, which `geom::MAX_SEGMENTS` reads), so it is deliberately the
  widest card in the panel: it is read as a gradient rather than as a set, and the owner
  wanted a finer ladder of shades than the tetradic card's four rotations allowed.
  Each card's swatches are ONE full-width segmented bar, no gaps,
  rounded at the bar's two ends only, drawn at the window's current alpha over a single shared
  checkerboard. Clicking a segment APPLIES it as the current colour (DRAGON-687's
  follow-up run reversed the original click-does-nothing rule), and every discrete
  replacement of the active colour first FILES the outgoing colour into the recents if
  the history does not hold it (`geom::files_outgoing`, the one bump inside
  `apply_picker_color`; continuous edits are exempt). Segments carry the shared adaptive
  hex card (`view::swatch_card`: the overlay chip's own dress, ink and border flipped by
  `Srgb::wants_dark_text`, one builder for hover tooltips and keyboard-pinned cards
  alike) and a right-click menu ("Set as active color", the click's exact path, and
  "Copy" in the remembered notation, which deliberately does NOT light the main copy
  button). The main round swatch wears the same card and its own menu (add to
  recents / add to palette / copy, and pointedly no set-active). Expanding RESIZES a window pinned by `min_size == max_size`, so the
  lock is released, the window resized and the lock re-applied at the new size, chained
  rather than batched; the picker's own column is laid out at a FIXED width in both states
  so nothing in it moves. Both the expanded flag and the active tab are persisted
  (`color_picker_expanded`, `color_picker_panel_tab`).

  **The Saved Palettes tab** (DRAGON-687) is the panel's second tab made real: named,
  ordered colour groups (`geom::Palette`, colours in the recents' own hex spelling),
  persisted in their OWN `palettes.toml` beside the config (`state::palettes`: same
  `app_config_dir` rule, same test sandboxing, same last-writer-wins write), so a factory
  reset never touches them; a fresh install starts with the Catppuccin Latte and
  Mocha palettes (MIT-licensed values, provenance at the constant; all four flavours
  until the owner's trim), the default applying only
  while the FILE is absent so deleting them sticks. Each group also carries a pipette
  button whose pick files STRAIGHT into that group across the process gap (the `color`
  verb's `palette=` nonce, protocol v5; the window keeps the `(index, name)` snapshot and
  degrades to an ordinary pick when the group is gone). The pinned create row is the tab's TOOLBAR: a sort icon (the
  six sorts' flyout, moved here from the group menus) and a search icon that expands
  into the app's one search field (`widgets::search_input`, the settings header's own,
  filtering group names live; `geom::visible_palettes`), then "New Palette", which
  clears any filter, PREPENDS a group at the top and opens its name inline in a mostly
  transparent editor (Enter or focus loss commits, Escape reverts). Each group is a
  heading (hover pencil, drag to reorder or off-window to delete, right-click for
  Delete palette) over a full-width palette bar, with the pipette and a plus button
  ("Add current color") in the title row. Every colour add APPENDS at the end and
  duplicates are no-ops; an intra-group drag reorders (analytic insertion line) and a
  cross-group drag COPIES (the menus keep the explicit Move). Under a filter,
  geometry and gestures live in visible-ROW space and every mutation resolves back to
  REAL indices in the handlers; the LIST view is virtualized
  (`geom::visible_row_window`: only the viewport's rows plus a small buffer become
  widgets, spacers carry the rest, an open rename's row is always kept built). The
  drop machine's palette hit tests go through the panel scrollable's mirrored offset
  (`geom::PanelShape`, visible rows), and a live drag near the scroll viewport's edges
  auto-scrolls the list on a bounded ramp (`geom::drag_autoscroll_velocity`), armed
  only once the pointer has been outside the bands so a drag born in one moves
  nothing. Group deletion always confirms, through the
  settings window's own `stack_dialog` modal; palette COLOURS never do. Ctrl+Tab and
  Ctrl+Shift+Tab cycle the panel's tabs (and the settings window's in-page strips), on
  Control on every platform (`shortcuts::tab_cycle_step`).

  Filing a colour into the history moved off plain Enter onto primary+Enter
  (`shortcuts::is_add_color_chord`) and copying gained primary+Shift+C
  (`is_copy_value_chord`, Shift-qualified so the boxes keep their own bare copy binding);
  both chords are named in their buttons' tooltips. The history keeps ALPHA
  (`geom::Recent`), drawing a translucent entry as a split swatch, opaque colour on the
  left and the colour over a checkerboard on the right.

  **Four entry points, one argv.** The tray / menu-bar entry
  (`recording_ui::CaptureAction::ColorPicker`, first in the idle menu, before
  Scanner), the preview editor's toolbar pipette (`PreviewMsg::OpenColorPicker`,
  which adds `CCK_COLOR_TO_PID`), the `color --picker` command, and the
  `CaptureHotkeySlot::ColorPicker` global-shortcut slot (index 6, LAST so the six
  capture slots keep their indices; ships UNBOUND like every slot, and on Linux the
  Global tab shows the command instead of a chord). Every one of them spawns
  `color --picker`; the result window's own pick-again pipette
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
  they cannot drift). `recent_colors` is the one field it WRITES: hex strings, newest
  first, capped at `geom::RECENTS_CAP` (18, two rows of nine since DRAGON-676 traded
  width for it; 20 through DRAGON-649). Since DRAGON-680 an entry carries its ALPHA, so
  it is written `#RRGGBBAA` when translucent and `#RRGGBB` when not, and both shapes are
  read back (`geom::Recent::parse`), which is also the migration: every entry any older
  build wrote loads opaque.
  It is persisted because the app is
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

### Recording is the exception: the toolbar opts out per window (DRAGON-690)

A STILL is over in a moment, so timing can carry it. A RECORDING runs for
minutes with our overlay on screen the whole time, because that overlay draws
the recording chip (stop, mic, system audio). Timing cannot exclude a window
that has to stay up, so recording is the one path with a real filter, and the
seam is `platform::exclude_window_from_capture(title)`.

It is deliberately PER-WINDOW, and that is a contract rather than a detail:
DRAGON-691 draws on the screen during a recording, and that ink must stay IN the
video while the controls stay OUT of it. So a window asks for itself, and no arm
may widen that to "our process's windows". The rule is the pure
`platform::mac_window_is_capture_excluded`, tested on every host. The capture
overlays are still the only caller: DRAGON-691 shipped the drawing surface as a
SEPARATE surface (see the next section) that simply does not call the seam,
which is the shape this paragraph predicted.

The two platforms exclude in different places, and the difference is visible:

* **macOS** excludes at the STREAM. The recording target's `SCContentFilter` is
  built `initWithDisplay:excludingWindows:` over the ids that opted out
  (`platform::mac::recording_display_target` and its named-display sibling), so
  only recordings WE make are affected. `NSWindow.sharingType = .none` was the
  obvious alternative and is ruled out: ScreenCaptureKit honoured it through
  macOS 14 and ignores it from macOS 15, and Apple's answer is that no public
  API prevents capture. A mechanism that lapses on the newest OS is worse than
  none.
* **Windows** excludes at the WINDOW, with
  `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, because Win32 offers
  nothing per-stream. That is OS-wide for the window, so another application's
  recorder does not see our capture overlay either. Below build 19041 there is
  no exclusion at all: the older `WDA_MONITOR` paints a black rectangle, which
  is worse than the toolbar it replaces. The band is the pure
  `platform::win_build_can_exclude_from_capture`.
* **Linux** has no arm, and that is a protocol fact. Neither
  `ext-image-copy-capture` nor the ScreenCast portal can leave a window out of a
  display capture.

Window-mode recording needs none of this anywhere: it records one chosen
toplevel's own pixels, and the toolbar is a different window.

### Drawing on the screen while recording (DRAGON-691)

A three-way tool selector (Pointer, Marker, Magic Marker) plus a Clear Marks
button, offered in the recording toolbar and in the tray's "Live Annotations"
rows (DRAGON-693, top level since DRAGON-757) while any recording runs.
Pointer is the default and the ordinary recording state. On the GOLD tier the
ink is an ordinary composited surface, so the recording captures it exactly as
it captures everything else on the desktop: nothing in `src/record/`,
`record::finalize`, the mixer or the encoders changes, and there is no
re-encode. The other two tiers are the next section.

**The Magic Marker's dissolve is gated on ACTIVITY, in GENERATIONS.** A
generation is a batch of magic ink that holds together and then fades together.
While one is still holding, new drawing joins it and extends its hold, which is
what makes "keep drawing and nothing moves" work; under `MAGIC_HOLD_MS` (2s) the
batch is solid, and from there one sweep runs so all of it is gone at
`MAGIC_GONE_MS` (3s).

The moment a generation's sweep BEGINS it is closed (`ink::gen_accepts`): new
drawing opens a fresh generation with its own hold, and the closed one runs to
completion underneath. That is the DRAGON-691 amendment 2 fix, and the behaviour
it replaces is the one the owner tested and disliked: with a single scene-wide
clock, drawing anything reset it, so starting a stroke while the previous batch
was halfway through dissolving stopped that dissolve and brought the ink back.
Several generations can now be fading at once at their own rates, which is what a
whiteboard actually does.

An activity gate rather than a per-point age, because the two are different
behaviours: a per-point age fades the start of a long explanation while its end
is still being drawn. Draw order still decides the ORDER things leave in, which
is why the per-point timestamps are carried.

The sweep is a soft FRONT travelling through the scene's draw order
(`ink::sweep_front`, `ink::magic_alpha`), from `-SWEEP_EDGE` to `1.0`, so nothing
has begun to lift when the hold ends and nothing is left when the sweep does.
**The scene is APPEND-ONLY, and that is what makes a bake possible at all.**
Nothing is ever removed from `ink::InkLog` while a recording runs: strokes and
generations are appended, and an Erase is appended as the INSTANT it was pressed.
What is on screen is a question about a TIME, answered by `ink::visible_at`
(`ink::any_visible_at` for the surface's lifetime, `ink::fading_at` for the fade
tick), never a question about what the log still holds. DRAGON-693 added a third
appended record beside the strokes and the erases, the FINAL SWEEP
(`InkLog::wipes`), for the same reason: a Silver or Bronze disarm has to REPLAY
as a lift, not as an absence.

The live scene used to DROP clips it could no longer show, and every term of that
justification was true of the overlay, which only ever asks about now, and false
of the window-mode bake, which re-reads the same log after the session and
replays it from media 0. "Invisible now" is not "invisible then". It cost the
owner a whole drawing: the middle of three, dissolving while the third was being
drawn, deleted by the fade tick that finished its sweep, and absent from the
finished video at every instant, including the seconds it had been on screen. A
renderer may SKIP ink it cannot see (an invisible clip costs `visible_at` one
comparison) and may never FORGET it.

**Escape during a live recording selects Pointer and does nothing else**
(`WindowChromeMsg::Close`). It used to stop and save the recording, which put the
one irreversible outcome a keypress away for the whole take, on the key a user
hits to back out of a mode. Stop and cancel stay on the toolbar and in the tray.
On Linux the armed ink surface takes `OnDemand` keyboard for exactly this reason:
without it a user who clicked the ink last would be holding the screen with no
key that gives it back.

**Three routes reach that disarm, and they share ONE method** (DRAGON-742,
`App::disarm_ink_tool`): Escape, a RIGHT CLICK on the recording's own surfaces,
and bare `V`, the letter the preview editor already spends on its own Pointer
tool. The owner asked for the second and third as "the same way escape does",
and one method is what makes that literally true. The reason it matters is what
a disarm does BESIDE moving the selector: the Silver / Bronze final sweep, the
tray's Live Annotations rows, the DRAGON-694 media un-duck, the surface
sync, and DRAGON-739's painted cursor, which `update_ink` drops to `None` after
every ink message and which a route that only moved `ink.tool` would leave
switched on, drawing a pointer into the recorded file for the rest of the
session. The two decisions are pure and unit-tested, one per question:
`shortcuts::is_ink_disarm_key` (which key) and
`app::update::window_chrome::ink_disarm_fires` (whether it applies, shared by
both new routes so the mouse and the keyboard cannot drift apart). The
convergence itself is guarded structurally by `app::ink`'s
`disarm_route_tests`, which refuses any other site spelling
`SelectTool(InkTool::Pointer)` for itself. Escape's OTHER duties, the settings
search collapse, the countdown step-back and the teardown, are not inherited by
either new route.

`crate::ink` is the pure half (the dissolve curve, the per-point timestamps that
drive it, the availability gate, the region clamp, the input-zone rect
subtraction) and `app::ink` is the surface and the scene. `widgets::ink_canvas`
draws finished polygons and reports press / move / release; it is deliberately
not `widgets::annotation_canvas`, which is an editor.

**The ink gets its own surface, and that is the whole reason DRAGON-690's seam is
per-window.** The recording toolbar is iced UI drawn inside the per-output
capture overlay, and that overlay opts out of our recordings. Ink drawn on it
would be excluded with it: on screen, absent from the file. So a second surface
is minted on the recorded output, and its contract is that it never calls
`platform::exclude_window_from_capture`.

The toolbar has to stay visible and clickable ABOVE the ink, or a user can arm a
tool and then not reach the button that disarms it. Each platform says that
differently:

* **Linux** puts the ink on `Layer::Top` while the capture overlays stay on
  `Layer::Overlay`, so the compositor orders them correctly by construction.
  Input is per-rect: the ink surface's region is the output MINUS the toolbar
  (`ink::rect_subtract`). A layer-shell input region is fixed at creation, so
  arming, disarming and a toolbar DRAG each re-mint the ink surface, and only
  it, so the toolbar never flickers for a change to the drawing.
* **macOS** places the ink window one level BELOW the overlays' shielding level
  (`platform::mac::window::place_ink_overlay`).
* **Windows** has one topmost band with no sub-levels, so the ink is placed
  topmost and the capture overlays are then re-asserted topmost above it.

macOS and Windows have no per-rect input region, so both extend the existing
60ms hover poll (`App::passthrough_poll`) by one window: the pointer over the
toolbar makes the ink click-through and the toolbar solid, anywhere else does
the reverse. The consequence is that while a tool is armed the user cannot click
the application being recorded on those two platforms. That is accepted, not
overlooked: it is a mode the user turned on and one press of the same button
turns it off.

The Magic Marker's clock is WALL CLOCK, not painted frames, and that is not
DRAGON-644 in reverse. The dim fade counts frames because a ramp spent during a
stall is an animation nobody sees, and being watched is that fade's whole job.
The ink's deadline is a promise to the FILE: the video runs in real time and the
user is talking over it, so "gone three seconds after I stopped drawing" has to
be true in the recording. A frame-counted clock would stretch that deadline by
however long the app stalled.

The tick has two cadences (`sub_ink_fade`), and the reason is the hold: through
it every frame draws the identical scene while rebuilding every stroke's ribbon,
on a machine that is already encoding video. It idles at `INK_HOLD_TICK_MS` and
goes to one frame's cadence one tick before the sweep. It cannot simply arm
itself late, because a subscription is only re-evaluated when a message arrives.

### Tiered support: gold, silver, bronze (DRAGON-693)

Not every session can host an always-present click-through ink surface, and the
one that cannot is a Linux session with no `zwlr_layer_shell_v1`: a GNOME
session, or a Flatpak build cosmic-comp hides the global from. Those record
through the desktop portal, and `recreate_active_overlays` tears every surface of
ours down (and clears `App::outputs`) the moment recording starts, because the
selection UI would otherwise be baked into the video and would swallow every
click meant for the app being recorded.

DRAGON-691 answered that by hiding the tools there. DRAGON-693 grades the session
instead. The probe is pure and unit-tested (`ink::tier`), fed by three facts that
are known before the first stroke. The third is DRAGON-738's:

| `portal_fallback` | `window_mode` | `window_unlocated` | tier |
| --- | --- | --- | --- |
| false (macOS, Windows, layer-shell Wayland) | false | false | Gold |
| false | true | false | Gold |
| false | true | **true** | **Bronze** |
| true | false (region / monitor) | either | Silver |
| true | true (window cast) | either | Bronze |

`window_unlocated` means "this is a portal WINDOW cast and we could not find out
where the recorded window is". It is the DRAGON-738 row, and it exists because a
window cast delivers the toplevel's OWN buffer, so the ink has to be composited
in by us and the map into that buffer needs the window's origin. On Linux only
`zcosmic_toplevel_info_v1` carries a toplevel's geometry (the portal names a
PipeWire node and never a window; `ext_foreign_toplevel_list_v1` carries identity
alone; core Wayland tells a client nothing about position), so KDE and wlroots
can NEVER answer, and COSMIC may not either when two windows share a size and
DRAGON-737's pixel tie-break cannot separate them. Those sessions have layer
shell, so they used to grade Gold: the ink drew perfectly on screen, the user
watched it, and the file got none of it with nothing to say so.

Three rows deliberately do NOT move. macOS and Windows never report the term
(`platform::window_rect` answers from the window server all session long). A
native Linux window recording never reports it either, because that path crops a
screen RECTANGLE and the compositor captures the ink with everything else. And a
region or monitor cast is untouched whatever the term says, because its ink is on
the screen being cast.

The UX is IDENTICAL across all three: the same selector, the same fade clocks,
the same Clear Marks. Only two things differ, and both hang off the tier rather
than off scattered `if`s (`InkTier::persists_after_disarm` /
`arms_own_window` / `shows_snapshot`):

* **GOLD** keeps today's behaviour byte for byte. The ink is on its own surface
  (a `Layer::Top` layer surface on Linux, an own window on macOS and Windows),
  the recording controls stay clickable above it, and going back to Pointer
  leaves the ink up, display-only, until Clear Marks or a dissolve takes it.
* **SILVER** presents ONE ordinary fullscreen transparent toplevel on the
  recorded output when a tool is armed (`shell::ink_armed_window`, the twin of
  the fallback capture overlay's `overlay_fallback_window`). It is focused and
  opaque to input, so the tier is "annotate OR control": the window hosts both
  the canvas and the same tool selector the toolbar shows. The portal's monitor
  or region cast carries the ink live, so NO bake is involved. It is a deliberate
  exception to the record-start teardown above, and it goes again on disarm.
* **BRONZE** is a WINDOW cast we cannot place on screen: the no-layer-shell
  session recording a window, and, since DRAGON-738, any session at all whose
  portal window cast could not be matched to a toplevel. Its cast streams that
  window's own pixels and tells us nothing about where the window is. Arming
  freezes the latest captured frame (`record::FramePeek`, a one-shot request
  served by the CPU PipeWire worker) and shows it fitted in the armed window;
  strokes are born in STREAM pixels (`ink::fit_rect` / `ink::to_stream`), so no
  window position is ever needed. The ink reaches the file through the EXISTING
  finalize bake with a single rect sample covering the frame, which is the
  identity when the encode kept the stream's size and the right rescale when the
  resolution cap shrank it.

**The disarm SWEEP is why Silver and Bronze do not just hide the window.** The
recording is running, so a hide is one frame with ink and the next without, which
reads as a glitch rather than as the user putting the pen down. A disarm appends
a FINAL SWEEP to the log instead (`InkLog::wipe`, `FINAL_SWEEP_MS` = 400ms) and
everything on screen lifts in draw order, over the same curve the Magic Marker
uses with a zero hold and a much shorter travel (`ink::sweep_front` /
`ink::sweep_alpha`, ONE curve called with two sets of durations). It is RECORDED
rather than applied, like every other clearing action, so the Bronze bake replays
the lift. `ink_bake_e2e_tests::a_final_sweep_lifts_the_baked_ink_over_its_own_span_e2e`
pins it against decoded pixels, in three states: solid, part gone, absent.

Gold records no sweep at all. Its ink stays, which is the whole of what the medal
buys, and encoding that difference in `InkTier` is what keeps the Gold paths
untouched.

**Bronze's honest caveats**, both stated in the README as footnotes: the ink is
placed against the FROZEN moment while the window keeps recording live
underneath, and a zero-copy recording (GPU DMA-BUFs the CPU never maps) serves no
snapshot at all, so the armed window falls back to a plain scrim. A third rides
on the DRAGON-738 rows: Bronze composites through the RETAINED `record::ink_bake`
finalize pass rather than DRAGON-724's live painter (its strokes are already in
stream coordinates, which the painter's window-rect mapping would place wrongly),
so an inked take there re-encodes its video once at stop. None of the three is
hidden: `ink::armed_note` prints the mode and its cause under the armed window's
selector, and `begin_ink_session` logs the same split. The ink still
works and still lands in the file in either case, because the strokes are in
stream coordinates and the stream's size comes from `RecordHandle::dims` when no
snapshot arrived. A GPU readback for the peek is the obvious follow-up.

**The tray is the arming surface on Silver and Bronze**, because those sessions
have no toolbar on screen at all. That is also what let `ink::tools_available`
drop its `toolbar_hidden` term: a hidden toolbar is no longer a feature with no
door. The block is portable data like every other tray row
(`recording_ui::ink_items`, `RadioSubmenu::Ink`), rendered by the two Linux ksni
trays, the two macOS `NSMenu`s and the Windows `HMENU`; a resident's pick is
relayed to the recording child as a new `daemon_ipc::Command` word, and the
mark rides back on `RecordingState`'s fifth wire field.

**Every tray renders the block FLAT** (DRAGON-757): the same four rows, in the
same place, at the TOP level of the recording menu, each carrying the glyph the
recording toolbar's button for that tool already draws. The owner asked for it
because the tools are what a user reaches for repeatedly mid-take and a submenu
put a hover and a second click in front of every one of them. The two Linux
trays went first and the two macOS menus and the Windows popup followed in the
same ticket. It stays ONE model entry rather than four, because the RENDERING
changed and not the control: the block still appears and disappears together and
is still recording-only. What the flat walkers need on top of the shared rows is
additive and lives in `recording_ui` so the five surfaces cannot drift:
`InkItem::icon` and `ink_row_label`. `INK_SUBMENU_LABEL` went with the last
submenu that rendered it.

**Where the armed mark lives, which is a per-renderer answer.** Three of the five
surfaces put it in the LABEL (`recording_ui::ink_row_label`, a trailing check
character) and the two macOS ones mark the item itself. The split is about
whether a mark and an icon compete for one slot.

* **Linux, the X11 half.** A ksni `CheckmarkItem` is the idiomatic way to show
  which of a set is armed. But dbusmenu carries `toggle-type`, `toggle-state` and
  `icon-data` as three independent properties and says nothing about one row
  rendering an indicator AND an icon; whether it does is the HOST's choice, and
  this project already recorded that the two fight for the same slot on several
  hosts (`recording_ui::MenuIcon`), which is why no radio row here has ever
  carried an icon. Those hosts are the GTK / libdbusmenu ones on X11, not the
  COSMIC applet, and the icon is exactly what was asked for. So the rows are
  plain `StandardItem`s and the icon slot is unambiguously theirs.
* **Windows, the same shape of problem.** Win32 draws a selected item's mark in
  the item's CHECK-MARK AREA, which is the same leading column an `hbmpItem`
  bitmap occupies: "About Menus" describes the selected-state bitmap as the one
  displayed next to a selected item and requires an application-supplied one to
  be exactly the size of the default check mark, and `SM_CXMENUCHECK` is the
  metric `attach_menu_icon` already rasterizes our bitmaps at. What is drawn for
  a row that is both checked and carries an `hbmpItem` is documented nowhere, so
  `MF_CHECKED` would put the icon at the renderer's discretion. The bitmap is
  unconditional and the mark is text.
* **macOS is the exception**: AppKit gives an `NSMenuItem`'s state its own column
  BESIDE the image, so `setState:` gives a native check mark and keeps the glyph.
  The two mac walkers read `InkItem::selected` directly and never call
  `ink_row_label`.

The armed tool then still reads on a host that renders no menu icons at all,
which an icon-based or tint-based mark could not promise. A pick also cannot arm
the wrong tool any more, and not by a range check: `recording_ui::ink_choice_at`
is gone, because each flat row holds its own `InkAction` (its own ksni closure,
its own `NSMenuItem` selector, its own Win32 command id) and there is no index
space left to shift. The Windows `CMD_INK_FIRST..=CMD_INK_LAST` ids stayed
contiguous, because `command_action` still resolves a chosen id back to a tool by
offset, but they are an id space now and no longer a `CheckMenuRadioItem` span.

### Window mode: the ink is data, and it is composited in (DRAGON-691 amendment 2, DRAGON-724)

Window recording used to be the one mode with no ink. It now has it everywhere,
by two different routes, and which route a SESSION takes is
`platform::window_capture_excludes_overlays`, fed the one runtime fact only the
app knows:

* **A rectangle of the SCREEN.** That is the Linux NATIVE screencopy path, where
  window mode funnels through `spawn_screencopy_worker`'s region crop, so the
  overlay is captured live exactly as it is for a region. Nothing extra runs.
* **The target window's OWN buffer**, which no floating surface is part of. That
  is macOS SCK, Windows WGC, and a Linux PORTAL window cast (an xdg ScreenCast
  WINDOW stream). It is why window recording never carried our toolbar, and
  equally why it never carried our ink. There the strokes are ALSO kept as data
  and composited in by the app.

**DRAGON-736 is where that stopped being a per-platform answer**, and the reason
matters for anything added here later. The predicate was `cfg!(any(macos,
windows))`, which is a question about the operating system; the real question is
what THIS session's capture saw. macOS and Windows have one window-capture
implementation each, so there the two questions coincide. Linux has two backends
and they answer opposite ways in the same build on the same desktop, so a portal
window recording drew the marker on screen and delivered a video with none of it.
The decision is pure now (`platform::window_capture_excludes_overlays_with` and
`ink::needs_composite`, both unit-tested across the whole matrix) and the session
fact is read where the backend is known (`App::portal_window_cast`).

The chain, end to end: a stroke is stamped in MEDIA time as it is drawn
(`App::ink_media_ms`, which is `recording_started` plus the same pause accounting
`MediaClock` applies, so a pause contributes none of it); the recorded window's
on-screen rect is sampled into a timeline on the existing 100ms recording poll,
keeping a sample only when it changed; and the pair is handed to the recording
worker, either live or at stop.

#### Live on the captured frame, the normal path (DRAGON-724)

`src/record/ink_live.rs`. `App::publish_live_ink` republishes the whole scene into
`RecordHandle::live_ink` after every ink message and on the 100ms recording poll;
the capture worker's `LiveInkPainter` paints it onto each frame on its way to
ffmpeg's stdin, just before `owned::make_frame_writer`. The finalize pass then
stream-copies its video exactly as an un-inked recording does.

Three things keep that off the throughput-critical capture path, and none of them
may go without re-measuring:

* **A SIZE GATE**, `ink_live::live_ink_affordable`, pure and unit-tested in the
  shared tree so the Linux gate proves it. The budget is pixels per SECOND, so
  the frame rate counts: 4K UHD at 30fps is admitted, 4K at 60 and a 6400x3600
  native Studio Display frame are refused. Above it, the bake below still runs.
* **The raster is cached per PUBLISHED SCENE, not per frame**, so ink sitting
  still costs one `Arc::ptr_eq` per tick and no rasterization.
* **The layer is only as big as the ink** (`ink_live::layer_bounds`), so the blend
  is proportional to the drawing rather than to the frame. A tick with nothing on
  screen returns the captured bytes untouched, which is what keeps a recording
  with no ink byte-identical.

The painter NEVER mutates the frame it was handed: the capture loop re-feeds the
freshest frame whenever a tick has nothing newer of its own, so painting in place
would stack one tick of ink per repeat.

**Which workers build a painter** (DRAGON-736): the macOS SCK worker, the Windows
WGC worker, and the Linux CPU PORTAL worker. The Linux NATIVE screencopy worker
takes neither ink slot, because compositing there would draw every stroke twice.
The Linux ZERO-COPY pipewire worker refuses the painter on purpose and takes the
bake: its frames are GPU dmabufs the CPU never maps, so a per-tick composite
would mean a readback, which is the cost that path exists to avoid. And the CPU
portal worker drops its fallback bake on `painted()` where the other two use
`active()`, because it is also the worker a BRONZE session runs on and the app
publishes no live scene for that tier, so "the gate admitted this session" and
"the ink is in the frames" are different statements there.

**It also sidestepped a clock defect the bake could not, and that property is
worth keeping now the defect is gone.** When DRAGON-724 was written,
`recording_started` latched on `settled_at`, the end of the worker's opening
phase, while the file's media 0 is `owned::media_zero`, taken before the pump
spawns. Everything `ink_media_ms` stamped was therefore early by the opening's
length (489.8ms measured, against a 500ms measured lead in the delivered file,
median of 21 samples), and DRAGON-717 had made that opening much longer than it
used to be. DRAGON-725 fixed the anchor in the same batch, so
`recording_started` is now `RecordHandle::media_zero_at` and the bake below
counts the same file its indices do. The live path is unaffected either way: it
only ever compares two values from the app's own clock, so any offset in that
clock cancels exactly, and `ink_live::offset_invariance_tests` pins it. Keep it
that way rather than resolving the ink instant from the worker's tick; the
anchor has been wrong twice.

#### The POINTER, drawn on the same path (DRAGON-739)

An armed annotation tool takes the cursor OUT of a window recording, and the
mechanism is a compositor rule we cannot reach: cosmic-comp composites the
embedded cursor into a toplevel capture only while pointer FOCUS is on that
toplevel, and the ink layer surface's input region is the output minus the
toolbar for exactly as long as a drawing tool is armed. niri has the identical
rule and the identical symptom, and KDE has been focus-based deliberately since
2024, so drawing the pointer yourself is the settled answer rather than a
workaround.

`App::publish_live_cursor` publishes the pointer's surface-LOCAL position into a
SECOND slot on `LiveInk`, and the painter blends a small pointer sprite over the
ink layer. Four things make that safe:

* **It is not part of the published scene.** The raster cache is keyed on the
  scene `Arc`, and the pointer moves on every mouse sample, so a pointer carried
  inside `LiveInkFrame` would re-rasterize the whole drawing on every frame
  written. Its own slot costs a lock and two floats.
* **Both blends land on ONE scratch copy**, so a re-fed frame draws the same
  picture rather than a TRAIL of cursors down the screen.
* **`ink_live::live_cursor_needed`** is the pure five-term gate: the session
  records one window whose capture is that window's own buffer, our surface draws
  on the LIVE screen over it (BRONZE fails this and must, since it draws on a
  frozen still inside a window of ours and the recorded window never had a
  pointer to lose; `ink::needs_composite` is true there by its other disjunct,
  which is exactly why this is a separate term), the user's "Preserve mouse
  cursor" extra is on (read through `app::cursor_wanted` over
  `effective_capture_extras`, the same pair the portal's own `CursorRequest` came
  from), a drawing tool is armed, and `platform::window_cursor_source()` says the
  platform composites the pointer by FOCUS. Windows Graphics Capture composites
  it by GEOMETRY and already over-includes it, so a second one there would be a
  doubled cursor; macOS is assumed geometric on the safe side, because no mac was
  reachable to settle it and `Geometry` is the arm that changes nothing.
* **`painted()` counts INK ticks only** and `pointed()` is a separate counter.
  The Linux CPU portal worker drops its fallback bake on `painted()`, so a
  cursor-only tick joining that count would silently cost a Bronze user their
  whole drawing. Do not recombine them.

The byte-identity contract gained a second row and both are pinned in
`ink_live::painter_tests`. No tool armed and no strokes still hands the CAPTURED
buffer back by pointer identity, with no copy and no allocation. A tool ARMED with
no strokes yet DOES paint, and that is the feature: the compositor drops the
cursor at arm time, well before the first stroke, so waiting for one would blank
the pointer for exactly the span the user is deciding where to draw. The window
rect timeline and the live scene therefore start at arm time too.

What is drawn is our own vector ARROW, not a captured bitmap. It was a CROSSHAIR
until DRAGON-744, justified by "`widgets::ink_canvas` asks for
`mouse::Interaction::Crosshair` while armed, so we know the shape by
construction". The owner tested it and reported "i dont see a crosshair when im
drawing mid recording", so the file was showing a shape the screen never did, and
they set the bar at a guarantee. There is none to be had, for a structural reason
before any timing one: the recording toolbar is iced UI inside the per-output
CAPTURE OVERLAY, a separate layer surface with its own iced `Window`, and
`iced_winit`'s `Window::update_mouse` issues a cursor only when THAT window's
stored interaction changes. Cross to the toolbar and the overlay sets the arrow;
cross back and the ink window's stored value is unchanged, so it issues nothing
and the arrow stands for the rest of the session. The DRAGON-331 re-assert dance
does not rescue it either: `app::color_picker` already runs it and already carries
the resulting defect, logged and declined, where a live grab in its 24ms dip
photographs the arrow the picker exists to hide, and a recording photographs
everything, roughly one frame of dip plus six of settle per crossing at 30fps. And
`wl_pointer.set_cursor` has no reply, no error and no event, so nothing here can
ever learn what the user is looking at: a guarantee that is unobservable by
construction is not one.

So the canvas asks for the DEFAULT (`ink_canvas::requested_interaction`, pure,
with the tombstone) and the painter draws the arrow that answers to. That arrow is
the right guess mechanically rather than by taste: the ink window issues no
request at all, since `Interaction::default()` IS its initial stored value; the
last of our windows to have issued one is the capture overlay, whose resolved
interaction with nothing hovered is `Interaction::None`, i.e. `CursorIcon::Default`;
and libcosmic's `pointer_frame` re-asserts that seat icon on every enter. Honoured
or dropped, both roads end at the arrow.
`ink_live::recorded_shape_agreement_tests` pins the two modules together.

Dead ends recorded rather than re-run. **Hide the system cursor and draw our own
crosshair in the canvas** is the obvious next idea and does not work: the fork's
`hidden` is a field on `SctkSeat` and therefore SEAT-global, so it would blank the
pointer over the recording toolbar too, and nothing would put it back
(`update_mouse` calls `set_cursor_visible(true)` only when leaving
`Interaction::Hidden`, which that window never held). `App::frozen_cursor` holds a
real sprite but it is the LAUNCH-time pointer, an arrow most of the time and an
I-beam or a resize handle the rest, so it trades a known approximation for an
unknown one. `screenshot::capture_cursor()` would open a whole Wayland capture
session on the capture thread, race the arm-time focus change and need rescaling
into video pixels. Loading the XCursor theme means a theme lookup, a size
negotiation and an animated-cursor format in the record worker.

#### Baked at finalize, the fallback above the gate

**The bake joins the finalize pass rather than adding one**, and that is the
design's whole point. The ink is a second ffmpeg input, `rawvideo` RGBA on the
child's stdin, `overlay`ed onto the video in the same `-filter_complex` the audio
already uses. So:

* the AUDIO chain, its two mute gates and its `amix` are byte-identical whether
  or not there is ink, and the audio is still encoded exactly once;
* the only change is that the VIDEO is encoded instead of stream-copied, and only
  when there is ink.

The obvious alternative, baking only the inked spans and stream-copying the rest,
would have to CUT AND CONCAT, and a concat splices the AUDIO too, where the
mixer's continuity guarantee is load-bearing (a mid-file PTS gap in one audio
stream desyncs under `amix`, see the recording invariants). Folding the overlay
into the existing pass buys most of the saving with none of that risk. Measured
on an M-series Mac, a 30-second 1080p30 recording: 0.79s to finalize with no ink,
3.48s with the full overlay, so about 0.09s of extra finalize per second of inked
recording. A recording with no ink pays nothing, which
`finalize_ink_args_tests::a_recording_with_no_ink_still_stream_copies_its_video`
pins at the argument level and
`ink_bake_e2e_tests::a_recording_with_no_ink_keeps_its_stream_copy_e2e` pins
against a decoded frame count.

The bake feeds frames from media 0 to `ink::last_visible_ms`, then closes the
pipe and lets `overlay`'s `eof_action=pass` carry the rest of the recording
through untouched, so the work is bounded by the INKED span rather than by the
recording's length. It starts at 0 rather than offsetting the overlay input,
which trades some transparent frames for not having to trust `overlay`'s
framesync to hold the main input before its second stream begins.

**Occlusion and out-of-window ink.** A window recording shows the window
unoccluded, so there is nothing to reason about there. Ink drawn OUTSIDE the
window is clipped away rather than squeezed in (`ink_bake::touches_video` plus
the rasterizer's own clip): the recording is of the window, so the parts of a
stroke that were not over it were never over anything the file contains. A stroke
that crosses the edge keeps the part that was inside, which is the same rule the
live region clamp follows. A window that MOVES or RESIZES mid-recording is why
the plan carries a rect TIMELINE: the mapping is re-derived per frame, so ink
stays where the user put it on the window. A window that hops displays needs
nothing special, because the mapping is a ratio of two rects in one global
capture space.

**The Linux PORTAL route's timeline is one sample, and that is a ceiling rather
than an oversight** (DRAGON-736). The xdg ScreenCast interface names a PipeWire
node and never a window, so there is no id to key `platform::window_rect` with;
the rect comes from DRAGON-735's grant-time size match instead, and re-resolving
it would mean `compositor::list_toplevels`, a blocking Wayland settle loop
measured at 167ms on the dev box, which cannot ride a 100ms poll during a live
recording. When several toplevels share the granted size, that match ties, and
DRAGON-737 settles it by looking at the pictures: one bounded frame from the
granted PipeWire node (a SECOND `OpenPipeWireRemote`, never a `dup` of the
recording's fd, which is one stateful socket) against one `screenshot::windows`
grab per tied candidate, compared as 64-bit difference hashes with a maximum
distance and a margin over the runner-up. It runs once, at grant time, only on a
tie, and every failure refuses back to the size-and-activation ladder. Two routes
are closed and tombstoned in `app/portal.rs`: compositor geometry off COSMIC
(only `zcosmic_toplevel_info_v1` carries it) and the pointer out of PipeWire
cursor metadata (COSMIC, GNOME and KDE all gate a window stream's cursor metadata
on pointer FOCUS, so it is blank exactly while the user draws, and the portal
spec restricts a stream's `position` to monitor streams). So ink drawn after the user MOVED the recorded window is placed by
where the window started. It costs the video nothing, since a window cast
delivers the toplevel's own buffer wherever it sits; only the screen-to-video
mapping goes stale.

**The whole thing is verified against decoded pixels**, not against file shape
(`src/record/ink_bake_e2e_tests.rs`, CLAUDE.md's DRAGON-417 rule): the ink is in
the video at the position the mapping puts it, absent before it was drawn, gone
after a dissolve or an erase, following a window that moved, and the audio comes
out the same length and the same RMS either way.

`stop_recording` publishes this plan through `RecordHandle::ink` BEFORE raising
the stop flag, on EVERY inked window session, including the ones the live painter
took. The worker drains the slot after its stop tail and simply drops it when it
painted live: the gate needs the encode size and the frame rate, which only the
worker knows, so having the app publish either way is what lets the fallback need
no coordination at all.

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
   LAUNCH MODE: a Window or Monitor launch (the daemon's `capture --window` /
   `capture --monitor`, and their `record` twins) goes straight to the portal
   picker of the matching source and delivers through the normal held-stream
   path, no region overlay at all; only a Region launch takes the
   frozen-overlay seed below. A dismissed launch
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
  twins' children under `record` instead of `capture`
  (`CaptureAction::spawn_args`), show wherever
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
- **Early branch** (`main.rs`, `cfg(macos)`) — BEFORE any GUI init: the `tray`
  command runs `daemon::run()` and never returns, whatever the setting says, and
  so does a BARE GUI launch (a login item, Finder, the Dock) while the
  `resident` setting is on. Every other launch falls through to `app::run`
  exactly as on Linux.
  DRAGON-695 changed what the bare cases MEAN, not the branch. A bare GUI launch
  with the setting off now opens the settings window, and a bare launch from a
  terminal prints the help and exits. Neither captures. "Bare means capture" is
  dead, and that is what makes the app safe as a login item: the old rule turned
  every login into a screenshot of the desktop.
  The SETTING keeps its name, `resident` in the config; only the argv word
  became `tray`. A Linux autostart entry written with the old word is migrated
  on first run.
- **Menu/hotkey/signal actions** — each spawns the full app as a DETACHED
  (`setsid`) one-shot child with the matching command (Scanner→`scan`,
  Region→`capture --region`, …, Settings→`settings`; hotkey and SIGUSR1 both
  give `capture --region`, the default). Detached so a child crash never touches the
  daemon and there's no SIGCHLD to reap. Each child captures and EXITS at
  finish, same as Linux.
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

The verbs that act on a recording already running are pause, finish, cancel, the
audio channels and the live annotation tool. They arrive from three places and
converge immediately: a resident menu click, a recording-tray click, and the
CONTROL form of the `record` command (`record --action pause|save|cancel`,
`record --sound <channels>`, `record --tool <tool>`, which is `record` with no
target option, DRAGON-695). All three become the SAME `TrayEvent`,
drained by the same `RecordingMsg::TrayPoll`, so there is one code path to
reason about and no verb can work on one surface but not another.

The control options combine in one invocation and are applied in a FIXED order,
`--sound`, then `--tool`, then `--action`, so `record --sound none --action save`
can only mean "mute the rest, then save". Anything else would make the outcome
depend on argv order, which a keybinding cannot see.

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
`--help`, unknown-input and metadata-dump behavior and for a `record` control
with no recording running; `tests/ocr/` holds a small labeled image corpus used
by the OCR benchmark harness, not `cargo test`.

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
