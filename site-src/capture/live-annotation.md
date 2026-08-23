---
title: Live annotation
---

# Live annotation

Draw on your screen while a recording is running, and the drawing is in the
video. It is for pointing at things: circling the button you are talking about,
underlining a line of output, scribbling an arrow at the thing that just went
wrong.

Nothing about it is added afterwards. What you draw is in the file, at the
moment you drew it.

## The tools

They live in two places, and both offer the same four controls: the recording
toolbar, and the tray icon's **Live Annotations** menu. The tray is the one to
use when the toolbar has hidden itself, and it is the only one on a desktop
where the toolbar cannot be shown at all. In the two cases further down where
choosing a tool presents a window of ours, that window carries the same four
controls again, so they are always within reach of what you are drawing on.

| Control | What it does |
|---|---|
| Pointer | The normal recording state. Nothing draws |
| Marker | Ink that stays until you clear it |
| Magic Marker | Ink that clears itself |
| Clear Marks | Clears both kinds at once, right now |

Pointer, Marker and Magic Marker are one selector, so exactly one of them is
chosen at any moment and pressing the one already chosen does nothing. Clear
Marks is not a fourth tool. It is something you do once, so it does not stay
selected.

On **Linux**, you can also switch tools from the command line while a recording
is running:

```
cosmic-capture-kit record --tool pointer|marker|magic-marker|erase
```

`erase` is Clear Marks. Point a shortcut in your own desktop settings at each
one and you get the tools without reaching for the toolbar, which is worth
doing if you annotate often. See [the command line](../cli.md) for the rest of
the controls that work this way, and for why they are Linux only.

### Magic Marker

Magic Marker ink dissolves on its own, and the timer is measured from the last
thing you drew rather than from each mark's own age. So it holds while you are
still drawing. Keep annotating and nothing moves; stop for two seconds and the
whole drawing lifts over the next second, gone three seconds after you stopped.

It lifts in the order you drew it, oldest first, sweeping forward through the
ink. An arrow disappears from its tail toward its head rather than the whole
line going at once.

That is the difference between the two tools. Marker is for something you want
on screen for a while. Magic Marker is for pointing at something in passing,
without having to reach for Clear Marks afterwards.

## Putting the tools away

Three things put the tools back on Pointer, and they all do exactly the same
thing:

| Keys | |
|---|---|
| Esc | |
| V | The key the editor uses for its own Pointer tool |
| Right click | Anywhere on the recording |

Use whichever is nearest. **None of them can stop, save or cancel the
recording.** Stopping stays on the toolbar's stop button and in the tray menu,
which is deliberate: while you are drawing, the screen is a mode, and the key
you hit to leave a mode should never be the key that ends your take.

They do nothing while Pointer is already chosen, so there is no harm in
pressing one to be sure.

## What it looks like

Ink is drawn in a thin line, the same weight the preview editor calls **Thin
line**, and in the same colour the editor's annotation tools use. There is no
colour or width control while a recording is running.

While a drawing tool is chosen your pointer becomes a crosshair, which is how
you can tell at a glance that the screen has become a drawing.

### Your pointer stays in the recording

While a tool is chosen, the pointer keeps appearing in the video, drawn as the
same crosshair you see on screen. You do not lose your cursor for the part of
the recording where you are pointing at things, which is the part where a
viewer most needs to see where you are pointing.

If you have turned **Preserve mouse cursor** off in
[Settings, under Capture Modes](../settings/capture-modes.md), no pointer is
drawn. A recording you asked to have no cursor in does not gain one here.

## Pausing other media while you draw

**Pause other media during live annotation** in
[Settings, under Audio & Video](../settings/audio-video.md) pauses your music
and video players while a drawing tool is chosen, and starts them again when
you go back to Pointer.

It is **off by default**, and it is a separate switch from the preview editor's
version of the same idea. This one changes what the recording contains, because
the pause is audible in the recorded system audio, and annotating over a video
that is meant to be playing is a real thing to want.

## Three ways it can behave

Which one you get depends on your desktop and on what you are recording. The
app decides once, when the recording starts, and tells you on screen when it is
the third.

### Drawing on the screen

The usual case, and what you get on macOS, on Windows, and on COSMIC.

The drawing has a surface of its own that sits over everything, so you can draw
and use the recording controls at the same time. Marks stay on screen after you
go back to Pointer, until Clear Marks takes them or a Magic Marker dissolve
does.

### Drawing on the screen, one thing at a time

What you get on a desktop with no layer shell, which today means a GNOME
session or a Flatpak build, when you are recording a region or a whole monitor.

There is no way to put a see-through surface over everything, so choosing a
drawing tool presents one full-screen window instead. You are still drawing on
the live screen and the recording still gets it, but that window has the
keyboard and the pointer, so it is draw or use the controls rather than both.
Going back to Pointer takes the window away, and the marks lift with it.

### Drawing on a frozen picture

What you get when the recording is of a single window and the desktop will not
say where that window is on screen.

Choosing a drawing tool shows the last frame the recording captured, held still
inside a window of ours, and you draw on that. The window carries a caption
saying what is happening:

> Drawing on a frozen frame of the recorded window, which keeps recording live
> underneath.

Or, when the reason is that the desktop would not locate the window:

> Drawing on a frozen frame: this desktop will not say where the recorded window
> is, so the marks are placed on the picture instead of on the screen.

The caption is the honest caveat. The window you are recording carries on
recording live the whole time, so what you are drawing on is a moment that has
already passed. Your marks land on the picture at the position you put them,
which is what you want for circling a button that has not moved, and is wrong
if the thing you are pointing at has scrolled away. Going back to Pointer
dismisses the frozen picture.

## Where each one applies

| Your setup | Region | Monitor | Window |
|---|---|---|---|
| macOS 13 and newer | On screen | On screen | On screen |
| Windows 10 and 11 | On screen | On screen | On screen |
| COSMIC | On screen | On screen | On screen |
| KDE, Sway, Hyprland, niri and the rest | On screen | On screen | Frozen picture |
| GNOME sessions, and Flatpak builds | One at a time | One at a time | Frozen picture |
| X11 sessions | Not yet | Not yet | Not yet |

Two limits are worth stating plainly rather than dressing up.

**A window recording on a desktop other than COSMIC uses the frozen picture.**
Recording one window streams that window's own pixels, so your drawing has to
be placed into them, and placing it needs to know where the window is. Only
COSMIC's compositor will tell an app that. The desktop portal names a video
stream and never a window, and plain Wayland tells an app nothing about where
anything is.

**One exception to the COSMIC row above.** When a COSMIC recording does go
through the desktop portal, and two of your windows are exactly the same size,
the app can fail to tell which one you picked. It falls back to the frozen
picture and says so in the caption rather than putting your marks in the wrong
place.

**GNOME cannot host the drawing surface at all.** Mutter does not implement
`wlr-layer-shell`, which is the thing that lets an app put a surface over the
whole screen, so there is nowhere to put a see-through drawing layer. The
full-screen window described above is the workaround, not a preference.

## See also

- [Recording](recording.md): starting, pausing, muting and stopping.
- [Keyboard shortcuts](keyboard.md): every key the overlay listens for.
