# Developing Cosmic Capture Kit

Everything day to day runs through [`just`](https://github.com/casey/just).
`just` on its own lists the recipes; this page says when to reach for each and
what it will do to your machine.

For what the app depends on, at runtime and at build time, see
[dependencies.md](dependencies.md); for the flag list, [CLI.md](CLI.md).

## The short version

```sh
just dev        # the loop you will live in
```

Build, stop every running instance, restart the resident daemon on the fresh
binary, print the path to use. Bind that path to a shortcut once and it survives
every rebuild, and every SWITCH between artifacts, because it is one stable path
that every build recipe repoints at whatever it just made.

There is no application window to open. This is a one-shot tool: a bare launch
goes straight into a region capture and then exits, and `--settings` opens the
settings window instead of capturing. [CLI.md](CLI.md) lists every flag.

## Before the first build

A recent stable Rust (the crate is edition 2024, so 1.85 or newer),
[`just`](https://github.com/casey/just), and the development packages for the
three system libraries the binary LINKS. A desktop machine already has the
runtime libraries, and usually not the headers, which is what makes a first
build fail.

```sh
# Arch / CachyOS
sudo pacman -S base-devel clang libxkbcommon libpulse libpipewire

# Debian / Ubuntu / Linux Mint / Pop!_OS
sudo apt install build-essential pkg-config libclang-dev \
                 libxkbcommon-dev libpulse-dev libpipewire-0.3-dev
```

libpulse is required by **every** Linux build, including one that will never
record: `src/audio/pulse_ffi.rs` declares `#[link(name = "pulse")]`, so it is a
real link dependency and not something loaded on demand. Without it the build
dies at `unable to find library -lpulse`.

[dependencies.md](dependencies.md) section 7 has the per-package reasoning and
two package-name traps worth knowing before you guess (on Arch the PipeWire
headers are in `libpipewire`; the `pipewire` package is the daemon and ships
none). The runtime extras are separate: `ffmpeg` to record, `tesseract` to OCR.

**The default `zero-copy` feature needs ffmpeg 8 headers**, and it is
Linux-DRM-only. Rolling distros have them. Ubuntu 24.04, and so Mint 22, ships
ffmpeg 6.1.1, where the build stops inside `ffmpeg-sys-next`. `just build`
notices that and retries with `--no-default-features`; a raw cargo command does
not, and the flag is then needed on **every** invocation (`build`, `test`,
`run`, `install`), not just the first. macOS and Windows always build
`--no-default-features`. Nothing is lost but the in-process GPU encode path:
recording still works through the `ffmpeg` binary, on ffmpeg 5 and newer.

## One output directory, one stable path

Every recipe leaves its finished artifact in the same place, `target/artifacts/`,
the same relative path on Linux, macOS and Windows:

```
target/artifacts/
    cosmic-capture-kit                 the STABLE PATH: a symlink to the most recent build
    cosmic-capture-kit-source          -> target/release/cosmic-capture-kit   (just build, just dev)
    cosmic-capture-kit-dist            the shipping binary                    (just dist)
    CosmicCaptureKit-x86_64.AppImage   the AppImage                           (just appimage)
    cosmic-capture-kit-flatpak         -> flatpak's exported launcher         (just flatpak)
```

`ls -lt target/artifacts/` therefore reads as a build history, newest first.

**Use the stable path for shortcuts and for CLI commands.** Every build recipe
prints it in cyan as its last line, absolute and ready to paste:

```
==> Use this path for shortcuts and CLI commands:
       /home/you/src/cosmic-capture-kit/target/artifacts/cosmic-capture-kit
       e.g.  /home/you/src/cosmic-capture-kit/target/artifacts/cosmic-capture-kit --region
```

Flags pass straight through whichever artifact is behind it. The plain binary and
the AppImage take them directly, and the Flatpak's exported launcher forwards
`"$@"`.

This exists because the alternative bit: with one shortcut on the AppImage and
another on `target/release`, a fix rebuilt into one and not the other looks
exactly like a live code bug, and the only thing that gives it away is the
`package:` field in the debug log.

Two per-platform differences, both because of what the OS can actually launch:

- **Windows** gets `cosmic-capture-kit.cmd`, a shim, not a symlink. A real
  symlink there needs Developer Mode or an elevated shell, so it would exist on
  some machines and not others, and a stable path that is sometimes absent is
  worse than none.
- **macOS** gets `cosmic-capture-kit` as a two-line launcher that `exec`s the
  bundle's real executable, plus a `Cosmic Capture Kit.app` symlink beside it for
  `open`, `open -a` and Finder. Symlinking straight into the bundle is avoided on
  purpose: the process would be exec'd by the symlink's path, and if that costs
  the enclosing bundle it costs the Info.plist, the code-signature identity and
  the TCC screen-recording grant with it.

If you install the app normally there is no symlink and none of this applies. The
Settings window's Global shortcuts tab shows the command for the package you are
actually running, which for a Flatpak reads
`flatpak run dev.thedragon.CosmicCaptureKit --flag`. Both are correct; the
symlink is a developer convenience for a machine carrying three artifacts at
once, and the settings command is what a normal install needs.

## The recipes

| Recipe | What it does | Needs |
|---|---|---|
| `just dev` | Build, stop every instance, restart the daemon | |
| `just build` | Build this platform's own artifact | |
| `just appimage` | Build the AppImage, restart the daemon on it | docker |
| `just dist` | Build the shipping Linux binary in the release container | docker |
| `just flatpak` | EXPERIMENTAL: build the Flatpak, restart the resident as it | flatpak-builder |
| `just docs` | Serve the documentation site with live reload | python3 |

### `just dev`

The inner loop. It stops **every** process of this app, not only the daemon and
not only its own kind of build: a preview editor or settings window left over
from the previous build keeps running against a deleted binary, serves stale
code, and its re-exec spawns fail silently. Then it relaunches the daemon and
prints the stable path.

Run it after any change you want to try through your capture shortcut.

### Stopping everything: `scripts/stop-all.sh`

Every recipe that restarts a daemon calls this one script first. It is the only
place that logic lives, and it must stay that way. The recipes each used to carry
their own patterns and each reached only their own kind of build, so building the
AppImage after building the Flatpak left you with two tray icons and two daemons
spawning capture children from two different builds.

On Linux it identifies instances by `/proc/<pid>/exe` rather than by command
line, because a Flatpak instance's argv on the host is a bare
`cosmic-capture-kit resident` with no path in it, and every path-anchored pattern
missed it. The exe path always ends in the binary's name, whether it is
`target/release/…`, a `/tmp/.mount_…/usr/bin/…` AppImage mount or `/app/bin/…`
inside the sandbox, and matching it cannot accidentally hit an editor with the
source open, which a name pattern always can (the checkout directory is itself
called `cosmic-capture-kit`). It sends SIGTERM, waits for the processes to
actually be gone, and only then escalates. SIGTERM matters: the Linux resident
handles it and tears its ksni tray item down first, where a SIGKILL leaves a
ghost icon behind until the tray host times it out.

They can coexist in the first place because the single-instance locks do not see
across the sandbox boundary. The daemon lock is a flock under `$XDG_RUNTIME_DIR`
and a Flatpak has its own, so neither daemon can tell the other exists. That is a
real gap, but it only bites a machine carrying more than one artifact, which is a
developer situation. The sweep is not redundant with the locks; the locks cannot
do its job.

### `just build`

Whatever this machine can build, and it means something different per platform,
matching what that platform actually ships:

- **Linux**: `cargo build --release`. Retries with `--no-default-features` if the
  first attempt fails, which on an older distro means no ffmpeg 8 headers for the
  zero-copy encoder.
- **macOS**: fetches the pinned sidecars, then builds and bundles the `.app`,
  signed if a Developer ID identity is in your login keychain. Never notarizes;
  that needs Apple credentials and is release-only.
- **Windows**: fetches the pinned sidecars, then builds the MSI.

Every arm also repoints the stable path, and bakes the cloud client ids in from
the repository's GitHub variables when `gh` is authenticated and allowed to read
them. That last part is the maintainer's machine, not a fresh clone; see the
cloud note under "Things that will bite you".

It does NOT produce the shipping Linux binary. See `just dist` for why.

### `just appimage`

The portable Linux artifact, built in a Rocky 9 container and bundling its own
ffmpeg, libav and tesseract. Afterwards it stops every instance and brings the
daemon up **on the AppImage**, so your capture shortcut exercises the real
artifact rather than a dev build.

Prints the produced path, its size, and its measured glibc floor. Watch that
floor: it is what decides which distros can run the release at all, and a
regression in the base image is invisible any other way.

First run pays 10-15 minutes building the container image. After that Docker's
layer cache makes it minutes.

**Architectures.** The release ships x86_64 and aarch64 AppImages, and nothing
cross-compiles: each is built by running the same container on that
architecture. On Linux you get your own machine's. On macOS the recipe takes an
argument:

```sh
just appimage            # x86_64, EMULATED on Apple Silicon (enable Rosetta for
                         # x86_64 containers first, or it goes from slow to unusable)
just appimage aarch64    # NATIVE on Apple Silicon, so full speed
```

That second one is the practical way to produce the ARM artifact, since an
Apple Silicon Mac is the fastest aarch64 Linux builder most of us have. An
OrbStack Arch ARM VM can then actually run what it produced. The two use
separate image tags, so switching between them does not force a rebuild.

### `just dist`

The shipping Linux binary, in the same container CI uses. Like the other Linux
build recipes it then stops every instance and brings the daemon up on what it
just built, so the stable path never points at something that is not running.

Deliberately separate from `just build`. That recipe is the source build the
README documents and the one `just dev` calls, so requiring docker there would
break building from source and put an image pull in the inner loop.

### `just flatpak`

Experimental. Same shape as the other two: build, stop every instance, bring the
resident up as the Flatpak. It installs the flathub runtime, SDK and the
rust-stable and llvm21 extensions if they are missing, and keeps its build state
in `~/.cache/cck-flatpak`, never in the repo and never in `/var/tmp` (the bwrap
sandbox cannot chdir into `/var/tmp`, and fails as a confusing "No such file or
directory" on the first module). The first run compiles leptonica and tesseract
and takes about fifteen minutes; later runs are incremental.

### `just docs`

Serves the MkDocs site at `http://127.0.0.1:8000` with live reload, on the
pinned toolchain from `docs-requirements.txt` rather than whatever `mkdocs`
happens to be on `PATH`. Creates `.venv-docs` on first run; a fresh clone needs
nothing but `python3`.

## Installing it on your PATH

No recipe installs anything. They leave the artifact where it was built and
repoint the stable path at it, which is all a keyboard shortcut needs. To get the
binary onto your `PATH` under its own name instead:

```sh
cargo install --path .
```

Add `--no-default-features` here too if that is how the build works on your
distro. It lands in `~/.cargo/bin`, so `cosmic-capture-kit --settings` then works
from any directory.

An application-menu entry is a separate step, and has to come after the install,
because the entry launches the binary by name:

```sh
install -Dm644 res/dev.thedragon.CosmicCaptureKit.desktop \
  ~/.local/share/applications/dev.thedragon.CosmicCaptureKit.desktop
install -Dm644 res/icons/dev.thedragon.CosmicCaptureKit.svg \
  ~/.local/share/icons/hicolor/scalable/apps/dev.thedragon.CosmicCaptureKit.svg
```

Worth doing even if you never launch from the menu: it is what makes the desktop
and xdg-desktop-portal show the app's real name instead of a generic fallback.
Launching from the menu is a bare launch, so it starts a region screenshot.

## Things that will bite you

**A build you made carries no cloud registrations.** Google Drive, OneDrive and
Dropbox are compiled into the official downloads and not into yours, so each
needs a free registration of your own before it appears in the Add cloud account
list. YouTube needs one from everybody, official builds included.
[CLOUD_ACCOUNTS.md](CLOUD_ACCOUNTS.md) has the steps.

**Never let a BUILD TREE write into `target/`.** It holds cargo's own state and
the binaries your capture shortcut launches. The container recipes keep their
work in `target-dist/` and `target-appimage/` for exactly that reason: those
containers run as root, and one writing over a running binary breaks the daemon
executing it. On Windows the tree is shared with the Linux boot, so every cargo
invocation there sets `CARGO_TARGET_DIR=target-win`, and `cargo clean` is never
the answer.

The one thing that does go in is a FINISHED artifact, into `target/artifacts/`,
written by you rather than by a container, and by rename rather than in place (a
running artifact cannot be written over, and on a dev box the previous AppImage
usually is the running daemon). That is a narrowing of the old blanket rule, not
a repeal of it. The cost is that `cargo clean` takes your artifacts with it,
which beats a fifth top-level build directory.

**`chmod +x` does not reach git here.** The repo sets `core.fileMode = false`,
so git keeps whatever mode a file had when it was added and ignores the
filesystem. A new script lands as `100644`, works locally, and fails
`Permission denied` on a fresh clone. Use `git update-index --chmod=+x <path>`
and confirm with `git ls-files -s <path>`.

**Third-party binaries are pinned in [`scripts/pins.env`](scripts/pins.env).**
One manifest, read by every fetcher and by the container builds, so the same
ffmpeg and tesseract versions land everywhere. Bash `source`s it, so any value
containing a space MUST be quoted: `KEY=a b c` means "set KEY=a, then run the
command b".

**The suite is the gate.** `cargo clippy --all-targets` must be at zero
warnings, and `cargo test` must pass in BOTH feature configurations
(`--no-default-features` too). GitHub CI is manual-only and never gates a merge.

**Do not run repo-wide `cargo fmt`.** The tree is not rustfmt-enforced on
purpose; match the surrounding hand style.

## Where to read next

- [CLAUDE.md](CLAUDE.md) is the deep working agreement: architecture, the
  platform seams, the recording invariants, the closed-platform split.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) is the module map.
- The `//!` doc at the top of any file you are about to edit. This codebase puts
  the WHY there, including the approaches already tried and rejected.
