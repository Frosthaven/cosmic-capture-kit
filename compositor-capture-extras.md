We bake extra features into some compositors when capturing screenshots:

- Freeze pixels during selection
- Preserve mouse cursor
- Preserve window transparency
- Preserve wallpapur

Below describes the behaviors that each should exhibit.

## Freeze pixels during selection

This option should ensure that when we are capturing a region, window, or
monitor that the visible pixels are static and at the state that they were when
we launched the selection/capture.

Swapping to recording (or turning on a countdown timer) should show live
changes, but swapping back to picture mode should return to our frozen capture.

## Preserve mouse cursor

This option will either show or hide the mouse cursor on image captures. If we
don't have a countdown timer, it should be a part of the frozen pixels that we
are capturing against. If we do have a countdown timer, the cursor should be
visible where it was when the timer ran out (same time that the capture actually
happens).

## Preserve window transparency

Windows like Wezterm can have transparency. Region/Window/Monitor capture should
all respect this option and capture the windows either with transparency if they
have some, or without transparency.

## Preserve wallpaper

When enabled, region/window/monitor captures should all include our wallpaper
(appropriately aligned to the window location if necessary). When not enabled,
one of two things can happen:

- if Preserve window transparency is enabled, then we should get the
  region/window/monitor against a transparent background.
- if Preserve window transparency is disabled, then we should get the
  region/window/monitor against a black background.

## Extra Notes

There shouldn't be any combination of the above functionalies that are forgotten
in the final output. All possible combinations should have an intuitive outcome.
