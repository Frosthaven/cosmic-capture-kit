# gate-dirs.sh: where a gate's scratch lives, what its directories are called,
# and how they are removed. SOURCED by scripts/gate-worktree.sh and
# scripts/prune-worktrees.sh, never executed, so it needs no executable bit (the
# repo sets core.fileMode = false, and a script that only works because of a
# local chmod dies on a fresh clone).
#
#   . scripts/gate-dirs.sh
#
# ── The bug this fixes ───────────────────────────────────────────────────────
#
# `~/.cck-worktrees/<name>` plus a `<name>-target` sibling holding that
# worktree's CARGO_TARGET_DIR was never written down anywhere: not in a recipe,
# not in a script, not in a line of CLAUDE.md. Every agent running a gate
# improvised it from CLAUDE.md's "separate CARGO_TARGET_DIRs" rule, ran
# `git worktree add` by hand, and cleaned up afterwards, or did not.
#
# On 2026-09-01 that came to 96 directories and 509 GB on one machine, every one
# of them a month old, every branch already deleted and every worktree already
# pruned from git's metadata. That combination is the whole lesson:
# `git worktree prune` drops the REGISTRATION and never deletes a directory, and
# nothing had ever removed the `-target` siblings at all, so the tidiest-looking
# cleanup still left 13 to 21 GB per gate on disk. Three machines have now been
# filled this way, and the rule as prose did not stop the third.
#
# So the convention lives HERE, in code that removes what it made, and the rule
# in CLAUDE.md points at it instead of asking for discipline.
#
# ── Portable on purpose ──────────────────────────────────────────────────────
#
# Plain POSIX-ish bash, no filesystem tricks and no GNU-only flags: this repo is
# worked from Linux and macOS, from more than one distro, and no part of the fix
# may depend on which one. There is no PowerShell counterpart because Windows
# never had this problem: that boot builds into `<repo>\target-win`, inside the
# checkout and git-excluded, and never creates a scratch tree outside it.

# The base directory every gate's scratch lives under. Overridable so the
# self-check can point the whole mechanism at a temp dir.
CCK_GATE_BASE="${CCK_GATE_BASE:-$HOME/.cck-worktrees}"

# How old, in days, a REGISTERED worktree has to be before the sweep will
# consider collecting it. Unregistered residue is collected whatever its age,
# because git has already forgotten it.
CCK_GATE_MAX_AGE_DAYS="${CCK_GATE_MAX_AGE_DAYS:-7}"

# The suffixes a gate hangs off its own name. Each one is a sibling DIRECTORY of
# the worktree, never a directory inside it, because cargo and the publish
# script both want a tree of their own.
CCK_GATE_SUFFIXES="-target -export"

# cck_gate_worktree <name>: the worktree itself.
cck_gate_worktree() { printf '%s\n' "$CCK_GATE_BASE/$1"; }

# cck_gate_target <name>: that worktree's CARGO_TARGET_DIR.
cck_gate_target() { printf '%s\n' "$CCK_GATE_BASE/$1-target"; }

# cck_gate_export <name>: the tree a publish gate exports into.
cck_gate_export() { printf '%s\n' "$CCK_GATE_BASE/$1-export"; }

# cck_gate_lock <name>: the in-use marker. It holds the owning pid on the first
# line, so a sweep running beside a live gate can tell "somebody is using this"
# from "somebody was killed holding this", and collect only the second.
cck_gate_lock() { printf '%s\n' "$CCK_GATE_BASE/$1.lock"; }

# cck_gate_owner <entry-name>: the gate name an entry belongs to, i.e. the name
# with a known suffix taken off. `d401-target` belongs to `d401`; `d401` belongs
# to itself.
cck_gate_owner() {
    local name="$1" suffix
    for suffix in $CCK_GATE_SUFFIXES; do
        case "$name" in
            *"$suffix") printf '%s\n' "${name%$suffix}"; return 0 ;;
        esac
    done
    printf '%s\n' "$name"
}

# cck_gate_in_use <name>: true while a live process holds the gate. A lock whose
# pid is gone is NOT in use: that is exactly the crashed run whose residue the
# sweep exists to collect.
cck_gate_in_use() {
    local lock pid
    lock="$(cck_gate_lock "$1")"
    [ -f "$lock" ] || return 1
    pid="$(head -n 1 "$lock" 2>/dev/null)"
    case "$pid" in
        '' | *[!0-9]*) return 1 ;;
    esac
    kill -0 "$pid" 2>/dev/null
}

# cck_gate_mkbase: create the base directory if it is missing, and echo it.
cck_gate_mkbase() {
    mkdir -p "$CCK_GATE_BASE"
    printf '%s\n' "$CCK_GATE_BASE"
}

# cck_gate_under_base <path>: true when <path> is a direct child of the base
# directory. Every removal below is guarded by this, so a mangled name or an
# empty variable can never turn `rm -rf` loose somewhere else.
cck_gate_under_base() {
    local path="$1" parent
    case "$path" in
        '' | '/' | */../* | */..) return 1 ;;
    esac
    parent="$(dirname "$path")"
    [ "$parent" = "$CCK_GATE_BASE" ]
}

# cck_gate_rm <path>: remove one gate directory and say so if it will not go.
# Never fails the caller: a sweep that cannot remove one entry still has the
# rest to collect, and a gate that cannot clean up still has an exit code of its
# own to report.
cck_gate_rm() {
    local path="$1"
    [ -e "$path" ] || return 0
    if ! cck_gate_under_base "$path"; then
        echo "==> WARNING: refusing to remove $path (not directly under $CCK_GATE_BASE)" >&2
        return 1
    fi
    rm -rf "$path" 2>/dev/null
    if [ -e "$path" ]; then
        echo "==> WARNING: could not remove $path (still on disk)" >&2
        return 1
    fi
    return 0
}

# cck_gate_rm_all <name>: remove everything a gate owns, worktree and siblings
# and lock, in one call. This is what BOTH the entry point's exit trap and the
# sweep use, so a run that ends cleanly and a run that was killed are collected
# by the same code.
cck_gate_rm_all() {
    local name="$1" suffix
    for suffix in $CCK_GATE_SUFFIXES; do
        cck_gate_rm "$CCK_GATE_BASE/$name$suffix"
    done
    cck_gate_rm "$(cck_gate_worktree "$name")"
    rm -f "$(cck_gate_lock "$name")" 2>/dev/null
    return 0
}

# cck_gate_release <name>: hand a gate's worktree back to git and then remove
# every directory it owns. Call it from inside the repo (git needs to know which
# repository's worktree list to edit) and never from inside the worktree itself.
#
# `git worktree remove` is tried FIRST because it is the only thing that also
# drops the registration, and `--force` because a gate worktree is expected to
# be dirty: it just built and tested in there. `rm -rf` then follows regardless,
# since `remove` refuses a worktree whose files another process is still holding,
# and `prune` closes the registration afterwards either way. That last pairing is
# the whole 509 GB bug in one line: a prune with no removal leaves the bytes, and
# a removal with no prune leaves a phantom registration.
cck_gate_release() {
    local name="$1" wt
    wt="$(cck_gate_worktree "$name")"
    if [ -e "$wt" ] && git rev-parse --git-dir >/dev/null 2>&1; then
        git worktree remove --force "$wt" >/dev/null 2>&1
    fi
    cck_gate_rm_all "$name"
    if git rev-parse --git-dir >/dev/null 2>&1; then
        git worktree prune >/dev/null 2>&1
    fi
    return 0
}
