---
title: Settings
---

# Settings

The settings window is the one part of the app that behaves like a normal
program. It takes no capture and it does not close itself.

## Opening it

- Press the gear button on the capture overlay toolbar.
- Press the Settings button in the preview editor.
- Use the tray or menu bar icon, if you have the background helper on.
- Run the app with `--settings`.
- On Windows, use the Cosmic Capture Kit Settings shortcut in the Start Menu.

It also opens by itself, on the Screen Recordings tab, if you try to record and
the app cannot find ffmpeg. That is not an error dialog, it is the app taking you
to the page that explains the problem.

Only one settings window can exist at a time. Opening it again just brings the
existing one forward.

## Everything saves as you go

There is **no Apply button and no OK button**. The moment you flick a switch or
type a number, it is saved. Closing the window is not a commit, and closing it
without "saving" loses nothing. Theme changes appear immediately.

## The pages

| Page | What it covers |
|---|---|
| [General](general.md) | The tray helper, launching at login, and how the app looks |
| [Capture Modes](capture-modes.md) | Where captures are saved, the scanner, and how a screenshot is framed |
| [Preview Editor](preview-editor.md) | Whether the editor is a window or an overlay, what it does on exit, and covermark text |
| [Audio & Video](audio-video.md) | Microphone and speakers, the cleanup chain, frame rate, quality and encoders |
| [Keyboard Shortcuts](shortcuts.md) | Every key the app listens for |
| [Cloud Accounts](cloud.md) | Connecting a drive to upload captures to |
| [Health](health.md) | What is working, what is missing, and debug logging |
| [About](about.md) | Version, updates and donating |

Four of those pages are split into tabs of their own. Tabs always start on the
first one when you open the window.

## Finding a setting

Press the magnifier in the header, or press Ctrl+F, or Cmd+F on macOS. Type what
you are after.

The search covers **every page**, not just the one you are looking at, and it
groups what it finds under the name of the page it lives on. It matches on the
name of a setting and on its description, and matching the name of a whole
section keeps all of that section's settings together. If nothing matches you
get "No matching settings."

One gap worth knowing: the settings on the **Mixing** tab of Audio & Video are
not in the search index. Open that tab directly to find them.

## Putting something back

**One setting.** Every row has a small reset icon at its right edge. It only
lights up, and only does something, when that setting is not at its default. Its
tooltip reads "Reset to default". A greyed icon means you have not changed
anything there.

**One page.** Most pages have a **Reset to defaults** chip at the bottom. On the
tabbed pages it resets **only the tab you are looking at**, and leaves the other
tabs alone. It asks first: "Reset this page? This restores the settings on this
page to their defaults. This cannot be undone."

About, Health and Cloud Accounts have no page reset, because they hold almost
nothing to reset.

**Everything.** A **Factory reset** button sits at the bottom of the page list.
It asks first too: "Factory reset? This restores every setting on every page to
its default. This cannot be undone."

## The page list

The list of pages on the left can be collapsed to icons with the hamburger
button in the header. It also collapses on its own when you make the window
narrow, and expands again when you widen it. The icons keep their tooltips
either way.

The window remembers the size you left it at.
