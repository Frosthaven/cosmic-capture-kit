# Cosmic Capture Kit

> [!NOTE]
> Cosmic Capture Kit is currently in the alpha stages. You are free to test this
> software as-is, and scroll below to find planned features and support.

![Cosmic Capture Kit capturing its own settings window](res/readme/hero.png)

Cosmic Capture Kit is a cross-platform program for capturing image, voice, and
video across screen regions, windows, and monitors.

## Supported platforms

| Platform | Capture backend | Status |
|---|---|---|
| macOS 13+ (Apple Silicon) | ScreenCaptureKit Compositor w/extras | ✅ |
| Windows 11 | Windows Capture w/extras | ✅ |
| Linux (Wayland): COSMIC | Cosmic Compositor w/extras | ✅ |
| Linux (Wayland): Sway 1.10+ / Hyprland / River (wlroots) | Planned | Planned |
| Linux (Wayland): KDE Plasma | PipeWire portal | Planned |
| Linux (Wayland): GNOME | PipeWire portal | Planned |

Capture backend extras, where available, include features such as:

- Freezing pixels on region capture
- Toggling mouse cursor availability
- Toggling window transparency
- Toggling wallpaper visibility
- Single window aesthetics (focus appearance, colored border, drop shadow,
  padding, etc)

## Planned Features / Milestones

- [x] Screen capture engine
  - [x] Encoder setup & configuration
- [x] Video capture engine
  - in-recording tools
    - [ ] mouse clicks
    - [ ] keypress overlay
    - [ ] live annotation (with delete)
- [x] Audio capture engine
  - [x] Audio cleanup and processing options
- [x] Preview editor (overlay & window variants)
  - [x] delete, save, save as, copy
  - [ ] copy + delete
  - images
    - annotation tooling:
      - [x] covermarks support
      - [ ] color picker swatch
      - [ ] arrows
      - [ ] text (size/resize)
      - [ ] sequence markers 
      - [ ] dim except areas 
      - [ ] pixelate (destructive) 
      - [ ] blur (destructive) 
      - [ ] box (fill/outline) 
      - [ ] draw (line widths) 
      - [ ] stickers 
      - [ ] eraser 
  - videos
    - editor tooling:
      - [x] simple cutting tool
      - [ ] simple transition dropdown (none/crossfade)
  - cloud uploader targets
      - [ ] Proton Drive
      - [ ] OneDrive
      - [ ] Google Drive
      - [ ] Dropbox
      - etc
    

## Installation

### macOS

1. Download the latest `.dmg` from
   [Releases](https://github.com/Frosthaven/cosmic-capture-kit/releases) and
   drag the app to Applications.
2. First launch: grant Screen Recording (System Settings > Privacy &
   Security), then relaunch. Microphone is optional (for recordings with mic).
3. Updating: the app checks automatically and installs new versions in one
   click from Settings > About.

### Windows 11

1. Download the latest `.msi` from
   [Releases](https://github.com/Frosthaven/cosmic-capture-kit/releases) and run
   it. It installs per-user (no admin prompt) to
   `%LOCALAPPDATA%\Programs\cosmic-capture-kit`, bundles ffmpeg, and adds Start
   Menu shortcuts (Cosmic Capture Kit, and Cosmic Capture Kit Settings), so
   there is nothing else to install.
2. The installer is not code-signed yet, so on first run SmartScreen may show
   "Windows protected your PC". Click More info, then Run anyway.
3. Microphone is optional (for recordings with mic). If it is not picked up,
   enable it under Settings > Privacy & security > Microphone.
4. Updating: the app checks automatically and installs new versions silently in
   the background (Settings > About).

### Linux (Wayland): COSMIC

Build from source for now (packaged channels are on the way):

```sh
git clone https://github.com/Frosthaven/cosmic-capture-kit
cd cosmic-capture-kit
cargo build --release
```

The default GPU zero-copy feature needs ffmpeg 8 headers (Arch, CachyOS,
recent Fedora); on older-ffmpeg distros build with
`--no-default-features` (recording still works through the `ffmpeg` binary).
Runtime dependencies: `ffmpeg` (screen recording), `tesseract` (OCR,
optional).

Install the desktop entry + icon, then point a keybind at the binary (COSMIC:
Settings > Input Devices > Keyboard > Shortcuts > Custom). Flags like
`--window --video`, `--scan`, and `--settings` make one-press flows; see
[CLI.md](CLI.md).

```sh
install -Dm644 res/dev.frosthaven.CosmicCaptureKit.desktop \
  ~/.local/share/applications/dev.frosthaven.CosmicCaptureKit.desktop
install -Dm644 res/icons/dev.frosthaven.CosmicCaptureKit.svg \
  ~/.local/share/icons/hicolor/scalable/apps/dev.frosthaven.CosmicCaptureKit.svg
```

Updating: `git pull` and rebuild; the in-app update check links to the
releases page on Linux.

AUR: coming soon. AppImage: coming soon.

---

## Tiling window managers

Under a tiling window manager the **capture overlays float automatically**. The app
tags them so AeroSpace (macOS), komorebi (Windows), and COSMIC's tiler (Linux) leave
them alone, since a capture overlay has to cover a whole display rather than a tile. No
configuration is needed for that.

The **Settings** and **preview editor** windows are ordinary windows, so a tiling WM
tiles them by default. If you would rather they float, add a floating rule for your WM.
The two windows have separate titles, so you can float just one of them or both:

* Settings window: title `Cosmic Capture Kit - Settings`
* Preview editor window: title `Cosmic Capture Kit - Preview Editor`
* Both share application id `dev.frosthaven.CosmicCaptureKit`

<details>
<summary><b>AeroSpace (macOS)</b></summary>

Add to `~/.config/aerospace/aerospace.toml`, then run `aerospace reload-config`. Match on
`app-id` to float BOTH windows, or on `window-title-regex-substring` to float just one. The two
titles are distinct (neither is a substring of the other), so a single-window rule needs no
anchoring:

```toml
# Both windows float:
[[on-window-detected]]
if.app-id = 'dev.frosthaven.CosmicCaptureKit'
run = ['layout floating']

# ...or float only ONE. Use instead of the app-id rule above, not alongside it:

# Settings only:
[[on-window-detected]]
if.window-title-regex-substring = 'Cosmic Capture Kit - Settings'
run = ['layout floating']

# Preview only:
[[on-window-detected]]
if.window-title-regex-substring = 'Cosmic Capture Kit - Preview Editor'
run = ['layout floating']
```

</details>

<details>
<summary><b>komorebi (Windows)</b></summary>

Edit your `komorebi.json` static config (by default `%USERPROFILE%\komorebi.json`) and add
the windows to `floating_applications`. komorebi reloads the file when you save it, or run
`komorebic reload-configuration`. The `Equals` strategy matches the exact title, so the two
entries never collide. Include both to float both windows, or just one to float only that
window:

```json
{
  "floating_applications": [
    { "kind": "Title", "id": "Cosmic Capture Kit - Settings", "matching_strategy": "Equals" },
    { "kind": "Title", "id": "Cosmic Capture Kit - Preview Editor", "matching_strategy": "Equals" }
  ]
}
```

`floating_applications` is the current key; the older `float_rules` config and the
`komorebic float-rule` CLI are deprecated.

</details>

<details>
<summary><b>COSMIC desktop (Linux)</b></summary>

COSMIC tiles both windows by default. There are two ways to make them float:

* **Preview editor, in-app toggle:** enable the windowed editor, then in the app under
  Settings > General turn on *"Float the preview window (don't tile)"*. The app writes the
  COSMIC exception for you. This covers the preview window only.

* **Manual, for the Settings window or fine control:** COSMIC has no per-application
  float-rule GUI yet, so edit its tiling-exception file directly (changes apply live, no
  logout needed):

  ```
  ~/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom
  ```

  Both `appid` and `title` are matched as regular expressions (unanchored, so they match as
  a substring), and both must match. The shared `Cosmic Capture Kit` prefix is a substring of
  both window titles, so it floats BOTH; the two full titles are distinct, so a single-window
  rule needs no anchoring:

  ```
  [
      // Both windows float (this title substring matches both):
      (enabled: true, appid: "dev.frosthaven.CosmicCaptureKit", title: "Cosmic Capture Kit"),
  ]
  ```

  To float only ONE, use one of these as the `title` instead:

  * Settings only: `"Cosmic Capture Kit - Settings"`
  * Preview only: `"Cosmic Capture Kit - Preview Editor"`

You can also float the focused window ad-hoc with `Super + G` (no config).

</details>

---

## License

The source code in this repository is licensed under [GPL-3.0-only](LICENSE).
The Linux app is free software: use it, build it, share it (it's free forever).
If it's useful to you, donating via [PayPal](https://paypal.me/Frosthaven) will
support future work but is not required.

Official macOS and Windows releases are separately licensed binary
builds by the copyright holder. (The author holds the copyright to all code
in this repository and additionally licenses their own code to themselves
for those proprietary builds; the GPL grant above applies to everyone else
and to this repository's contents.).

## Contributions & Credits

- Icon by [Ashley Ball](https://ashleythedesigner.com/);
- Embedded icon licensing lives in [res/icons/ATTRIBUTION.md](res/icons/ATTRIBUTION.md).
