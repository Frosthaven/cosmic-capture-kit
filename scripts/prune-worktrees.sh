#!/usr/bin/env bash
# prune-worktrees.sh: the ONE sweep that collects gate residue. Run it, do not
# source it, and run it from inside the repo:
#
#   bash scripts/prune-worktrees.sh              # collect, and say what went
#   bash scripts/prune-worktrees.sh --dry-run    # say what WOULD go, touch nothing
#   bash scripts/prune-worktrees.sh --quiet      # only speak when something goes
#
# `just prune-worktrees` is the same thing. Invoked through `bash` on purpose,
# so it needs no executable bit (the repo sets core.fileMode = false, so a
# `chmod +x` here would never reach the index and the script would fail on a
# fresh clone).
#
# ── What it is for ───────────────────────────────────────────────────────────
#
# scripts/gate-worktree.sh removes its own worktree and target dir on the way
# out, pass or fail, and that covers every gate that gets to run its exit trap.
# This sweep is for the ones that do not: a killed shell, a reboot, an OOM, or
# an agent that went around the entry point and hand-rolled `git worktree add`
# the way every agent used to. Residue from those is collected by the NEXT gate
# rather than kept forever, which is why gate-worktree.sh calls this at its own
# start and why `just build` calls it too. Collection has to be somebody's
# routine job, not somebody's good intention.
#
# ── What counts as residue ───────────────────────────────────────────────────
#
# For each entry directly under the base dir (~/.cck-worktrees by default), with
# `<name>-target` and `<name>-export` read as belonging to `<name>`:
#
#   in use          a `<name>.lock` naming a LIVE pid. Left alone, silently.
#                   This is what keeps the sweep safe to run while a sibling
#                   agent's gate is mid-build.
#   registered      `git worktree list` knows the worktree. Kept, unless it is
#                   older than --age-days AND clean AND detached, which is the
#                   shape an abandoned gate leaves. Anything with uncommitted
#                   changes, or sitting on a named branch, is NAMED and KEPT:
#                   a sweep that silently eats somebody's work in progress is
#                   worse than the disk it saves.
#   unregistered    git does not know it. This is the 509 GB case, and it is
#                   collected whatever its age: a directory git has forgotten
#                   cannot be anybody's live work, and `git worktree prune` has
#                   already run at some point and left the bytes behind.
#
# An unregistered entry that is not gate-shaped at all (no `.git`, no known
# suffix, no lock) is only collected once it is older than --age-days, and is
# named when it goes. That is the "somebody put a directory in here by hand"
# case, and it gets the benefit of the doubt for a week.
#
# ── Portable on purpose ──────────────────────────────────────────────────────
#
# Linux and macOS, plain bash, no GNU-only flags. See scripts/gate-dirs.sh for
# why there is no PowerShell counterpart.
set -uo pipefail
# NOT `set -e`: a sweep that finds nothing, or that trips over one stubborn
# directory, must never fail the build recipe or the gate that called it.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/gate-dirs.sh"

DRY_RUN=0
QUIET=0
FORCE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --quiet) QUIET=1; shift ;;
        # Collect an aged registered worktree even when it is dirty or on a
        # named branch. Never used by the automatic call sites; this is for a
        # human who has read the warnings and meant it.
        --force) FORCE=1; shift ;;
        --age-days) CCK_GATE_MAX_AGE_DAYS="$2"; shift 2 ;;
        --base) CCK_GATE_BASE="$2"; shift 2 ;;
        -h | --help) sed -n '2,52p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "==> prune-worktrees.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
done

say() { [ "$QUIET" -eq 1 ] || echo "$@"; }

if [ ! -d "$CCK_GATE_BASE" ]; then
    say "==> No gate scratch directory at $CCK_GATE_BASE, nothing to collect"
    exit 0
fi

# Every worktree this repository currently registers, one absolute path per
# line. Empty when the sweep is run from outside a repo, which makes every entry
# read as unregistered, so the sweep refuses to run at all in that case rather
# than guessing.
if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "==> prune-worktrees.sh must run inside the repo (it reads git worktree list)" >&2
    exit 2
fi
REGISTERED="$(git worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')"

# registered_p <dir>: true when git currently registers <dir> as a worktree.
# Fed by a here-doc rather than a pipe, because a `while` on the right of a pipe
# runs in a subshell and its `return` reaches nothing. Both the literal path and
# its physical form are compared, so a symlinked home directory cannot make a
# live worktree look like residue.
registered_p() {
    local path="$1" physical line
    physical="$(cd "$path" 2>/dev/null && pwd -P)"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        [ "$line" = "$path" ] && return 0
        [ -n "$physical" ] && [ "$line" = "$physical" ] && return 0
    done <<REGISTERED_LIST
$REGISTERED
REGISTERED_LIST
    return 1
}

older_than_limit() {
    # `find -maxdepth 0 -mtime +N` is the one age test that reads the same on
    # GNU and BSD find, which is why it is not `stat`.
    [ -n "$(find "$1" -maxdepth 0 -mtime "+$CCK_GATE_MAX_AGE_DAYS" 2>/dev/null)" ]
}

worktree_is_safe_to_collect() {
    # Safe means: nothing in there is work somebody would miss. A gate worktree
    # is always detached (gate-worktree.sh adds it with --detach) and always
    # clean once its build output has gone to the sibling target dir, so
    # anything else is a hand-made worktree wearing a gate's clothes.
    local wt="$1"
    [ "$FORCE" -eq 1 ] && return 0
    git -C "$wt" rev-parse --git-dir >/dev/null 2>&1 || return 0
    if [ -n "$(git -C "$wt" status --porcelain 2>/dev/null)" ]; then
        UNSAFE_REASON="uncommitted changes"
        return 1
    fi
    if git -C "$wt" symbolic-ref -q HEAD >/dev/null 2>&1; then
        UNSAFE_REASON="checked out on a named branch, not a detached gate"
        return 1
    fi
    return 0
}

# Group the entries by owner first, so a worktree and its siblings are judged
# together and collected in one move instead of three.
OWNERS=""
for entry in "$CCK_GATE_BASE"/*; do
    [ -e "$entry" ] || continue
    name="$(basename "$entry")"
    case "$name" in *.lock) continue ;; esac
    owner="$(cck_gate_owner "$name")"
    case " $OWNERS " in
        *" $owner "*) ;;
        *) OWNERS="$OWNERS $owner" ;;
    esac
done

collected=0
kept=0

for owner in $OWNERS; do
    wt="$(cck_gate_worktree "$owner")"

    if cck_gate_in_use "$owner"; then
        kept=$((kept + 1))
        continue
    fi

    if [ -d "$wt" ] && registered_p "$wt"; then
        if ! older_than_limit "$wt"; then
            kept=$((kept + 1))
            continue
        fi
        UNSAFE_REASON=""
        if ! worktree_is_safe_to_collect "$wt"; then
            echo "==> KEEPING $wt: $UNSAFE_REASON"
            echo "    It is over $CCK_GATE_MAX_AGE_DAYS days old. Deal with it, or re-run with --force."
            kept=$((kept + 1))
            continue
        fi
        say "==> Collecting $owner (registered, over $CCK_GATE_MAX_AGE_DAYS days old, clean)"
    elif [ -e "$wt/.git" ] || [ -f "$(cck_gate_lock "$owner")" ] \
        || [ -d "$(cck_gate_target "$owner")" ] || [ -d "$(cck_gate_export "$owner")" ]; then
        # Gate-shaped and git does not know it: the exact residue this exists
        # for. Collected whatever its age.
        say "==> Collecting $owner (gate residue, no longer registered)"
    elif older_than_limit "$wt"; then
        echo "==> Collecting $owner (not gate-shaped, but over $CCK_GATE_MAX_AGE_DAYS days old)"
    else
        kept=$((kept + 1))
        continue
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        collected=$((collected + 1))
        continue
    fi
    cck_gate_release "$owner"
    collected=$((collected + 1))
done

# Drop registrations whose directory is already gone, whether this run removed
# it or something else did. Cheap, and it is what keeps `git worktree list` an
# honest answer for the next sweep.
git worktree prune >/dev/null 2>&1

if [ "$collected" -eq 0 ]; then
    say "==> No gate residue in $CCK_GATE_BASE ($kept live or recent)"
elif [ "$DRY_RUN" -eq 1 ]; then
    echo "==> Would collect $collected gate(s) from $CCK_GATE_BASE, keeping $kept"
else
    echo "==> Collected $collected gate(s) from $CCK_GATE_BASE, keeping $kept"
fi
exit 0
