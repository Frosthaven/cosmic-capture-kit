#!/usr/bin/env bash
# Cross-compile the Windows ffmpeg (DRAGON-675). RUNS INSIDE the container built
# from the Dockerfile beside this file; `just ffmpeg-win` is the intended caller,
# from Linux, macOS or Windows, since all three only need docker.
#
# It expects:
#   /src    the repo, mounted READ-ONLY (nothing here writes to the checkout)
#   /out    a writable scratch dir, mounted from the host's `target-ffmpeg-win/`
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
CROSS=x86_64-w64-mingw32

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ -f "$SRC/scripts/pins.env" ]] || die "expected the repo mounted read-only at $SRC"
[[ -d "$OUT" ]] || die "expected a writable output dir at $OUT"
[[ -d "$W64/include/AMF" ]] || die "this script must run inside the cck-ffmpeg-win image"

# shellcheck source=../pins.env
. "$SRC/scripts/pins.env"

REV="${FFMPEG_WINDOWS_BUILD_REV:?pins.env has no FFMPEG_WINDOWS_BUILD_REV}"

# THE NAME CARRIES THE DRIVER FLOOR. `ffmpeg-9.0-nvcodec-n13.0.19.1-cck1` says
# the ffmpeg release, the nv-codec-headers tag (which IS the minimum NVIDIA
# driver, and the whole reason this build exists), and our own revision for a
# rebuild that changes neither. Anyone reading the asset list on the mirror, or
# the URL in pins.env, can see which floor they are about to ship without
# opening anything.
NAME="ffmpeg-$FFMPEG_VERSION-nvcodec-$NVCODEC_VERSION-cck$REV-win64-shared"
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
CONFIGURE_ARGS=(
    --prefix="$WORK/stage"
    --arch=x86_64 --target-os=mingw32
    --cross-prefix="$CROSS-" --enable-cross-compile
    --pkg-config=pkg-config --pkg-config-flags=--static
    --extra-version="cck$REV"
    --enable-shared --disable-static
    --disable-doc --disable-debug --disable-autodetect
    --enable-w32threads
    --enable-gpl --enable-version3
    --enable-zlib
    --enable-libx264 --enable-libx265
    --enable-ffnvcodec --enable-nvenc
    --enable-amf
    --enable-libvpl
    --enable-d3d11va --enable-dxva2
    --enable-sdl2
    --extra-cflags="-I$W64/include"
    --extra-ldflags="-L$W64/link-static -L$W64/lib -static-libgcc -static-libstdc++"
    --extra-libs="-Wl,-Bstatic -lstdc++ -Wl,-Bdynamic"
)

log "configuring ffmpeg $FFMPEG_VERSION (mingw-w64 cross, shared)"
PKG_CONFIG_LIBDIR="$W64/lib/pkgconfig" ./configure "${CONFIGURE_ARGS[@]}"

log "building (this is the long part)"
make -j"$(nproc)"
make install

# ── Verify, do not assume ────────────────────────────────────────────────────

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
check_imports() {
    local f="$1" bad
    bad="$("$CROSS-objdump" -p "$f" | awk '/DLL Name:/ {print $3}' \
        | grep -Ei '^(libwinpthread|libgcc_s|libstdc\+\+|libssp|zlib1|SDL2|libx264|libx265)' || true)"
    [[ -z "$bad" ]] || die "$(basename "$f") imports libraries we do not ship:
$bad
The link line lost -static-libgcc/-static-libstdc++, or a dependency was found
as an import library (a *.dll.a) where only its static archive should exist."
}
for f in "$BIN"/*.exe "$BIN"/*.dll; do check_imports "$f"; done

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

NVENC_API="$(sed -n 's/.*NVENCAPI_MAJOR_VERSION *\([0-9]*\).*/\1/p' "$W64/include/ffnvcodec/nvEncodeAPI.h" | head -1)"
NVENC_API_MINOR="$(sed -n 's/.*NVENCAPI_MINOR_VERSION *\([0-9]*\).*/\1/p' "$W64/include/ffnvcodec/nvEncodeAPI.h" | head -1)"

# THE INPUT LIST IS `key=value`, NOT PROSE, and that is a correction rather than
# a preference: `fetch-win-vendor.ps1` reads this block back and refuses an
# archive whose inputs disagree with pins.env, so its shape is a contract. The
# first version laid the versions out as an aligned table inside the prose, and
# the check promptly matched the sentence "ffmpeg tarball by the FFmpeg
# project's GPG signature" and reported that ffmpeg 9.0 had been "built from
# tarball". A block a machine parses should look like one.
cat > "$STAGE/BUILD.txt" <<EOF
Cosmic Capture Kit's own ffmpeg build for Windows x86_64.

Built by scripts/ffmpeg-win/ (Dockerfile + build.sh) with mingw-w64, from the
pinned upstream sources below. Every one is verified before use: the source
tarball by the FFmpeg project's GPG signature, the rest by SHA-256.

The nv-codec-headers line is the one that matters most: it decides the MINIMUM
NVIDIA DRIVER this binary will accept, and nothing in the finished DLLs reports
it, which is why it is written down here.

[build-inputs]
ffmpeg=$FFMPEG_VERSION
build-rev=cck$REV
nv-codec-headers=$NVCODEC_VERSION
nvenc-api=$NVENC_API.$NVENC_API_MINOR
x264=$X264_VERSION
x265=$X265_VERSION
zlib=$ZLIB_VERSION
sdl2=$SDL2_VERSION
amf=$AMF_VERSION
libvpl=$LIBVPL_VERSION
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
log "ffmpeg:          $FFMPEG_VERSION (cck$REV)"
log "NVENC API:       $NVENC_API.$NVENC_API_MINOR (from nv-codec-headers $NVCODEC_VERSION)"
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

FFMPEG_WINDOWS_X64_URL=https://github.com/Frosthaven/cosmic-capture-kit/releases/download/vendor-mirror/$NAME.7z
FFMPEG_WINDOWS_X64_SHA256=$SHA

EOF

printf 'CCK_FFMPEG_WIN_ARCHIVE=%s\nCCK_FFMPEG_WIN_SHA256=%s\nCCK_FFMPEG_WIN_NVENC_API=%s.%s\n' \
    "$NAME.7z" "$SHA" "$NVENC_API" "$NVENC_API_MINOR" > "$OUT/ffmpeg-win.env"
