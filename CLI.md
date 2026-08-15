# Command-line flags

`cosmic-capture-kit` is a one-shot tool: it opens the capture overlay, and exits
after each capture. The flags below let a keybinding (or a script) launch straight
into a specific capture flow instead of the default region screenshot. Run
`cosmic-capture-kit --help` to print this list from the binary.

```
cosmic-capture-kit [FLAGS]
```

`cosmic-capture-kit` here means whichever file you installed. The Linux AppImage
takes exactly the same flags, so substitute its filename
(`CosmicCaptureKit-x86_64.AppImage --region`) and everything below applies
unchanged. The AppImage runtime reserves its own `--appimage-*` flags, which
collide with none of ours.

## Launch flags

These open the capture overlay. With no flags it opens in region-select mode for a
screenshot — identical to a bare launch.

| Flag | Effect |
|---|---|
| `--region` | Start in region-select mode (default) |
| `--window` | Start in window-select mode |
| `--monitor` | Start in monitor-select mode |
| `--image` | Capture a screenshot (default) |
| `--video` | Capture a screen recording |
| `--scan` | Start the QR / OCR scanner (forces region mode) |
| `--all-in-one` | Open the full capture picker overlay |
| `--active-window` | Capture the active window immediately, no picker |
| `--active-monitor` | Capture the monitor under the cursor immediately, no picker |
| `--no-editor` | With any launch flag: skip the preview editor — save, copy and notify instead |
| `--countdown <secs>` | Pre-capture countdown, in seconds — any value works (e.g. `7`), not just the UI presets |
| `--audio <channels>` | Arm exactly these audio channels for this launch only: `both`, `mic`, `system` or `none`. The saved setting is untouched |

Mode and kind flags combine — e.g. `--monitor --video` records a monitor. `--scan`
always uses region mode (its capture invariant), so a mode flag alongside it is
ignored. When several mode (or several kind) flags are passed, the most specific
wins in this order: monitor > window > region, and scan > video > image.

A mode flag plus `--video` is also exactly what the tray's direct recording
entries run: Record Region spawns `--region --video`, Record Window spawns
`--window --video`, and Record Monitor spawns `--monitor --video`. There is no
separate record flag; the pair above is the whole spelling, so a keybinding can
do precisely what the tray entries do.

`--audio` is a modifier like `--no-editor`: it combines with any launch above.
It arms the recording's audio channels for this launch only, overriding the
saved defaults without writing them back: the next launch without the flag uses
the saved arms again. Accepted values (case does not matter): `both` (also
`all` or `mic+system`), `mic` (or `microphone`), `system` (or `sys`), and `none`
(or `off`). Anything else rejects the launch with an error instead of guessing,
because a typo must never record a microphone you asked to silence. The flag
matters for `--video` launches; a screenshot reads no audio arms.

`--active-window` and `--active-monitor` ask the compositor which target is active,
through protocols a Flatpak sandbox hides. Under Flatpak the two flags are not
available: the launch records the failure and exits instead of guessing a target.

`--no-editor` is a modifier rather than a mode, so it combines with every launch flag
above: `--region --no-editor`, `--active-window --no-editor`, and so on. The capture is
still saved to the capture folder, copied to the clipboard and announced by a
notification — exactly what happens today when no editor can be opened. Only the editor
is skipped. It is named for the editor, not the preview, because `--preview <file>`
already means "open this file in the viewer".

The notification is the whole feedback for such a capture, so it names what it delivered
— "Region copied to clipboard", "Window saved", "Monitor saved" — and clicking it opens
the capture's folder with the file selected. It says "saved" rather than "copied"
whenever the clipboard write did not happen, and the body then gives the reason. (A
still image over 1 GB is not copied automatically; a recording is copied at any size,
because it goes on the clipboard as a file reference rather than as pixels.)

## Other flags

| Flag | Effect |
|---|---|
| `--preview <file>` | Open an existing image or video in the preview editor (a viewer; no capture). Opens in a resizable **window** by default |
| `--overlay` | With `--preview`: use the fullscreen overlay instead of a window |
| `--inspect <file>` | Print a capture's embedded metadata and exit |
| `--make-sync-clip [path]` | Write the A/V-sync reference clip (black with four flash + beep events) and exit. Default: `cck-sync-reference.mp4` in the recordings folder |
| `--calibrate-sync <file> [--apply]` | Verify end-to-end A/V sync from a recording of the reference clip and print the measured offset (positive = audio leads video); `--apply` stores a manual override. Recordings already compensate for device latency automatically, so this is normally just a check |
| `--settings` | Open the settings window only (no capture overlay) |
| `--cloud-upload <path>` | Upload a capture to a connected cloud account. Needs `--account`. Runs as a detached helper (no window): the transfer happens in the background and a desktop notification reports how it went |
| `--account <id>` | With `--cloud-upload`: which connected account to use. The id is the one in `cloud_accounts.toml`, minted when the account was connected in Settings |
| `--auto-share` | With `--cloud-upload`: also ask the provider for a share link and copy it. Ignored for a provider that cannot make one |
| `--color-picker` | Open the color picker: the screen dims, a magnifier follows the pointer, and a click copies the color's hex and opens a window with its HEX / RGB / HSL / HSV / OKLCH / CMYK / LAB values |
| `--palette-viewer` | Open the palette viewer: the same window the color picker opens, on its own, with no dim, no magnifier and no pick. It loads your most recent color (white if you have never picked one) so you can read and reuse the palette you already have. If a picker window is already open anywhere, that one comes forward instead |
| `--permissions` | **macOS only** — open the permission-checker window (Screen Recording / Microphone / Notifications) with live status and Request / Open System Settings / Relaunch actions. On other platforms the flag is inert (there are no TCC grants) and falls through to a normal launch |
| `-h`, `--help` | Show the usage summary |

## The color picker

`--color-picker` is its own tool rather than a capture. It writes no file and it
takes no screenshot. The tray menu's **Colors > Color Picker** entry, the preview
editor's pipette button and the Color Picker global shortcut all run exactly this
flag.

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

A click copies the hex and opens a small window: a swatch, then one editable row
per notation (HEX, RGB, HSL, HSV, OKLCH, CMYK, LAB) each with its own copy button,
then the last twenty colors you picked. Typing in any row updates the swatch and every
other row. The recents are persisted, because the app is one-shot and an in-memory
list would be empty at every launch; clicking one loads it without reordering the
list, and only an actual pick writes to it. Picking a color already in the row
moves it back to the front rather than adding a second copy.

**Only one picker window is ever open.** Pick again, from that window's own
pipette or from anywhere else, and the window you already have takes the new
color and puts it at the head of its recents. A second window never appears.

## The palette viewer

`--palette-viewer` opens that same window on its own, so you can get at the
colors you already saved without picking a new one first. There is no dim, no
magnifier and no click to make: the window opens straight away, loaded with your
most recent color, or white if you have never picked one. The tray menu's
**Colors > Palette Viewer** entry runs exactly this flag.

Everything the window does is the same as after a pick: every notation, every
copy button, the editable rows and the recents row. Opening it does not change
your palette and does not touch the clipboard, so looking is free. Its window
title is `CCK Palette Viewer`, which is the name tiling window manager rules
should match on (see the README).

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

## A/V sync check (and manual override)

Recordings compensate for the audio **device's** output latency automatically: a
per-recording probe reads the sink monitor's signed latency (via the libpulse
async client API — the value ffmpeg's own pulse input clamps to zero) and folds it
into the system channel at finalize (auto mode only; nothing is persisted). Combined
with the in-app auto-calibration of the compositor's frame-delivery lag, a fresh
recording of a lip-synced source lands in sync with no calibration step.

The tools below are therefore a **verification** pass — and a manual override for
exotic setups where a stubborn residual remains:

1. `cosmic-capture-kit --make-sync-clip` — writes the reference clip (path printed).
2. Play the clip in any video player, with system audio audible.
3. Record it with a normal capture (region around the player; system audio ON).
4. `cosmic-capture-kit --calibrate-sync <recording.mp4>` — measures the flash-vs-beep
   offset and prints it. It should read ≈0 for a recording made with auto sync on.
   Add `--apply` to store a manual override; without it, nothing is written.

Because the compensation is now live per recording, measuring an OLD recording and
applying its offset is usually unnecessary. When you do `--apply`, the per-recording
auto-calibration keeps tracking the frame-delivery lag and adds the stored base on
top, so the override survives device and load changes.

## Controlling a recording that is already running

These do not launch anything. Each one reaches the recording in progress and acts on
it, then exits. They are **Linux only**.

| Flag | Effect |
|---|---|
| `--pause-recording` | Pause the recording, or resume it when paused |
| `--finish-recording` | Finish the recording and save it |
| `--cancel-recording` | Cancel the recording and delete it |
| `--toggle-mic` | Toggle the microphone |
| `--toggle-system-audio` | Toggle system audio |

They exist because on Linux an in-app keyboard shortcut cannot reach a recording. Once
recording starts, the app hands the keyboard back to the window you are recording, so you
can keep typing into it, and COSMIC's desktop portal has no way to give an app a shortcut
that works without focus. A shortcut bound in your own desktop settings has no such
problem, which is the same reason the capture key is bound that way. Point one at each
command and you get focus-free recording controls. See the "Shortcuts" section of the
[README](https://github.com/Frosthaven/cosmic-capture-kit#readme).

They act on whichever recording is running, whether it was started from the overlay, the
tray or a keybinding. With no recording in progress the command prints
`no recording is in progress` and exits with status 1, so a script can tell.

On macOS and Windows these flags are inert: they print a line saying so and exit. Those
platforms keep their capture windows through a recording, so the in-app shortcuts still
work there, and their menu-bar / tray item carries the same five controls.

## Examples

```sh
# Region recording after a 3-second countdown
cosmic-capture-kit --region --video --countdown 3

# Record a monitor with system audio only, whatever the saved arms say
# (this launch only; the saved setting is untouched)
cosmic-capture-kit --monitor --video --audio system

# Record a window with no audio at all
cosmic-capture-kit --window --video --audio none

# Jump straight to picking a monitor to screenshot
cosmic-capture-kit --monitor

# Pick a color off the screen
cosmic-capture-kit --color-picker

# Scan whatever's on screen for a QR code / text
cosmic-capture-kit --scan

# Re-open the last capture in the preview overlay
cosmic-capture-kit --preview ~/Capture/latest.png

# Upload a capture to a connected cloud account, and copy a share link for it
cosmic-capture-kit --cloud-upload ~/Capture/latest.png \
  --account 0123456789abcdef --auto-share

# Mute the microphone on the recording that is already running (Linux)
cosmic-capture-kit --toggle-mic

# Finish that recording and save it
cosmic-capture-kit --finish-recording
```

## Binding to keys

Point separate desktop shortcuts at different flag sets to get one-press capture
flows (region screenshot on one key, window recording on another, and so on). See
the "Launching with a keyboard shortcut" section of the
[README](https://github.com/Frosthaven/cosmic-capture-kit#readme) for the
per-desktop setup.
