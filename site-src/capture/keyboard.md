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

- While a recording is running, it stops the recording and saves it.
- While a countdown is running, it cancels the countdown and puts you back in
  region select. Your selection is still there.
- Otherwise it closes the tool.

Esc is the shortcut named **Close** in settings, so changing it there changes
all three.

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
| Enter | Stops the recording and saves it |
| M | Turns the microphone on or off |
| S | Turns system audio on or off |

If you have turned on push to talk in
[Settings, under Audio & Video](../settings/audio-video.md), the microphone key
stops being a switch and becomes a hold to talk key instead. Your microphone is
live only while you hold it down. That works before a recording starts too, so
you can hold the key and watch the microphone button light up as a quick test.

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
