# Command-line flags

`cosmic-capture-kit` is a one-shot tool: it opens the capture overlay, and exits
after each capture. The flags below let a keybinding (or a script) launch straight
into a specific capture flow instead of the default region screenshot. Run
`cosmic-capture-kit --help` to print this list from the binary.

```
cosmic-capture-kit [FLAGS]
```

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
| `--countdown <secs>` | Pre-capture countdown, in seconds — any value works (e.g. `7`), not just the UI presets |

Mode and kind flags combine — e.g. `--monitor --video` records a monitor. `--scan`
always uses region mode (its capture invariant), so a mode flag alongside it is
ignored. When several mode (or several kind) flags are passed, the most specific
wins in this order: monitor > window > region, and scan > video > image.

## Other flags

| Flag | Effect |
|---|---|
| `--preview <file>` | Open an existing image or video in the preview editor (a viewer; no capture). Opens in a resizable **window** by default |
| `--overlay` | With `--preview`: use the fullscreen overlay instead of a window |
| `--inspect <file>` | Print a capture's embedded metadata and exit |
| `--make-sync-clip [path]` | Write the A/V-sync reference clip (black with four flash + beep events) and exit. Default: `cck-sync-reference.mp4` in the recordings folder |
| `--calibrate-sync <file> [--apply]` | Verify end-to-end A/V sync from a recording of the reference clip and print the measured offset (positive = audio leads video); `--apply` stores a manual override. Recordings already compensate for device latency automatically, so this is normally just a check |
| `--settings` | Open the settings window only (no capture overlay) |
| `--permissions` | **macOS only** — open the permission-checker window (Screen Recording / Microphone / Notifications) with live status and Request / Open System Settings / Relaunch actions. On other platforms the flag is inert (there are no TCC grants) and falls through to a normal launch |
| `-h`, `--help` | Show the usage summary |

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

## Examples

```sh
# Region recording after a 3-second countdown
cosmic-capture-kit --region --video --countdown 3

# Jump straight to picking a monitor to screenshot
cosmic-capture-kit --monitor

# Scan whatever's on screen for a QR code / text
cosmic-capture-kit --scan

# Re-open the last capture in the preview overlay
cosmic-capture-kit --preview ~/Capture/latest.png
```

## Binding to keys

Point separate desktop shortcuts at different flag sets to get one-press capture
flows (region screenshot on one key, window recording on another, and so on). See
the "Launching with a keyboard shortcut" section of the [README](README.md) for the
per-desktop setup.
