---
title: Crop and covermark
---

# Crop and covermark

Two things that change the whole picture rather than a part of it.

## Cropping

Press the **Crop** button in the Canvas group, or press **C**. Images only.

Entering crop takes over the editor. Every other control disappears for the
duration and every other key is ignored, so there is nothing to accidentally
press. The app also zooms out a little for you, until there is room around the
picture to grab the edges, so you can always pull the crop rectangle outward.

### Adjusting

Drag any of the eight handles, four corners and four edges, or drag the middle
to move the whole rectangle. A rule-of-thirds grid is drawn inside as a guide.

Edges **snap gently to the edge of the picture** when you get close, which makes
"trim exactly to the left edge" easy. Hold Ctrl, or Cmd on macOS, to ignore the
snap.

You can zoom and pan while you are cropping.

The smallest crop is 8 pixels on a side. You are also allowed to drag the
rectangle *outside* the picture, and that extra area comes out black in the
saved file, which is a quick way to pad an image.

### Finishing

- **Apply Crop**, the accent-coloured button with a tick, or press **Enter**.
- The **x** button, or press **Esc**, cancels the crop. Esc here cancels the
  crop session only. It never closes the editor.

### A crop is never permanent

The crop is stored as a rectangle and applied only when you export. Your
original pixels are never thrown away. Open the crop tool again and the whole
picture is back, with your rectangle sitting live on it, ready to be dragged
outwards again.

It is also part of [the shared undo history](index.md#undo-covers-everything),
one step per Apply.

Cropping does not delete or move your annotations. Anything outside the
rectangle simply is not drawn into the file you export. Crop, change your mind,
uncrop, and your arrows are all still where you left them.

## Covermarks

A covermark is a pattern laid over your whole capture, tiled across it. It is
the "DRAFT stamped diagonally across the page" idea. It works for both images
and recordings.

Press the **Covermark** button on the bottom bar, or press **W**.

### Choosing one

A panel of thumbnails opens upward, offering:

1. **None**, which takes the covermark off again.
2. **Confidential**, the built-in mark.
3. **Your own text**, labelled with the words you configured. If you have not
   set any it reads "Custom text".
4. **Any SVG image you have put in the covermarks folder**, each labelled with
   its filename.

Whichever one is applied carries a tick in front of its name.

You can drive the panel from the keyboard: **left and right** move between
options, **Enter** applies the one you are on, and **Esc** closes the panel.

### Adjusting it

Once a covermark is on, two sliders appear beside the button:

- **Zoom**, which scales the pattern. All the way down means it is fitted to
  cover the capture exactly.
- **Opacity**, which decides how strongly it shows.

Each of those is remembered per covermark, so going back to one you used before
brings its settings with it. Each drag is one undo step.

### Your own text and images

The words come from
[Settings, under Preview Editor](../settings/preview-editor.md), in the
Covermarks section. Out of the box they read "CONFIGURE TEXT IN SETTINGS",
which is the app pointing you at where to change them.

Your own artwork goes in the covermarks folder as SVG files. The settings page
prints the exact folder path under that setting, so you do not have to guess.

On the file you export, the covermark is composited at the capture's full
resolution, so it stays crisp no matter how large the capture is.

The covermark is hidden while you are cropping, so it cannot get in the way of
framing.
