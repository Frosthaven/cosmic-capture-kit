#!/usr/bin/env bash
# Cross-compile the Windows ffmpeg (DRAGON-675). RUNS INSIDE the container built
# from the Dockerfile beside this file; `just ffmpeg-win` is the intended caller,
# from Linux, macOS or Windows, since all three only need docker.
#
# It expects:
#   /src    the repo, mounted READ-ONLY (nothing here writes to the checkout)
#   /out    a writable scratch dir, mounted from the host's `target-ffmpeg-win/`
#
# TWO TARGET ARCHITECTURES (DRAGON-702). The image tells this script which one it
# is, through `CCK_WIN_ARCH` and `/opt/cross.env`, both written by the Dockerfile;
# `just ffmpeg-win aarch64` builds and runs the ARM64 flavour. The x86_64 path is
# unchanged. What differs here is the configure line (no NVENC, AMF or Quick Sync
# on ARM64 Windows, so Media Foundation is the whole hardware story), the archive
# name, and which two `pins.env` keys the report tells you to paste.
#
# WHAT IT PRODUCES: one `.7z` laid out exactly like the gyan.dev archive it
# replaces, a top-level folder holding `bin/` with the three exes and the seven
# libav DLLs. That is deliberate and it is the reason `fetch-win-vendor.ps1`
# barely changed: it already unpacks a `.7z`, already finds `*/bin`, and already
# ships the whole of it flat.
#
# THIS IS A PRODUCER, AND IT RUNS ONCE PER PIN MOVE. Nothing else in the repo
# ever builds ffmpeg: GitHub CI, `just dev` and the MSI all FETCH the published
# archive through `fetch-win-vendor.ps1`, gated by FFMPEG_WINDOWS_X64_SHA256. So
# the dev loop downloads the exact bytes a customer's installer carries, and a
# hosted Windows runner (which cannot run a Linux container at all) needs
# nothing but the download.
#
# WHAT IT DOES NOT DO: publish. The archive and its sha256 land in /out, and the
# script prints the upload command plus the two `pins.env` lines to paste.
# Publishing is a human step on purpose: it puts a binary in front of users.
#
# ── STATUS: both producer runs landed 2026-08-17, from the Linux box ─────────
#
# The cck2 archives are on the vendor mirror and pinned in scripts/pins.env,
# with FFMPEG_WINDOWS_BUILD_REV bumped to 2 (the ffmpeg version did not move,
# so only the revision distinguishes the new archives from the old cck1).
#
#   1. x86_64 (DRAGON-701): the first shipped archive carrying
#      `--enable-mediafoundation`, so `h264_mf` / `hevc_mf` finally exist in an
#      ffmpeg we ship and the app's `mf` ladder arm (`src/encode/plan.rs`)
#      wakes on the next vendor fetch. STILL OWED on a Windows box:
#      `debug test windows-record 5 mf`, the end-to-end proof.
#   2. aarch64 (DRAGON-702): the first ARM64 archive at all. The llvm-mingw
#      toolchain and link line are now measured, not guessed: the libc++
#      `--extra-libs` line below survived AS WRITTEN, and the one real
#      iteration was resolution order instead, x265.pc's own `-lc++` pulling
#      libc++.dll.a against our static libc++.a (ld.lld refuses the duplicate;
#      the Dockerfile's link-static comment carries the fix). The binaries
#      pass check_imports and the feature guards but have never executed on
#      real ARM64 Windows hardware; the fleet has none.
#
# ── Why this build's feature set looks so much smaller than gyan's ───────────
#
# gyan's full_build enables about eighty external libraries. This one enables
# five, and the difference is not thrift: every one of them is a thing whose
# version we would then own and have to track. The list below is derived from
# what the app actually invokes, and each entry names the code that needs it.
# Everything else ffmpeg can do here it does with its own built-in codecs,
# muxers and filters, which come with the source and cost nothing to keep.
set -euo pipefail

SRC=/src
OUT=/out
WORK=/tmp/cck-ffmpeg
W64=/opt/w64

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ -f "$SRC/scripts/pins.env" ]] || die "expected the repo mounted read-only at $SRC"
[[ -d "$OUT" ]] || die "expected a writable output dir at $OUT"
# The image writes this, both arches, so it is the honest "am I in the right
# container" marker. It used to be the AMF header dir, which the ARM64 image does
# not build at all.
[[ -f /opt/cross.env ]] || die "this script must run inside a cck-ffmpeg-win image"

# CROSS (the toolchain prefix) and FFARCH (what ffmpeg's --arch wants) both come
# from the image rather than from an argument, so the script cannot be told one
# architecture while the compilers in the container are for another.
# shellcheck disable=SC1091
. /opt/cross.env
WIN_ARCH="${CCK_WIN_ARCH:?the image did not set CCK_WIN_ARCH}"
: "${CROSS:?/opt/cross.env set no CROSS}"
: "${FFARCH:?/opt/cross.env set no FFARCH}"
[[ "$WIN_ARCH" == "x86_64" || "$WIN_ARCH" == "aarch64" ]] \
    || die "CCK_WIN_ARCH must be x86_64 or aarch64, got '$WIN_ARCH'"

# shellcheck source=../pins.env
. "$SRC/scripts/pins.env"

REV="${FFMPEG_WINDOWS_BUILD_REV:?pins.env has no FFMPEG_WINDOWS_BUILD_REV}"

# THE NAME CARRIES THE DRIVER FLOOR, on x86_64. `ffmpeg-9.0-nvcodec-n13.0.19.1-cck1`
# says the ffmpeg release, the nv-codec-headers tag (which IS the minimum NVIDIA
# driver, and the whole reason this build exists), and our own revision for a
# rebuild that changes neither. Anyone reading the asset list on the mirror, or
# the URL in pins.env, can see which floor they are about to ship without
# opening anything.
#
# The ARM64 name DROPS the nvcodec field, because there is no NVENC on ARM64
# Windows and therefore no driver floor to advertise. Carrying the field anyway
# would state a requirement this binary does not have, which is worse than
# saying nothing.
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    NAME="ffmpeg-$FFMPEG_VERSION-nvcodec-$NVCODEC_VERSION-cck$REV-win64-shared"
    PINS_KEY=FFMPEG_WINDOWS_X64
else
    NAME="ffmpeg-$FFMPEG_VERSION-cck$REV-winarm64-shared"
    PINS_KEY=FFMPEG_WINDOWS_ARM64
fi
ARCHIVE="$OUT/$NAME.7z"

rm -rf "$WORK"
mkdir -p "$WORK"
cd "$WORK"

# ── The ffmpeg source: the SAME tarball the Linux build compiles ─────────────
#
# Mirror list, tried in order, first success wins, then VERIFIED BY THE UPSTREAM
# GPG SIGNATURE and not by a checksum alone. A hash we carry only proves the
# bytes match what we saw when the pin was made; the signature proves the FFmpeg
# project produced them. The fingerprint is pinned in pins.env and published on
# ffmpeg.org/download.html, and the signature must be made by exactly it, so a
# compromised key URL supplying both a key and a matching signature still fails.
# Identical to what `scripts/appimage/Dockerfile` and the justfile's `dist`
# recipe do, because it is the same tarball.
fetch_any() {
    local out="$1"; shift
    for u in "$@"; do
        curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 \
             --speed-time 30 --speed-limit 1024 -o "$out" "$u" && return 0
        echo "mirror failed, trying the next: $u" >&2
    done
    die "every mirror failed for $out"
}

log "fetching ffmpeg $FFMPEG_VERSION source"
fetch_any ffmpeg.tar.xz $FFMPEG_SOURCE_URL
fetch_any ffmpeg.tar.xz.asc $FFMPEG_SOURCE_ASC_URL
fetch_any ffmpeg-devel.asc $FFMPEG_GPG_KEY_URL
echo "$FFMPEG_SOURCE_SHA256  ffmpeg.tar.xz" | sha256sum -c -

export GNUPGHOME="$WORK/gnupg"
rm -rf "$GNUPGHOME"; mkdir -p "$GNUPGHOME"; chmod 700 "$GNUPGHOME"
gpg --batch --quiet --import ffmpeg-devel.asc
gpg --batch --with-colons --fingerprint | grep -E '^fpr:' | cut -d: -f10 \
  | grep -qx "$FFMPEG_GPG_FPR" \
  || die "ffmpeg-devel.asc lacks the pinned signing key $FFMPEG_GPG_FPR"
# VALIDSIG carries the fingerprint that actually made the signature, so this
# asserts authorship rather than just a good signature by some imported key.
gpg --batch --status-fd 1 --verify ffmpeg.tar.xz.asc ffmpeg.tar.xz \
  | grep -q "^\[GNUPG:\] VALIDSIG $FFMPEG_GPG_FPR " \
  || die "ffmpeg $FFMPEG_VERSION is not signed by $FFMPEG_GPG_FPR"

tar xJf ffmpeg.tar.xz
cd "ffmpeg-$FFMPEG_VERSION"

# ── configure ────────────────────────────────────────────────────────────────
#
# --disable-autodetect is what fixes this build's FEATURE SET, rather than
# leaving it as "whatever the container happened to have installed". With it,
# every external library is an explicit decision below, and a missing one FAILS
# the build instead of quietly producing an ffmpeg without an encoder somebody
# needs. (It says nothing about byte-for-byte reproducibility, which this build
# does not have; see the publish note at the bottom.)
#
# What each enable is for:
#   zlib      ffmpeg's PNG encoder is gated on it, and `preview/video.rs` pulls
#             frames out of a recording with `-f image2pipe -vcodec png`
#   libx264   the software H.264 tier (`encode/plan.rs`)
#   libx265   the software HEVC tier, used when the user picks HEVC explicitly
#   nvenc     the NVIDIA tier, via the pinned nv-codec-headers. THE POINT.
#   amf       the AMD tier (`h264_amf` / `hevc_amf`)
#   libvpl    the Intel Quick Sync tier (`h264_qsv` / `hevc_qsv`)
#   mediafoundation  the Media Foundation tier (`h264_mf` / `hevc_mf`,
#             DRAGON-701): the vendor-agnostic hardware encoder, last among the
#             hardware tiers on x86 and the ONLY one on an ARM64 Windows build,
#             where none of the three above exist. It needs no third-party
#             source at all, only the mingw-w64 headers and mfplat, so unlike
#             the entries around it there is no version for us to own
#   sdl2      ffplay.exe, the preview editor's audio sink on Windows
#   d3d11va / dxva2  hardware DECODE, available to the editor's playback
#
# d3d12va is NOT here and that is measured, not an oversight: Debian 12's
# mingw-w64 headers have no ID3D12VideoDecoder, so configure refuses it
# outright. It costs nothing to leave out. ffmpeg never selects a hwaccel on
# its own, so all three of these are doors we hold open rather than paths the
# app currently walks, and the two that do build cover the same decoders.
#
# --enable-w32threads is passed explicitly for the same reason: with autodetect
# off, leaving threading to be discovered is how a build ends up single-threaded
# and nobody notices until a recording cannot keep up.
#
# --extra-version puts our build revision in `ffmpeg -version`, which is what
# the app captures into its debug log. "ffmpeg version 9.0-cck1" says at a
# glance whose binary a customer is running, which the whole of DRAGON-671 was
# spent not knowing.
#
# The static libgcc/libstdc++ flags are not an optimisation: the container's
# mingw uses the posix threading flavour (see the Dockerfile), whose output
# otherwise needs three toolchain DLLs at runtime that we do not ship.
# `check_imports` below refuses the build if any of them survives.
#
# THE `-Wl,-Bstatic` AROUND `-lstdc++` IS LOAD-BEARING, and both halves of that
# were measured the hard way:
#
#   A BARE `-lstdc++` resolves to the SHARED libstdc++'s import library, which
#   drags in the libgcc_s import library, while -static-libgcc has already
#   supplied libgcc_eh.a. avfilter then dies with "multiple definition of
#   `_Unwind_Resume'".
#
#   REMOVING IT ALTOGETHER fails earlier and more confusingly, at
#   "ERROR: libvpl >= 2.6 not found". oneVPL is C++ and its vpl.pc declares an
#   empty Libs.private, so configure's link test has no C++ runtime and the
#   probe fails as if the library were missing rather than unlinkable.
#
# -Bstatic/-Bdynamic bracket exactly the one library, so nothing else on the
# line changes mode.
#
# `-L$W64/link-static` MUST STAY FIRST. That directory holds the static pthread
# archives and nothing else, and it is the only thing that stops libstdc++'s
# pthread calls importing libwinpthread-1.dll, a DLL we do not ship. The
# Dockerfile's comment on it records why no flag here can do the job instead.
#
# ONE array, used to configure AND recorded verbatim into the archive's
# BUILD.txt, so what a shipped binary says it was built with cannot drift from
# what it was actually built with.
#
# ── THE THREE VENDOR TIERS ARE x86_64-ONLY (DRAGON-702) ──────────────────────
#
# NVENC, AMF and Quick Sync do not exist on ARM64 Windows: there is no NVIDIA or
# AMD driver for the platform and no Intel iGPU in it, so all three would
# configure against headers the image deliberately does not install and produce
# encoders that could never open. Media Foundation is the whole hardware story
# there, which is exactly why DRAGON-701 added it, and it stays enabled on BOTH
# arches so the two builds differ by subtraction rather than by two lists.
#
# THE C++ RUNTIME FLAGS DIFFER BECAUSE THE COMPILER DOES. On x86_64 this is GCC
# and the library is libstdc++; on ARM64 it is clang and the library is libc++,
# with libunwind in place of libgcc's unwinder. Both spellings mean the same
# thing: link the C++ runtime statically so no toolchain DLL survives as an
# import (`check_imports` below refuses the build if one does).
CONFIGURE_ARGS=(
    --prefix="$WORK/stage"
    --arch="$FFARCH" --target-os=mingw32
    --cross-prefix="$CROSS-" --enable-cross-compile
    --pkg-config=pkg-config --pkg-config-flags=--static
    --extra-version="cck$REV"
    --enable-shared --disable-static
    --disable-doc --disable-debug --disable-autodetect
    --enable-w32threads
    --enable-gpl --enable-version3
    --enable-zlib
    --enable-libx264 --enable-libx265
    --enable-mediafoundation
    --enable-d3d11va --enable-dxva2
    --enable-sdl2
    --extra-cflags="-I$W64/include"
)
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    CONFIGURE_ARGS+=(
        --enable-ffnvcodec --enable-nvenc
        --enable-amf
        --enable-libvpl
        --extra-ldflags="-L$W64/link-static -L$W64/lib -static-libgcc -static-libstdc++"
        --extra-libs="-Wl,-Bstatic -lstdc++ -Wl,-Bdynamic"
    )
else
    # llvm-mingw's default C++ library is libc++ and its unwinder is LLVM's, so
    # `-lstdc++` would not resolve at all. `-static-libstdc++` still means "link
    # the C++ library statically" to clang, which is why it stays. Measured on
    # the first producer run (2026-08-17): this line is correct as written; the
    # link failure that run hit was next door, x265.pc's own `-lc++` resolving
    # to the import library (see the Dockerfile's link-static comment).
    CONFIGURE_ARGS+=(
        --extra-ldflags="-L$W64/link-static -L$W64/lib -static-libgcc -static-libstdc++"
        --extra-libs="-Wl,-Bstatic -lc++ -lunwind -Wl,-Bdynamic"
    )
fi

log "configuring ffmpeg $FFMPEG_VERSION ($CROSS cross, shared, $WIN_ARCH)"
PKG_CONFIG_LIBDIR="$W64/lib/pkgconfig" ./configure "${CONFIGURE_ARGS[@]}"

log "building (this is the long part)"
make -j"$(nproc)"
make install

# ── Verify, do not assume ────────────────────────────────────────────────────

# THE ENCODERS THAT COME FROM A PLATFORM API GET CHECKED HERE, because nothing
# else would notice their absence. A missing third-party library fails at
# configure (--disable-autodetect makes every one of them explicit) or shows up
# as a missing DLL below. Media Foundation has neither tell: it is built from
# the mingw-w64 headers, links no library of ours, and configure treats it as an
# autodetect feature, so a header set that cannot satisfy the probe would leave
# a perfectly healthy-looking ffmpeg with no `h264_mf` in it. The app would then
# go on silently skipping its Media Foundation tier, which is exactly the state
# this build exists to end (DRAGON-701).
# config_components.h FIRST: ffmpeg 5.1 moved every per-component CONFIG_*
# define out of config.h into it, so grepping config.h alone reports a healthy
# build as broken (measured on the first 9.0 producer run, 2026-08-17: both
# MF encoders were in config_components.h while config.h had no component
# lines at all). The other two stay as fallbacks for older layouts.
CONFIG_H=""
for f in config_components.h config.h ffbuild/config.h; do
    if [[ -f "$f" ]]; then CONFIG_H="$f"; break; fi
done
[[ -n "$CONFIG_H" ]] || die "no config.h in the ffmpeg build tree; cannot verify the feature set"
for sym in CONFIG_H264_MF_ENCODER CONFIG_HEVC_MF_ENCODER; do
    grep -q "^#define $sym 1" "$CONFIG_H" || die "$sym is not enabled in this build.
--enable-mediafoundation was requested and configure did not deliver it. Check
config.log for the mediafoundation probe (it needs mftransform.h and
MFCreateAlignedMemoryBuffer from the mingw-w64 headers in the image)."
done

BIN="$WORK/stage/bin"

# The three programs are each load-bearing: ffmpeg records, ffprobe measures,
# ffplay is the editor's audio sink. A missing one is a customer-visible feature
# gone, so prove all three arrived rather than trusting `make install`.
for exe in ffmpeg ffprobe ffplay; do
    [[ -f "$BIN/$exe.exe" ]] || die "$exe.exe was not built"
done

# The exact DLL set the shared build must produce. Named in full rather than
# globbed because the numbers ARE the ffmpeg generation (9.x), and a set that
# does not match this list means the source moved under the pin.
EXPECTED_DLLS=(avcodec-63.dll avdevice-63.dll avfilter-12.dll avformat-63.dll
               avutil-61.dll swresample-7.dll swscale-10.dll)
for dll in "${EXPECTED_DLLS[@]}"; do
    [[ -f "$BIN/$dll" ]] || die "expected $dll in the build output; got: $(cd "$BIN" && echo *.dll)"
done

# NO TOOLCHAIN RUNTIME MAY SURVIVE AS AN IMPORT. The posix mingw flavour links
# libwinpthread-1.dll, libgcc_s_seh-1.dll and libstdc++-6.dll dynamically by
# default, and we ship none of them, so a binary importing one starts with a
# missing-DLL dialog on the customer's machine and nowhere else. This is the
# single most likely way a cross build breaks, and it is invisible until then.
#
# The ARM64 toolchain has its OWN three (libc++.dll, libunwind.dll and its own
# libssp), for the same reason and with the same consequence, so the pattern
# covers both toolchains' runtimes rather than branching. A name that cannot
# appear on one arch costs nothing to refuse there (DRAGON-702).
#
# `$CROSS-objdump` is Debian's binutils on x86_64 and llvm-objdump behind
# llvm-mingw's wrapper on ARM64. Both print PE imports as `DLL Name:` lines, so
# the awk is unchanged.
check_imports() {
    local f="$1" bad
    bad="$("$CROSS-objdump" -p "$f" | awk '/DLL Name:/ {print $3}' \
        | grep -Ei '^(libwinpthread|libgcc_s|libstdc\+\+|libc\+\+|libunwind|libssp|zlib1|SDL2|libx264|libx265)' || true)"
    [[ -z "$bad" ]] || die "$(basename "$f") imports libraries we do not ship:
$bad
The link line lost -static-libgcc/-static-libstdc++, or a dependency was found
as an import library (a *.dll.a) where only its static archive should exist."
}
for f in "$BIN"/*.exe "$BIN"/*.dll; do check_imports "$f"; done

# WATCH THIS LIST FOR mfplat.dll (DRAGON-701). Media Foundation is not linked: ffmpeg's
# `mfenc` opens mfplat.dll with dlopen at ENCODER INIT and reports a plain error when it
# is absent, which our encoder probe already treats as "this tier is unavailable". It is
# NOT in the refusal list above, because it is a Windows system DLL rather than something
# we ship. But if a future ffmpeg or a configure probe turns it into a load-time IMPORT,
# it belongs in that list immediately: Windows N and KN editions ship without the media
# stack, and an import they cannot resolve stops ffmpeg.exe from starting at all, taking
# recording with it. A dlopen fails one encoder; an import fails the program.
log "imports (for the record):"
for f in "$BIN"/*.exe "$BIN"/*.dll; do
    printf '    %-20s %s\n' "$(basename "$f")" \
        "$("$CROSS-objdump" -p "$f" | awk '/DLL Name:/ {print $3}' | sort -u | tr '\n' ' ')"
done

# ── Stage the archive ────────────────────────────────────────────────────────
#
# The LICENCE and the build record travel WITH the binary. ffmpeg here is built
# --enable-gpl --enable-version3, so what we hand a user is GPLv3 code, and the
# obligation that comes with it is to be able to say exactly which source and
# which configuration produced it. BUILD.txt is that answer, in the archive,
# rather than in a repo the recipient may not have.
STAGE="$WORK/pkg/$NAME"
rm -rf "$WORK/pkg"
mkdir -p "$STAGE/bin"
cp "$BIN"/*.exe "$BIN"/*.dll "$STAGE/bin/"
cp COPYING.GPLv3 "$STAGE/LICENSE"

# The NVENC API version, and therefore the NVIDIA driver floor, exists only on
# the x86_64 build. The ARM64 one installs no nv-codec-headers at all, so there
# is nothing to read and nothing to report.
NVENC_API=""
NVENC_API_MINOR=""
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    NVENC_API="$(sed -n 's/.*NVENCAPI_MAJOR_VERSION *\([0-9]*\).*/\1/p' "$W64/include/ffnvcodec/nvEncodeAPI.h" | head -1)"
    NVENC_API_MINOR="$(sed -n 's/.*NVENCAPI_MINOR_VERSION *\([0-9]*\).*/\1/p' "$W64/include/ffnvcodec/nvEncodeAPI.h" | head -1)"
fi

# The `[build-inputs]` lines, which `fetch-win-vendor.ps1` reads back and checks
# against pins.env. THE ARM64 SET IS SHORTER, and it has to be: a key naming a
# library this build never compiled would be a claim the fetch would then verify
# against a pin that had nothing to do with it.
BUILD_INPUTS=(
    "ffmpeg=$FFMPEG_VERSION"
    "build-rev=cck$REV"
    "arch=$WIN_ARCH"
    "x264=$X264_VERSION"
    "x265=$X265_VERSION"
    "zlib=$ZLIB_VERSION"
    "sdl2=$SDL2_VERSION"
)
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    BUILD_INPUTS+=(
        "nv-codec-headers=$NVCODEC_VERSION"
        "nvenc-api=$NVENC_API.$NVENC_API_MINOR"
        "amf=$AMF_VERSION"
        "libvpl=$LIBVPL_VERSION"
    )
else
    BUILD_INPUTS+=("llvm-mingw=$LLVM_MINGW_VERSION")
fi

# THE INPUT LIST IS `key=value`, NOT PROSE, and that is a correction rather than
# a preference: `fetch-win-vendor.ps1` reads this block back and refuses an
# archive whose inputs disagree with pins.env, so its shape is a contract. The
# first version laid the versions out as an aligned table inside the prose, and
# the check promptly matched the sentence "ffmpeg tarball by the FFmpeg
# project's GPG signature" and reported that ffmpeg 9.0 had been "built from
# tarball". A block a machine parses should look like one.
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    ARCH_NOTE="The nv-codec-headers line is the one that matters most: it decides the MINIMUM
NVIDIA DRIVER this binary will accept, and nothing in the finished DLLs reports
it, which is why it is written down here."
else
    ARCH_NOTE="This is the ARM64 build. It carries NO NVENC, AMF or Quick Sync, because none
of the three exists on ARM64 Windows; Media Foundation (h264_mf / hevc_mf) is
its only hardware encoder, and it needs no third-party source, so this build
imposes no driver floor of its own. The toolchain is llvm-mingw rather than
Debian's GCC mingw-w64, since GCC has no aarch64 Windows target at all."
fi

cat > "$STAGE/BUILD.txt" <<EOF
Cosmic Capture Kit's own ffmpeg build for Windows $WIN_ARCH.

Built by scripts/ffmpeg-win/ (Dockerfile + build.sh) with $CROSS, from the
pinned upstream sources below. Every one is verified before use: the source
tarball by the FFmpeg project's GPG signature, the rest by SHA-256.

$ARCH_NOTE

[build-inputs]
$(printf '%s\n' "${BUILD_INPUTS[@]}")
[/build-inputs]

License: GPL version 3 (--enable-gpl --enable-version3). See LICENSE. The
corresponding ffmpeg source is the unmodified upstream release tarball:
$(echo $FFMPEG_SOURCE_URL | cut -d' ' -f1)

configure:
$(printf '  %s\n' "${CONFIGURE_ARGS[@]}")
EOF

log "packing $NAME.7z"
rm -f "$ARCHIVE"
(cd "$WORK/pkg" && 7z a -t7z -mx=9 "$ARCHIVE" "$NAME" >/dev/null)
SHA="$(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
printf '%s  %s\n' "$SHA" "$NAME.7z" > "$ARCHIVE.sha256"

SIZE="$(du -h "$ARCHIVE" | cut -f1)"

# ── Report ───────────────────────────────────────────────────────────────────
#
# The NVENC API version is the headline, because it IS the deliverable: it is
# the NVIDIA driver floor, it is the number that moved under us when the build
# was somebody else's, and it is the number no finished binary can be asked for.
log "ffmpeg:          $FFMPEG_VERSION (cck$REV, $WIN_ARCH)"
if [[ "$WIN_ARCH" == "x86_64" ]]; then
    log "NVENC API:       $NVENC_API.$NVENC_API_MINOR (from nv-codec-headers $NVCODEC_VERSION)"
else
    log "hardware tier:   Media Foundation only (no NVENC/AMF/QSV on ARM64 Windows)"
fi
log "archive:         $NAME.7z  ($SIZE)"
log "sha256:          $SHA"

# The publish instructions, with the sha256 of the file that was JUST built.
#
# THE HASH IS RECORDED, NOT PREDICTED. This build is not byte-reproducible: a PE
# carries a link timestamp in its COFF header and the 7z container stores
# mtimes, so two runs from identical sources produce different bytes and a
# different sha256. That is fine, because the sha256's job here is to pin the
# artifact we are ACTUALLY publishing, not to prove determinism we do not have.
# It is the INPUTS that are reproducible: pinned, checksummed, and signed in
# ffmpeg's case. Never copy a hash from a previous build.
cat <<EOF

Next, publish it. Nothing else in this repo ever builds ffmpeg: CI, \`just dev\`
and the MSI all FETCH this archive, so it has to exist on the mirror first.

  gh release upload vendor-mirror \\
      target/artifacts/$NAME.7z \\
      --repo Frosthaven/cosmic-capture-kit

Then set these two lines in scripts/pins.env, in the same commit as whatever pin
move prompted the rebuild:

${PINS_KEY}_URL=https://github.com/Frosthaven/cosmic-capture-kit/releases/download/vendor-mirror/$NAME.7z
${PINS_KEY}_SHA256=$SHA

EOF

# NVENC_API is empty on ARM64, and the `just` recipes only print the line when it
# is not, so an ARM64 build reports no driver floor rather than reporting a blank
# one.
printf 'CCK_FFMPEG_WIN_ARCHIVE=%s\nCCK_FFMPEG_WIN_SHA256=%s\nCCK_FFMPEG_WIN_ARCH=%s\nCCK_FFMPEG_WIN_NVENC_API=%s\n' \
    "$NAME.7z" "$SHA" "$WIN_ARCH" \
    "$([[ -n "$NVENC_API" ]] && printf '%s.%s' "$NVENC_API" "$NVENC_API_MINOR" || true)" \
    > "$OUT/ffmpeg-win.env"
