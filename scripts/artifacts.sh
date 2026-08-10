# artifacts.sh — the ONE build-output directory, and the stable path into it
# (DRAGON-590). SOURCED by the justfile's bash recipes, never executed, so it
# needs no executable bit (the repo sets core.fileMode = false, and a new script
# that only works because of a local chmod dies on a fresh clone).
#
#   . scripts/artifacts.sh
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Every build recipe used to leave its artifact somewhere else: `just build` in
# target/release, `just dist` in target-dist, `just appimage` in target-appimage,
# `just flatpak` nowhere at all (an installed app). So a desktop shortcut had to
# name ONE of them, and the day a fix was rebuilt into one artifact but not the
# other, the owner's PrintScreen key and their --active-window key were running
# different code. That looked exactly like a live bug for as long as it took to
# read the `package:` field in the debug log.
#
# So: ONE directory, the same relative path on Linux, macOS and Windows, with
# every kind of artifact side by side, plus ONE stable name inside it that always
# means "the build I made last". Point a shortcut at the stable name once and it
# never has to be edited again.
#
# ── Only FINISHED artifacts go in target/ ────────────────────────────────────
#
# This deliberately amends the old rule that nothing but cargo may write into
# `target/`. The reason for that rule was never the directory: it was that a
# CONTAINER build tree writing there (as root, or over the live binary the
# PrintScreen shortcut launches) would break a running daemon and leave
# root-owned state behind. That still holds and is unchanged. The container work
# dirs stay exactly where they were, OUTSIDE target/:
#
#   target-appimage/   the AppImage container's CARGO_HOME + CARGO_TARGET_DIR
#   target-dist/       the Rocky 9 container's output mount (root-owned)
#   target-win/        the Windows boot's whole cargo tree (dual-boot: never touch)
#
# What lands in target/artifacts/ is one finished file per build, written by the
# CALLING user, by rename. `cck_publish` never writes over an existing artifact
# in place: it copies to `<name>.part` and renames, because the kernel refuses to
# write to a running executable ("Text file busy") and on this machine the
# previous AppImage is normally still running as the resident daemon. A rename is
# immune to that, and a process already running the old inode keeps it until it
# exits.
#
# `cargo clean` DOES take this directory with it, since it is under target/. That
# is an accepted cost: the alternative is a fifth top-level build dir, which is
# the problem this file exists to remove.
#
# ── The symlink survives an AppImage self-update (measured) ──────────────────
#
# Worth knowing before someone talks themselves out of this arrangement: the
# AppImage updates ITSELF IN PLACE, `install_linux_appimage` renaming the new
# release over `$APPIMAGE`, so the obvious worry is that an update would land on
# the stable symlink and turn it into an ordinary file. It does not. The type-2
# runtime resolves the invocation path before it exports the variable. Launched
# through a symlink, the app sees:
#
#   ARGV0=/var/tmp/cck-symlink-probe                    the symlink
#   APPIMAGE=/…/target/artifacts/CosmicCaptureKit-x86_64.AppImage   the real file
#
# So an update replaces the artifact and the symlink keeps pointing at it, which
# is exactly what a stable path has to do.

# The output directory, relative to the repo root. Same on every platform; the
# Windows recipes hardcode the same two path components.
CCK_ARTIFACTS_SUBDIR="target/artifacts"

# The stable name inside it. `cosmic-capture-kit`, so a shortcut or a shell line
# reads the same as it would for an installed program.
CCK_STABLE_NAME="cosmic-capture-kit"

# The absolute output directory, created if missing.
cck_artifacts_dir() {
    local dir="$PWD/$CCK_ARTIFACTS_SUBDIR"
    mkdir -p "$dir"
    printf '%s\n' "$dir"
}

# cck_publish <src> [name] — install a finished FILE artifact into the directory
# and echo where it landed. Copies to `<name>.part` and renames (see the header:
# "Text file busy" on a running artifact). Mode carries over from the source, so
# an executable stays executable.
cck_publish() {
    local src="$1"
    local name="${2:-$(basename "$src")}"
    local dir dst
    dir="$(cck_artifacts_dir)"
    dst="$dir/$name"
    cp -f "$src" "$dst.part"
    mv -f "$dst.part" "$dst"
    printf '%s\n' "$dst"
}

# cck_publish_move <src> [name] — the same, then remove the source. For an
# artifact whose only other home is a container scratch dir, so one build does
# not leave two 33MB copies of the same AppImage on disk.
cck_publish_move() {
    local dst
    dst="$(cck_publish "$@")"
    rm -f "$1"
    printf '%s\n' "$dst"
}

# cck_link <name> <target> — (re)point a named symlink in the directory and echo
# it. `-n` matters: without it, re-pointing a link that currently resolves to a
# DIRECTORY (the macOS .app bundle) creates a link INSIDE the bundle instead of
# replacing the link.
cck_link() {
    local name="$1" target="$2"
    local dir
    dir="$(cck_artifacts_dir)"
    ln -sfn "$target" "$dir/$name"
    printf '%s\n' "$dir/$name"
}

# cck_stable <target> — point the stable name at <target> and print the closing
# line. UNCONDITIONAL by contract: it always repoints, never "only if missing",
# because a stale link left over from a different artifact is precisely the bug
# this whole arrangement exists to prevent.
cck_stable() {
    local link
    link="$(cck_link "$CCK_STABLE_NAME" "$1")"
    cck_say_use "$link"
}

# cck_stable_launcher <real executable> — the macOS form of cck_stable, and the
# one place the two platforms diverge.
#
# macOS builds a .app BUNDLE, so there is no bare executable at the top level to
# symlink. Symlinking the one INSIDE the bundle is the obvious move and is the
# move not taken: the process would be exec'd by the symlink's path, and whether
# CoreFoundation still resolves the enclosing bundle from there is not something
# this repo can verify from the Linux box. Getting it wrong costs the Info.plist,
# the code-signature identity and with it the TCC screen-recording grant, which
# fails as "the app just stopped being allowed to record" with no obvious cause.
#
# A two-line launcher that `exec`s the REAL absolute path has none of that doubt:
# the kernel sees exactly the path launchd would have used, so bundle identity is
# whatever it always was. It costs one short-lived /bin/sh per launch. That buys
# macOS the SAME stable name as Linux, which is the point.
#
# The .app itself still gets a symlink next to it (see the recipe), because
# `open`, `open -a` and a Finder double-click all want the bundle, and the dev
# recipe's launchd handoff goes through `open -a`.
cck_stable_launcher() {
    local real="$1"
    local dir link
    dir="$(cck_artifacts_dir)"
    link="$dir/$CCK_STABLE_NAME"
    # Written aside and renamed, same reason as cck_publish.
    {
        printf '#!/bin/sh\n'
        printf '# Generated by a `just` build recipe (scripts/artifacts.sh). Do not edit;\n'
        printf '# the next build overwrites it.\n'
        printf 'exec "%s" "$@"\n' "$real"
    } > "$link.part"
    chmod 0755 "$link.part"
    mv -f "$link.part" "$link"
    cck_say_use "$link"
}

# The closing line, printed by every build recipe as the LAST thing on screen,
# after the glibc floor / proton-drive / daemon lines. Cyan, matching
# scripts/win-package.ps1's `Write-Host "==> $m" -ForegroundColor Cyan`, which is
# the only colour idiom this repo's build tooling already had.
#
# It names an ABSOLUTE path and says what the path is FOR, because it goes into
# two places that never run from the repo root: a COSMIC custom shortcut and a
# terminal. The example carries a flag so it is obvious that arguments pass
# through, which is true of all three Linux artifact kinds (the plain binary and
# the AppImage take flags directly; the Flatpak's exported launcher forwards
# "$@").
cck_say_use() {
    local link="$1"
    local c='' r=''
    if [ -t 1 ]; then c=$'\033[1;36m'; r=$'\033[0m'; fi
    printf '%s==> Use this path for shortcuts and CLI commands:%s\n' "$c" "$r"
    printf '%s       %s%s\n' "$c" "$link" "$r"
    printf '%s       e.g.  %s --region%s\n' "$c" "$link" "$r"
}

# cck_say_artifact <path> — for a recipe that produced an artifact THIS machine
# cannot run, so the stable path must not move to it: the AppImage cross-built on
# macOS or Windows, and the Windows MSI (an installer, not the app). Saying "use
# this path" there would be a lie, and silently repointing the stable name at an
# unrunnable file is the exact failure this all exists to prevent.
cck_say_artifact() {
    local path="$1"
    local c='' r=''
    if [ -t 1 ]; then c=$'\033[1;36m'; r=$'\033[0m'; fi
    printf '%s==> Artifact: %s%s\n' "$c" "$path" "$r"
    printf '    (not runnable on this host, so the stable path is unchanged)\n'
}
