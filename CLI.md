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

Mode and kind flags combine — e.g. `--monitor --video` records a monitor. `--scan`
always uses region mode (its capture invariant), so a mode flag alongside it is
ignored. When several mode (or several kind) flags are passed, the most specific
wins in this order: monitor > window > region, and scan > video > image.

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

# Upload a capture to a connected cloud account, and copy a share link for it
cosmic-capture-kit --cloud-upload ~/Capture/latest.png \
  --account 0123456789abcdef --auto-share
```

## Binding to keys

Point separate desktop shortcuts at different flag sets to get one-press capture
flows (region screenshot on one key, window recording on another, and so on). See
the "Launching with a keyboard shortcut" section of the
[README](https://github.com/Frosthaven/cosmic-capture-kit#readme) for the
per-desktop setup.
