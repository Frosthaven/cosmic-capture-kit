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
| Linux (Wayland): COSMIC | Cosmic Compositor w/extras | Supported |
| Linux (Wayland): Sway 1.10+ / Hyprland / River (wlroots) | Planned | Planned |
| Linux (Wayland): KDE Plasma | PipeWire portal | Planned |
| Linux (Wayland): GNOME | PipeWire portal | Planned |
| macOS 13+ (Apple Silicon) | ScreenCaptureKit Compositor w/extras | Supported |
| Windows 11 | Planned | Planned |

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

### Linux

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

### Windows

Coming Soon.

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
