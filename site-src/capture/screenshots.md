---
title: Screenshots
---

# Taking a screenshot

Choose **Photo** in the kind group, pick a mode, and take the shot. The result
opens in [the preview editor](../editor/index.md) unless you launched with the
"no editor" variant of your shortcut.

## The countdown

The chip next to the kind buttons is the delay. It shows the current delay as a
two-digit number with a small caret, and clicking it opens a short list:

| Choice | Shown on the chip |
|---|---|
| No delay | `00` |
| 3s delay | `03` |
| 5s delay | `05` |
| 10s delay | `10` |

**No delay** is the default. A chip set to `00` is drawn muted. Any other value
lights the chip up, so a delay you forgot about is visible at a glance.

The delay chip does not appear when you are scanning. A scan never counts down.

### While the countdown runs

Once you take the shot with a delay set, the toolbar clears down to just the
timer chip, and the audio buttons if you are recording. The chip turns dark red
and shows three things: an icon saying what is about to happen (a check mark for
a photo, a record dot for a video), the seconds remaining, and a cross.

Your selection stays exactly where it is and stops responding, so nothing can
nudge it while the clock runs.

**The rest of the screen stays usable during the countdown.** Clicks pass
straight through the overlay everywhere except the toolbar itself, which is the
whole point: you set a five second delay so you can go and open the menu you
want in the picture.

### Cancelling

Click the cross on the chip, or press Esc. Either one stops the timer and puts
you back in region select with your selection intact. It does not close the app.

A delayed shot captures the live screen at the moment the timer fires, and
grabs your pointer again at that moment too, so the cursor in the picture is
where it was when the shutter went, not where it was when you started.

## Copying a region without the editor

If all you want is the region on your clipboard, draw it and press Ctrl+C, or
Cmd+C on macOS. The app copies it and closes, with no editor in between.

## What a screenshot looks like

Several options change how a captured image comes out. They live in
[Settings, under Capture Modes](../settings/capture-modes.md) rather than on the
overlay, because they are choices you make once rather than per capture.

| Option | Default | What it changes |
|---|---|---|
| Preserve mouse cursor | On | Whether your pointer appears in the picture |
| Preserve window transparency | On | Whether glass and blur effects survive, instead of being flattened |
| Preserve wallpaper | On | Whether your wallpaper shows behind a captured window, instead of transparency |
| Freeze pixels during selection | Off | Whether the screen stands still while you select. See [Capture modes](modes.md#freezing-the-screen) |

A row only appears if the way your system captures the screen can actually
honour it, so you will never be offered a switch that does nothing.

There are more options for how a single captured window is framed, including
padding, rounded corners, shadow and two configurable borders. Those are
covered on the
[Capture Modes settings page](../settings/capture-modes.md).
