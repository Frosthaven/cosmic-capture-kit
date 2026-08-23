---
title: Keyboard shortcuts
---

# Keyboard shortcuts in the capture overlay

Every key on this page works while the capture overlay is on screen. The editor
has [its own set](../editor/index.md#keyboard-shortcuts).

Most of these can be changed in
[Settings, under Keyboard Shortcuts](../settings/shortcuts.md). The two that
cannot are marked as fixed, and the reason is given.

## Reading the tables

One key is spelled differently depending on your computer. This guide calls it
the **primary key**:

- **Linux and Windows**: Ctrl
- **macOS**: Cmd

So "primary + C" means Ctrl+C on Linux and Windows, and Cmd+C on macOS.

## Always available

| Keys | What it does |
|---|---|
| Esc | Steps back. See below |
| primary + C | Copies the region you have drawn straight to the clipboard, without going through the editor |
| primary + F | Jumps to the search box in the settings window |

**Esc is a step back, not always a quit.** What it does depends on what is
happening:

- While a recording is running, it puts the drawing tools back on Pointer. It
  never stops the recording. Stop and cancel stay on the toolbar and in the
  tray menu.
- While a countdown is running, it cancels the countdown and puts you back in
  region select. Your selection is still there.
- Otherwise it closes the tool.

Esc is a fixed shortcut. It cannot be changed.

**primary + C** only fires when all of this is true: you are in region mode, you
have actually drawn a region, you are not scanning, and no countdown or
recording is running. It is a fixed shortcut rather than a changeable one,
because it is your system's own copy convention rather than an app preference,
and because the scanner already uses the same keys for something else. See
below.

**primary + F** is also fixed. It does nothing unless the settings window is
open.

## While a recording is set up or running

These work whether or not a recording has started yet, so you can set your audio
up before you press record and watch the buttons light up as you do.

| Keys | What it does |
|---|---|
| Shift+Alt+Enter | Stops the recording and saves it |
| Shift+Alt+M | Turns the microphone on or off |
| Shift+Alt+S | Turns system audio on or off |

These three are the same on every computer. There is no Cmd version on macOS.

They are chords rather than plain keys so they cannot fire while you are
typing, and so the microphone and stop keys can be registered with your
desktop's own shortcut service. Where your desktop offers that service, those
two work even when the app you are recording has the focus, which is what hold
to talk needs. COSMIC does not offer it yet, so there they work while the
capture overlay has the focus.

If you have turned on push to talk in
[Settings, under Audio & Video](../settings/audio-video.md), the microphone
shortcut stops being a switch and becomes a hold to talk key instead. Your
microphone is live only while you hold it down. That works before a recording
starts too, so you can hold the keys and watch the microphone button light up
as a quick test.

## While you are drawing on a recording

[Live annotation](live-annotation.md) lets you draw on the screen while a
recording runs, and the drawing is in the video. While a drawing tool is turned
on the pointer becomes a crosshair and the screen is a drawing, until you put
the tools back on Pointer.

| Keys | What it does |
|---|---|
| Esc | Puts the tools back on Pointer |
| V | The same thing. It is the key the editor uses for its own Pointer tool |
| Right click | The same thing again |

All three do exactly the same thing, so use whichever is nearest. None of them
can stop or cancel the recording.

These three are fixed shortcuts. They cannot be changed, and they do nothing
while no drawing tool is turned on.

## While scanning

The scanner keeps primary + C for its own meaning, so copying recognised text
always works the same way.

| Keys | What it does |
|---|---|
| primary + A | Selects every piece of recognised text in the region |
| primary + C | Copies the recognised text you have selected |
| primary + D | Clears the text selection |

primary + D is the deselect key everywhere in the app, which is why the editor
uses the same keys to clear an annotation selection.
