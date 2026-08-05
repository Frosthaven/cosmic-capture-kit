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

# Bare `just` lists recipes instead of running the first one by surprise.
default:
    @just --list

# Documentation site: serve it locally with live reload, on every platform.
docs:
    # Needs the pinned tools once: `pip install -r docs-requirements.txt` (a virtualenv is fine).
    mkdocs serve

# macOS: build + bundle the .app (signed if a Developer ID identity is available).
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
    pwsh scripts/win-package.ps1
    Write-Host 'Built: target-win\CosmicCaptureKit-*.msi'

# Linux: plain release build, retrying without zero-copy if the first attempt fails.
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
