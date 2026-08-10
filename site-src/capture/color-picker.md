---
title: The colour picker
---

# The colour picker

The colour picker reads a colour off your screen and gives you its value in
seven notations. It is a tool of its own rather than a capture. Nothing is
saved, and no file is written.

## Starting it

| Where | How |
|---|---|
| The tray or menu bar | **Color Picker**, the first entry, above Scanner |
| The preview editor | The pipette button at the left of the image toolbar, just before the colour swatch |
| A terminal, or your own shortcut | `cosmic-capture-kit --color-picker` |
| A global key | The **Color Picker** row in [Settings, under Shortcuts](../settings/shortcuts.md) |

**The Color Picker shortcut ships with no key set**, like every other global
shortcut. Give it one if you use the tool often.

## Picking a colour

The screen dims and a magnifier follows your pointer.

The magnifier shows the pixels around the pointer as squares, so you can see the
grid rather than a blur. The pixel you are about to take sits in the middle,
outlined. A ring in your accent colour runs around the rim. Beside it, a small
chip is filled with the colour itself and carries its hex value, so you can read
the answer before you commit to it.

**The pointer itself is hidden while you pick**, on every platform. The arrow
would otherwise sit on top of the very pixel the lens is reading. The lens is
centred on where the pointer is, so that is still what you aim with.

| What you do | What happens |
|---|---|
| Move the pointer | Moves the sample |
| Arrow keys, or `h` `j` `k` `l` | Moves the sample one pixel; hold one down to keep moving |
| Click, Enter or Space | Takes the colour the magnifier is showing |
| Scroll wheel, or a two-finger trackpad scroll | Zooms the magnifier |
| Numpad + and numpad - | Zooms the magnifier |
| Esc, or right-click | Leaves without taking anything |

**The keys are there so an exact pixel does not need a steady hand.** Nudge with
the arrows or the vim letters until the outlined pixel is the one you want, then
press Enter or Space. Reaching back for the mouse to click would undo the aim you
just made. One tap is always one pixel, whatever your desktop's key-repeat
settings are, and a held key falls into a steady repeat after a moment.

**Zooming changes how much of the screen the lens holds, not how big the lens
is.** It opens holding 13 of your screen's pixels across the lens and travels
both ways: out to 52 when you want to see what is around the sample, in to 6 when
you need to tell one pixel from its neighbours. It stops a little short of 1:1 at
the wide end, because a lens showing your pixels at their own size adds nothing
your eyes do not already give you. The zoom is not remembered: every pick starts
at the default.

The ring's thickness is your **Selection box thickness** from
[Settings, under General](../settings/general.md), the same setting the capture
region's box uses.

Near the edge of a screen the magnifier is **cut off by the edge** rather than
pushed back inland, so it keeps following your pointer instead of stopping at the
wall, and it is never squashed to fit. The part hanging off the screen shows the
dimmed backdrop rather than invented pixels. The last row and column of pixels on
a screen are still reachable.

**How dark the screen goes** is the **During Color Picker** slider in
[Settings, under General](../settings/general.md). It is 33 percent by default.
The dim is there to say the tool is armed. It does not change the colour you get,
because the value is read from a picture of your screen taken before the dim went
on. See [What it cannot do](#what-it-cannot-do).

## The result window

Clicking copies the hex to your clipboard and opens a small window. The window is
a fixed size and cannot be resized, because nothing in it scrolls: it is exactly
as big as the rows it holds.

At the top is a wide swatch of the colour. Under it is one row per notation, and
under those the colours you picked before.

| Row | What it looks like |
|---|---|
| HEX | `#FF8800` |
| RGB | `rgb(255, 136, 0)` |
| HSL | `hsl(32, 100%, 50%)` |
| HSV | `hsv(32, 100%, 100%)` |
| OKLCH | `oklch(75.6% 0.176 60.7)` |
| CMYK | `cmyk(0%, 47%, 100%, 0%)` |
| LAB | `lab(70.2% 34.4 76.4)` |

Every row has a **copy button** on the right that copies that row's text. The
button shows a tick for a moment so you know it worked.

**Every row is editable.** Type a value into any of them and the swatch and all
the other rows follow it. The boxes are forgiving: the function name, the
parentheses and the commas are all optional, spaces work as separators, and a
trailing `%` is accepted. What they will not guess at is the wrong number of
values, so three numbers typed into the CMYK box is treated as a typo rather
than as a colour. A half-typed value is left alone until it makes sense, so the
swatch never flashes through nonsense on the way to what you meant.

Two notes on the numbers themselves:

- **CMYK here is the device-agnostic kind**, the same numbers a design tool
  shows. It is not an ICC separation for a real press, which would depend on the
  ink, the paper and the profile.
- **LAB and OKLCH can describe colours your screen cannot show.** Type one of
  those and the value is brought back to the nearest colour sRGB can hold, rather
  than wrapping round to something unrelated.

### Recent colours

The bottom row holds the colours you picked before, newest first, up to **ten**.
The oldest falls off the end. Picking a colour off the screen that is already in
the row moves it back to the front rather than adding a second copy of it. They
are saved between launches, so they are still there the next time you open the
tool.

- **Clicking one loads it** into the swatch and the rows. It does not move it to
  the front. The row stays a stable place to look rather than reshuffling itself
  under your pointer.
- **Only an actual pick adds to the list.** Loading a recent colour does not, and
  neither does typing in a box, so the row never fills up with values you were
  only trying out.

**The pipette at the right of that same row starts another pick.** The window
stays open while you do, and the colour you pick lands in the window you already
have: it becomes the new swatch and the newest entry in the row below. There is
only ever one picker window, so picking again from the tray, a shortcut or the
command line updates that same window rather than stacking up another.

Each pick is still a fresh start that dims your screen again, rather than a
re-read of the picture the window was opened from. That is deliberate: it leaves
you free to take the next colour off a different monitor.

## Picking a colour to draw with

The preview editor's image toolbar has its own pipette, just before the colour
swatch. It is there so you can take a colour off the screen and draw with it.

A pick started that way **goes straight into the editor**: it becomes the
annotation colour, it recolours anything you have selected, and it joins the
editor's own recent colours, exactly as choosing a colour from the swatch would.
**No result window opens.** You asked for a colour to draw with, not for a window
to read and dismiss, and the colour is already showing in the swatch.

If the editor has closed by the time you click, nothing is lost. The pick falls
back to the ordinary result window.

## What it cannot do

These are worth knowing before you reach for the tool.

**It reads a picture of your screen taken when the tool opened, not the live
screen.** So anything that changes after you start picking, a video playing, a
progress bar, an animation, cannot be picked: you will get the colour it had at
the moment you opened the tool. This is deliberate. The picker dims your screen
while you aim, so reading the live screen would read the dim as well and report a
colour that is darker than the real one. A picker that reports the wrong colour is
worse than one that shows you a still.

**Under a Flatpak install it covers one monitor, not all of them.** A sandboxed
app has to ask your desktop for screen access, and that permission covers the one
monitor you granted. A normal install covers every screen at once.

**The lens never straddles two screens.** It belongs to whichever screen your
pointer is on, so pushing across the seam between two displays moves the whole
lens to the other one rather than drawing half on each. Since every screen's own
last row and column stay reachable from that screen, this is never in your way.
