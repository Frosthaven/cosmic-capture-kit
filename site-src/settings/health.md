---
title: Health
---

# Settings: Health

The Health page answers one question: is everything this app needs actually
present on your computer? It is the first place to look when something does not
work.

You do not have to open it to see the answer. The Health entry in the page list
carries a coloured icon that tracks the worst thing it found.

| Colour | Meaning |
|---|---|
| Green | *All dependencies are satisfied.* Everything works. |
| Amber | *Some optional features are unavailable. See below.* The app works, and you lose only the feature named. |
| Red | *A required dependency is missing. Application may not work as expected.* Something core is missing. |

## Status

One row, **Overall health**, whose description is that same one-line summary.

## Debug

**Enable Debug Logging**
:   **Off by default.** Turn it on if you have been asked for a log, then turn it
    off again.
:   The description under this row is not a description. It is the actual folder
    the log is written to. Beside the switch is a folder button that opens that
    folder for you, and it works whether logging is on or off.
:   The log records what the app did, never what you captured. See the
    [privacy page](../privacy.md).

**Application Permissions**
:   *macOS only.* A **Manage permissions** button that opens the permission
    checker, the same window you saw on your first launch.

## Required

One row per thing the app needs, each either a green tick or a red error, with a
sentence saying what breaks without it.

| Item | If it is missing |
|---|---|
| Screen Recording (macOS) | Screenshots and recordings come out blank. Grant it, then relaunch the app |
| Screenshot capture | No capture method is available at all |
| Screen recording | No recording method is available at all |
| ffmpeg | Recording, video playback in the editor, the microphone test and the audio meters are all disabled |
| ffprobe | Recordings cannot be previewed. It comes with ffmpeg, though some systems package it separately |

## Optional features

The same shape, in amber. Missing one of these costs you exactly the feature it
names, and nothing else.

| Item | What it buys you |
|---|---|
| tesseract | Text recognition in the scanner |
| tesseract language data | The language pack that recognition actually needs. Having the program without a language pack means every scan fails |
| pactl, or audio device selection | Choosing specific microphones and speakers instead of your system defaults |
| Hardware video encoder | Recording using your graphics chip. Without it, recording uses your processor and still works |
| NVIDIA GPU recording | Saves copying every frame through main memory, which costs processing time on large screens. Linux only, and only shown on machines that have NVIDIA hardware to begin with |
| Microphone (macOS) | Recording your voice with a video. Video-only recording still works |

Each missing row spells out the exact package to install, so you can copy the
name straight into your package manager.

On macOS, a permission that is missing shows a button instead of an icon:
**Request**, which asks the system for it, or **Open Settings**, which takes you
to the right pane of System Settings.
