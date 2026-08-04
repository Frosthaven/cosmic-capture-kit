---
title: Recording
---

# Recording your screen

Choose **Video** in the kind group, pick a mode, and take the shot the same way
you would for a screenshot. Press the Region button a second time, press the
Monitor button a second time, or click a window.

If the Video button is greyed out and its tooltip reads "Video (another capture
is recording)", another recording is already running. Finish that one first.

The recording begins immediately unless you set a delay, in which case it starts
when the countdown reaches zero.

## Sound

Two buttons appear beside the kind group as soon as you choose Video:
**Microphone** and **System audio**. System audio is your computer's own sound,
the thing you would hear from the speakers. Microphone is your voice.

You can turn each on or off before you start and while you are recording. A
button that is on wears a ring in your accent colour. A button that is off is
dimmed.

Once a channel is armed, its button doubles as a **level meter**. The fill rises
with the sound going into it, in green, and turns red if the level reaches the
red zone. That gives you a way to check your microphone is actually picking you
up before you commit to a take.

Muting is not the same as not recording. Both channels are captured the whole
time, and your mute choices are applied when the recording is finished. That is
why you can toggle freely mid-take without leaving a gap.

### Push to talk

If you turn on push to talk in
[Settings, under Audio & Video](../settings/audio-video.md), the microphone
button stops being a switch and becomes an indicator. Your microphone is live
only while you hold the microphone key, which is **M** by default.

Holding the key lights the button up even before you start recording, so it
doubles as a quick microphone test.

## The controls while recording

The toolbar changes to three controls:

1. **Pause and resume.** Shows pause bars while recording, and a play icon while
   paused.
2. **The red chip**, which is **stop and save**. It shows a stop icon and the
   elapsed time. The time is *recorded* time, so it stops counting while you are
   paused, and it is the length the finished file will be.
3. **Cancel**, which throws the recording away.

Pausing genuinely stops the recording. Nothing is captured while paused, and the
result is one continuous file with no seam. Resume picks up exactly where you
left off.

The microphone and system audio buttons stay on the toolbar throughout, so you
can mute yourself mid-sentence and unmute later.

For a **region** recording the dimmed surround and the selection outline stay on
screen the whole time, so what you see inside the line is exactly what is going
into the file. Window and monitor recordings draw no frame and show only the
controls.

## Controlling it from the tray

Every recording also puts an icon in your system tray, or the menu bar on macOS.
The icon itself tells you the state: corner brackets on their own means idle,
brackets with a dot in the middle means recording, and brackets with pause bars
means paused.

Its menu is the same everywhere:

- Toggle Microphone
- Toggle System Audio
- Pause Recording, or Resume Recording
- Finish & Save Recording
- Cancel & Delete Recording

Below that, a **Capture Menu** submenu holds Scanner, Capture Region, Capture
Window, Capture Monitor, Settings and Quit.

## When the toolbar hides itself

There is a setting called **Hide toolbar on full screen captures**, off by
default. With it on, the toolbar disappears in the cases where it cannot get out
of the recorded picture: a whole-monitor recording, a region too big to leave
room beside it, or a window whose position the app was not told.

When it hides, the overlay stops taking clicks and keys entirely, and the tray
menu becomes your only way to control the recording. Know where it is before you
turn this on.

## Keyboard

| Key | What it does |
|---|---|
| Enter | Stops the recording and saves it |
| Esc | Also stops the recording and saves it |
| M | Microphone on or off |
| S | System audio on or off |

These work before a recording starts too, so you can set your channels up with
the keyboard and watch the buttons respond.

## If recording will not start

If the app cannot find ffmpeg, which is the tool it uses to write video files,
it does not fail silently. It opens Settings on the Screen Recordings tab so you
can see what is missing. On Windows and macOS the official builds include
everything needed. On Linux you install ffmpeg yourself.
