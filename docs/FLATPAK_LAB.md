# `lab/flatpak` handoff

An experimental Flatpak, built to MEASURE what a sandbox costs rather than predict it. It
builds, installs, and runs. Recording, OCR, the preview editor, cloud, tray and clipboard all
work inside it. **Capture is now BUILT too** (2026-08-07): region selection through the
portal-frozen fallback overlay, monitor and window stills through the portal's own picker,
and the `--active-*` flags disabled honestly where "active" cannot be resolved. The owner's
first live test confirmed region capture works; the fix round that followed landed the mode
routing (window/monitor launches honor their mode), the every-kind frozen backdrop, the
per-source-type restore token, and the ffmpeg 7.x recording deadlock fix (items 7 and 8 in
the fixes list below). The second live test surfaced the finished recording's DELIVERY:
"Finish & Save" from the daemon tray reached the child fine (the socket relay, pause and
stop all worked), but the fallback path tears the selection window and `self.outputs` down
at record START, so the stop-time editor anchor resolved to nothing, the preview spinner
refused to open, and the file was saved with no editor ever appearing. Fixed by
snapshotting the anchor at record start and letting the stop prefer a fresh answer
(`snapshot_preview_anchor` / `stop_preview_anchor` in `app/recording.rs`, protocol-keyed
on `overlay_fallback_active`, native path byte-identical). A third round then landed
eight more fixes in one sweep: the preview anchored to the captured display (the old
fallback was the FIRST registered output, an 800x480 panel here), the work-area height
budget, the unified copy decision reaching auto-copy and both cloud-link sites, the
overlay toggle hidden where the overlay cannot exist, the letterboxed backdrop with
selection confined to visible pixels, the slimmed tray menu ("Start Capture") with the
MPRIS gate, portal-routed folder/file opens, and the opening-prime fix for the frozen
first seconds of recordings (items 8 to 11 below). The tray SLIMMING was then REVERTED
by DRAGON-558 (the MPRIS gate stays): the owner moved the audio pre-arm into the tray
itself, an "Audio Recording: <state>" radio submenu on every tray/menu-bar surface,
directly above Quit — its title carries the current arm state, one radio pick (Both /
Microphone only / System only / None) sets the complete pair, persisted while idle and
applied live (as the toggle diff) while recording. With the pre-arm reachable from the
tray, the portal-picker launchers no longer skip anything the user cannot set, so all
three capture entries ("Capture Region" under its own name again, Window, Monitor) show
on every session, and the in-recording menus slimmed instead (Pause/Resume, Finish &
Save, Cancel & Delete). Awaiting the next live test; GUI behaviour is
headless-unverifiable here, so the owner's session stays the gate.

Read this before resuming. It is the whole context.

---

## Run it

```sh
just flatpak     # build, stop every running instance, relaunch the resident AS the Flatpak
just appimage    # put the normal AppImage daemon back
```

`just flatpak` needs `flatpak-builder` plus `org.freedesktop.Platform//25.08`,
`org.freedesktop.Sdk//25.08`, `Sdk.Extension.rust-stable//25.08` and `Sdk.Extension.llvm21//25.08`
(the recipe installs the two extensions if missing). Build state lives in `~/.cache/cck-flatpak`.

Switching between the two is safe in both directions (DRAGON-590). Each recipe
runs `scripts/stop-all.sh` first, which stops EVERY instance of the app whatever
build it came from, so you cannot end up with an AppImage tray icon and a Flatpak
tray icon at once. That used to happen: the recipes matched on command lines, and
a Flatpak instance's argv on the host is a bare `cosmic-capture-kit resident`
with no path, so every pattern missed it.

Each recipe also repoints `target/artifacts/cosmic-capture-kit`, the stable path
your capture shortcuts use, so the shortcut follows whichever build you last
made. For the Flatpak that symlink points OUT of the repo, at the launcher
flatpak itself exports:

```
~/.local/share/flatpak/exports/bin/dev.thedragon.CosmicCaptureKit
```

which is a two-line script doing `exec flatpak run … "$@"`, so flags pass
through unchanged. The recipe derives that path from the app id and from the
installation it used (`--user` here, `/var/lib/flatpak` for a system one) rather
than hardcoding it, because the exported script is pinned to the branch and arch
that were installed. A missing export fails the recipe instead of leaving a
dangling symlink.

Note that the Settings window's Global shortcuts tab shows a different command
for a Flatpak build, `flatpak run dev.thedragon.CosmicCaptureKit --flag`, and
that is correct too: it is what a normal user with one install needs, where the
symlink is a developer convenience for a machine carrying three artifacts.

Manifest: `scripts/flatpak/dev.thedragon.CosmicCaptureKit.yml`.

Two gotchas that cost real time, both already handled but worth knowing:

* Build dirs must NOT be under `/var/tmp`. flatpak-builder's bwrap sandbox cannot `chdir` there
  and fails as a misleading "No such file or directory" on the first module.
* The source dir is copied wholesale, so `target/` (100G+) is skipped in the manifest. Without
  that the copy alone fills the disk.

---

## Which build is actually installed? (DRAGON-605)

**`just flatpak` exiting zero is not evidence that the thing now installed is the thing
you just wrote.** Two different failures both look exactly like a clean build, and both
have really happened on this branch:

* a **stale tree**, where the module cache is reused and the app is never recompiled, so
  the install succeeds and carries the previous source, and
* a **clobber**, where your build installs fine and something else installs over it
  minutes later. On a machine with several people or agents building, this is the common
  one.

Two cheap checks answer two different questions, and you want both. The first says
whether the installed binary is YOURS. The second says whose it is instead, which turns a
guess into a named cause.

**1. String markers: is this mine?** Pick a literal your change introduces, and a control
literal that exists in the base too, then read them out of the installed binary:

```sh
BIN=~/.local/share/flatpak/app/dev.thedragon.CosmicCaptureKit/x86_64/master/active/files/bin/cosmic-capture-kit
strings -a "$BIN" | grep -c "a literal only my change adds"
strings -a "$BIN" | grep -c "a literal the base already had"
```

The control is not optional. Without it a mistyped pattern reads as "my change is
missing" and you go hunting a bug that is not there. Markers absent while controls are
present is the real signal.

A log line makes a good marker for free: it is a plain literal, it survives into the
binary, and the same string then proves the code RAN when you find it in
`~/.var/app/dev.thedragon.CosmicCaptureKit/.local/state/cosmic-capture-kit/logs/debug.log`.

**2. The ostree commit chain: and whose is it?**

```sh
flatpak info --user dev.thedragon.CosmicCaptureKit | grep -E "Commit|Parent|Date"
```

`just flatpak` prints the commit it exports (`Commit: <hash>` near the end of the
flatpak-builder output). If the INSTALLED commit is not that hash, you are not running
your build. If the installed commit's **Parent** is your hash, someone installed directly
on top of you, and the `Date` says when. That is the difference between "my build is
stale" and "my build was replaced", which need opposite fixes: rebuild versus rebuild
*and* re-verify immediately before observing anything.

The same rule in one sentence: **install last, verify markers, then observe, with nothing
in between.** Any gap is a window for someone else's install.

---

## THE blocking item: the capture overlay

**Nothing about capture can be tested until this is done.** The daemon's capture entries DO
spawn their children correctly; the child then cannot draw and exits, which is why they look
like they do nothing. It says so in the debug log:

```
WARN  cck::failure: [overlay-never-shown] no wlr-layer-shell: sandboxed ... or a compositor
                    that never implemented it
ERROR app::shell: overlay: zwlr_layer_shell_v1 is not available to this process
```

### Why

cosmic-comp hides `zwlr_layer_shell_v1` from any client carrying a `wp_security_context_v1`,
which is every Flatpak. Measured on a live session: **22 of 57 globals are hidden**. We have a
portal fallback for PIXELS and none for the OVERLAY.

Note this is not COSMIC being unusual in the direction that matters here: **mutter has never
implemented `wlr-layer-shell` for anyone**, so an xdg-shell overlay is what any GNOME support
would need too. This work is not Flatpak-only.

### The owner's design, which is the right one

1. Capture the full monitor first (portal ScreenCast, already working).
2. Draw the region selection OVER that captured frame.

So the overlay becomes an ordinary fullscreen toplevel painting a still image, and "freeze"
stops being a capability we lack and becomes the only mode there is. The cost is one portal
prompt BEFORE selection rather than after.

### What exists to build on

* `window::Settings` has a `fullscreen: bool` field, so a fullscreen xdg-shell toplevel is
  available with no new plumbing.
* `platform::layer_overlay_available()` is the runtime seam, already written and already
  consulted by `app::shell::overlay_surface_with`. It is protocol-keyed, so it is true on a
  normal COSMIC session and false in a sandbox or on GNOME.
* The mac/Windows **PlainWindows** path is a working template for "capture overlays as ordinary
  windows": `app::shell::overlay_window` plus `App::seed_outputs_mac` in `app/surfaces.rs`.
* Sizing math is already portable: `app/preview/sizing.rs`, `geometry::OverlayUnits`.
* Portal capture is proven in the sandbox: `--test pw` was granted a stream and pulled
  5120x1440 frames at 21ms with `restore_token=true`, so repeat captures of the same monitor do
  not re-prompt.

### The hard part, stated honestly

Seeding is PER-OUTPUT (`on_output` allocates a `window::Id`, then
`shell::overlay_surface(output, id, …)` anchors a layer surface to that output). Wayland has no
client-side window placement, so a sandboxed build gets ONE fullscreen window on the
compositor's choice of monitor, not one per output.

That is a restructure of the seeding, the view and the message flow, not a surface-type swap.
It is why this was not attempted in-session: GUI behaviour is headless-unverifiable here, so it
needs the owner driving it.

One consolation: the portal picker already makes the user choose a monitor, so a single-window
model is not as wrong as it first sounds. The mismatch to watch for is the window landing on a
different monitor than the one captured. That mismatch is now handled by LETTERBOXING: the
frozen frame keeps its own aspect, centred over black bars, and selection is confined to the
visible pixels (`geometry::OverlayUnits::letterbox`; the round-1 per-axis stretch distorted
the still and is gone). See ARCHITECTURE.md's "The monitor mismatch".

---

## Ruled out, with the reason (do not re-litigate)

**"Let the tray daemon hold the clipboard for editor-less copies."** It cannot, and the reason
generalises: `wl_data_device.set_selection(source, serial)` needs a serial from an input event
delivered to that client. A client with no surface never receives input events, so it has no
serial and the compositor refuses. Our daemon has no Wayland connection at all, and giving it
one would not help: an unmapped surface never takes focus, and mapping a focused window to do a
background copy would steal focus from the user.

The blocker is FOCUS, not process lifetime. Any "a background process holds the clipboard"
design hits the same wall, because that is exactly what data-control exists for and exactly why
sandboxes gate it.

The variant that could work is the capture child lingering as its own selection server (it HAS
focus during an interactive capture). **The owner has declined this**: it fights the one-shot
model, where `finish_session` always exits. Do not build it without a fresh decision.

So editor-less copy (`--no-editor`, and immediate `--active-window` / `--active-monitor`) stays
unavailable in a sandbox, and reports failure honestly rather than claiming success.

---

## Verified environment facts (measured, not assumed)

From a live COSMIC session, Flatpak 1.18.0, cosmic-store 1.5.0:

| Fact | Value |
|---|---|
| Native capture caps in-sandbox | `screenshot=false record=false window_list=false window_capture=false cursor=false layer_overlay=false wallpaper=true` |
| Portal ScreenCast | WORKS: 5120x1440 @ 21ms, restore token honoured |
| Runtime provides | `ffmpeg`, `ffprobe`, `pactl`, `libpulse`, `libpipewire-0.3` 1.4.9, `libxkbcommon`, `libpng` |
| Runtime does NOT provide | `tesseract` (module), `clang` (llvm21 extension), libavcodec 61 = ffmpeg 7.1 |
| Portals present on COSMIC | ScreenCast, Screenshot, Notification, FileChooser, Settings, Access |
| Portals ABSENT on COSMIC | **Background**, **GlobalShortcuts** |
| PID inside sandbox | **2**, and every instance is 2 (own PID namespace) |
| `XDG_RUNTIME_DIR` | shared per-app across instances; `flock` works |
| `XDG_CONFIG_HOME` | the app's PRIVATE store; host config readable at `$HOME/.config` |

**Why `--no-default-features`:** the runtime's libavcodec is 61 (ffmpeg 7.1) and `zero-copy`
binds the ffmpeg 9.0 headers. Building ffmpeg 9 as a module adds ~15 min to every build for a
feature the sandbox degrades anyway. Recording still works through the runtime's ffmpeg binary,
which is the path macOS and Windows already ship.

---

## Feature status in the sandbox

**Works:** recording (all modes), preview editor, timeline editor, OCR + QR, cloud accounts and
OAuth, notifications, save to disk, audio with device pickers, tray resident, clipboard copy
(text and image), and (DRAGON-562) single-window aesthetics on portal WINDOW stills: padding,
both configured borders, drop shadow, corner rounding, and the wallpaper backdrop placed from
the grant's window position (a truly-fullscreen window keeps the bare frame, same rule as
native). Awaiting the owner's live test.

**Gone, with cause:**

| Feature | Cause |
|---|---|
| Capture overlay / region select | layer shell hidden. THE blocker |
| Window picker grid | `ext_foreign_toplevel_list_v1` hidden |
| Per-window / occluded capture | toplevel capture-source hidden |
| Transparency, glass | transparency: pixfmt forces alpha opaque, pending the DRAGON-562 alpha probe (`CCK_ALPHA_PROBE=1 --test pw window`); glass: needs the scene BEHIND the window, portal can't provide |
| Cursor sprite | no cursor session; portal offers only embedded/hidden |
| GPU zero-copy encoding | build choice (ffmpeg 7.1 runtime), not a sandbox limit |
| Editor-less copy | no window to hold a selection; see "Ruled out" |
| `--active-window` / `--active-monitor` | no compositor access to resolve the active target; the launch fails honestly (`no-outputs`) instead of guessing |
| Autostart | Background portal absent on COSMIC |
| One-click update | `/app` is read-only |
| Colour picker beyond the granted monitor | it mints the SAME overlays a capture does, and on the fallback path only the granted output is backed by a real surface. The tool WORKS, on that one screen; a native session covers all of them. Nothing picker-specific to fix, it moves when the overlay does |

---

## Fixes here that are NOT Flatpak-specific

**These are the most valuable output of the branch and should be considered for `main`
independently of whether a Flatpak ever ships.** Each was found by running the sandbox but is a
bug in the shipping product.

1. **The clipboard reported success on a copy that never happened.** `copy_to_clipboard`
   returned "the helper spawned", and the helper discarded its own error with `let _ =`. The
   editor showed "Copied to clipboard" over an empty clipboard, and copy-then-delete would
   delete a capture on that basis. Affects **any** compositor without data-control.
2. **Copy silently did nothing on GNOME**, same root cause: GNOME has never implemented
   data-control. The window fallback (`copy_text_task` / `window_payload` +
   `iced::clipboard::write_data`) makes copy work there for the first time. Keyed on the
   PROTOCOL, so a normal COSMIC/KDE/wlroots session keeps the worker path unchanged, which is
   still preferred because it OUTLIVES the process.
3. **`AppImage://` labels vanished on Linux Mint.** `appimage_dir` required `$APPDIR`, which our
   symlink `AppRun` never exports and the runtime does not always set. Now derives the mount
   from `current_exe` when absent, with the `usr/bin` shape checked and a derived root of `/`
   rejected (a distro install at `/usr/bin/cosmic-capture-kit` matches the shape otherwise).
4. **The tesseract language-data warning named no directory.** Health rows only carried a
   location while the dep was PRESENT, which is right for a binary and backwards for a folder
   the user is being told to put a file in.
5. **`cosmic-client-toolkit` panics rather than degrading.** `ToplevelInfoState::new` and
   `ScreencopyState::new` UNWRAP the globals they bind. Five construction sites now check the
   protocol probe first. Any compositor lacking those globals would have crashed the app.
6. **Shown paths leak the account name.** All path rows now collapse `$HOME` to `~`.
7. **A portal restore token replayed across source types.** cosmic's portal RESTORES the
   stored source instead of re-prompting, so a Window request replaying a Monitor token
   silently granted the whole monitor with no picker. Affects any Linux session using the
   portal backend, sandboxed or not. The token now persists with its source type
   (`pw_restore_source`) and replays only on a match; a pre-upgrade token never replays
   (one extra prompt, then self-heals).
8. **Recordings opened with seconds of frozen video** (DRAGON-554, also long-standing on
   macOS). ffmpeg 7.x will not finish opening an f32le FIFO input until it has read exactly
   16384 bytes, and the pump's render horizon delivered the first byte at ~1.5s, parking
   ffmpeg's frame loop; the audio pre-flight and video bring-up also ran serialized. Fixed
   by the pump's opening prime (the same bytes the horizon would emit, just early, bounded
   far under the horizon by a compile-time assert) plus overlapping the pre-flight with the
   capture bring-up (`AudioPreflight`). Media 0 stays the audio-capture start; the
   DRAGON-417 content E2Es pin it. The mac worker mirrors the overlap and NEEDS ON-MAC
   VERIFICATION (this tree cannot compile it).
9. **"Save and open" for scanned contacts/events was broken on every desktop** (DRAGON-556):
   `OpenURI` explicitly rejects `file://` URIs, so the open silently failed everywhere. Local
   files now go through the portal's fd-based `OpenFile`; folders through `FileManager1` when
   reachable, else portal `OpenDirectory` (opening the parent with the entry selected).
10. **The upload child claimed clipboard copies it could not make** (DRAGON-553): `shared`
   meant "a link exists", not "it was copied". Copy is now one decision (`share::CopyRoute`)
   consulted by every site: the worker path where data-control exists, the focused-window
   write elsewhere, deferred to the window's focus event (whose serial the selection needs),
   bounded, with honest toasts.
11. **The windowed preview under-asked its height by 10% on COSMIC** (DRAGON-549, second
   cause): `USABLE_H_FRAC = 0.9` was a blind haircut; cosmic-comp unconditionally clamps a
   fresh floating toplevel to the work area (`FloatingLayout::map_internal`, verified in
   source), so asking full height yields exactly the usable area. Now a decision keyed on
   `toplevel_clamped_to_work_area()`; other platforms keep 0.9.
12. **Recording deadlocked on ffmpeg older than 8.** ffmpeg 7.x blocks the mic FIFO input's
   stream analysis on real audio data before it will open the sys FIFO, while the pump
   rendered no audio until both write ends were open. Circular wait, 12s muxer-watchdog
   kill, dead recording. This is every `--no-default-features` build on a distro ffmpeg
   older than 8 (Debian / Ubuntu / Pop!_OS LTS), not just the Flatpak runtime's 7.1; the
   "works on ffmpeg 5+" claim was quietly false for the owned media-clock recorder. Fixed
   with `-probesize 32 -analyzeduration 0` on the fully-specified raw-PCM FIFO inputs plus
   the POSIX sys rendezvous riding the pump's writer thread as a pending sink (bounded, 20ms
   retry, Windows named pipes byte-identical). MEASURED across the LTS ladder (DRAGON-568,
   containers, byte-precision): 4.4/5.1/6.1 all block-until-fed and are HUNGRIER than 7.1
   unflagged (204800 B vs 16384); the flags reduce every one of them to exactly 4096 B and
   the fix clears the park in ~0.1s. "Works on ffmpeg 5+" is measured-true, 4.4 included.

---

## Architecture notes worth keeping

* **Guards are protocol-keyed, never sandbox-keyed.** A global can be missing because we are
  refused it OR because the compositor never implemented it, and every caller wants the same
  answer. This is what keeps the normal build byte-identical and makes the fixes portable.
* **`util::flatpak_sandboxed()`** detects via `/.flatpak-info` existing, NOT `$FLATPAK_ID`: we
  spawn detached children constantly and an env var is inherited and spoofable.
* **`util::host_config_dir()`** exists because `XDG_CONFIG_HOME` is the private store in a
  sandbox while the host's real config is bind-mounted under `$HOME`. Only the COSMIC DESKTOP
  readers use it; the app's own config stays private, which is correct.
* **The PID-namespace trap is real and was silent.** Two instances are both PID 2, so
  `kill(2, SIGUSR1)` signals YOURSELF and `/proc/2/exe` passes the same-program check. The
  daemon lock file now records the writer's namespace beside the pid; `addressable_pid` refuses
  a foreign one and reads the legacy bare-pid form.
* **`ksni` needs `disable_dbus_name(true)` in a sandbox.** Flatpak refuses to let a sandboxed app
  own `org.kde.StatusNotifierItem-<pid>-<n>`, and no finish-arg fixes it (the wildcard form is
  invalid syntax and Flathub never grants the exception). Without this the daemon exited on
  startup and the tray never appeared.
* **`seed_tessdata` reads `<tool dir>/tessdata`**, so language data must sit BESIDE the binary,
  not in `share/`. The whole tree, because `configs/` carries the output-mode configs the OCR
  pass names on the command line.

---

## Distribution, if it ever matters

COSMIC Store has **no AppImage backend** (`flatpak`, `packagekit`, `pkgar`, `rpm_ostree`), so the
AppImage can never be listed there. It has no policy of its own either: it is a client that
enumerates whatever remotes are configured.

Two remotes ship by default. Flathub, and **`cosmic` at `apt.pop-os.org/cosmic/`**, System76's
own, whose repo (`pop-os/cosmic-flatpak`) says it "hosts applets and other flatpaks for COSMIC
that are not suitable for upload to Flathub". No AI policy, no content policy, and
COSMIC-specific software is its purpose rather than a disqualifier.

Flathub has two clauses that bite: **environment-locked** (fixed by going portal-first) and a
**Generative AI policy** barring AI-assisted code, which closes with "Exceptions may be granted
for mature, well-maintained projects". Full analysis and verbatim quotes are on DRAGON-543.

---

## State

Branch `lab/flatpak`. Gate green throughout: clippy zero on both feature
configs, 2757 default / 2738 no-default unit tests (counts at the DRAGON-582
close, 2026-08-08; they shift with in-flight work), Windows cross-check clean.

MERGED to `main` 2026-08-08 by owner decision after seven live-test rounds:
the product fixes and the Flatpak scaffolding (manifest, `just` recipe,
sandbox seams) ship together; the fallback path is caps-keyed and inert on
native sessions. `lab/flatpak` and `main` have stayed in lockstep since, so
the branch is now just where this work continues to land, not a divergence.

### What the sandbox taught the rest of the app

Three fixes started as Flatpak bugs and turned out to be everyone's, which is
the pattern to expect from this lab:

- **The portal replay policy** (DRAGON-570, then 580). "Reuse the saved grant"
  is right for a re-request that CONTINUES a target the session already has,
  and wrong for any action where the USER is choosing a target. The launch half
  was fixed first; the toolbar's Monitor / Window buttons kept silently reusing
  the saved grant until DRAGON-580 gave them their own origin.
- **The preview height budget** (DRAGON-549, then 579). Asking cosmic-comp for
  the full output height and trusting its map-time clamp lost the bet wherever
  the README-recommended floating exception routes our windows around that
  clamp, which is every properly configured machine, not just the sandbox.
- **The tray as primary launcher** (DRAGON-574, then 584). Once the tray became
  the way in, a Linux default of "no tray" left a sandboxed user with no
  obvious entry point at all, since their PrintScreen shortcut points at a dev
  binary path the sandbox does not have.

One inversion used to be recorded here as the only place the sandbox was the
BETTER session, and DRAGON-597 removed it. The colour picker (DRAGON-587) hides
the pointer sprite wherever the overlay is a plain toplevel, and could NOT where
it is a layer surface, so a sandboxed pick hid the pointer while a native COSMIC
pick did not. Our iced fork implements `set_cursor_visible` for layer surfaces
(the iced `[patch]` block in `Cargo.toml`), so both sessions hide it now.
`platform::overlay_hides_pointer` is still keyed on the SURFACE KIND rather than
on the OS or a sandbox probe, which is why closing the gap was a one-line change
here and not a Flatpak special case.

### Recording control without a focus-free hotkey (DRAGON-583)

COSMIC ships no `org.freedesktop.portal.GlobalShortcuts` interface (verified by
introspection, empty), so the DRAGON-109 portal binding always dies, and during
a recording no surface of ours holds the keyboard: record start hands focus back
to the recorded window, and the fallback path destroys its toplevel outright.
The in-app recording chords therefore CANNOT fire on Linux, and never could.

The answer is the CLI, bound as a desktop shortcut like the capture hotkey:
`--pause-recording`, `--finish-recording`, `--cancel-recording`, `--toggle-mic`,
`--toggle-system-audio`. They reach the live recording through the EXISTING
relay, extended by one address: the recording child now also listens on its own
per-pid socket beside the marker `instance.rs` already writes, the shape
`preview_ipc` established. Same `Command` words, same `TrayEvent` drain, so a
hotkey, a resident menu click and a tray click are one code path. The inlet is
deliberately independent of the tray, because a sandboxed child often fails to
register a tray item, and that is exactly the session where these commands are
the only control left.
