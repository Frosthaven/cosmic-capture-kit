# The command line

`cosmic-capture-kit` is a one-shot tool: it opens what you asked for, finishes
the job, and exits. The commands below let a keybinding, or a script, go straight
into one flow. Run `cosmic-capture-kit --help` to print this list from the
binary.

```
cosmic-capture-kit <command> [options]
```

**One command per launch.** The first word decides what happens, and the options
after it belong to that command alone. Two commands never combine in one line,
and nothing is guessed: input the app cannot read is an error, not an overlay.

`cosmic-capture-kit` here means whichever file you installed. The Linux AppImage
takes exactly the same commands, so substitute its filename
(`CosmicCaptureKit-x86_64.AppImage capture --region`) and everything below
applies unchanged. The AppImage runtime reserves its own `--appimage-*` flags,
which collide with none of ours.

## The commands

```
cosmic-capture-kit <command> [options]

  scan                          Scan the screen for QR codes / text (region select)
      --action <refresh>        retake the screen snapshot in the scanner already open

  color                         Color tools
      --picker                  pick a color from the screen (magnifier overlay)
      --viewer                  open the palette viewer window
      --timer <0-99>            countdown before the pick (--picker only)

  capture                       Take a screenshot
      --region                  select a region
      --window                  select a window
      --monitor                 select a monitor
      --active-window           capture the active window immediately, no picker
      --active-monitor          capture the monitor under the cursor immediately
      --timer <0-99>            countdown before the capture (this launch only)
      --skip-editor             save, copy and notify instead of opening the editor

  record                        Start a screen recording, or control the one running
    starting one:
      --region                  select a region
      --window                  select a window
      --monitor                 select a monitor
      --sound <both|mic|system|none>   arm audio for this launch only
      --timer <0-99>            countdown before recording starts (this launch only)
      --skip-editor             save, copy and notify instead of opening the editor
    controlling the one running (no --region/--window/--monitor):
      --action <pause|save|cancel>     pause/resume, finish and save, or discard
      --tool <pointer|marker|magic-marker|erase>   live annotation tool; erase clears marks
      --sound <both|mic|system|none>   set audio channels on the live recording

  editor <file>                 Open an image or video in the editor
      --overlay                 fullscreen overlay instead of a window

  timer <0-99>                  Set the default countdown for every capture and record
                                (0 clears it; bare `timer` prints the current value)

  sound <both|mic|system|none>  Set the default audio channels for every recording
                                (bare `sound` prints the current value)

  settings                      Open the settings window
  permissions                   Permission checker (macOS only)
  tray                          Run the tray / menu bar resident

  --version, -V                 Print the version
  --help, -h                    Show this help
```

## A launch with no command at all

From a **terminal**, a bare `cosmic-capture-kit` prints the list above and exits.
It takes nothing: a screenshot of your own terminal is never what you meant, and
a mistyped command must not go and grab the screen instead of saying so.

From the **desktop**, meaning a login item, Finder, the Start Menu or an
application-menu entry, there is no terminal to print to. A bare launch there
starts the tray when the tray setting is on, and opens the settings window when
it is off. It never captures either way, which is what makes the app safe to put
in a login item.

## Taking a screenshot

`capture` opens the capture overlay, and it wants **exactly one target**. The
target says which selector the overlay lands on, and the overlay's own toolbar
switches between them afterwards without relaunching, so one binding on
`capture --region` is enough for everyday use.

| Command | Effect |
|---|---|
| `capture --region` | The overlay, region selected |
| `capture --window` | The overlay, window selected |
| `capture --monitor` | The overlay, monitor selected |
| `capture --active-window` | Capture the active window immediately, no picker |
| `capture --active-monitor` | Capture the monitor under the cursor immediately, no picker |

`capture` on its own, or with two targets, is a usage error rather than a guess.

`--active-window` and `--active-monitor` ask the compositor which target is
active, through protocols a Flatpak sandbox hides. Under Flatpak the two are not
available: the launch records the failure and exits instead of guessing a target.

`--timer <0-99>` puts a countdown in front of the capture, for this launch only.
Any value in that range works, not just the ones the toolbar offers, and the
saved default is untouched.

With `--active-window` or `--active-monitor`, the timer also postpones the
choice of target: which window is active, or which monitor holds the cursor, is
decided when the countdown ends, not when the command starts. Focus a different
window, or move the cursor to another monitor, and that is what gets captured.
This applies to the saved `timer` default the same way.

`--skip-editor` skips the preview editor. The capture is still saved to the
capture folder, copied to the clipboard and announced by a notification, exactly
what happens when no editor can be opened. Only the editor goes away.

The notification is the whole feedback for such a capture, so it names what it
delivered, "Region copied to clipboard", "Window saved", "Monitor saved", and
clicking it opens the capture's folder with the file selected. It says "saved"
rather than "copied" whenever the clipboard write did not happen, and the body
then gives the reason. (A still image over 1 GB is not copied automatically; a
recording is copied at any size, because it goes on the clipboard as a file
reference rather than as pixels.)

## Starting a recording

`record --region`, `record --window` and `record --monitor` record that target.
They are exactly what the tray's direct recording entries run, so a keybinding
can do precisely what the tray does. There is no `record --active-window`: an
immediate grab with no picker is a screenshot idea, and `record` says so rather
than inventing one.

`--timer` and `--skip-editor` mean the same here as they do for a screenshot: a
countdown before recording starts, and a delivery with no editor at the end.

`--sound` arms the recording's audio channels for this launch only, overriding
the saved defaults without writing them back: the next launch without it uses the
saved arms again. The values are `both`, `mic`, `system` and `none`, and case
does not matter. Anything else rejects the launch with an error instead of
guessing, because a typo must never record a microphone you asked to silence.

## Controlling the recording that is already running

These launch nothing. Each one reaches the recording in progress, acts on it, and
exits. They are **Linux only**.

| Command | Effect |
|---|---|
| `record --action pause` | Pause the recording, or resume it when paused |
| `record --action save` | Finish the recording and save it |
| `record --action cancel` | Cancel the recording and delete it |
| `record --sound <both\|mic\|system\|none>` | Set which audio channels the live recording keeps |
| `record --tool <pointer\|marker\|magic-marker\|erase>` | Switch the live annotation tool; `erase` clears the marks |

They are control options rather than commands of their own because they act on
the same recording, so they combine in one line and are applied in a fixed order:
`--sound` first, then `--tool`, then `--action`. That order is what makes
`record --sound none --action save` mean "mute the rest of it, then save", and
never the other way round.

They exist because on Linux an in-app keyboard shortcut cannot reach
a recording. Once recording starts, the app hands the keyboard back to the window
you are recording, so you can keep typing into it, and COSMIC's desktop portal
has no way to give an app a shortcut that works without focus. A shortcut bound
in your own desktop settings has no such problem, which is the same reason the
capture key is bound that way. Point one at each command and you get focus-free
recording controls. See the "Shortcuts" section of the
[README](https://github.com/Frosthaven/cosmic-capture-kit#readme).

They act on whichever recording is running, whether it was started from the
overlay, the tray or a keybinding. With no recording in progress the command
prints `no recording is in progress` and exits with status 1, so a script can
tell.

On macOS and Windows these are inert: they print a line saying so and exit. Those
platforms keep their capture windows through a recording, so the in-app shortcuts
still work there, and their menu bar or tray item carries the same controls.

## Scanning the screen

`scan` opens the scanner. It always works on a region, which is its capture
invariant, so there is no mode option to give it.

`scan --action refresh` is the one exception to "these commands launch
something": it retakes the screen snapshot inside the scanner window you already
have open, so you can point it at something that changed without closing and
reopening the tool. With no scanner open there is nothing to refresh, and the
command says so.

## The color picker

`color --picker` is its own tool rather than a capture. It takes no screenshot and
saves no capture. The tray menu's **Color > Color Picker** entry, the preview
editor's pipette button and the Color Picker global shortcut all run exactly this
command.

While the overlay is up: move the pointer to aim, click to take the color under
it, and press Esc or right-click to leave with nothing. The arrow keys and `h`
`j` `k` `l` move the sample one pixel per tap, and holding one down keeps it
moving, so you can land on an exact pixel without the mouse. Enter or Space then
takes the color the magnifier is showing, which is the point of having them:
reaching for the mouse to click would undo the aim you just made.

The mouse wheel, a trackpad scroll and the numpad `+` / `-` all zoom the
magnifier. It opens holding 13 of the screen's pixels across the lens, and goes
both ways from there: out to 52 when you want context, in to 6 when you need to
tell one pixel from its neighbors. It stops a little short of 1:1 at the wide
end, where a loupe would only show you what your eyes already do. The zoom is
not remembered between runs.

The dim behind it is the **During Color Picker** slider in Settings, under
General (33% by default), and the ring around the magnifier is drawn at the
**Selection box thickness** from the same page.

`--timer <0-99>` puts a countdown in front of the pick, for this launch only:
the screen stays yours until the timer runs out, so you can open the menu or
hover the tooltip you want to sample from, and the picker samples what is on
screen when the countdown ends. Without the flag the saved `timer` default
applies, and `--timer 0` skips that default for one pick. The viewer takes no
timer; it opens a window and samples nothing.

### The window a pick opens

A click copies the color and opens a small window. It is a fixed size and nothing
in it scrolls: it is exactly as big as the parts it holds.

Top to bottom, those parts are a **saturation and value square**; a **controls
row** carrying the pipette that starts another pick, a round swatch of the current
color, and the hue and alpha strips stacked beside it; the **value row**; a
hairline divider with an **Add to recents** button sitting on it; and two rows of
the colors you kept.

The value row is one row whatever the notation. A copy button leads it, the boxes
follow, and a pair of chevrons at the right end is a single button that opens the
menu of the seven notations: HEX, RGB, HSL, HSV, OKLCH, CMYK and LAB. **Hex is one
box** holding the whole spelling, `#FF8800CC` and all, because that is the value
people paste whole; every other notation splits into a box per component, each
captioned underneath with what it holds ("R", "G", "B", "A" in RGB). Typing in a
box moves the swatch, the square and the strips with it.

**The notation is remembered between launches, and it is the spelling a pick
copies.** Changing it copies nothing, so you can cycle the seven to read them
without overwriting your clipboard seven times. The copy button, a swatch's own
Copy and the copy chord are what write to the clipboard.

Two chords belong to this window, alongside Tab, which walks the boxes, then the
chevron unit, then the history, then the panel when it is open:

| Chord | What it does |
|---|---|
| `Ctrl+Enter` (`Cmd+Enter` on macOS) | Keep this color: files it into the recents, the same as the Add to recents button |
| `Ctrl+Shift+C` (`Cmd+Shift+C` on macOS) | Copy the value. Plain `Ctrl+C` is left to the boxes, so it still copies whatever you selected in one |

**The recents hold eighteen colors**, two rows of nine, newest first, each kept
with the alpha it was filed at. They are persisted, because the app is one-shot
and an in-memory list would be empty at every launch. Clicking one loads it
without reordering the row, so the row stays a stable place to look. A pick
writes to it, and so does any discrete replacement of the current color, which
files the color you are leaving first, so a color you were holding cannot go
missing. Typing in a box never writes: exploring with the square, the strips or
the boxes is free, and only the explicit add files. The same color at the same
alpha is never filed twice.

Right-click a swatch for its own menu. What it offers depends on which swatch it
is, between making it the current color, adding it to the recents, copying it,
removing it, and sending it to one of your saved palettes.

### The panel: harmonies and saved palettes

The button at the right of the window's header **doubles the window's width** and
opens a panel beside the picker. The picker's own column does not move. Both the
open state and the tab you left it on are remembered, and `Ctrl+Tab` cycles the
tabs.

**Harmonies** shows five cards worked out from the current color: Complementary,
Analogous, Triadic, Tetradic and Monochromatic. Clicking a swatch makes it the
current color, and each card's heading explains in one line what the relationship
is.

**Saved Palettes** holds your own named groups of colors:

* **New Palette** is at the top right of the tab, and a new group lands at the TOP
  of the list with its name selected, ready to type over. A name is edited in
  place at any time by clicking it.
* Beside that button, a search field filters the list by name, and a sort button
  offers six one-shot orders: name A to Z or Z to A, lightest or darkest first,
  warmest or coolest first.
* Each group's title row carries a **pipette**, which starts a pick that lands
  straight in that group, and a **plus**, which adds the color the window is
  showing.
* **Dragging** does the rest: along a group's own bar to reorder it, onto another
  group to COPY the color there, onto the picker's side of the window to make it
  the current color, and off the window to remove it. A group's NAME drags too, to
  reorder the groups or, dropped off the window, to delete the group, which asks
  first. Colors never ask; groups do.
* The swatch menus keep the explicit verbs a drag does not offer, including **Move
  to palette**, which vacates the group it came from.
* Clicking a swatch applies it as the current color. A color already in a group is
  not added to it twice.

Palettes live in their own `palettes.toml` beside the config file, so a factory
reset leaves them alone. Before you save anything of your own you start with two
groups, Catppuccin Latte and Mocha.

**Only one picker window is ever open.** Pick again, from that window's own
pipette or from anywhere else, and the window you already have takes the new
color and puts it at the head of its recents. A second window never appears.

## The palette viewer

`color --viewer` opens that same window on its own, so you can get at the
colors you already saved without picking a new one first. There is no dim, no
magnifier and no click to make: the window opens straight away, loaded with the
color you picked most recently, with its alpha. With no history yet it opens on a
vivid pink, `#FF06B5`, rather than on white, so a first-run window is obviously
working rather than looking blank. The tray menu's **Color > Palette Viewer**
entry runs exactly this command.

Everything the window does is the same as after a pick: the square, the strips,
the value row, the recents and the panel with your saved palettes in it. Opening
it does not change your palette and does not touch the clipboard, so looking is
free. Its window title is `CCK Palette Viewer`, which is the name tiling window
manager rules should match on (see the README).

The one-window rule above covers this too. If a picker window is already open,
asking for the palette viewer brings that window forward rather than opening a
second one, and the color it is showing is left exactly as it was.

Three limits are worth knowing:

* **The sample comes from a snapshot taken when the tool launched**, not from live
  pixels, so content that changes after you open the picker cannot be picked. This
  is deliberate: the picker's own dim is on screen while it reads, so a live read
  would report a darkened color.
* **Under Flatpak the picker covers only the monitor the portal granted.** A native
  install covers every screen at once.
* **CMYK is the device-agnostic conversion, not a color-managed one.** It is the
  plain separation a design tool shows, with no ICC profile behind it, so it is a
  readable approximation rather than a value to hand to a press. LAB and OKLCH can
  also describe colors sRGB cannot hold; typed values outside it are brought back
  to the nearest one your screen can show.

## Opening a file in the editor

`editor <file>` opens an image or a video you already have in the preview editor.
It captures nothing, so it is a viewer with the editor's tools attached. It opens
in a resizable **window** by default; `editor <file> --overlay` uses the
fullscreen overlay instead.

## The saved defaults: `timer` and `sound`

These two write settings rather than doing anything to the screen. They are the
command-line half of the matching rows in Settings, so a script can set what a
person would otherwise click.

| Command | Effect |
|---|---|
| `timer` | Print the saved countdown and change nothing |
| `timer <0-99>` | Save that countdown for every capture and every recording |
| `timer 0` | Clear the countdown |
| `sound` | Print the saved audio channels and change nothing |
| `sound <both\|mic\|system\|none>` | Save those channels for every recording |

Keep them apart from the options of the same name on `capture` and `record`.
Those apply to one launch and never write anything down; these write the default
that every later launch starts from.

## Settings, permissions and the tray

| Command | Effect |
|---|---|
| `settings` | Open the settings window. Only one ever exists; asking again brings it forward |
| `permissions` | **macOS only.** The permission-checker window (Screen Recording / Microphone / Notifications) with live status and Request / Open System Settings / Relaunch actions. Linux and Windows have no such grants to check |
| `tray` | Run the resident: the tray on Linux and Windows, the menu bar on macOS |

`tray` runs the resident whatever the tray setting says, which is what makes it
useful for testing and for a launcher entry of your own. Turning the setting on
is still the normal way to get one at login.

On Linux an autostart entry written by an older version carries the word
`resident` instead. It keeps working: the entry is migrated to `tray` for you the
first time the new version runs. The setting itself never changed its name, only
the word on the command line did.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The command did what it said |
| `1` | The command was understood and could not be carried out: no recording is running, no scanner is open, the file is not there, or this build does not carry it |
| `2` | The command line could not be read at all: an unknown command, an option that command does not have, a missing value, or a value out of range |

## Examples

```sh
# Region recording after a 3-second countdown
cosmic-capture-kit record --region --timer 3

# Record a monitor with system audio only, whatever the saved arms say
# (this launch only; the saved setting is untouched)
cosmic-capture-kit record --monitor --sound system

# Record a window with no audio at all
cosmic-capture-kit record --window --sound none

# Jump straight to picking a monitor to screenshot
cosmic-capture-kit capture --monitor

# Screenshot the active window and deliver it without the editor
cosmic-capture-kit capture --active-window --skip-editor

# Pick a color off the screen
cosmic-capture-kit color --picker

# Scan whatever's on screen for a QR code / text
cosmic-capture-kit scan

# Re-open the last capture in the editor, as a fullscreen overlay
cosmic-capture-kit editor ~/Capture/latest.png --overlay

# Mute the microphone on the recording that is already running (Linux)
cosmic-capture-kit record --sound system

# Finish that recording and save it
cosmic-capture-kit record --action save

# Make every capture wait 3 seconds from now on
cosmic-capture-kit timer 3
```

## Binding to keys

Point separate desktop shortcuts at different commands to get one-press capture
flows (region screenshot on one key, window recording on another, and so on). See
the "Shortcuts" section of the
[README](https://github.com/Frosthaven/cosmic-capture-kit#readme) for the
per-desktop setup.
