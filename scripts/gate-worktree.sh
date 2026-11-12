#!/usr/bin/env bash
# gate-worktree.sh: the ONE way a gate gets a worktree and a build directory of
# its own, and the ONE thing that removes them again, PASS OR FAIL. Run it, do
# not source it, and run it from the repo root:
#
#   bash scripts/gate-worktree.sh d401 -- cargo test
#   bash scripts/gate-worktree.sh d401 -- bash -c 'cargo build && cargo clippy --all-targets'
#   bash scripts/gate-worktree.sh --ref shane/dev d401 -- cargo test --no-default-features
#
# `just gate d401 cargo test` is the same thing. Invoked through `bash` on
# purpose, so it needs no executable bit (the repo sets core.fileMode = false, so
# a `chmod +x` here would never reach the index and the script would fail on a
# fresh clone).
#
# ── Why the entry point RUNS the gate ────────────────────────────────────────
#
# Because the version that hands back a path and trusts the caller to tidy up is
# the version that filled three machines. This script owns the whole life of the
# scratch: it creates the worktree and the target dir, runs YOUR command inside
# them, and removes both from an EXIT trap, so a pass, a failed test, a Ctrl-C,
# a `kill`, a syntax error in your own command and an `exit 1` halfway through
# all end the same way. The command's exit status is this script's exit status,
# so a gate that fails still fails.
#
# There is no mode that leaves the scratch behind by accident. `--keep` exists,
# it is explicit, and it prints the two directories and the one command that
# collects them.
#
# ── What it gives the command ────────────────────────────────────────────────
#
#   cwd                   the worktree (unless --no-worktree)
#   CARGO_TARGET_DIR      <base>/<name>-target, so this gate's build never
#                         collides with the repo's own target/ or a sibling
#                         agent's gate
#   CCK_GATE_NAME         the name, for anything that wants to label itself
#   CCK_GATE_DIR          the worktree, or the repo root under --no-worktree
#   CCK_GATE_TARGET_DIR   the same path as CARGO_TARGET_DIR
#   CCK_GATE_EXPORT_DIR   only with --export: an empty tree for
#                         `scripts/publish-public.sh --out`, removed with the
#                         rest, which is the residue that rule names third
#
# --no-worktree covers the last one CLAUDE.md names: the Windows cross-check,
# which used to build into a hand-picked /var/tmp directory that nothing ever
# swept. It runs in the CURRENT checkout with only a scratch CARGO_TARGET_DIR,
# and that dir is removed with everything else:
#
#   bash scripts/gate-worktree.sh --no-worktree wincheck -- \
#       cargo clippy --target x86_64-pc-windows-msvc --no-default-features --all-targets
#
# ── Portable on purpose ──────────────────────────────────────────────────────
#
# Linux and macOS, plain bash, no GNU-only flags and no filesystem tricks. See
# scripts/gate-dirs.sh for why there is no PowerShell counterpart.
set -uo pipefail
# NOT `set -e`. The gate command's failure is an ORDINARY outcome here, to be
# caught and reported, never something that skips the cleanup on its way out.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/gate-dirs.sh"

usage() { sed -n '2,53p' "${BASH_SOURCE[0]}"; }

REF=""
NAME=""
WANT_WORKTREE=1
WANT_EXPORT=0
KEEP=0
SELF_CHECK=0

while [ $# -gt 0 ]; do
    case "$1" in
        --ref) REF="$2"; shift 2 ;;
        --no-worktree) WANT_WORKTREE=0; shift ;;
        --export) WANT_EXPORT=1; shift ;;
        --keep) KEEP=1; shift ;;
        --base) CCK_GATE_BASE="$2"; shift 2 ;;
        --self-check) SELF_CHECK=1; shift ;;
        -h | --help) usage; exit 0 ;;
        --) shift; break ;;
        -*) echo "==> gate-worktree.sh: unknown option: $1" >&2; exit 2 ;;
        *)
            if [ -n "$NAME" ]; then
                echo "==> gate-worktree.sh: unexpected argument: $1 (put the command after --)" >&2
                exit 2
            fi
            NAME="$1"; shift ;;
    esac
done

# ── The self-check ───────────────────────────────────────────────────────────
#
# There is no shell test harness in this repo, so the proof lives here and runs
# against a throwaway repo in a temp dir. It never touches the real base dir.
#
#   bash scripts/gate-worktree.sh --self-check
self_check() {
    local tmp rc out
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/cck-gate-selfcheck.XXXXXX")" || return 1
    local base="$tmp/base" repo="$tmp/repo"
    local failures=0

    check() {
        if [ "$1" = 0 ]; then
            echo "    ok    $2"
        else
            echo "    FAIL  $2"
            failures=$((failures + 1))
        fi
    }
    absent() { [ ! -e "$1" ]; }

    mkdir -p "$repo"
    (
        cd "$repo" || exit 1
        git init -q .
        git config user.email selfcheck@example.invalid
        git config user.name "Gate Self Check"
        echo hello > file.txt
        git add file.txt
        git commit -q -m "seed"
    ) || { echo "    FAIL  could not build the throwaway repo"; rm -rf "$tmp"; return 1; }

    cd "$repo" || return 1
    export CCK_GATE_BASE="$base"

    echo "==> gate-worktree.sh self-check in $tmp"

    # 1. A passing gate leaves nothing behind, and runs where it says it does.
    out="$(bash "$SCRIPT_DIR/gate-worktree.sh" --base "$base" pass -- \
        bash -c 'test -f file.txt && printf "%s\n" "$CARGO_TARGET_DIR"' 2>/dev/null | tail -n 1)"
    rc=$?
    check "$rc" "a passing gate exits 0"
    [ "$out" = "$base/pass-target" ]
    check $? "the command sees CARGO_TARGET_DIR pointing at <name>-target"
    absent "$base/pass" && absent "$base/pass-target"
    check $? "a passing gate removes its worktree and target dir"

    # 2. A failing gate propagates its status and STILL removes everything.
    bash "$SCRIPT_DIR/gate-worktree.sh" --base "$base" fail -- \
        bash -c 'mkdir -p "$CARGO_TARGET_DIR/junk"; exit 7' >/dev/null 2>&1
    [ $? -eq 7 ]
    check $? "a failing gate exits with the command's own status"
    absent "$base/fail" && absent "$base/fail-target"
    check $? "a FAILING gate removes its worktree and target dir"

    # 3. The command killing itself is the Ctrl-C case: the trap still fires.
    bash "$SCRIPT_DIR/gate-worktree.sh" --base "$base" interrupted -- \
        bash -c 'mkdir -p "$CARGO_TARGET_DIR/junk"; kill -INT $$; sleep 5' >/dev/null 2>&1
    absent "$base/interrupted" && absent "$base/interrupted-target"
    check $? "an interrupted gate removes its worktree and target dir"

    # 4. --export gets a third directory, and it goes too.
    out="$(bash "$SCRIPT_DIR/gate-worktree.sh" --base "$base" --export exp -- \
        bash -c 'printf "%s\n" "$CCK_GATE_EXPORT_DIR"' 2>/dev/null | tail -n 1)"
    [ "$out" = "$base/exp-export" ]
    check $? "--export exports CCK_GATE_EXPORT_DIR"
    absent "$base/exp-export"
    check $? "--export's tree is removed with the rest"

    # 5. --no-worktree runs in the checkout and still gets a swept target dir.
    out="$(bash "$SCRIPT_DIR/gate-worktree.sh" --base "$base" --no-worktree scratch -- \
        bash -c 'printf "%s\n" "$PWD"' 2>/dev/null | tail -n 1)"
    [ "$out" = "$(cd "$repo" && pwd -P)" ]
    check $? "--no-worktree runs in the current checkout"
    absent "$base/scratch-target"
    check $? "--no-worktree still removes its scratch target dir"

    # 6. The 509 GB shape: a killed run's residue, git no longer registering it.
    #    This is built by hand, exactly as it was found on disk.
    mkdir -p "$base/ghost" "$base/ghost-target/debug"
    echo "gitdir: /nowhere" > "$base/ghost/.git"
    bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" >/dev/null 2>&1
    absent "$base/ghost" && absent "$base/ghost-target"
    check $? "the sweep collects an unregistered worktree AND its -target sibling"

    # 7. A live gate is not swept out from under a sibling agent.
    mkdir -p "$base/busy" "$base/busy-target"
    echo $$ > "$base/busy.lock"
    bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" >/dev/null 2>&1
    [ -d "$base/busy" ] && [ -d "$base/busy-target" ]
    check $? "the sweep leaves a gate whose lock names a live pid alone"

    # 8. A lock naming a dead pid is residue, not a live gate.
    #    Pid 2^22 + 1 is above every default pid_max, so it cannot be running.
    echo 4194305 > "$base/busy.lock"
    bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" >/dev/null 2>&1
    absent "$base/busy" && absent "$base/busy-target" && absent "$base/busy.lock"
    check $? "the sweep collects a gate whose lock names a dead pid"

    # 9. A registered worktree that is RECENT is left alone whatever state it
    #    is in. This is the sibling agent mid-gate, and the reason the sweep is
    #    safe to run from `just build`.
    git -C "$repo" worktree add -q --detach "$base/fresh" HEAD 2>/dev/null
    bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" >/dev/null 2>&1
    [ -d "$base/fresh" ]
    check $? "the sweep leaves a recent registered worktree alone"
    git -C "$repo" worktree remove --force "$base/fresh" >/dev/null 2>&1

    # 10. A registered worktree with uncommitted changes is NAMED and KEPT, even
    #     when it is old enough to collect. `touch -t` backdates it; that form is
    #     the one both GNU and BSD touch read.
    git -C "$repo" worktree add -q --detach "$base/dirty" HEAD 2>/dev/null
    echo "work in progress" > "$base/dirty/file.txt"
    touch -t 202001010000 "$base/dirty"
    out="$(bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" 2>&1)"
    [ -d "$base/dirty" ]
    check $? "the sweep keeps an aged registered worktree that is dirty"
    case "$out" in *"KEEPING"*"uncommitted changes"*) true ;; *) false ;; esac
    check $? "and says so by name rather than removing it quietly"

    # 11. The same worktree, clean, IS collected once it is over the age limit.
    git -C "$base/dirty" checkout -q -- file.txt
    touch -t 202001010000 "$base/dirty"
    bash "$SCRIPT_DIR/prune-worktrees.sh" --base "$base" >/dev/null 2>&1
    absent "$base/dirty"
    check $? "the sweep collects an aged registered worktree that is clean"
    git -C "$repo" worktree list --porcelain | grep -q "$base/dirty"
    [ $? -ne 0 ]
    check $? "and drops its registration, so git worktree list stays honest"

    cd / || true
    rm -rf "$tmp"
    if [ "$failures" -eq 0 ]; then
        echo "==> self-check passed"
        return 0
    fi
    echo "==> self-check FAILED ($failures)"
    return 1
}

if [ "$SELF_CHECK" -eq 1 ]; then
    self_check
    exit $?
fi

if [ -z "$NAME" ]; then
    echo "==> gate-worktree.sh: a gate needs a name" >&2
    usage >&2
    exit 2
fi
case "$NAME" in
    */* | .. | . | *' '*)
        echo "==> gate-worktree.sh: '$NAME' is not a usable gate name (no slashes or spaces)" >&2
        exit 2 ;;
esac
if [ $# -eq 0 ]; then
    echo "==> gate-worktree.sh: nothing to run. Put the gate command after --." >&2
    usage >&2
    exit 2
fi

REPO="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ -z "$REPO" ]; then
    echo "==> gate-worktree.sh must run inside the repo" >&2
    exit 2
fi
cd "$REPO" || exit 2
[ -n "$REF" ] || REF="$(git rev-parse HEAD)"

# Collect whatever an earlier run was killed holding, BEFORE allocating our own.
# This is the half of the fix that catches the runs no exit trap ever reached.
bash "$SCRIPT_DIR/prune-worktrees.sh" --quiet --base "$CCK_GATE_BASE"

cck_gate_mkbase >/dev/null

if cck_gate_in_use "$NAME"; then
    echo "==> gate '$NAME' is already running (pid $(head -n 1 "$(cck_gate_lock "$NAME")"))" >&2
    echo "    Pick another name, or wait for it to finish." >&2
    exit 2
fi
# A dead run's leftovers under this exact name go now, so the git worktree add
# below is not refused by a directory nobody owns any more.
cck_gate_release "$NAME"

WT="$(cck_gate_worktree "$NAME")"
TARGET="$(cck_gate_target "$NAME")"
EXPORT_DIR="$(cck_gate_export "$NAME")"
LOCK="$(cck_gate_lock "$NAME")"

CCK_GATE_RC=""
cleanup() {
    local rc="${CCK_GATE_RC:-$?}"
    trap - EXIT INT TERM HUP
    cd "$REPO" 2>/dev/null || cd / || true
    if [ "$KEEP" -eq 1 ]; then
        echo "==> --keep: left behind, collect with 'just prune-worktrees'"
        [ "$WANT_WORKTREE" -eq 1 ] && echo "    $WT"
        echo "    $TARGET"
        [ "$WANT_EXPORT" -eq 1 ] && echo "    $EXPORT_DIR"
        rm -f "$LOCK" 2>/dev/null
        exit "$rc"
    fi
    cck_gate_release "$NAME"
    exit "$rc"
}
# EXIT alone would miss a Ctrl-C in some shells and every signal in others, so
# the four that end a build are named. Whatever ends this script, the scratch
# goes with it.
trap cleanup EXIT INT TERM HUP

echo "$$" > "$LOCK"
printf 'gate %s started %s\n' "$NAME" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >> "$LOCK"

if [ "$WANT_WORKTREE" -eq 1 ]; then
    # --detach on purpose: a gate checks out a COMMIT to test it, it never owns
    # a branch. It also means the sweep can tell a gate worktree from somebody's
    # hand-made one, and refuse to collect the second.
    if ! git worktree add -q --detach "$WT" "$REF"; then
        echo "==> gate-worktree.sh: could not create a worktree at $WT for $REF" >&2
        CCK_GATE_RC=2
        exit 2
    fi
    GATE_DIR="$WT"
else
    GATE_DIR="$REPO"
fi

mkdir -p "$TARGET"
[ "$WANT_EXPORT" -eq 1 ] && mkdir -p "$EXPORT_DIR"

export CARGO_TARGET_DIR="$TARGET"
export CCK_GATE_NAME="$NAME"
export CCK_GATE_DIR="$GATE_DIR"
export CCK_GATE_TARGET_DIR="$TARGET"
[ "$WANT_EXPORT" -eq 1 ] && export CCK_GATE_EXPORT_DIR="$EXPORT_DIR"

cd "$GATE_DIR" || { CCK_GATE_RC=2; exit 2; }
"$@"
CCK_GATE_RC=$?
exit "$CCK_GATE_RC"
