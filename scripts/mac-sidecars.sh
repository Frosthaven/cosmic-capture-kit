# shellcheck shell=bash
# mac-sidecars.sh: the ONE list of files the macOS ffmpeg sidecar is made of
# (DRAGON-689). SOURCED, never executed, so it needs no executable bit (the repo
# sets core.fileMode = false, and a script that only works because of a local
# chmod dies on a fresh clone).
#
#   . scripts/mac-sidecars.sh
#
# ── Why this file exists ─────────────────────────────────────────────────────
#
# The macOS ffmpeg used to be TWO self-contained static binaries, so "the
# sidecar" was two file names and every script could just say `ffmpeg` and
# `ffprobe`. DRAGON-689 replaced them with a SHARED build: two thin front-ends
# over seven libav dylibs, 132 MB down to a third of that, but now the file set
# is nine names instead of two and THREE scripts need to agree on it:
#
#   scripts/ffmpeg-mac/build.sh   asserts the build produced exactly these
#   scripts/fetch-mac-vendor.sh   asserts the downloaded archive carries them
#   scripts/mac-package.sh        copies them into the .app and signs each one
#
# Three hand-maintained copies of one list is how a future ffmpeg bump ships a
# bundle missing a dylib: everything passes on the build machine, where the old
# file is still sitting in vendor/, and the app dies with a dyld error on the
# first customer's Mac. One list, sourced by all three.
#
# ── The dylibs travel WITH the executables, in the same directory ────────────
#
# Each dylib records its own install name as `@loader_path/<name>`, which dyld
# resolves relative to the directory of whatever is doing the loading. So the
# whole set works from any folder as long as it stays together and stays FLAT.
# Copying only `ffmpeg` and `ffprobe` somewhere produces two binaries that cannot
# start, and there is no error at copy time to say so.
#
# ── The version numbers ARE the ffmpeg generation ────────────────────────────
#
# `libavcodec.63` is ffmpeg 9.x. They move as a set when FFMPEG_VERSION moves,
# and a mismatch here means the source moved under the pin, so the assertions
# that read this list are load-bearing rather than decorative.

# The two programs. `ffplay` is deliberately absent: macOS plays preview audio
# through its own AudioQueue sink (`platform/mac/services/audio_out.rs`), so
# unlike Windows there is nothing for an ffplay sidecar to do, and
# `scripts/ffmpeg-mac/build.sh` passes --disable-ffplay.
CCK_MAC_FFMPEG_EXES=(ffmpeg ffprobe)

# The shared libraries, MAJOR-versioned. Only these names are ever loaded
# (that is what -install_name recorded), so the build stages one real file per
# name and no symlinks, which keeps codesign and notarization simple.
CCK_MAC_FFMPEG_DYLIBS=(
    libavcodec.63.dylib
    libavdevice.63.dylib
    libavfilter.12.dylib
    libavformat.63.dylib
    libavutil.61.dylib
    libswresample.7.dylib
    libswscale.10.dylib
)

# Every file the sidecar is made of, one per line, exes first. Every one is a
# Mach-O, so this doubles as the list of things that must be signed.
cck_mac_ffmpeg_files() {
    printf '%s\n' "${CCK_MAC_FFMPEG_EXES[@]}" "${CCK_MAC_FFMPEG_DYLIBS[@]}"
}
