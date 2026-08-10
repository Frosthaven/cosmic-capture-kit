---
title: General
---

# Settings: General

Two tabs: **Settings** and **Appearance**. The window opens on Settings.

## Settings

### Behavior

**System tray icon**
:   *Cosmic Capture Kit will remain in the system tray for easily launching
    capture sessions.*
:   A small helper process that stays running so a capture is always one press
    away. On macOS it is a menu bar item, on Windows and Linux a tray icon.
:   **On by default on macOS and Windows**, because the global capture keys only
    work while it is running. **Off by default on Linux**, where your desktop owns
    the capture key instead, so nothing breaks without it.

**Automatically start on login**
:   Brings the tray helper back after a restart.
:   **On by default.** This row only appears while the tray icon setting above is
    on, since there would be nothing to start otherwise.

## Appearance

### Theme

**System Default**
:   *Disable to customize the theme.*
:   **On by default.** While it is on, the app follows your desktop's own theme
    and the four rows below are hidden.

**Light/dark mode**
:   *Automatic follows the system's light or dark preference.*
:   Automatic, Dark or Light. **Automatic** by default.

**Accent**
:   A row of colour swatches. The first one hands the choice back to your system
    accent colour. After the nine theme colours there is a **+** button that opens
    a **Custom Accent** panel where you can type an exact colour as hex or RGB,
    drag a hue slider, or pick one you used recently. That panel has **Apply**,
    **Use theme default** and **Cancel**.
:   **By default no accent is set**, so the app follows your system.

**Automatic Contrast Boost**
:   *Adapts your selected accent color for optimal contrast.*
:   **On by default.** Keeps a chosen accent readable against the background
    rather than letting it wash out.

**Edge rounding**
:   Three preview boxes: **Round**, **Slightly Round** and **Square**.
:   **Round** by default on macOS and Linux, **Slightly Round** on Windows, so the
    app matches the shape of the desktop around it.

**Selection box thickness**
:   How thick the line around your capture region is drawn, from 1 to 8 pixels.
    It is also the thickness of the ring around
    [the colour picker's magnifier](../capture/color-picker.md), so the two match.
:   **2** by default. This one is always available, even with System Default on.

### Overlay Opacity

Four sliders, each from 0 to 100 percent. They control how much the app dims the
parts of the screen you are not capturing. Higher means darker.

| Setting | Default | When it applies |
|---|---|---|
| During Color Picker | 33% | While the colour picker is up, so you can still read colours through it |
| During Region Selection | 66% | While you are drawing and adjusting a region |
| During Countdown & Recording | 33% | Once the timer starts and while recording, so you can still see what is happening |
| During Preview | 90% | Behind the editor when it opens as an overlay |

The colour picker's dim never changes the colour you get. It reads a picture of
your screen taken before the dim went on.
