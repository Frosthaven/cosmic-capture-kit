---
title: Capture modes
---

# Capture modes

There are three modes, and they are the three buttons on the right of the
toolbar. They work the same whether you are taking a photo, recording a video,
or scanning.

## Region

Draw a rectangle around the part of the screen you want. Everything outside it
dims, and the rectangle itself is drawn with your accent colour and corner
brackets.

Once a rectangle exists you can keep adjusting it:

| Gesture | What happens |
|---|---|
| Drag on empty space | Draws a new rectangle |
| Drag a corner | Resizes both sides at once |
| Drag an edge | Moves that one side |
| Drag inside the rectangle | Moves the whole thing |
| Click without moving | Keeps what you have |

The pointer tells you which of those you are about to get. It is a crosshair
over empty space, a resize arrow on a corner or an edge, and a grab hand inside
the rectangle.

**Edges stop at the screen border.** Drag an edge out to the edge of a display
and it sticks there, which makes "exactly this whole screen" easy to hit. Push
about 40 pixels further and it breaks through and follows your pointer again.
Hold Alt, or Option on macOS, while you drag to turn that off for the whole
drag.

Your last region is remembered, so the next capture starts with the same
rectangle. That makes repeated captures of the same panel or window quick.

## Window

Every window on the screen appears as a thumbnail in a grid, over your
wallpaper. Click the one you want.

While the app is gathering the list you will see a spinner with a line like
"Rounding up your windows". If a screen genuinely has nothing on it, it says "No
windows on this display" instead.

## Monitor

Hover a display and the whole thing lights up with a border. Click anywhere on
it to capture it. With one screen this is a single click.

## Freezing the screen

There is a setting called **Freeze pixels during selection**. It is **off** by
default. Turn it on in
[Settings, under Capture Modes](../settings/capture-modes.md).

With it on, the moment you press your capture key the app takes a picture of
your screen and shows you that instead of the live desktop. The screen appears
to stop. That is the point: a video keeps playing, a menu keeps closing, and an
animation keeps moving while you are trying to draw a rectangle around it.
Freezing lets you take your time.

Some details worth knowing:

- Freezing applies to **region** selection only, and only for photos and scans.
  Picking a window fetches fresh pixels, because a frozen picture would show the
  window in its unfocused state. Picking a monitor has nothing to stand still
  for. Video never freezes.
- **Setting a delay releases the freeze.** If you pick a countdown, the overlay
  goes back to showing you the live screen, because what the delayed shot will
  actually capture is the live screen a few seconds from now. Set the delay back
  to "No delay" and the freeze comes back.
- The scanner does not use the frozen picture. It takes a fresh shot of your
  region each time you scan.
