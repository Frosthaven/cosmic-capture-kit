---
title: Keyboard Shortcuts
---

# Settings: Keyboard Shortcuts

This page is where you change any key the app listens for. It is split into
three tabs, and it always opens on the first one.

| Tab | What it holds |
|---|---|
| Capture | The global capture keys, and the scanner's text keys |
| Recording | Stop, microphone and system audio |
| Preview Editor | Everything the editor listens for |

## How a row works

Every row is the same shape: the name of the action, a button showing the key it
currently uses, and a small **x** beside it.

- **Press the button**, and it changes to "Press a key…". The next key you press
  becomes the new shortcut. Press Esc instead to back out and keep what you had.
- **Press the x** to remove the shortcut entirely. The row then reads "Unbound",
  and that action has no key until you give it one.
- A row you have changed grows a **reset** control that puts the factory key
  back. For the handful of tools that ship with no key at all, reset clears the
  row again rather than inventing one.

Keys are shown the way your computer writes them. macOS shows the modifier
symbols, Windows writes Ctrl, Alt, Shift and Win, and Linux writes Ctrl, Alt,
Shift and Super.

## Capture

### Global

*macOS and Windows only.* On Linux your desktop owns global keys, so this
section is not shown. See [Getting started](../getting-started.md) for how to
add them on Linux instead.

These six are different from every other row on the page. They are system-wide
keys, owned by the background helper, so they work when no window of the app is
focused. **All six ship with no key set.** Give a key to the ones you want.

| Shortcut | What it does |
|---|---|
| Capture All In One | Opens the full picker, so you choose region, window or monitor |
| Capture Active Window | Captures the frontmost window straight away, with no picker |
| Capture Active Monitor | Captures the monitor under the pointer straight away, with no picker |
| Capture All In One (no editor) | The same picker, but the result is saved, copied and announced without the editor opening |
| Capture Active Window (no editor) | The same instant window capture, delivered without the editor |
| Capture Active Monitor (no editor) | The same instant monitor capture, delivered without the editor |

A clash is never accepted quietly. If you give one row a key that another row
already had, the row that took it says "This shortcut was taken from *the other
one*. That shortcut is now unbound", and the row that lost it says "Unbound.
*The other one* now uses this shortcut". If some other app on your computer
already owns the key system-wide, the row tells you: "Another app is already
using this shortcut, so it will not work here."

### OCR Text Recognition

These work while you are scanning.

| Action | Default |
|---|---|
| Select all text | Ctrl+A, or Cmd+A on macOS |
| Deselect all text | Ctrl+D, or Cmd+D on macOS |
| Copy selected text | Ctrl+C, or Cmd+C on macOS |

## Recording

These work in the capture overlay whether or not a recording is running.

| Action | Default |
|---|---|
| Stop and save recording | Enter |
| Toggle Microphone | M |
| Toggle system audio | S |

## Preview Editor

### Action Shortcuts

| Action | Default |
|---|---|
| Save | Ctrl+S, or Cmd+S on macOS |
| Copy to clipboard | Ctrl+C, or Cmd+C on macOS |
| Upload | Ctrl+U, or Cmd+U on macOS |
| Close | Esc |
| Covermark | W |
| Undo | Ctrl+Z, or Cmd+Z on macOS |
| Redo | Ctrl+Shift+Z, or Cmd+Shift+Z on macOS |

### Image Editor Shortcuts

Six of these tools ship with **no key of their own**. That is on purpose. They
belong to a shared key that steps through them, and the row is here so you can
give one a key of its own if you use it constantly. A key you set on the tool
itself always wins over the shared key.

| Action | Default | Notes |
|---|---|---|
| Select tool | V | Select, multi-select and move annotations |
| Hand tool | H | Pan the picture by dragging it |
| Select all annotations | Ctrl+A, or Cmd+A on macOS | |
| Deselect all annotations | Ctrl+D, or Cmd+D on macOS | |
| Arrow tool | A | |
| Step marker tool | I | Drops a numbered marker |
| Box tool | none | Reached with Cycle shape tools |
| Highlight tool | none | Reached with Cycle shape tools |
| Box Highlight tool | none | Reached with Cycle shape tools |
| Pixelate tool | none | Reached with Cycle redaction tools |
| Blur tool | none | Reached with Cycle redaction tools |
| Spotlight tool | none | Reached with Cycle shape tools |
| Pencil tool | B | The editor's own button calls it Freehand |
| Text tool | T | |
| Eraser | E | |
| Cycle redaction tools | M | Pixelate and blur. Press again to switch |
| Cycle shape tools | U | Highlight, border, and border highlight. Press again to switch |
| Cycle line width | L | |
| Duplicate annotation | D | |
| Crop tool | C | |
| Color | S | Opens the annotation colour picker |
| Swap color | X | Swaps to the companion colour, and back again |

### Video Editor Shortcuts

| Action | Default |
|---|---|
| Play | P |
| Previous frame | , |
| Next frame | . |
| Delete segment | Delete |

## Two keys you cannot change

Two shortcuts sit outside this page on purpose.

- **Ctrl+C, or Cmd+C, in the capture overlay** copies a region you have drawn.
  It is your system's own copy convention rather than an app preference, and the
  scanner already uses the same keys for copying recognised text, so making it
  changeable would create a clash the app would have to ask you to resolve.
- **Ctrl+F, or Cmd+F**, jumps to the search box in this settings window.
