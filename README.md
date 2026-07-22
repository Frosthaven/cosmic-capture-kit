# Cosmic Capture Kit

> [!NOTE]
> Cosmic Capture Kit is currently in the alpha stages. You are free to test this
> software as-is, and scroll below to find planned features and support.

![Cosmic Capture Kit capturing its own settings window](res/readme/hero.png)

Cross-platform screen region, window, and monitor capture with support for glass
windows, image, video, voice, QR, barcodes, OCR text and more. 

## Supported Core Platforms

These are the platforms currently planned for support, along with their current
status.

| Platform                                                 | Capture backend                     | Status  |
| -------------------------------------------------------- | ----------------------------------- | ------- |
| macOS 13+ (Apple Silicon)                                | ScreenCaptureKit                    | ✅      |
| Windows 11                                               | Windows Capture                     | ✅      |
| Linux (Wayland): COSMIC                                  | Cosmic Compositor / PipeWire Portal | ✅      |
| Linux (Wayland): Sway 1.10+ / Hyprland / River (wlroots) | TBD                                 | Planned |
| Linux (Wayland): KDE Plasma                              | TBD                                 | Planned |
| Linux (Wayland): GNOME                                   | TBD                                 | Planned |

## Supported Compositor Extras

These features require platform-specific functionality. One example is on macOS,
where capturing windows with their glass effects is not possible using the
available APIs - clever recompositing tricks are required.

| Platform                                                 | Freeze Pixels for Region Select | Toggle Mouse Cursor | Toggle Window Transparency | Toggle Wallpaper | Single Window Aesthetics (neon border, etc) |
| -------------------------------------------------------- | ------------------------------- | ------------------- | -------------------------- | ---------------- | ------------------------------------------- |
| macOS 13+ (Apple Silicon)                                | ✅                              | ✅                  | ✅                         | ✅               | ✅                                          |
| Windows 11                                               | ✅                              | ✅                  | ✅                         | ✅               | ✅                                          |
| Linux (Wayland): COSMIC                                  | ✅                              | ✅                  | ✅                         | ✅               | ✅                                          |
| Linux (Wayland): Sway 1.10+ / Hyprland / River (wlroots) | TBD                             | TBD                 | TBD                        | TBD              | TBD                                         |
| Linux (Wayland): KDE Plasma                              | TBD                             | TBD                 | TBD                        | TBD              | TBD                                         |
| Linux (Wayland): GNOME                                   | TBD                             | TBD                 | TBD                        | TBD              | TBD                                         |

## Supported Features

These are cross-platform features that are currently planned for the project
along with their statuses.

| Feature                                    | Status |
| ------------------------------------------ | ------ |
| Screen capture engine                      | ✅     |
| Video capture engine                       | ✅     |
| Audio capture engine                       | ✅     |
| Preview editor (overlay & window variants) | ✅     |
| System tray daemon                         | ✅     |

### Screen capture engine

| Feature                       | Status |
| ----------------------------- | ------ |
| Encoder setup & configuration | ✅     |

### Video capture engine

#### In-recording tools

| Feature                       | Status  |
| ----------------------------- | ------- |
| mouse clicks                  | Planned |
| keypress overlay              | TBD     |
| live annotation (with delete) | TBD     |

### Audio capture engine

| Feature                              | Status |
| ------------------------------------ | ------ |
| Audio cleanup and processing options | ✅     |

### Preview editor (overlay & window variants)

| Feature       | Status  |
| ------------- | ------- |
| delete        | ✅      |
| save          | ✅      |
| save as       | ✅      |
| copy          | ✅      |
| copy + delete | Planned |

#### Image annotation tooling

| Feature                | Status  |
| ---------------------- | ------- |
| covermarks support     | ✅      |
| color picker swatch    | Planned |
| arrows                 | Planned |
| text (size/resize)     | Planned |
| sequence markers       | Planned |
| dim except areas       | Planned |
| pixelate (destructive) | Planned |
| blur (destructive)     | Planned |
| box (fill/outline)     | Planned |
| draw (line widths)     | Planned |
| stickers               | Planned |
| eraser                 | Planned |

#### Video editor tooling

| Feature                                     | Status  |
| ------------------------------------------- | ------- |
| simple cutting tool                         | ✅      |
| simple transition dropdown (none/crossfade) | Planned |

#### Cloud uploader targets

| Feature      | Status  |
| ------------ | ------- |
| Proton Drive | Planned |
| OneDrive     | Planned |
| Google Drive | Planned |
| Dropbox      | Planned |

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

The experimental GPU zero-copy feature needs ffmpeg 8 headers (Arch, CachyOS,
recent Fedora)

Runtime dependencies: `ffmpeg` (screen recording), `tesseract` (OCR,
optional).

Updating: `git pull` and rebuild; the in-app update check links to the
releases page on Linux.

---

## Tiling window managers

Cosmic Capture Kit makes an effort to play nicely with a few popular tiling
window managers. The overlay tools will bypass tiling, while the preview editor
and the settings window will not by default. You can change the behavior of the
settings window and the preview editor by using the information below.

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
[[on-window-detected]]
if.window-title-regex-substring = 'Cosmic Capture Kit - Settings'
run = ['layout floating']

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

</details>

<details>
<summary><b>COSMIC desktop (Linux)</b></summary>

  ```
  ~/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom
  ```

  Both `appid` and `title` are matched as regular expressions (unanchored, so they match as
  a substring), and both must match. The shared `Cosmic Capture Kit` prefix is a substring of
  both window titles, so it floats BOTH; the two full titles are distinct, so a single-window
  rule needs no anchoring:

  ```
  [
      (enabled: true, appid: "dev.frosthaven.CosmicCaptureKit", title: "Cosmic Capture Kit - Settings"),
      (enabled: true, appid: "dev.frosthaven.CosmicCaptureKit", title: "Cosmic Capture Kit - Preview Editor"),
  ]
  ```

</details>

---

## License

The source code in this repository is licensed under [GPL-3.0-only](LICENSE).
The Linux app is free software: use it, build it, share it (it's free forever).
If it's useful to you, donating via [PayPal](https://paypal.me/Frosthaven) will
support future work but is not required.

Official macOS and Windows releases are separately licensed binary builds by the
copyright holder. (The author holds the copyright to all code in this repository
and additionally licenses their own code to themselves for those proprietary
builds; the GPL grant above applies to everyone else and to this repository's
contents.).

## Contributions & Credits

- Icon by [Ashley Ball](https://ashleythedesigner.com/);
- Embedded icon licensing lives in [res/icons/ATTRIBUTION.md](res/icons/ATTRIBUTION.md).
