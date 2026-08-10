---
title: The preview editor
---

# The preview editor

Every capture lands here, unless you used one of the "no editor" shortcuts. It
is where you mark a screenshot up, cut a recording down, and decide where the
result goes.

**Your capture is already on your clipboard.** The moment the editor opens, the
capture is copied for you. If all you wanted was to paste it somewhere, you can
do that immediately and close the editor.

## A window or a whole screen

The editor comes in two shapes, and you can swap between them at any time with
the button in its header. Its tooltip reads "Fullscreen overlay" or "Windowed",
depending on which one you would get.

- **Windowed** is a normal window you can move, resize, and put beside the thing
  you are writing about. This is the default.
- **Overlay** fills the screen, the way the capture overlay does.

Which one you get for the *next* capture is a setting, under
[Preview Editor](../settings/preview-editor.md). On Windows 10 only the window
is available.

You can have several editors open at once. The one you last clicked owns the
keyboard.

## What is on screen

- The **top bar** holds the tools, grouped and captioned: Canvas, Shape, Redact,
  Box Highlight, Text and Draw. Those captions can be turned off in settings.
- The **bottom bar** holds colour, line width, the covermark, and the zoom
  control.
- **Save, Copy, Share and Upload** sit in the header, along with Undo, Redo,
  Settings and Close.

A recording gets a timeline instead of the annotation tools. There are no
annotation tools for video.

Buttons that carry your edits out of the editor tint themselves while you have
unsaved changes, so you can see at a glance that something is waiting.

## Zoom and pan

Zoom and pan apply to images. The video editor has no zoom.

| Control | What it does |
|---|---|
| Ctrl + scroll wheel | Zooms towards your pointer |
| Scroll wheel | Pans up and down, and sideways |
| Shift + scroll wheel | Pans sideways |
| Middle-mouse drag | Pans, whatever tool you have armed |
| Alt + left drag | Pans |
| The Hand tool, then drag | Pans |
| Pinch on a trackpad | Zooms, on macOS |

Scrollbars appear when the picture is bigger than the space for it, and they
work normally.

The zoom control at the bottom is a dropdown plus a slider. The dropdown offers
**Fit**, **100%**, **125%**, **150%**, **200%**, **300%**, **400%** and
**500%**. It is also the readout: at any other zoom it shows the exact
percentage. The slider has a magnetic notch at 100 percent so you can land on it
exactly.

**Fit** shows the whole picture and never blows it up bigger than life. It is
the reset. Zoom runs from 50 percent to 500 percent, and the list quietly drops
any preset your graphics hardware cannot actually draw.

One thing that surprises people: **100% means the size the picture was captured
at**, not physical pixels on your screen. On a high-resolution display, a
capture is taken at that display's real density, so true 1:1 is the 200 percent
preset.

## Undo covers everything

There is **one** undo history, and everything goes into it in the order you did
it: annotations, crops, covermark choices and their sliders, the spotlight dim,
timeline cuts and segment deletions.

That means undo always walks backwards through your actual session. You never
have to work out which undo stack you are in.

| Keys | What it does |
|---|---|
| Ctrl+Z, or Cmd+Z | Undo |
| Ctrl+Shift+Z, or Cmd+Shift+Z | Redo |

There are Undo and Redo buttons in the header too. They grey out when there is
nothing left to undo.

While you are typing inside a text annotation, those same keys step through that
text box's own history instead, which is what you would expect from any text
field.

## Keyboard shortcuts

Every one of these can be changed in
[Settings, under Keyboard Shortcuts](../settings/shortcuts.md), where you will
also find the full list. The primary key is Ctrl on Linux and Windows, and Cmd
on macOS.

### Doing something with the capture

| Keys | Action |
|---|---|
| primary + S | Save |
| primary + C | Copy to clipboard |
| primary + U | Upload |
| Esc | Close |
| primary + Z | Undo |
| primary + Shift + Z | Redo |
| W | Covermark picker |

### Tools

| Key | Tool |
|---|---|
| C | Crop |
| V | Select |
| H | Hand |
| A | Arrow |
| I | Step marker |
| M | Redaction tools: pixelate, then blur |
| U | Shape tools: highlight, border, border highlight, spotlight |
| T | Text |
| B | Freehand |
| E | Eraser |
| S | The annotation colour swatch and its palette |
| X | Swap to the companion colour |
| L | Next line width |
| D | Duplicate what is selected |
| primary + A | Select every annotation |
| primary + D | Deselect |
| Delete or Backspace | Delete what is selected |

### Video

| Key | Action |
|---|---|
| P | Play or pause |
| , | Previous frame |
| . | Next frame |
| Delete | Delete the selected segment |

**Esc happens in stages.** The first press ends whatever you are in the middle
of: a text edit, or a selection. Only a press with nothing left to give up
reaches the editor and closes it. You cannot lose your work to a stray Esc.

## Messages and waiting

Short messages appear as small notices, for about four seconds. They shorten
themselves as soon as you get back to work, they stack no more than three deep,
and repeating the same one refreshes it rather than piling up copies.

When the app is working on your export it dims the editor, shows a spinner, and
says what it is doing, with lines like "Baking in your edits" or "Rendering the
final cut". It ignores clicks while it works. The same thing happens with a
different set of lines while a fresh capture is being opened.

## Where to go next

- [Annotating](annotating.md): every tool, and how to select, move and delete.
- [Crop and covermark](crop.md).
- [Editing a recording](video.md): the timeline, cutting and deleting.
- [Saving, copying and uploading](sharing.md).
