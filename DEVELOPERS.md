# Developing Cosmic Capture Kit

Everything day to day runs through [`just`](https://github.com/casey/just).
`just` on its own lists the recipes; this page says when to reach for each and
what it will do to your machine.

For what the app depends on at runtime see [dependencies.md](dependencies.md),
and for the flag list see [CLI.md](CLI.md).

## The short version

```sh
just dev        # the loop you will live in
```

Build, stop every running instance, restart the resident daemon on the fresh
binary, print its path. Bind that path to a shortcut once and it survives every
rebuild, because the path never changes.

## The recipes

| Recipe | What it does | Needs |
|---|---|---|
| `just dev` | Build, stop every instance, restart the daemon | |
| `just build` | Build this platform's own artifact | |
| `just appimage` | Build the AppImage, restart the daemon on it | docker |
| `just dist` | Build the shipping Linux binary in the release container | docker |
| `just docs` | Serve the documentation site with live reload | python3 |

### `just dev`

The inner loop. It stops **every** process of this app, not only the daemon: a
preview editor or settings window left over from the previous build keeps
running against a deleted binary, serves stale code, and its re-exec spawns fail
silently. Then it relaunches the daemon and prints the binary path.

Run it after any change you want to try through your capture shortcut.

### `just build`

Whatever this machine can build, and it means something different per platform,
matching what that platform actually ships:

- **Linux**: a plain `cargo build --release`. Retries with
  `--no-default-features` if the first attempt fails, which on an older distro
  means no ffmpeg 8 headers for the zero-copy encoder.
- **macOS**: fetches the pinned sidecars, then builds and bundles the `.app`,
  signed if a Developer ID identity is in your login keychain. Never notarizes;
  that needs Apple credentials and is release-only.
- **Windows**: fetches the pinned sidecars, then builds the MSI.

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

The shipping Linux binary, in the same container CI uses.

Deliberately separate from `just build`. That recipe is the source build the
README documents and the one `just dev` calls, so requiring docker there would
break building from source and put an image pull in the inner loop.

### `just docs`

Serves the MkDocs site at `http://127.0.0.1:8000` with live reload, on the
pinned toolchain from `docs-requirements.txt` rather than whatever `mkdocs`
happens to be on `PATH`. Creates `.venv-docs` on first run; a fresh clone needs
nothing but `python3`.

## Things that will bite you

**Never let anything write into `target/`.** It holds the binaries your capture
shortcut launches. The container recipes use `target-dist/` and
`target-appimage/` for exactly this reason. On Windows the tree is shared with
the Linux boot, so every cargo invocation there sets
`CARGO_TARGET_DIR=target-win`, and `cargo clean` is never the answer.

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
