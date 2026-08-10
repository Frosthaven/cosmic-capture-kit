#!/usr/bin/env bash
# stop-all.sh — stop EVERY running instance of this app, whatever artifact it
# came from (DRAGON-590). Run it, don't source it:
#
#   bash scripts/stop-all.sh
#
# Invoked through `bash` on purpose, so it needs no executable bit (the repo sets
# core.fileMode = false, so a `chmod +x` here would never reach the index and the
# script would fail on a fresh clone).
#
# ── The bug this fixes ───────────────────────────────────────────────────────
#
# Three recipes each grew their OWN partial version of "stop every running
# instance", and each of them only really reached its own kind of build. Build
# the AppImage, then build the Flatpak, and you end up with TWO tray icons, two
# resident daemons, and capture children spawning from two different builds.
# So the sweep lives here, once, and every recipe that restarts a daemon calls
# it. Adding a new artifact kind means adding a case HERE, not a fourth copy of
# this logic.
#
# The specific hole, measured: a Flatpak instance's argv on the host is
# `cosmic-capture-kit resident`, with NO leading path, because the sandbox execs
# it by bare name. Every pattern the recipes carried was anchored to a path
# (`/app/bin/cosmic-capture-kit`, `target/release/...`), so NONE of them matched
# it and `just appimage` cheerfully left the Flatpak's tray icon in place.
#
# ── Why they can coexist at all (a product gap, not a recipe gap) ────────────
#
# The single-instance locks do not see across the sandbox boundary. The daemon
# lock is a flock in `util::runtime_dir()`, i.e. under $XDG_RUNTIME_DIR, and a
# Flatpak gets its OWN runtime dir, so the Flatpak's daemon lock is invisible to a
# native daemon and vice versa. Neither can tell the other exists, so neither
# stands down. That is a real limitation, but it only bites a machine carrying
# more than one artifact at once, which is a DEVELOPER situation rather than a
# user one, and fixing it properly means a cross-sandbox rendezvous (a well-known
# D-Bus name, say) that is well outside a build recipe. Do not delete this sweep
# as redundant with the locks; the locks cannot do its job.
#
# ── How instances are found: the EXE, not the argv ───────────────────────────
#
# On Linux this reads /proc/<pid>/exe rather than matching command lines, and
# that is the whole reason it works where the old patterns did not. Measured, one
# machine, three artifacts running at once:
#
#   /mnt/…/target/release/cosmic-capture-kit          source build
#   /tmp/.mount_CosmicnEDILP/usr/bin/cosmic-capture-kit   AppImage (FUSE mount)
#   /app/bin/cosmic-capture-kit                        Flatpak (inside the sandbox)
#
# All three end in the binary's name no matter what argv says, and bwrap (exe
# /usr/bin/bwrap) is excluded for free, which is what we want: we signal the app,
# not its sandbox wrapper. It also cannot match an editor with the source open or
# a shell sitting in the checkout, which a name-based pattern always can, because
# the checkout directory is itself called "cosmic-capture-kit".
#
# Only processes we OWN are considered: readlink on another user's /proc entry
# fails anyway, and the explicit ownership test says so out loud.
#
# macOS has no /proc, so it falls back to pgrep over path-anchored patterns. That
# is sound there because macOS has only two shapes (the source build and the .app
# bundle) and neither is sandboxed.
#
# ── SIGTERM, then wait, then escalate ────────────────────────────────────────
#
# SIGTERM is the CLEAN stop. The Linux resident installs its own SIGTERM handler
# (platform/linux/daemon.rs) that shuts the ksni item down before exiting, which
# is the same thing the settings UI's `SetResident(false)` does through
# `instance::signal_daemon_quit`. A bare SIGKILL skips that and leaves a GHOST
# tray icon until the status-notifier host times the item out, which is the
# visible symptom this is judged on.
#
# The old recipes then slept 0.3s and immediately launched the replacement, which
# is not long enough for that teardown to finish. So this WAITS for the processes
# to actually be gone, up to a bounded budget, and only escalates for whatever
# outlives it.
set -uo pipefail
# NOT `set -e`: a sweep that finds nothing running must never fail its caller.

APP_ID=dev.thedragon.CosmicCaptureKit

# The pids of every running instance we own, one per line.
if [ -d /proc ]; then
    cck_matches() {
        local d pid exe
        for d in /proc/[0-9]*; do
            pid="${d#/proc/}"
            [ "$pid" = "$$" ] && continue
            [ "$pid" = "$PPID" ] && continue
            [ -O "$d" ] || continue
            exe="$(readlink "$d/exe" 2>/dev/null)" || continue
            # A rebuilt binary leaves the running process pointing at a deleted
            # inode, which readlink reports with this suffix.
            exe="${exe% (deleted)}"
            # A cargo DEBUG build is never a daemon: it is `cargo run`, or the
            # `tests/cli.rs` integration tests driving the compiled binary. Those
            # can be running in a parallel worktree with its own CARGO_TARGET_DIR,
            # and a build recipe must not fail somebody else's test run. Every
            # artifact this sweep is actually about is a release build.
            case "$exe" in */debug/*) continue ;; esac
            case "$exe" in
                */cosmic-capture-kit) printf '%s\n' "$pid" ;;
                # The AppImage runtime's own process (it holds the FUSE mount the
                # app above is running out of). Killing the app alone is enough,
                # since the mount keeper exits with it, but naming it here means
                # the "are they gone yet" wait cannot return early.
                */CosmicCaptureKit-*.AppImage) printf '%s\n' "$pid" ;;
            esac
        done
    }
else
    # macOS. Anchored to the binary, never to the project name: the checkout
    # directory is itself called "cosmic-capture-kit", so a bare match would also
    # hit an editor with the source open.
    MAC_PATTERNS=(
        '(^|/)target/artifacts/cosmic-capture-kit( |$)'
        '(^|/)target/release/cosmic-capture-kit( |$)'
        'Cosmic Capture Kit\.app/Contents/MacOS/cosmic-capture-kit'
    )
    cck_matches() {
        local p
        for p in "${MAC_PATTERNS[@]}"; do
            pgrep -f "$p" 2>/dev/null
        done | sort -un | grep -v -x -e "$$" -e "$PPID"
    }
fi

cck_signal() {
    local sig="$1" pid
    for pid in $(cck_matches); do
        kill "-$sig" "$pid" 2>/dev/null
    done
    return 0
}

before="$(cck_matches)"
count="$(printf '%s' "$before" | grep -c . || true)"

if [ "$count" -eq 0 ]; then
    echo "==> No running instances to stop"
    exit 0
fi

echo "==> Stopping $count running process(es) of this app (any build)"

# 1. Ask nicely, so a resident tears its own tray item down.
cck_signal TERM

# 2. Wait for them to actually be gone. Bounded, because nothing here may hang a
#    build recipe.
deadline=$((SECONDS + 6))
while [ "$SECONDS" -lt "$deadline" ]; do
    [ -z "$(cck_matches)" ] && break
    sleep 0.2
done

# 3. Escalate on whatever outlived the budget. `flatpak kill` first: it is the
#    documented way to stop a sandboxed instance and can reach one a host signal
#    somehow did not. It exits non-zero and prints when the app is not running,
#    hence the redirects.
if [ -n "$(cck_matches)" ]; then
    if command -v flatpak >/dev/null 2>&1; then
        flatpak kill "$APP_ID" >/dev/null 2>&1
    fi
    sleep 0.5
    if [ -n "$(cck_matches)" ]; then
        echo "==> Some processes ignored SIGTERM; killing them"
        cck_signal KILL
        sleep 0.3
    fi
fi

remaining="$(cck_matches)"
if [ -n "$remaining" ]; then
    echo "==> WARNING: still running after SIGKILL: $(printf '%s' "$remaining" | tr '\n' ' ')"
    exit 0
fi

echo "==> All instances stopped"
