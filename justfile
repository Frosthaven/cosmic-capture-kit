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

set unstable

# Bare `just` lists recipes instead of running the first one by surprise.
default:
    @just --list

# Documentation site: serve it locally with live reload, on every platform.
[doc("Serve the documentation site locally with live reload")]
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
[doc("Build this platform's own artifact (app/msi on mac/Windows, binary on Linux)")]
[macos]
build:
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

# Windows: build the MSI via scripts/win-package.ps1.
#
# Two Windows-only quirks, both hit on first run (DRAGON-524):
# - `[extension('.ps1')]`: just saves a shebang recipe to an EXTENSIONLESS temp file, and
#   `pwsh -File` refuses anything not named `*.ps1`. The attribute renames the temp.
# - `#!pwsh.exe`, not `#!/usr/bin/env pwsh`: a shebang containing `/` makes just translate
#   the path through `cygpath`, which only exists inside a Git-Bash/Cygwin PATH. A bare
#   Windows program name skips the translation, so this runs from plain PowerShell too.
#   This recipe is `[windows]`-gated, so the unix-style shebang buys nothing here.
[doc("Build this platform's own artifact (app/msi on mac/Windows, binary on Linux)")]
[windows]
[extension('.ps1')]
build:
    #!pwsh.exe
    # win-package.ps1 does build + bundle in one step, including setting
    # CARGO_TARGET_DIR=target-win itself (the dual-boot rule: never touch
    # target/, that is the shared tree's live Linux build). Ships unsigned,
    # same as win-package.ps1 always has, since no code-signing cert exists yet.
    #
    # Bake the cloud client ids from the same GitHub repository variables
    # release.yml uses (see the Linux recipe's comment for the full why).
    # Needs an authenticated `gh`; silently unbaked without one.
    if ((Get-Command gh -ErrorAction SilentlyContinue) -and ($(gh auth status 2>$null; $?))) {
        foreach ($v in @('CCK_BAKED_GDRIVE_CLIENT_ID','CCK_BAKED_GDRIVE_CLIENT_SECRET','CCK_BAKED_ONEDRIVE_CLIENT_ID','CCK_BAKED_DROPBOX_CLIENT_ID')) {
            if (-not (Get-Item "env:$v" -ErrorAction SilentlyContinue)) {
                $val = (gh variable get $v 2>$null)
                if ($val) { Set-Item "env:$v" $val }
            }
        }
        Write-Host '==> Baked cloud ids from GitHub repository variables'
    }
    # Pull the SAME sidecars CI ships, rather than packaging whatever happens to
    # be on PATH (DRAGON-531). Pinned + checksummed, and idempotent, so this is
    # free once vendor\ is populated. The mac recipe above calls its own analog.
    pwsh scripts/fetch-win-vendor.ps1
    pwsh scripts/win-package.ps1
    Write-Host 'Built: target-win\CosmicCaptureKit-*.msi'

# Linux: plain release build, retrying without zero-copy if the first attempt fails.
[doc("Build this platform's own artifact (app/msi on mac/Windows, binary on Linux)")]
[linux]
build:
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
[doc("Build the SHIPPING Linux binary in the release container (needs docker)")]
[linux]
dist:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v docker >/dev/null 2>&1 || { echo "==> docker is required for a shipping build"; exit 1; }
    # NEVER the repo's own target/: it holds the live binaries the PrintScreen
    # shortcut launches, and a container writing there as root would break them.
    OUT="$PWD/target-dist"
    mkdir -p "$OUT"
    docker run --rm -v "$PWD:/src:ro" -v "$OUT:/out" -w /src rockylinux:9 bash -euo pipefail -c '
        dnf -y install epel-release dnf-plugins-core >/dev/null
        dnf config-manager --set-enabled crb >/dev/null
        dnf -y install gcc gcc-c++ make pkgconfig clang-devel libxkbcommon-devel \
          pulseaudio-libs-devel pipewire-devel mesa-libgbm-devel libva-devel \
          git zip xz nasm diffutils file gnupg2 >/dev/null
        curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal >/dev/null
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
    echo "==> Built: $OUT/cosmic-capture-kit"
    echo "==> glibc floor: $(objdump -T "$OUT/cosmic-capture-kit" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"

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
# NEVER the repo's own `target/`: that is where the owner's PrintScreen shortcut
# launches from, and a container writing there would replace a running binary.
[doc("Build the AppImage, restart the daemon on it, and print its glibc floor (needs docker)")]
[linux]
appimage:
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
    echo "==> Preparing the build image (cached after the first run)..."
    docker build -f scripts/appimage/Dockerfile -t cck-appimage-base scripts
    # As the CALLING user, so nothing in target-appimage/ comes back root-owned
    # and needs sudo to clean up. Only the Linux arm does this: on macOS and
    # Windows the bind mount is already uid-virtualised.
    docker run --rm --user "$(id -u):$(id -g)" \
        -v "$PWD:/src:ro" -v "$OUT:/work" "${ENVS[@]+"${ENVS[@]}"}" \
        cck-appimage-base bash /src/scripts/appimage/build.sh
    # The script reports the artifact's NAME; the path is the host's, not the
    # container's (they are the same file under two different roots).
    . "$OUT/appimage.env"
    APPIMAGE="$OUT/$CCK_APPIMAGE_NAME"
    # EVERY process of this app, not just the resident daemon: a preview editor,
    # settings window or stale overlay left behind keeps serving the old code.
    #
    # Anchored to the BINARY, never to the project name: the checkout path itself
    # contains "cosmic-capture-kit", so a bare match also kills an editor with
    # the source open and anything whose temp dir is named after the repo
    # (gpg-agent and scdaemon, in practice). Three patterns, because an AppImage
    # instance can appear under any of three names: the `.AppImage` file itself,
    # the binary inside its FUSE mount, and (for a source build left running)
    # target/release.
    pkill -f '(^|/)target/release/cosmic-capture-kit( |$)' 2>/dev/null || true
    pkill -f '(^|/)CosmicCaptureKit-[^/]*\.AppImage( |$)' 2>/dev/null || true
    pkill -f '/\.mount_[^/]*/usr/bin/cosmic-capture-kit( |$)' 2>/dev/null || true
    sleep 0.3
    setsid -f "$APPIMAGE" resident >/dev/null 2>&1
    sleep 2
    pgrep -f 'CosmicCaptureKit-[^/]*\.AppImage resident' >/dev/null \
        && echo "==> Daemon restarted on the AppImage" \
        || { echo "==> ERROR: the AppImage daemon did not come up"; exit 1; }
    echo "==> AppImage:    $APPIMAGE"
    echo "==> glibc floor: $CCK_GLIBC_FLOOR"
    echo "==> size:        $CCK_SIZE"

[doc("Build the AppImage and print its glibc floor (needs docker). Pass aarch64 to build the ARM one, which is NATIVE and fast on Apple Silicon.")]
[macos]
appimage arch="x86_64":
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
    echo "==> Preparing the build image (cached after the first run)..."
    docker build --platform "$DOCKER_PLATFORM" -f scripts/appimage/Dockerfile -t "$IMAGE" scripts
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
    # No launch step here, unlike the Linux arm: a macOS host cannot run a Linux
    # binary, so the artifact is built and handed over rather than tried.
    echo "==> AppImage:    $OUT/$CCK_APPIMAGE_NAME"
    echo "==> glibc floor: $CCK_GLIBC_FLOOR"
    echo "==> size:        $CCK_SIZE"

[doc("Build the AppImage, restart the daemon on it, and print its glibc floor (needs docker)")]
[windows]
[extension('.ps1')]
appimage:
    #!pwsh.exe
    $ErrorActionPreference = 'Stop'
    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        Write-Error 'docker is required to build the AppImage (Docker Desktop)'
    }
    # The dual-boot rule: this tree is SHARED with the Linux boot, and `target/`
    # holds the live binaries its PrintScreen shortcut launches. Everything here
    # lands in target-appimage/, which is git-excluded like target-win/.
    $out = Join-Path $PWD 'target-appimage'
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $envArgs = @()
    if ((Get-Command gh -ErrorAction SilentlyContinue) -and ($(gh auth status 2>$null; $?))) {
        foreach ($v in @('CCK_BAKED_GDRIVE_CLIENT_ID','CCK_BAKED_GDRIVE_CLIENT_SECRET','CCK_BAKED_ONEDRIVE_CLIENT_ID','CCK_BAKED_DROPBOX_CLIENT_ID')) {
            $val = (Get-Item "env:$v" -ErrorAction SilentlyContinue).Value
            if (-not $val) { $val = (gh variable get $v 2>$null) }
            if ($val) { $envArgs += @('-e', "$v=$val") }
        }
        Write-Host '==> Baked cloud ids from GitHub repository variables'
    }
    Write-Host '==> Preparing the build image (cached after the first run)...'
    docker build -f scripts/appimage/Dockerfile -t cck-appimage-base scripts
    docker run --rm -v "${PWD}:/src:ro" -v "${out}:/work" @envArgs `
        cck-appimage-base bash /src/scripts/appimage/build.sh
    # No launch step: a Windows host cannot run a Linux binary.
    Get-Content (Join-Path $out 'appimage.env') | ForEach-Object { Write-Host "==> $_" }

# Update the RUNNING resident daemon to the freshly built binary, on any
# platform: build, stop the old daemon, start the new one, and print the
# binary's full path (paste it into a system-level hotkey, e.g. COSMIC's
# custom shortcut, and it survives rebuilds because the path never changes).
[doc("Build, stop every running instance, and restart the resident daemon on it")]
[linux]
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    just build
    # EVERY process of this build, not just the resident daemon. A preview
    # editor, settings window or stale overlay left from the previous binary
    # keeps running against a now-deleted inode, so its `current_exe` respawns
    # fail silently and it goes on serving the old code.
    #
    # The pattern is anchored to the BINARY, not the repo name: the checkout path
    # itself contains "cosmic-capture-kit", so a bare match also hits an editor
    # with the source open, a shell sitting in the directory, and anything whose
    # temp dir is named after the repo (gpg-agent and scdaemon, in practice).
    # `(^|/)` accepts the relative form the daemon is launched with as well as an
    # absolute one, and `( |$)` stops it matching the directory.
    pkill -f '(^|/)target/release/cosmic-capture-kit( |$)' 2>/dev/null || true
    sleep 0.3
    setsid -f target/release/cosmic-capture-kit resident >/dev/null 2>&1
    sleep 1
    pgrep -f 'target/release/cosmic-capture-kit resident' >/dev/null \
        && echo "==> Daemon restarted on the fresh binary" \
        || { echo "==> ERROR: daemon did not come up"; exit 1; }
    echo "==> Binary path: $(realpath target/release/cosmic-capture-kit)"

[doc("Build, stop every running instance, and restart the resident daemon on it")]
[macos]
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    just build
    # EVERY process of this build, not just the resident daemon (see the Linux
    # recipe for why the pattern is anchored to the binary rather than the repo
    # name). Two patterns here: the bare `target/release` binary an earlier
    # version of this recipe launched directly, and the .app bundle this recipe
    # (and `just build`) actually uses.
    pkill -f '(^|/)target/release/cosmic-capture-kit( |$)' 2>/dev/null || true
    pkill -f 'Cosmic Capture Kit.app/Contents/MacOS/cosmic-capture-kit' 2>/dev/null || true
    sleep 0.3
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
    echo "==> Binary path: $(cd 'target/release/bundle/Cosmic Capture Kit.app/Contents/MacOS' && pwd)/cosmic-capture-kit"

[doc("Build, stop every running instance, and restart the resident daemon on it")]
[windows]
[extension('.ps1')]
dev:
    #!pwsh.exe
    $ErrorActionPreference = 'Stop'
    # A dev-loop build, NOT the msi packaging: plain release into target-win
    # (the dual-boot rule: never touch target/, that is the Linux boot's live
    # build), with the same baked-id fetch the packaged build performs.
    if ((Get-Command gh -ErrorAction SilentlyContinue) -and ($(gh auth status 2>$null; $?))) {
        foreach ($v in @('CCK_BAKED_GDRIVE_CLIENT_ID','CCK_BAKED_GDRIVE_CLIENT_SECRET','CCK_BAKED_ONEDRIVE_CLIENT_ID','CCK_BAKED_DROPBOX_CLIENT_ID')) {
            if (-not (Get-Item "env:$v" -ErrorAction SilentlyContinue)) {
                $val = (gh variable get $v 2>$null)
                if ($val) { Set-Item "env:$v" $val }
            }
        }
        Write-Host '==> Baked cloud ids from GitHub repository variables'
    }
    $env:CARGO_TARGET_DIR = 'target-win'
    # Windows locks a RUNNING exe against deletion, and the daemon this recipe
    # started last time is still running from target-win\release, so a build
    # that needs to relink dies with "failed to remove file ... .exe". Windows
    # DOES allow renaming a running exe, so the old binary is moved aside
    # before the build and the aside copies are cleaned up after the stop
    # below. The old daemon keeps serving from the renamed file for the whole
    # build, the same no-downtime property the Linux and mac recipes get for
    # free from their filesystems. The `.old-$PID` fallback covers an aside
    # file still locked by a daemon from an earlier failed run.
    $exeFile = 'target-win\release\cosmic-capture-kit.exe'
    if (Test-Path $exeFile) {
        Remove-Item "$exeFile.old*" -Force -ErrorAction SilentlyContinue
        $aside = "$exeFile.old"
        if (Test-Path $aside) { $aside = "$exeFile.old-$PID" }
        Move-Item $exeFile $aside -Force
    }
    cargo build --release --no-default-features
    if ($LASTEXITCODE -ne 0) {
        Write-Error '==> build failed; the running daemon is untouched (still on the renamed old exe)'
    }
    # The pinned ffmpeg/tesseract sidecars, fetched once (stamped, so a later
    # run is free) and staged NEXT TO the exe. util::locate_tool prefers an
    # exe-adjacent sidecar over PATH, so the dev daemon exercises the same
    # bundled tools a shipped MSI carries instead of whatever the machine
    # happens to have installed (the DRAGON-531 reasoning; the copy mirrors
    # win-dev-install.ps1, which does the identical staging for the installed
    # QA build). Without this, a bare dev build has no OCR and no recording on
    # a machine with nothing on PATH, which mac's `dev` never suffers because
    # its recipe builds the sidecar-carrying .app.
    pwsh scripts/fetch-win-vendor.ps1
    if ($LASTEXITCODE -ne 0) { Write-Error 'scripts/fetch-win-vendor.ps1 failed' }
    # EVERY process of this build, not just the resident daemon: a preview
    # editor, settings window or stale overlay left from the previous binary
    # keeps running against the old exe and goes on serving the old code.
    #
    # Filtering on the process NAME is already precise here, unlike the unix
    # recipes which have to anchor a cmdline pattern to avoid matching the
    # checkout path. Dropping the `resident` filter is the whole change.
    Get-CimInstance Win32_Process -Filter "Name='cosmic-capture-kit.exe'" |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 300
    # Nothing runs from the aside copies anymore, so they can go.
    Remove-Item "$exeFile.old*" -Force -ErrorAction SilentlyContinue
    # Stop first, copy second: a capture child mid-scan holds its tesseract or
    # ffmpeg open, and copying over a running exe fails on Windows.
    $rel = 'target-win\release'
    $vendorFfmpeg = 'vendor\ffmpeg\windows-x86_64'
    $vendorTess = 'vendor\tesseract\windows-x86_64'
    # The exes AND their DLLs: BtbN's shared ffmpeg build is stubs beside the
    # libav DLLs, so the exes alone cannot start.
    Get-ChildItem $vendorFfmpeg -File | Where-Object { $_.Extension -in '.exe', '.dll' } |
        ForEach-Object { Copy-Item $_.FullName (Join-Path $rel $_.Name) -Force }
    Copy-Item (Join-Path $vendorTess 'tesseract.exe') $rel -Force
    Get-ChildItem (Join-Path $vendorTess '*.dll') | ForEach-Object { Copy-Item $_.FullName $rel -Force }
    # The WHOLE tessdata dir: it carries `configs/` as well as the language
    # file, and the OCR path passes `tsv`, which tesseract resolves as a
    # CONFIG FILE NAME. Language data alone leaves OCR silently empty.
    Copy-Item (Join-Path $vendorTess 'tessdata') $rel -Recurse -Force
    Write-Host '==> Sidecars staged next to the exe (ffmpeg + tesseract)'
    # Win32_Process.Create, NOT Start-Process: Windows Terminal wraps each
    # tab's process tree in a kill-on-close Job Object, a Start-Process child
    # inherits that job, and closing the terminal window then kills the
    # daemon with it. WMI creates the process from the WMI provider host,
    # outside the terminal's job and ancestry entirely. Same class of problem
    # and same shape of fix as the macOS recipe's `open -a` handoff to
    # launchd (see that recipe's comment). The exe is GUI-subsystem, so no
    # console window appears and no ShowWindow plumbing is needed.
    $exe = (Resolve-Path 'target-win\release\cosmic-capture-kit.exe').Path
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine = "`"$exe`" resident"; CurrentDirectory = "$PWD" }
    if ($r.ReturnValue -ne 0) { Write-Error "==> ERROR: daemon launch failed (Win32_Process.Create returned $($r.ReturnValue))" }
    Start-Sleep -Seconds 1
    if (-not (Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue)) {
        Write-Error '==> ERROR: daemon did not come up'
    }
    Write-Host '==> Daemon restarted on the fresh binary'
    Write-Host "==> Binary path: $exe"
