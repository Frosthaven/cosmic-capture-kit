We bake extra features into some compositors when capturing screenshots:

- Freeze pixels during selection
- Preserve mouse cursor
- Preserve window transparency
- Preserve wallpaper

Below describes the behaviors that each should exhibit.

Each extra is a capability BIT of the active capture backend
(`Caps::capture_extras`, DRAGON-186), and what a capture applies is the
user's toggle AND that bit, so a stale "on" from a supporting backend can
never make an unsupporting one try to honor it. The settings rows render only
where the bit is set. Since DRAGON-562 the bits are decided per extra, per
backend, not as a family: the native (ext-image-copy-capture) backend
declares all of them, while the portal fallback backend declares freeze and
transparency off (no launch-instant scene, and pixels forced opaque pending a
measured alpha probe) but keeps cursor, wallpaper, fullscreen-awareness and
the single-window aesthetics (padding, both borders, shadow, rounding) on.
The aesthetics and the wallpaper backdrop are pure image math over the
finished frame it is handed. The cursor is the stream's own cursor mode:
DRAGON-592 renamed that capability bit from `cursor_session` to
`cursor_toggle`, because "include or omit the pointer" is the question the
toggle asks and a sprite session is only one way to answer it. Frosted glass
stays absent on the portal path either way: it needs the scene BEHIND the
window, which the portal cannot provide.

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

How a backend delivers that is its own business, and only the SHOW-OR-HIDE part
is a promise (DRAGON-592). A backend with a real cursor sprite can also honour
the position rules above, which is what the native path does. The portal has no
sprite: it bakes the pointer in at grab time (`CursorMode::Embedded`) or leaves
it out entirely (`CursorMode::Hidden`), so on a portal session the toggle works
and the position is whatever the pointer was doing when the frame was taken.
The colour picker always asks for the pointer to be left out, on every backend:
its snapshot is sampled per pointer move, so a baked pointer would sit over the
pixels it exists to read for the whole session.

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

There shouldn't be any combination of the above functionalities that are
forgotten in the final output. All possible combinations should have an
intuitive outcome.
