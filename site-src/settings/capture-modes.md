---
title: Capture Modes
---

# Settings: Capture Modes

Three tabs: **Scanner**, **Screenshots** and **Screen Recordings**. The page
opens on Scanner.

## Scanner

### QR/Barcode Recognition

**Enable QR/barcode recognition**
:   *Recognized QR codes and barcodes become interactible.*
:   **On by default.**

### Text Recognition (OCR)

**Enable text recognition (OCR)**
:   *Recognized words become naturally selectable.*
:   **On by default.**
:   This needs a separate piece of software called `tesseract`, plus a language
    pack. If either is missing the switch goes inert and amber, and a note tells
    you exactly what to install. The rest of the scanner keeps working.

**Text matching strictness**
:   A slider from 0 to 60. Lower accepts more guesses, higher only keeps text the
    recogniser is confident about.
:   **20** by default. Only shown when text recognition is available and on.

## Screenshots

### Location

**Save screenshots to**
:   A folder, typed or picked with the folder button beside it.
:   **`~/Capture`** by default.

### Capture

**Capture method**
:   How the app reads your screen. The list only offers what actually works on
    your machine, and it is set to the best one for your system already. Most
    people never touch it.
:   On Linux the choices are **Compositor screencopy** and **PipeWire portal**. On
    macOS it is **ScreenCaptureKit**, and on Windows **Windows capture**.

On Linux, choosing the PipeWire portal adds a row about screen access, since
that method asks your system for permission. If you granted it once, a **Forget**
button lets you be asked again next time. With the portal, capture requests go
through your system's own approval dialog, and for a region you should pick the
monitor your region is on.

The four switches below are only shown when your capture method can honour them.
The PipeWire portal cannot, so they disappear if you choose it.

| Setting | Default | What it does |
|---|---|---|
| Freeze pixels during selection | Off | Shows a still picture of your screen while you select, so moving content holds still. *Great for capturing images in motion and OCR content.* |
| Preserve mouse cursor | On | Includes your pointer in the picture |
| Preserve window transparency | On | Keeps glass and blur effects instead of flattening them |
| Preserve wallpaper | On | Shows your wallpaper behind a captured window, instead of see-through nothing |

### Single Window Aesthetics

These decide how a single captured window is framed. They are the difference
between a plain rectangle and something that looks presentable in a document.
The whole section disappears if your capture method cannot support it.

**Window focus appearance**
:   **Active** or **Inactive**: whether the captured window looks focused or
    unfocused. **Active** by default.

**Active border**
:   A colour swatch and a width slider from 0 to 10 pixels. The swatch opens an
    **Active Border Color** panel with hex and RGB entry, a hue slider, and a
    **Follow accent color** button.
:   **By default it follows your accent colour**, at **3** pixels wide.

**Inactive border**
:   The same swatch and slider for the unfocused look.
:   **A grey** by default, at **1** pixel wide.

**Drop shadow**
:   Puts a soft shadow behind the captured window. **On by default.**

**Add padding around the window**
:   Leaves breathing room around the window rather than cropping tight to its
    edges. **On by default.**

**Padding**
:   How much room, in pixels. **50** by default, and it stops at 512. Only shown
    while padding is on.

## Screen Recordings

### Location

**Save recordings to**
:   **`~/Capture`** by default, same as screenshots.

### Capture

**Capture method**
:   Same idea as on the Screenshots tab, chosen separately because recording and
    screenshotting have different strengths. **On Linux this defaults to the
    PipeWire portal**, and elsewhere to your platform's own method.

### Behavior

**Hide toolbar on full screen captures**
:   *When the floating toolbar can't fit outside of the recording area, this will
    hide it instead of placing it in-frame. You can still control the recording
    via the system tray icon.*
:   **Off by default.** Turn it on and read
    [the recording page](../capture/recording.md#when-the-toolbar-hides-itself)
    first, so you know where the controls went.

The microphone and system audio switches are not here. They are on the capture
toolbar, because they are a per-recording choice.

## If a capture method is missing

Each tab shows a red **Availability** note when the app cannot capture at all,
or when a tool it needs for recording is not installed. That note names exactly
what to install. The [Health page](health.md) collects all of them in one place.
