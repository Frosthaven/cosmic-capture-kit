---
title: Getting started
---

# Getting started

Cosmic Capture Kit is a one-shot capture tool. It does not sit on screen waiting
for you. You press a key, it takes over the screen so you can pick what to
capture, it hands you the result, and then it exits.

That is the whole shape of the app:

1. You press your capture key.
2. The **capture overlay** appears over a frozen picture of your screen.
3. You pick a region, a window or a monitor, and take a screenshot, start a
   recording, or scan what is on screen.
4. The **preview editor** opens with the result, so you can mark it up, cut it,
   and decide where it goes.
5. You save, copy, share or upload, and the app closes.

## Installing

### macOS

Download the latest `.dmg` from the
[releases page](https://github.com/Frosthaven/cosmic-capture-kit/releases) and
drag the app into Applications. macOS 13 or later, on Apple Silicon.

### Windows

Download the latest `.msi` from the
[releases page](https://github.com/Frosthaven/cosmic-capture-kit/releases) and
run it. It installs for your user only, so there is no administrator prompt, and
it adds two Start Menu shortcuts: Cosmic Capture Kit, and Cosmic Capture Kit
Settings. Everything the app needs to record is installed with it.

The installer is not code-signed yet. On the first run Windows may show
"Windows protected your PC". Click **More info**, then **Run anyway**.

### Linux

The Linux app is free software and you build it yourself. The
[README](https://github.com/Frosthaven/cosmic-capture-kit#build-from-source)
has the exact commands for your distribution, including the packages to install
first. COSMIC is the supported desktop today.

## The first launch on macOS

macOS will not let any app read your screen until you say so, so the first
launch opens a permissions window that lists what the app is asking for and
whether it has it yet. The statuses update live as you grant them.

| Permission | Needed? | What it buys you |
|---|---|---|
| Screen Recording | Required | Without it every screenshot and recording comes out blank. macOS only applies this grant to the *next* launch, so relaunch the app after granting it. |
| Microphone | Optional | Records your voice into a video. Recording without your voice still works. |
| Notifications | Optional | Shows a banner when a capture is saved. Clicking the banner reveals the file in Finder. |
| Accessibility | Recommended | Lets Capture Active Window and Capture Active Monitor target the window you are actually focused on, and capture it in its active appearance. Without it the app guesses from the window stacking order, which is usually right but can pick the wrong window. |

Only Screen Recording is required. You can skip the rest and the window will
stop asking. If Screen Recording is missing, the window keeps coming back,
because there is nothing useful the app can do without it.

## Setting up your capture key

The app is meant to be launched from a key, not from an icon.

### macOS and Windows

Both platforms run a small background helper that lives in the menu bar (macOS)
or the notification area (Windows). It is what listens for your capture key, and
it is on by default. You can turn it off in
[Settings, under General](settings/general.md).

A fresh install ships with **no capture key set**. You choose your own in
[Settings, under Shortcuts](settings/shortcuts.md), in the section called
**Global**. There are six of them, and every one is optional:

| Shortcut | What it does |
|---|---|
| Capture All In One | Opens the full picker, so you choose region, window or monitor |
| Capture Active Window | Captures the frontmost window straight away, with no picker |
| Capture Active Monitor | Captures the monitor under the pointer straight away, with no picker |
| Capture All In One (no editor) | The same picker, but the result is saved, copied and announced without the editor opening |
| Capture Active Window (no editor) | The same instant window capture, delivered without the editor |
| Capture Active Monitor (no editor) | The same instant monitor capture, delivered without the editor |

### Linux

Your desktop owns the keys, so you add them there. Point a custom shortcut at
the app with the flag for the capture you want. On COSMIC that is
Settings, then Keyboard, then Shortcuts, then Custom shortcuts.

| Command | A key that suits it |
|---|---|
| `cosmic-capture-kit --region` | `Alt+Shift+1` |
| `cosmic-capture-kit --active-window` | `Alt+Shift+2` |
| `cosmic-capture-kit --active-monitor` | `Alt+Shift+3` |

Add `--no-editor` to any of them for a variant that skips the editor. The
capture is still saved, copied to the clipboard and announced by a
notification, and clicking that notification opens the folder with the file
selected.

The [command line page](cli.md) lists every flag.

## Opening settings

Settings is a normal window, and it does not take a capture.

- **macOS and Windows**: use the menu bar or notification area icon, or the
  Cosmic Capture Kit Settings shortcut in the Start Menu on Windows.
- **Any platform**: run the app with `--settings`.

Everything you change is saved as you change it. There is no Apply button.

Start there and set your save folders before your first capture, so you know
where things are landing.

## Where to next

- [The capture overlay](capture/index.md) covers picking what to capture,
  recording, and the scanner.
- [The preview editor](editor/index.md) covers marking up a capture and sending
  it somewhere.
- [Settings](settings/index.md) walks through every option, page by page.
