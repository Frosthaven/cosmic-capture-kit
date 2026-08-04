---
title: Annotating
---

# Annotating a screenshot

The annotation tools appear for images. A recording gets
[a timeline](video.md) instead.

Every tool button's tooltip shows the key it is currently on, so if you rebind
something the tooltips follow.

## The tools

The top bar groups the tools by what they are for. The captions below are the
group names you see on screen.

### Canvas

The three that never draw anything.

| Tool | Key | What it does |
|---|---|---|
| Crop | C | Opens a crop session. See [Crop](crop.md) |
| Select | V | Picks things up, moves them, resizes them |
| Hand | H | Drags the picture around |

### Shape

| Tool | Key | What it does |
|---|---|---|
| Arrow | A | Draws an arrow. This is the one you will use most |
| Step Marker | I | Drops a numbered round badge |

**Arrows can bend.** Select an arrow and you get three handles: one on each end,
and one in the middle of the shaft. Drag the middle one and the arrow bows into
a curve, which is how you point around something in the way. Drag it back onto
the straight line and the arrow straightens out again.

**Step markers number themselves.** You place them with a click rather than a
drag, and they count up as you go. Delete number two and the rest renumber
themselves straight away, so your numbers are never wrong. Undo restores the
right numbers too. Markers are always round, and the size you pick is remembered
for your next capture.

### Redact

Both of these are **permanent** once you save. They destroy the pixels
underneath rather than covering them, so nobody can peel them back off.

| Tool | What it does |
|---|---|
| Pixelate | Replaces the area with a coarse mosaic |
| Blur | Blurs the area heavily |

They share the **M** key. Press M to pick up the current one, press M again to
switch to the other.

There is no strength slider for either. The app chooses a strength that actually
hides the content, rather than one that looks hidden but is not.

### Box Highlight

Four ways to draw attention to a rectangle. They share the **U** key, and each
press moves to the next one.

| Tool | What it does |
|---|---|
| Highlight | A translucent highlighter wash, the marker-pen look |
| Border | An outline, with nothing inside it |
| Border Highlight | Both at once: a wash with an outline around it |
| Spotlight | Dims everything *outside* the rectangle, so the rectangle looks lit |

The highlighter adapts to what is underneath, so it stays readable on dark
content as well as light.

**Spotlight comes with a dim slider** on the toolbar, next to the group. It
controls how dark the rest of the picture goes, and it reads as visibility, so
all the way up means no dimming at all. The first spotlight you place turns the
dim on for you. The slider works whatever tool you have armed.

### Text

| Tool | Key |
|---|---|
| Text | T |

Two ways to place text: **click** to get a box that grows with what you type, or
**drag** a box of a set width, and your text wraps inside it.

Two dropdowns sit beside the tool:

- **Size**, from 12 to 128 pixels. **32** to start with.
- **Font**, with two choices, each shown in its own lettering: **Hand**, a
  handwritten style, and **Clean**. Hand is the default.

Both apply to new text and to text you have selected, so you can change your
mind afterwards. The line width control changes the outline weight around your
letters, which is what keeps text readable over a busy screenshot.

### Draw

| Tool | Key | What it does |
|---|---|---|
| Freehand | B | Draws ink wherever you drag |
| Eraser | E | Rubs out freehand ink |

The eraser only removes freehand ink. It will not touch an arrow, a box or a
text caption. Delete those instead.

The freehand tool is called "Pencil tool" on the
[shortcuts settings page](../settings/shortcuts.md), which is the same tool
under an older name.

## Colour and thickness

**Colour** is the swatch on the bottom bar, on the **S** key. Its picker offers,
in order:

1. A **swap** button, on the **X** key, which flips to the companion of your
   current colour and back again.
2. Your accent colour and its complement.
3. Nine colours from the theme.
4. Up to five colours you used recently.
5. A **+** that opens a full colour wheel, where you can type an exact value.

Picking a colour also **recolours whatever you have selected**, as a single
undoable step. So the way to change an arrow's colour is: select it, pick a
colour.

Pixelate, blur and spotlight have no colour, for obvious reasons.

**Line width** is the control beside it, on the **L** key. There are seven
thicknesses, and the tooltips name them in plain words from "Thinnest line" to
"Thickest line" rather than giving you numbers. Like colour, it applies to your
current selection as well as to the next thing you draw.

## Working with what you have drawn

### Selecting

- **Click** an item with the Select tool.
- **Ctrl+click** or **Shift+click** adds and removes items from a selection.
- **Shift+click also works while a drawing tool is armed**, so you can grab
  something without switching tools first. Ctrl+click does not, because with a
  drawing tool Ctrl means "draw on top of this".
- **Drag on empty space** with Select to sweep up everything inside a box.
- **Ctrl+A** selects everything, **Ctrl+D** clears the selection. Cmd on macOS.

The freehand and eraser tools never select. A press with the pencil is always
ink.

### Moving and resizing

Drag the body of anything selected to move it. Drag a corner or an edge handle
to resize it. Select several things and you get one box around the lot, and
resizing that scales all of them together.

Everything you do this way is one undo step per gesture.

### Deleting and reordering

Press **Delete** or **Backspace**, or use the right-click menu:

| Menu item | What it does |
|---|---|
| Set to current color | Recolours the selection to whatever the colour swatch holds |
| Duplicate | Makes a copy, offset towards the middle |
| Bring to Front | Puts it on top of everything |
| Send to Back | Puts it under everything |
| Move Up | One step up |
| Move Down | One step down |
| Delete | Removes it |

**D** duplicates without the menu.

### Two shortcuts worth knowing

**Double-click a tool button** and the app places a ready-made one in the middle
of the picture at a sensible size, instead of waiting for you to drag. Handy
when you know you want a box and do not care exactly where yet. It does nothing
for Freehand, Eraser, Select, Hand and Text, which have no default form.

**Swap one rectangle tool for another in place.** Select a box, then click a
different rectangle tool, and it turns into that instead. Border, Highlight,
Border Highlight, Pixelate, Blur and Spotlight all interchange this way, in one
undo step. So "that should have been a blur, not a box" is one click, not a
delete and a redraw. Step markers are not part of that family.

**Ctrl and a drawing tool** forces a brand new shape on top of an existing one,
rather than grabbing the one already there.
