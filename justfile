# justfile — local, cross-platform build entry point.
#
# `just build` builds whatever THIS machine can build, the same command
# on macOS, Windows, or Linux. It is the local counterpart to the CI-only
# ".github/workflows/release.yml" ("Build For Release"), which is manual
# dispatch and produces the signed/notarized release artifacts. This is for
# trying a local change, not for shipping: it signs opportunistically (mac)
# and never notarizes, and it never touches release.yml's tagging/draft flow.
#
# Requires `just` (https://github.com/casey/just). On Windows, requires
# PowerShell 7+ (`pwsh`), same as scripts/win-package.ps1 already does.
#
# The Windows recipes call PowerShell via a checked-in `.ps1` wrapper so the
# file parses under older `just` releases as well as current ones. This avoids
# the temp-file path that `just` creates for shebang recipes and keeps the
# recipe syntax portable across distro-packaged `just` builds.
#
# Plain (non-shebang) `[windows]` recipe lines run through the default shell.
# Force that shell to PowerShell so Windows hosts without `sh` on PATH still run.
set windows-shell := ['pwsh.exe', '-NoLogo', '-NoProfile', '-Command']

# Bare `just` lists recipes instead of running the first one by surprise.
default:
    @just --list

[linux]
build:
    just build-linux

[macos]
build:
    just build-macos

[windows]
build:
    just build-windows

[linux]
appimage arch="x86_64":
    just appimage-linux

[macos]
appimage arch="x86_64":
    just appimage-macos {{arch}}

[windows]
appimage arch="x86_64":
    just appimage-windows

[linux]
ffmpeg-win:
    just ffmpeg-win-linux

[macos]
ffmpeg-win:
    just ffmpeg-win-linux

[windows]
ffmpeg-win:
    just ffmpeg-win-windows

[linux]
dev:
    just dev-linux

[macos]
dev:
    just dev-macos

[windows]
dev:
    just dev-windows

# Documentation site: serve it locally with live reload, on every platform.
docs:
    #!/usr/bin/env bash
    set -euo pipefail
    # Serve the site locally with live reload, on the PINNED toolchain rather
    # than whatever `mkdocs` happens to be on PATH (a global install may lack
    # the plugins mkdocs.yml requires). The venv is created on first run, so a
    # fresh clone needs nothing but python3.
    if [ ! -x .venv-docs/bin/mkdocs ]; then
        echo "==> Creating .venv-docs from docs-requirements.txt (first run)..."
        python3 -m venv .venv-docs
        .venv-docs/bin/pip install -q -r docs-requirements.txt
    fi
    echo "==> Serving on http://127.0.0.1:8000 (Ctrl-C to stop)"
    .venv-docs/bin/mkdocs serve

# macOS: build + bundle the .app (signed if a Developer ID identity is available).
build-macos:
    #!/usr/bin/env bash
    set -euo pipefail
    # Signs with a Developer ID identity if one is in the login keychain,
    # otherwise falls back to an unsigned local build. That is fine to run
    # directly: only Gatekeeper on ANOTHER machine cares about signing, and a
    # locally-copied, non-quarantined .app is not gated by it. Never
    # notarizes here; that needs Apple credentials and is a release-only step.
    #
    # Bake the cloud client ids from the same GitHub repository variables
    # release.yml uses (see the Linux recipe's comment for the full why).
    # Needs an authenticated `gh`; silently unbaked without one.
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        for v in CCK_BAKED_GDRIVE_CLIENT_ID CCK_BAKED_GDRIVE_CLIENT_SECRET CCK_BAKED_ONEDRIVE_CLIENT_ID CCK_BAKED_DROPBOX_CLIENT_ID; do
            if [ -z "${!v:-}" ]; then
                val="$(gh variable get "$v" 2>/dev/null || true)"
                [ -n "$val" ] && export "$v=$val"
            fi
        done
        echo "==> Baked cloud ids from GitHub repository variables"
    fi
    # Pull the SAME sidecars CI ships, rather than packaging whatever happens to
    # be in vendor/ from some earlier download (DRAGON-531). Idempotent, so this
    # is free once they are present.
    ./scripts/fetch-mac-vendor.sh
    if security find-identity -v -p codesigning 2>/dev/null | grep -q "Developer ID Application"; then
        echo "==> Developer ID identity found; building signed."
        ./scripts/mac-package.sh --build --icns --bundle --sign
    else
        echo "==> No Developer ID identity in the login keychain; building unsigned."
        ./scripts/mac-package.sh --build --icns --bundle
    fi
    echo "==> Built: target/release/bundle/Cosmic Capture Kit.app"
    # The shared output dir (DRAGON-590). TWO entries on macOS, because a bundle
    # and an executable are not interchangeable here:
    #
    #   "Cosmic Capture Kit.app"  a symlink to the bundle, for `open`, `open -a`
    #                             and a Finder double-click (the dev recipe's
    #                             launchd handoff below needs exactly this).
    #   cosmic-capture-kit        the STABLE PATH, the same name Linux uses, a
    #                             tiny launcher that execs the bundle's real
    #                             executable. See cck_stable_launcher for why it
    #                             is a launcher here and a symlink on Linux.
    . scripts/artifacts.sh
    APP_BUNDLE="$PWD/target/release/bundle/Cosmic Capture Kit.app"
    cck_link "Cosmic Capture Kit.app" "$APP_BUNDLE" >/dev/null
    cck_stable_launcher "$APP_BUNDLE/Contents/MacOS/cosmic-capture-kit"

# Windows: build the MSI via scripts/win-package.ps1.
#
# Keep these entries as plain `just` recipes that shell out to a checked-in
# PowerShell script. Older `just` releases do not understand
# `[extension('.ps1')]`, and the shebang temp-file route breaks on
# extensionless files when `pwsh -File` is used. Calling `pwsh` directly keeps
# the file compatible with both older and newer `just` while retaining the
# Windows, Wayland and X11 paths described in the README.
build-windows:
    pwsh -NoLogo -NoProfile -File ./scripts/just-win.ps1 build

# Linux: plain release build, retrying without zero-copy if the first attempt fails.
build-linux:
    #!/usr/bin/env bash
    set -euo pipefail
    # There is no packaged install yet (build from source per README). Tries
    # default features first (the zero-copy GPU encode path, needs ffmpeg 8
    # headers); on failure, most likely an older distro's ffmpeg, retries
    # with --no-default-features rather than leaving you to guess why it
    # died inside ffmpeg-sys-next.
    #
    # Bake the cloud client ids into the binary by reading the SAME GitHub
    # repository variables release.yml bakes from, so a local `just build` and
    # an official artifact carry identical credentials and no launch path can
    # lose them to a stale session environment. Needs an authenticated `gh`;
    # without one the build proceeds unbaked, exactly as before this existed.
    # An already-exported CCK_BAKED_* wins over the fetch. YouTube is
    # deliberately never baked, matching the release policy.
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        for v in CCK_BAKED_GDRIVE_CLIENT_ID CCK_BAKED_GDRIVE_CLIENT_SECRET CCK_BAKED_ONEDRIVE_CLIENT_ID CCK_BAKED_DROPBOX_CLIENT_ID; do
            if [ -z "${!v:-}" ]; then
                val="$(gh variable get "$v" 2>/dev/null || true)"
                [ -n "$val" ] && export "$v=$val"
            fi
        done
        echo "==> Baked cloud ids from GitHub repository variables"
    fi
    echo "==> Building (default features)..."
    if cargo build --release; then
        echo "==> Built: target/release/cosmic-capture-kit"
    else
        echo "==> Default build failed, most likely missing ffmpeg 8 headers for the"
        echo "    zero-copy path (see README's Debian/Ubuntu/Mint/Pop!_OS note)."
        echo "==> Retrying with --no-default-features..."
        cargo build --release --no-default-features
        echo "==> Built: target/release/cosmic-capture-kit (--no-default-features)"
    fi
    # The shared output dir (DRAGON-590). This artifact stays where cargo put it,
    # because copying a 65MB binary on every build buys nothing; what lands in
    # target/artifacts/ is a NAMED SYMLINK to it, so all four kinds of Linux build
    # are listed side by side there and `ls -lt` shows which is newest. The stable
    # path is repointed unconditionally, which is the whole mechanism.
    . scripts/artifacts.sh
    cck_link cosmic-capture-kit-source "$PWD/target/release/cosmic-capture-kit" >/dev/null
    cck_stable "$PWD/target/release/cosmic-capture-kit"

# Build the SHIPPING Linux artifact, in the same Rocky 9 container release.yml
# uses (DRAGON-528). Needs docker.
#
# This is deliberately NOT folded into `just build`. That recipe is the
# user-facing source build the README documents, and silently requiring docker
# would break it for anyone building from source. It is also what `just dev`
# calls, so containerising it would put a multi-minute image pull in the hot
# edit-test loop.
#
# The container exists because the host cannot produce the artifact: CachyOS
# glibc is far newer than the floor we ship against, so a local build would
# only run on other bleeding-edge distros. Rocky 9 floors it at GLIBC_2.34,
# which reaches Ubuntu 22.04 and everything newer.
[linux]
dist:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v docker >/dev/null 2>&1 || { echo "==> docker is required for a shipping build"; exit 1; }
    # The CONTAINER's output mount stays outside target/, and the reason is
    # unchanged by DRAGON-590: this container runs as ROOT (it dnf-installs a
    # toolchain), so everything it writes comes back root-owned, and target/ holds
    # the live binaries plus cargo's own state. Only the FINISHED binary is copied
    # into target/artifacts/ afterwards, by us, as the calling user.
    OUT="$PWD/target-dist"
    mkdir -p "$OUT"
    # ONE source of truth for the compiler: rust-toolchain.toml. This container is
    # rustup-driven and mounts /src, so it WOULD read the pin; installing `stable`
    # here would make rustup fetch the pinned version at build time instead, which
    # is slower at best and a hard failure wherever the prefix is not writable.
    RUST_TOOLCHAIN="$(sed -n 's/^ *channel *= *"\(.*\)".*/\1/p' rust-toolchain.toml)"
    [ -n "$RUST_TOOLCHAIN" ] || { echo "could not read channel from rust-toolchain.toml" >&2; exit 1; }
    docker run --rm -e RUST_TOOLCHAIN -v "$PWD:/src:ro" -v "$OUT:/out" -w /src rockylinux:9 bash -euo pipefail -c '
        dnf -y install epel-release dnf-plugins-core >/dev/null
        dnf config-manager --set-enabled crb >/dev/null
        dnf -y install gcc gcc-c++ make pkgconfig clang-devel libxkbcommon-devel \
          pulseaudio-libs-devel pipewire-devel mesa-libgbm-devel libva-devel \
          git zip xz nasm diffutils file gnupg2 >/dev/null
        curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain "$RUST_TOOLCHAIN" --profile minimal --component clippy,rustfmt >/dev/null
        . "$HOME/.cargo/env"
        # The SHARED pin manifest (/src is the repo, mounted read-only), so this
        # container can never build a different ffmpeg than the mac and Windows
        # packages bundle. Kept in step with release.yml build-linux, which runs
        # the same steps and carries the same reasoning.
        . /src/scripts/pins.env
        cd /tmp
        # Verified by the UPSTREAM GPG SIGNATURE, with the signing fingerprint
        # pinned in pins.env and published on ffmpeg.org/download.html. A checksum
        # we computed ourselves would only prove the file did not change between
        # our own two downloads; the signature proves FFmpeg produced it. Requiring
        # the pinned fingerprint is what stops a compromised key URL supplying
        # both a key and a matching signature. sha256 runs first as a cheap check.
        # MIRROR LIST, tried in order, same as the CI job and the AppImage
        # Dockerfile: ffmpeg.org flapped its TLS and took a whole release build
        # with it, and every Linux artifact builds ffmpeg from source.
        fetch_any() {
          local out="$1"; shift
          for u in "$@"; do
            curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 \
                 --speed-time 30 --speed-limit 1024 -o "$out" "$u" && return 0
            echo "mirror failed, trying the next: $u" >&2
          done
          echo "every mirror failed for $out" >&2; return 1
        }
        fetch_any ffmpeg.tar.xz $FFMPEG_SOURCE_URL
        fetch_any ffmpeg.tar.xz.asc $FFMPEG_SOURCE_ASC_URL
        fetch_any ffmpeg-devel.asc $FFMPEG_GPG_KEY_URL
        echo "$FFMPEG_SOURCE_SHA256  ffmpeg.tar.xz" | sha256sum -c -
        # Isolated keyring, so this never touches a real one.
        export GNUPGHOME=/tmp/cck-gnupg
        rm -rf "$GNUPGHOME"; mkdir -p "$GNUPGHOME"; chmod 700 "$GNUPGHOME"
        gpg --batch --quiet --import ffmpeg-devel.asc
        gpg --batch --with-colons --fingerprint | grep -E "^fpr:" | cut -d: -f10 \
          | grep -qx "$FFMPEG_GPG_FPR" \
          || { echo "==> ffmpeg-devel.asc lacks the pinned signing key $FFMPEG_GPG_FPR"; exit 1; }
        # VALIDSIG carries the fingerprint that actually made the signature, so
        # this asserts authorship, not just a good signature by some imported key.
        # gpg reports [unknown] trust for a valid signature from an uncertified
        # key, which is expected here and not an error.
        gpg --batch --status-fd 1 --verify ffmpeg.tar.xz.asc ffmpeg.tar.xz \
          | grep -q "^\[GNUPG:\] VALIDSIG $FFMPEG_GPG_FPR " \
          || { echo "==> ffmpeg $FFMPEG_VERSION is not signed by $FFMPEG_GPG_FPR"; exit 1; }
        tar xJf ffmpeg.tar.xz && cd "ffmpeg-$FFMPEG_VERSION"
        ./configure --prefix=/usr/local --enable-shared --disable-static \
          --disable-programs --disable-doc --disable-everything \
          --enable-vaapi --enable-encoder=rawvideo --enable-filter=scale_vaapi >/dev/null
        make -j"$(nproc)" >/dev/null && make install >/dev/null && ldconfig
        cd /src
        # RHEL pkg-config does not search /usr/local, where ffmpeg installed.
        export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/local/lib64/pkgconfig
        export LD_LIBRARY_PATH=/usr/local/lib:/usr/local/lib64
        CARGO_TARGET_DIR=/tmp/t cargo build --release
        cp /tmp/t/release/cosmic-capture-kit /out/
    '
    # Into the shared output dir, then report from THERE, so the path printed is
    # the one that still exists and is owned by the caller.
    . scripts/artifacts.sh
    DIST="$(cck_publish "$OUT/cosmic-capture-kit" cosmic-capture-kit-dist)"
    echo "==> Built: $DIST"
    echo "==> glibc floor: $(objdump -T "$DIST" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    # Every Linux build recipe leaves exactly ONE daemon running, on what it just
    # built, with the stable path pointing at it. Consistency is the point: the
    # moment one recipe repoints the stable path without also becoming the running
    # daemon, "the path means my latest build" stops being true and the confusion
    # this all exists to remove comes straight back.
    bash scripts/stop-all.sh
    setsid -f "$DIST" resident >/dev/null 2>&1
    sleep 1
    # A WARNING, not a failure, unlike the appimage and flatpak recipes. Those two
    # have always ended in a daemon and a broken one means the recipe did not do
    # its job. This one's job is the ARTIFACT, and it took ten minutes of container
    # time to make; a daemon that needed 1.5 seconds instead of 1 must not throw
    # that away.
    pgrep -f 'cosmic-capture-kit-dist resident' >/dev/null \
        && echo "==> Daemon restarted on the shipping binary" \
        || echo "==> WARNING: the shipping-binary daemon did not come up (the artifact is fine)"
    cck_stable "$DIST"

# Build the Linux AppImage (DRAGON-510), from Linux, macOS or Windows.
#
# All three arms drive the SAME container, and that is not an accident of
# convenience: an AppImage does not fix glibc, its floor is entirely the build
# host's, and no machine here runs an old enough base. CachyOS can no more
# produce a GLIBC_2.34 binary than macOS can. So the container IS the build
# environment on every host, and only the container runtime differs (docker on
# Linux and Windows, OrbStack or Docker Desktop on macOS).
#
# Two stages, both cached by docker:
#   1. `docker build` the base image: Rocky 9 plus ffmpeg 8.1.2, x264 and
#      tesseract compiled from pinned sources. Ten to fifteen minutes ONCE; a
#      later run with unchanged pins is instant.
#   2. `docker run` scripts/appimage/build.sh, which compiles this repo and
#      assembles the AppImage. `target-appimage/` holds cargo's state between
#      runs, so a second build is incremental.
#
# The CONTAINER never writes into the repo's own `target/` (DRAGON-590 narrowed
# this rule but did not drop it): `/src` is mounted READ-ONLY and everything the
# build produces goes to `target-appimage/`. What changed is that the recipe then
# MOVES the one finished .AppImage into `target/artifacts/`, from the host, as the
# calling user. A finished artifact landing there is safe; a container build tree
# landing there is not, and that was always the actual hazard.
#
# EXPERIMENTAL Flatpak (lab/flatpak). Same shape as `just appimage` and `just dev`:
# build, stop every running instance, relaunch the resident on the new artifact.
#
# Needs flatpak-builder plus three runtime pieces, all from flathub:
#   org.freedesktop.Platform//25.08          the runtime
#   org.freedesktop.Sdk//25.08               the build SDK
#   org.freedesktop.Sdk.Extension.rust-stable//25.08   Rust 1.97.1
#   org.freedesktop.Sdk.Extension.llvm21//25.08        libclang, for bindgen
#
# Build state lives under ~/.cache/cck-flatpak, never in the repo and never in
# /var/tmp: flatpak-builder's bwrap sandbox cannot chdir into /var/tmp, which
# fails as a confusing "No such file or directory" on the first module.
[linux]
flatpak:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v flatpak-builder >/dev/null 2>&1 || { echo "==> flatpak-builder is required"; exit 1; }
    APP=dev.thedragon.CosmicCaptureKit
    STATE="$HOME/.cache/cck-flatpak"
    mkdir -p "$STATE"
    for r in "org.freedesktop.Sdk.Extension.rust-stable//25.08" "org.freedesktop.Sdk.Extension.llvm21//25.08"; do
        flatpak info --user "$r" >/dev/null 2>&1 || flatpak info "$r" >/dev/null 2>&1 || {
            echo "==> Installing $r"; flatpak install -y --user flathub "$r"; }
    done
    # The SDK's compiler is the ONE toolchain we cannot derive from
    # rust-toolchain.toml: the SDK chooses it, and this build does not use rustup.
    # So CHECK it rather than keeping it in step by hand. A silent divergence is
    # what produced two warnings visible only in the Flatpak (DRAGON-625), and the
    # whole point of the pin is that every build sees one compiler.
    PINNED="$(sed -n 's/^ *channel *= *"\(.*\)".*/\1/p' rust-toolchain.toml)"
    SDK_RUSTC="$(flatpak info org.freedesktop.Sdk.Extension.rust-stable//25.08 2>/dev/null \
        | sed -n 's/^ *Version: *//p' | head -1)"
    if [ -n "$SDK_RUSTC" ] && [ "$SDK_RUSTC" != "$PINNED" ]; then
        echo "==> TOOLCHAIN MISMATCH: rust-toolchain.toml pins $PINNED, the SDK ships $SDK_RUSTC." >&2
        echo "==> The SDK moved. Update rust-toolchain.toml to $SDK_RUSTC in the same commit," >&2
        echo "==> then re-run the gate: an older or newer rustc emits a different lint set." >&2
        exit 1
    fi
    echo "==> Building (first run compiles leptonica + tesseract, ~15 min; later runs are incremental)..."
    flatpak-builder --user --force-clean --disable-rofiles-fuse \
        --state-dir="$STATE/state" --repo="$STATE/repo" \
        "$STATE/build" scripts/flatpak/$APP.yml
    flatpak remote-add --user --if-not-exists --no-gpg-verify cck-local "$STATE/repo" >/dev/null 2>&1 || true
    flatpak install -y --user --reinstall cck-local "$APP" >/dev/null
    # The INSTALLED proton-drive must be byte-identical to the pin: flatpak-builder's
    # finalize strip once severed this Bun single-file binary AFTER every in-build
    # check passed (the manifest's global no-debuginfo prevents it; this catches any
    # regression before a broken provider ships to the desktop).
    . scripts/pins.env
    flatpak run --user --command=sh "$APP" -c \
        "echo \"$PROTON_DRIVE_CLI_SHA256  /app/bin/proton-drive\" | sha256sum -c -" \
        || { echo "==> INSTALL ABORTED: /app/bin/proton-drive does not match the pin (stripped again?)"; exit 1; }
    # EVERY process of this app, of EVERY build (DRAGON-590): one shared sweep, not
    # a per-recipe copy. This recipe used to carry its own four pkill patterns, all
    # anchored to a path, and the Flatpak's own argv on the host has no path at all,
    # so the recipe never actually stopped the thing it was about to reinstall. Read
    # scripts/stop-all.sh before adding a pattern anywhere else.
    bash scripts/stop-all.sh
    # Seed `resident = true` in the Flatpak's own config, which is the one thing that
    # makes this recipe's promise true.
    #
    # The sandbox has a PRIVATE config dir (~/.var/app/<id>/config), so it starts fresh
    # no matter what the host's copy says, and `default_resident()` is FALSE on Linux by
    # design: PrintScreen there is a COSMIC custom shortcut, not something a daemon owns.
    # A bare `resident` launch therefore falls straight through to `app::run` and tries to
    # CAPTURE instead of becoming the daemon, which looks exactly like "the daemon did not
    # start" while a process is very much running.
    CFG="$HOME/.var/app/$APP/config/cosmic-capture-kit/config.toml"
    mkdir -p "$(dirname "$CFG")"
    if [ -f "$CFG" ] && grep -q '^resident *=' "$CFG"; then
        sed -i 's/^resident *=.*/resident = true/' "$CFG"
    else
        echo 'resident = true' >> "$CFG"
    fi
    setsid -f flatpak run --user "$APP" resident >/dev/null 2>&1
    sleep 3
    # Match the RESIDENT specifically, not merely "some instance of this app is alive".
    # The host CAN see the argv through the bwrap wrapper (`bwrap ... -- cosmic-capture-kit
    # resident`), so this is a real check; `flatpak ps` is not, because it answers true for
    # a capture child, a settings window, or a lingering one-shot that never became a daemon.
    pgrep -f 'cosmic-capture-kit resident' >/dev/null \
        && echo "==> Daemon restarted as the Flatpak" \
        || { echo "==> ERROR: the Flatpak daemon did not come up"; exit 1; }
    echo "==> App:  $APP (user installation)"
    echo "==> Run:  flatpak run --user $APP"
    echo "==> Logs: flatpak run --user --command=sh $APP -c 'ls ~/.var/app/$APP'"
    # The Flatpak IS symlinkable, which is the thing that lets one stable path
    # cover all three Linux artifacts (DRAGON-590). `flatpak install` exports a
    # launcher named after the app id:
    #
    #   #!/bin/sh
    #   exec /usr/bin/flatpak run --branch=master --arch=x86_64 <app id> "$@"
    #
    # It forwards "$@", so every flag reaches the app exactly as it would a plain
    # binary. DERIVED from the app id and from which installation we used, never
    # hardcoded: a --user install exports under $XDG_DATA_HOME and a system one
    # under /var/lib/flatpak, and the exported script itself is pinned to the
    # branch and arch that were installed, so guessing the path is how you get a
    # symlink to the wrong build (or to nothing). This recipe installs --user, so
    # that root is checked first, and a missing export FAILS the recipe rather
    # than leaving a dangling stable path behind.
    EXPORT="${XDG_DATA_HOME:-$HOME/.local/share}/flatpak/exports/bin/$APP"
    [ -e "$EXPORT" ] || EXPORT="/var/lib/flatpak/exports/bin/$APP"
    [ -e "$EXPORT" ] || { echo "==> ERROR: flatpak exported no launcher for $APP"; exit 1; }
    . scripts/artifacts.sh
    cck_link cosmic-capture-kit-flatpak "$EXPORT" >/dev/null
    cck_stable "$EXPORT"

appimage-linux:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v docker >/dev/null 2>&1 || { echo "==> docker is required to build the AppImage"; exit 1; }
    OUT="$PWD/target-appimage"
    mkdir -p "$OUT"
    # Bake the cloud client ids from the same GitHub repository variables
    # release.yml uses (see the Linux `build` recipe for the full why).
    ENVS=()
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        for v in CCK_BAKED_GDRIVE_CLIENT_ID CCK_BAKED_GDRIVE_CLIENT_SECRET CCK_BAKED_ONEDRIVE_CLIENT_ID CCK_BAKED_DROPBOX_CLIENT_ID; do
            val="${!v:-}"
            [ -n "$val" ] || val="$(gh variable get "$v" 2>/dev/null || true)"
            [ -n "$val" ] && ENVS+=(-e "$v=$val")
        done
        echo "==> Baked cloud ids from GitHub repository variables"
    fi
    # NOT `-q`: the first run compiles ffmpeg and tesseract for ten to fifteen
    # minutes, and a silent terminal for that long reads as a hang. A cached run
    # prints a few CACHED lines and returns.
    # ONE source of truth for the compiler: rust-toolchain.toml. The image must
    # carry the pinned toolchain, because the container IS rustup-driven and its
    # rustup prefix is read-only, so a version it lacks fails as an unrelated-looking
    # "Permission denied" on a temp file. Derived, never hand-copied.
    RUST_TOOLCHAIN="$(sed -n 's/^ *channel *= *"\(.*\)".*/\1/p' rust-toolchain.toml)"
    [ -n "$RUST_TOOLCHAIN" ] || { echo "could not read channel from rust-toolchain.toml" >&2; exit 1; }
    echo "==> Preparing the build image (cached after the first run)..."
    docker build --build-arg "RUST_TOOLCHAIN=$RUST_TOOLCHAIN" -f scripts/appimage/Dockerfile -t cck-appimage-base scripts
    # As the CALLING user, so nothing in target-appimage/ comes back root-owned
    # and needs sudo to clean up. Only the Linux arm does this: on macOS and
    # Windows the bind mount is already uid-virtualised.
    docker run --rm --user "$(id -u):$(id -g)" \
        -v "$PWD:/src:ro" -v "$OUT:/work" "${ENVS[@]+"${ENVS[@]}"}" \
        cck-appimage-base bash /src/scripts/appimage/build.sh
    # The script reports the artifact's NAME; the path is the host's, not the
    # container's (they are the same file under two different roots).
    . "$OUT/appimage.env"
    # MOVE it into the shared output dir (DRAGON-590), so the AppImage sits beside
    # the other artifacts and one stable path can point at it. A move, not a copy:
    # two 33MB copies of the same file is the kind of thing that later makes
    # somebody ask which one is real. cck_publish writes `.part` and renames, so
    # this cannot fail with "Text file busy" when the previous AppImage is the
    # daemon currently running out of that exact filename, which on this machine is
    # the normal state.
    . scripts/artifacts.sh
    APPIMAGE="$(cck_publish_move "$OUT/$CCK_APPIMAGE_NAME")"
    # EVERY process of this app, of EVERY build (DRAGON-590): one shared sweep, not
    # a per-recipe copy. This recipe used to carry three of its own patterns and
    # reached neither a Flatpak instance nor anything launched through the stable
    # path. See scripts/stop-all.sh, which also explains why more than one can be
    # running at once (the single-instance locks cannot see across a sandbox).
    bash scripts/stop-all.sh
    setsid -f "$APPIMAGE" resident >/dev/null 2>&1
    sleep 2
    pgrep -f 'CosmicCaptureKit-[^/]*\.AppImage resident' >/dev/null \
        && echo "==> Daemon restarted on the AppImage" \
        || { echo "==> ERROR: the AppImage daemon did not come up"; exit 1; }
    echo "==> AppImage:    $APPIMAGE"
    echo "==> glibc floor: $CCK_GLIBC_FLOOR"
    echo "==> size:        $CCK_SIZE"
    cck_stable "$APPIMAGE"

appimage-macos arch="x86_64":
    #!/usr/bin/env bash
    set -euo pipefail
    # Same container, different runtime. OrbStack and Docker Desktop both provide
    # the `docker` CLI, so nothing here has to know which one is installed.
    command -v docker >/dev/null 2>&1 || { echo "==> docker is required (OrbStack or Docker Desktop)"; exit 1; }
    # DRAGON-529: which architecture's AppImage to produce. Defaults to x86_64,
    # the primary artifact, which on Apple Silicon means EMULATION. `aarch64` is
    # the interesting one here: it is NATIVE on an Apple Silicon Mac, so it is
    # both fast and the only practical way to produce and try the ARM artifact
    # (an OrbStack Arch ARM VM can then actually run it).
    case "{{arch}}" in
        x86_64)  DOCKER_PLATFORM=linux/amd64 ;;
        aarch64) DOCKER_PLATFORM=linux/arm64 ;;
        *) echo "==> unknown architecture '{{arch}}' (want x86_64 or aarch64)"; exit 1 ;;
    esac
    # A per-arch image tag, so building one does not evict the other from the
    # docker cache and force a 10-15 minute rebuild on the next switch.
    IMAGE="cck-appimage-base-{{arch}}"
    echo "==> Target: {{arch}} ($DOCKER_PLATFORM)"
    OUT="$PWD/target-appimage"
    mkdir -p "$OUT"
    ENVS=()
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        for v in CCK_BAKED_GDRIVE_CLIENT_ID CCK_BAKED_GDRIVE_CLIENT_SECRET CCK_BAKED_ONEDRIVE_CLIENT_ID CCK_BAKED_DROPBOX_CLIENT_ID; do
            val="${!v:-}"
            [ -n "$val" ] || val="$(gh variable get "$v" 2>/dev/null || true)"
            [ -n "$val" ] && ENVS+=(-e "$v=$val")
        done
        echo "==> Baked cloud ids from GitHub repository variables"
    fi
    # ONE source of truth for the compiler: rust-toolchain.toml. The image must
    # carry the pinned toolchain, because the container IS rustup-driven and its
    # rustup prefix is read-only, so a version it lacks fails as an unrelated-looking
    # "Permission denied" on a temp file. Derived, never hand-copied.
    RUST_TOOLCHAIN="$(sed -n 's/^ *channel *= *"\(.*\)".*/\1/p' rust-toolchain.toml)"
    [ -n "$RUST_TOOLCHAIN" ] || { echo "could not read channel from rust-toolchain.toml" >&2; exit 1; }
    echo "==> Preparing the build image (cached after the first run)..."
    docker build --platform "$DOCKER_PLATFORM" --build-arg "RUST_TOOLCHAIN=$RUST_TOOLCHAIN" -f scripts/appimage/Dockerfile -t "$IMAGE" scripts
    # --platform is always explicit, so the target does not depend on what the
    # host happens to be.
    #
    # On Apple Silicon, `x86_64` means EMULATION and a full Rust build under it
    # is far slower than the same build on the Linux box. Both OrbStack and
    # Docker Desktop can back x86_64 containers with Rosetta rather than qemu,
    # which is the difference between slow and unusable; turn it on before
    # assuming this recipe has hung.
    #
    # `aarch64` on the same machine is NATIVE, so it runs at full speed. That is
    # what makes this Mac the practical place to build the ARM artifact.
    docker run --rm --platform "$DOCKER_PLATFORM" \
        -v "$PWD:/src:ro" -v "$OUT:/work" "${ENVS[@]+"${ENVS[@]}"}" \
        "$IMAGE" bash /src/scripts/appimage/build.sh
    . "$OUT/appimage.env"
    # Into the shared output dir like every other artifact (DRAGON-590), so a
    # cross-built AppImage is findable in the same place on every host.
    . scripts/artifacts.sh
    APPIMAGE="$(cck_publish_move "$OUT/$CCK_APPIMAGE_NAME")"
    # No launch step here, unlike the Linux arm: a macOS host cannot run a Linux
    # binary, so the artifact is built and handed over rather than tried. For the
    # same reason the STABLE PATH is deliberately left alone: repointing it at a
    # file this machine cannot execute would break the one guarantee it makes.
    echo "==> glibc floor: $CCK_GLIBC_FLOOR"
    echo "==> size:        $CCK_SIZE"
    cck_say_artifact "$APPIMAGE"

appimage-windows:
    pwsh -NoLogo -NoProfile -File ./scripts/just-win.ps1 appimage

# Cross-compile the WINDOWS ffmpeg, from Linux, macOS or Windows (DRAGON-675).
#
# Like `just appimage`, all three arms drive the SAME container and only the
# docker runtime differs. Unlike it, the container is not here to fix a glibc
# floor: it cross-compiles PE binaries with mingw-w64, so the host distro is
# irrelevant and the image is Debian. See scripts/ffmpeg-win/Dockerfile.
#
# WHY WE BUILD SOMEBODY ELSE'S PROGRAM AT ALL: because when the Windows ffmpeg
# was a prebuilt, the NVIDIA driver version our users needed was a property we
# INHERITED, and a patch-level pin refresh moved it from ~570 to 610.00 without
# a word (DRAGON-671). Nothing in the finished binary reports that number, so no
# fetch-time check could have caught it. Building it makes the floor
# `NVCODEC_URL` in scripts/pins.env, the same pin the Linux AppImage uses.
#
# THIS IS A PRODUCER, run once per pin move. Nothing else here ever builds
# ffmpeg: CI, `just dev` and the MSI all FETCH the published archive through
# scripts/fetch-win-vendor.ps1, so the dev loop runs the exact bytes a customer
# installs. It does NOT publish either: the archive lands in target/artifacts/
# and the build prints the `gh release upload` line plus the two pins.env lines
# to paste, because putting a binary in front of users is a human's decision.
ffmpeg-win-linux:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v docker >/dev/null 2>&1 || { echo "==> docker is required to build the Windows ffmpeg"; exit 1; }
    OUT="$PWD/target-ffmpeg-win"
    mkdir -p "$OUT"
    # NOT `-q`: the first run compiles x264, x265, SDL2 and oneVPL for several
    # minutes, and a silent terminal for that long reads as a hang.
    echo "==> Preparing the build image (cached after the first run)..."
    docker build -f scripts/ffmpeg-win/Dockerfile -t cck-ffmpeg-win scripts
    # As the CALLING user on Linux, so nothing in target-ffmpeg-win/ comes back
    # root-owned. macOS virtualises the bind mount's uid already.
    #
    # An `if`, not `[ … ] && USER_ARGS=(…)`: under `set -e` a false test as a
    # bare command is a failing command, so the one-liner would exit the recipe
    # on macOS before docker ever ran.
    USER_ARGS=()
    if [ "$(uname -s)" = "Linux" ]; then
        USER_ARGS=(--user "$(id -u):$(id -g)")
    fi
    docker run --rm "${USER_ARGS[@]+"${USER_ARGS[@]}"}" \
        -v "$PWD:/src:ro" -v "$OUT:/out" \
        cck-ffmpeg-win bash /src/scripts/ffmpeg-win/build.sh
    # The script reports the artifact's NAME; the path is the host's, not the
    # container's (they are the same file under two different roots).
    . "$OUT/ffmpeg-win.env"
    . scripts/artifacts.sh
    # The ARCHIVE only. Its sha256 stays in target-ffmpeg-win/ beside the build
    # log: target/artifacts/ holds one finished artifact per recipe, and the
    # hash's real home is the pins.env line the build just printed.
    ARCHIVE="$(cck_publish_move "$OUT/$CCK_FFMPEG_WIN_ARCHIVE")"
    echo "==> NVENC API: $CCK_FFMPEG_WIN_NVENC_API (the NVIDIA driver floor this build declares)"
    # The stable path is deliberately untouched: this is not the app, and on two
    # of the three hosts it is not even a runnable binary.
    cck_say_artifact "$ARCHIVE"

ffmpeg-win-windows:
    pwsh -NoLogo -NoProfile -File ./scripts/just-win.ps1 ffmpeg-win

# Update the RUNNING resident daemon to the freshly built binary, on any
# platform: build, stop the old daemon, start the new one, and print the
# binary's full path (paste it into a system-level hotkey, e.g. COSMIC's
# custom shortcut, and it survives rebuilds because the path never changes).
dev-linux:
    #!/usr/bin/env bash
    set -euo pipefail
    # Build first, but suppress build's stable-path banner: this recipe prints the
    # same banner after daemon restart as its final line.
    CCK_SUPPRESS_USE_LINE=1 just build
    # EVERY process of this app, of EVERY build (DRAGON-590): one shared sweep, not
    # a per-recipe copy. This recipe used to stop only its OWN target/release
    # binary, so building here after building the AppImage or the Flatpak left the
    # other one's resident alive and the owner with two tray icons, each spawning
    # capture children from a different build.
    #
    # A preview editor, settings window or stale overlay also has to go, not just
    # the daemon: it keeps running against a now-deleted inode, so its
    # `current_exe` respawns fail silently while it goes on serving the old code.
    bash scripts/stop-all.sh
    setsid -f target/release/cosmic-capture-kit resident >/dev/null 2>&1
    sleep 1
    pgrep -f 'target/release/cosmic-capture-kit resident' >/dev/null \
        && echo "==> Daemon restarted on the fresh binary" \
        || { echo "==> ERROR: daemon did not come up"; exit 1; }
    # `just build` already pointed the stable path here; say it again, last, so it
    # is what is left on screen after the daemon lines.
    . scripts/artifacts.sh
    cck_stable "$PWD/target/release/cosmic-capture-kit"

dev-macos:
    #!/usr/bin/env bash
    set -euo pipefail
    # Build first, but suppress build's stable-path banner: this recipe prints the
    # same banner after daemon restart as its final line.
    CCK_SUPPRESS_USE_LINE=1 just build
    # EVERY process of this app, of EVERY build (DRAGON-590): one shared sweep, not
    # a per-recipe copy. macOS has only two shapes (the bare `target/release`
    # binary an earlier version of this recipe launched directly, and the .app
    # bundle it uses now) and neither is sandboxed, so the mac arm of the sweep
    # stays pgrep-based; there is no /proc to read.
    bash scripts/stop-all.sh
    # `open -a`, NOT `nohup ... & disown`: launching through Launch Services hands
    # the daemon to launchd as a genuinely independent process, immune to whatever
    # the calling shell does to its OWN process tree once this script exits.
    # `nohup`/`disown` only stop the immediate parent shell from signalling this
    # process on ITS exit; they do not move the daemon out of that shell's
    # process group or its ancestry, so a shell that cleans up a whole job by
    # walking its descendants (nushell does this) can still kill the daemon
    # regardless of nohup or disown, the instant `just` returns control to it,
    # which is exactly what made the daemon vanish right after "Daemon
    # restarted" printed, under a nushell caller, while a bash-only
    # reproduction (this recipe's own shebang) never showed the bug. `open -a`
    # sidesteps the whole class of problem: it is the same mechanism already
    # used everywhere else to relaunch this app reliably during development.
    open -a "$(pwd)/target/release/bundle/Cosmic Capture Kit.app" --args resident
    sleep 1
    pgrep -f 'Cosmic Capture Kit.app/Contents/MacOS/cosmic-capture-kit' >/dev/null \
        && echo "==> Daemon restarted on the fresh binary" \
        || { echo "==> ERROR: daemon did not come up"; exit 1; }
    # `just build` already wrote the stable launcher; say it again, last, so it is
    # what is left on screen after the daemon lines.
    . scripts/artifacts.sh
    cck_stable_launcher "$PWD/target/release/bundle/Cosmic Capture Kit.app/Contents/MacOS/cosmic-capture-kit"

dev-windows:
    pwsh -NoLogo -NoProfile -File ./scripts/just-win.ps1 dev
