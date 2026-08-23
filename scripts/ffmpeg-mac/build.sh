#!/usr/bin/env bash
# Build the macOS ffmpeg (DRAGON-689). RUNS ON an Apple Silicon Mac; there is no
# container, because an ObjC build against the macOS SDK needs a Mac. Invoke it
# straight from the repo root, once per architecture:
#
#   bash scripts/ffmpeg-mac/build.sh                 # arm64, the default
#   bash scripts/ffmpeg-mac/build.sh --arch x86_64   # Intel, cross-compiled
#
# WHAT IT PRODUCES: one `.tar.xz` in `target/artifacts/` PER ARCHITECTURE,
# holding a top-level folder with `bin/` (thin `ffmpeg` and `ffprobe` front-ends
# beside ONE set of libav dylibs), plus the GPLv3 text and a build manifest. A
# `.sha256` lands beside it.
#
# ── Why TWO archives rather than one universal binary (DRAGON-700) ───────────
#
# `lipo` could fuse the two into one fat Mach-O, and that is deliberately not
# what we ship. The sidecar IS the bulk of the .app (the seven libav dylibs plus
# two front-ends, ~23 MB compressed against a few MB of our own code), so a
# universal build would very nearly DOUBLE every download to give each user a
# copy of an architecture their Mac cannot execute. Splitting per architecture
# also matches what Linux already does: four artifacts, one per (kind, arch)
# pair, each named for the machine it runs on. One rule for every platform beats
# a mac-shaped exception, and the update manifest already has a key per artifact
# to address them with.
#
# THIS IS A PRODUCER, AND IT RUNS ONCE PER PIN MOVE. Nothing else in the repo
# ever builds ffmpeg for macOS: GitHub CI, `just build` and the dmg all FETCH the
# published archive through `fetch-mac-vendor.sh`, gated by
# FFMPEG_MACOS_ARM64_SHA256. So the dev loop runs the exact bytes a customer's
# .app carries, and a hosted macOS runner needs nothing but the download.
#
# WHAT IT DOES NOT DO: publish. The archive and its sha256 land in
# `target/artifacts/`, and the script prints the upload command plus the
# `pins.env` lines to paste. Publishing is a human step on purpose: it puts a
# binary in front of users.
#
# ── Why this build exists at all ─────────────────────────────────────────────
#
# It replaces two STATIC prebuilts from martin-riedl.de, 66 MB each. Static means
# every codec library is compiled into BOTH programs, so the pair carried two
# copies of the same code and made up 132 MB of a 175 MiB installed .app. One
# shared build is one copy.
#
# The other half is the two-class rule at the top of `scripts/pins.env`: what an
# ffmpeg binary demands of the user's machine is decided when it is COMPILED, and
# for a prebuilt we neither choose those inputs nor can always read them back.
# On macOS the inherited floor (the Mach-O deployment target) at least WAS
# readable, so DRAGON-675 left the prebuilt in place with a fetch-time assertion.
# Size is what changed the answer, not safety. The deployment target is now one
# of OUR pins, asserted the same way, rather than whatever the upstream builder
# happened to target.
#
# ── Why this build's feature set looks so much smaller than the prebuilt's ────
#
# The martin-riedl builds enable dozens of external libraries. This one enables
# TWO, zlib and x264, plus three Apple system frameworks that bundle no code of
# their own. The difference is not thrift: every external library is a thing
# whose version we would then own and have to track. The list below is derived
# from what the app actually invokes, and each entry names the code that needs
# it. Everything else ffmpeg can do here it does with its own built-in codecs,
# muxers and filters, which come with the source and cost nothing to keep.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# ── Which architecture ───────────────────────────────────────────────────────
#
# `arm64` by default, so a bare invocation does exactly what it did before
# DRAGON-700. The label is Apple's spelling of the CPU (`uname -m`, `lipo
# -archs`, `-arch`), which is what every check below compares against; the Rust
# target triple's `aarch64` is a different vocabulary and is deliberately not
# used here.
ARCH=arm64
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)
            ARCH="${2:?--arch needs a value: arm64 or x86_64}"
            shift 2
            ;;
        --arch=*)
            ARCH="${1#--arch=}"
            shift
            ;;
        -h|--help)
            sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done
case "$ARCH" in
    arm64|x86_64) ;;
    aarch64) die "use --arch arm64 (Apple's spelling); 'aarch64' is the Rust triple's" ;;
    *) die "unknown --arch '$ARCH' (expected arm64 or x86_64)" ;;
esac

# The work dir is PER ARCHITECTURE, so building both in a row cannot have the
# second run's configure land in the first one's tree, and a failed x86_64 build
# leaves an arm64 one alone.
WORK="${TMPDIR:-/tmp}/cck-ffmpeg-mac-$ARCH"

# THE WORK DIR GOES AWAY WHEN THIS SCRIPT ENDS, PASS OR FAIL. It holds an ffmpeg
# source tree plus two build trees, well over a gigabyte, and CLAUDE.md's rule
# about build residue is a disk-space bug report rather than housekeeping: three
# machines have had their drives filled by exactly this. The failure path prints
# the tail of ffmpeg's config.log first, so the one file worth reading survives
# the cleanup as terminal output.
cleanup() {
    local rc=$?
    if [[ $rc -ne 0 && -f "$WORK/ffmpeg-${FFMPEG_VERSION:-}/ffbuild/config.log" ]]; then
        printf '\n--- last 40 lines of ffmpeg ffbuild/config.log ---\n' >&2
        tail -40 "$WORK/ffmpeg-${FFMPEG_VERSION}/ffbuild/config.log" >&2 || true
        printf -- '--- end of config.log ---\n\n' >&2
    fi
    rm -rf "$WORK"
    return $rc
}
trap cleanup EXIT

# ── Host preflight ───────────────────────────────────────────────────────────
#
# Everything is checked BEFORE anything is downloaded or compiled. A build that
# gets twenty minutes in and then dies on a missing `xz` has wasted the time and
# says nothing useful about what to install.
[[ "$(uname -s)" == "Darwin" ]] || die "this builds Mach-O binaries and only runs on macOS (host is $(uname -s))"
HOST_ARCH="$(uname -m)"

# WHICH HOST MAY BUILD WHICH TARGET (DRAGON-700). x86_64 builds on EITHER host,
# natively on an Intel Mac and cross-compiled from Apple Silicon, because clang
# targets both from one macOS SDK and every check below reads the finished
# Mach-O rather than running it. arm64 still needs an arm64 host: an Intel Mac's
# SDK can emit arm64 too, but nobody on this project has one to prove it on, and
# an unverified cross-build of the archive every Apple Silicon user runs is not
# a trade worth taking silently.
if [[ "$ARCH" == "arm64" && "$HOST_ARCH" != "arm64" ]]; then
    die "an arm64 archive must be built on an arm64 Mac (host is $HOST_ARCH).
Cross-building it from Intel is untested here; run this on Apple Silicon."
fi

# `nasm` IS required for x86_64 and is genuinely useless for arm64, which is why
# the list is conditional rather than one superset. x264 assembles its x86 SIMD
# with nasm and has no other use for it; on arm64 that assembly goes through the
# C compiler's own integrated assembler instead. Asking for nasm on an arm64
# build would fail an Apple Silicon machine for a tool the build never invokes.
#
# An `if`, not `[[ … ]] && tools+=(…)`: under `set -e` a false test as a bare
# command is the script's exit status, so the arm64 build would die right here
# for taking the branch it is supposed to take. The same trap `mac_min_os` in
# fetch-mac-vendor.sh already documents.
tools=(clang make pkg-config xz curl gpg shasum tar otool vtool lipo install_name_tool)
if [[ "$ARCH" == "x86_64" ]]; then
    tools+=(nasm)
fi
missing=()
for tool in "${tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [[ ${#missing[@]} -gt 0 ]]; then
    die "missing host tools: ${missing[*]}
  clang, make, otool, vtool, lipo, install_name_tool  Xcode command line tools:  xcode-select --install
  pkg-config, xz, gpg                                 brew install pkg-config xz gnupg
  nasm (x86_64 builds only, for x264's SIMD)          brew install nasm"
fi
xcrun --sdk macosx --show-sdk-path >/dev/null 2>&1 \
    || die "xcrun cannot resolve the macOS SDK. Point xcode-select at a working
developer dir:  sudo xcode-select --switch /Applications/Xcode.app"

PINS="$REPO_ROOT/scripts/pins.env"
[[ -f "$PINS" ]] || die "missing $PINS (the shared pin manifest)"
# shellcheck source=../pins.env
. "$PINS"

# The ONE list of files this build must produce, shared with
# `scripts/fetch-mac-vendor.sh` (which stages them) and `scripts/mac-package.sh`
# (which bundles and signs them). Kept out of this file on purpose: three
# hand-maintained copies of one list is how a future ffmpeg bump publishes an
# archive whose contents nothing downstream expects.
# shellcheck source=../mac-sidecars.sh
. "$REPO_ROOT/scripts/mac-sidecars.sh"

REV="${FFMPEG_MACOS_BUILD_REV:?pins.env has no FFMPEG_MACOS_BUILD_REV}"
MINOS="${FFMPEG_MACOS_MINOS:?pins.env has no FFMPEG_MACOS_MINOS}"

# THE NAME CARRIES THE FFMPEG RELEASE, OUR OWN REVISION AND THE ARCHITECTURE, so
# a rebuild that changes the configure line or a dependency pin without moving
# FFMPEG_VERSION still produces a different asset name, a different URL and a
# different stamp in `vendor/`, and the two architectures can never be served for
# each other. Anyone reading the asset list on the mirror can see what they are
# about to ship without downloading it.
NAME="ffmpeg-$FFMPEG_VERSION-cck$REV-macos-$ARCH-shared"

# EVERY COMPILER INVOCATION IN THIS SCRIPT INHERITS THE DEPLOYMENT TARGET, x264's
# included. clang reads MACOSX_DEPLOYMENT_TARGET and stamps LC_BUILD_VERSION from
# it, which is the number `fetch-mac-vendor.sh` asserts and the number that
# decides which Macs can run the app at all. It matches the .app's own
# LSMinimumSystemVersion in `scripts/mac-package.sh`; raising one without the
# other is how a supported macOS ends up installing a bundle that dies at launch
# on a dyld error.
export MACOSX_DEPLOYMENT_TARGET="$MINOS"

# THE ARCHITECTURE TRAVELS IN THE COMPILER COMMAND, not in a per-project flag.
# `clang -arch <arch>` decides what the object files are, and because clang also
# does the LINKING for both x264 and ffmpeg, one string covers compiling and
# linking in every sub-build. That is what makes the cross-build one variable
# rather than a matrix of configure options per project.
#
# The macOS SDK is universal, so cross-compiling here is a compiler flag and not
# a toolchain: there is no sysroot to fetch, no second clang, and the frameworks
# (VideoToolbox, AVFoundation, AudioToolbox) resolve exactly as they do natively.
TARGET_CC="clang -arch $ARCH"
# Is this run cross-compiling? Everything conditional below hangs off this one
# question rather than re-comparing strings, so a future third architecture, or
# an Intel host, gets the same answer everywhere.
#
# An `if` again, for the `set -e` reason spelled out at the tool list above.
CROSS=0
if [[ "$ARCH" != "$HOST_ARCH" ]]; then
    CROSS=1
fi

STAGE="$WORK/stage"
rm -rf "$WORK"
mkdir -p "$WORK" "$STAGE"
cd "$WORK"

# Mirror list, tried in order, first success wins. Shared by every fetch below
# and identical to what `scripts/appimage/Dockerfile` and
# `scripts/ffmpeg-win/build.sh` do, because they fetch the same tarballs.
#
# Availability, never trust: every one of these is still gated by the pinned
# sha256 (and for ffmpeg, by the pinned GPG fingerprint), so a hostile mirror
# fails exactly as a hostile upstream would.
fetch_any() {
    local out="$1"; shift
    for u in "$@"; do
        curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 \
             --speed-time 30 --speed-limit 1024 -o "$out" "$u" && return 0
        echo "mirror failed, trying the next: $u" >&2
    done
    die "every mirror failed for $out"
}

verify_sha256() { # <file> <expected> <label>
    local got
    got="$(shasum -a 256 "$1" | awk '{print $1}')"
    [[ "$got" == "$2" ]] && return 0
    die "SHA-256 mismatch for $3
  expected  $2
  got       $got
Either the upstream file changed under a URL that promised to be immutable, or a
pin in scripts/pins.env was edited without its hash. Neither gets better on a
second download."
}

# ── The ffmpeg source: the SAME tarball Linux and Windows compile ────────────
#
# VERIFIED BY THE UPSTREAM GPG SIGNATURE and not by a checksum alone. A hash we
# carry only proves the bytes match what we saw when the pin was made; the
# signature proves the FFmpeg project produced them. The fingerprint is pinned in
# pins.env and published on ffmpeg.org/download.html, and the signature must be
# made by exactly it, so a compromised key URL supplying both a key and a
# matching signature still fails.
log "fetching ffmpeg $FFMPEG_VERSION source"
fetch_any ffmpeg.tar.xz $FFMPEG_SOURCE_URL
fetch_any ffmpeg.tar.xz.asc $FFMPEG_SOURCE_ASC_URL
fetch_any ffmpeg-devel.asc $FFMPEG_GPG_KEY_URL
verify_sha256 ffmpeg.tar.xz "$FFMPEG_SOURCE_SHA256" "ffmpeg $FFMPEG_VERSION source"

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

# ── x264, STATIC, linked INTO libavcodec ─────────────────────────────────────
#
# The software H.264 tier (`encode/plan.rs`'s `software_plan` is literally
# `-c:v libx264`). ffmpeg has no native H.264 ENCODER, so without this a Mac that
# cannot open VideoToolbox could not record at all.
#
# STATIC ON PURPOSE, and it is the one place this build's layout differs from a
# distro's. A shared libx264 would be an eighth dylib to ship, sign and keep in
# step; the static archive disappears into libavcodec.dylib and nothing outside
# the bundle can see it. `--enable-pic` is what makes that legal: a static
# archive linked into a dylib must be position-independent, and while clang
# builds PIC by default on Apple Silicon, asking for it explicitly means the
# build fails loudly rather than depending on that default staying true.
#
# The SAME pin the Linux AppImage and the Windows cross-build use, so all three
# platforms' software H.264 comes from one version of one encoder.
log "fetching x264 $X264_VERSION source"
fetch_any x264.tar.xz $X264_URL
verify_sha256 x264.tar.xz "$X264_SHA256" "x264 $X264_VERSION source"
tar xJf x264.tar.xz

log "building x264 $X264_VERSION (static, PIC, $ARCH)"
(
    cd "x264-$X264_VERSION"
    # `--host` is what tells x264's configure it is cross-compiling: without it
    # the configure script probes the BUILD machine, picks arm64 assembly for an
    # x86_64 target, and the mismatch only surfaces as a link error much later.
    # It is passed on a native build too, where it is simply the truth, so the
    # two paths differ in the value and not in the shape.
    x264_args=(--prefix="$STAGE" --host="$ARCH-apple-darwin"
        --enable-static --enable-pic --disable-cli --disable-opencl)
    CC="$TARGET_CC" ./configure "${x264_args[@]}" >/dev/null \
        || die "x264 configure failed"
    make -j"$(sysctl -n hw.ncpu)" >/dev/null
    make install >/dev/null
)
[[ -f "$STAGE/lib/libx264.a" ]] || die "x264 built no static archive at $STAGE/lib/libx264.a"
# The static archive is about to disappear into libavcodec, where nothing
# downstream can tell what architecture it was. Catch a wrong one HERE, where the
# message can name x264, rather than as an unresolved-symbol wall in ffmpeg's
# link step.
x264_arch="$(lipo -archs "$STAGE/lib/libx264.a" 2>/dev/null || true)"
[[ "$x264_arch" == *"$ARCH"* ]] || die "libx264.a is '$x264_arch', not $ARCH.
x264's configure did not take the cross-compilation settings."

# ── configure ────────────────────────────────────────────────────────────────
#
# --disable-autodetect is what fixes this build's FEATURE SET, rather than
# leaving it as "whatever this Mac happened to have installed". With it, every
# external library is an explicit decision below, and a missing one FAILS the
# build instead of quietly producing an ffmpeg without an encoder somebody needs.
# It also shuts Homebrew out: a developer's /opt/homebrew is full of libraries a
# stock configure would happily find and link, producing a bundle that runs here
# and dies on every customer's Mac.
#
# What each enable is for:
#   pthreads       threading. It is in ffmpeg's AUTODETECT list, so with
#                  autodetect off it must be asked for by name or the build is
#                  SINGLE-THREADED and nobody notices until a recording cannot
#                  keep up. It is also a hard dependency of the avfoundation
#                  input and the VideoToolbox hwaccel below.
#   zlib           ffmpeg's PNG encoder is gated on it, and `preview/video.rs`
#                  pulls frames out of a recording with `-f image2pipe
#                  -vcodec png`
#   libx264        the software H.264 tier (`encode/plan.rs`)
#   videotoolbox   the mac hardware tier: `h264_videotoolbox` /
#                  `hevc_videotoolbox` (`encode/plan.rs::videotoolbox_plan`),
#                  AND the hwaccel the preview editor's `-hwaccel auto` decode
#                  picks up (`app/preview/playback.rs`)
#   avfoundation   the mic INVENTORY, `ffmpeg -f avfoundation -list_devices true`
#                  (`audio/devices.rs::list_avfoundation_inputs`, DRAGON-132),
#                  the settings-page mic meter (`audio/meters.rs`) and the AEC
#                  far-end monitor tap (`audio/clean_mic.rs::spawn_pulse_pcm`).
#                  Mic CAPTURE during a recording is native and does not need it
#                  (`platform/mac/services/mic_capture.rs`), but enumeration
#                  still shells this out.
#   audiotoolbox   the `-f audiotoolbox` OUTPUT device. The shipped app does not
#                  use it: the preview player owns an AudioQueue directly
#                  (`platform/mac/services/audio_out.rs`, which replaced exactly
#                  this). It is kept for the one live test that plays a tone
#                  through it to prove the system-audio meter is fed
#                  (`record_worker_live_tests::live_sck_sys_meter_publishes_levels`),
#                  and it costs nothing: AudioToolbox is a system framework, so
#                  nothing extra is bundled.
#
# NOT ENABLED, deliberately:
#   libx265        `encode/plan.rs` NEVER puts it on a mac command line. The
#                  software tier is unconditionally H.264 (DRAGON-674: libx265
#                  cannot encode 1080p in real time, so the resolver degrades
#                  rather than trusting the UI), and the HEVC choice is served by
#                  `hevc_videotoolbox`. `encode/preset.rs` pins that in a test.
#   sdl2, ffplay   the Windows build needs ffplay as its preview audio sink
#                  because ffmpeg has no audio output device there. macOS has the
#                  native AudioQueue instead, and today's bundle already ships no
#                  ffplay, so there is nothing to keep working.
#   nvenc, amf, qsv  no such hardware exists on Apple Silicon. This is also why
#                  the NVIDIA-driver-floor bug class DRAGON-671 chased is
#                  structurally impossible here.
#
# --install-name-dir='@loader_path' IS THE WHOLE POINT OF THE SHARED LAYOUT. It
# makes each dylib record its own install name as `@loader_path/<name>`, so a
# binary resolves its siblings from ITS OWN directory. The exes and the dylibs
# then sit flat in one folder (`Contents/Resources/` in the .app,
# `vendor/ffmpeg/macos-<arch>/` in a checkout) and work from ANY directory, with
# no absolute path baked in and no rpath to rewrite at bundle time.
# `@loader_path` and not `@executable_path`: the latter would break the moment a
# dylib were loaded by anything other than these two exes.
#
# --extra-version puts our build revision in `ffmpeg -version`, which is what the
# app captures into its debug log. "ffmpeg version 9.0-cck1" says at a glance
# whose binary a customer is running.
#
# ONE array, used to configure AND recorded verbatim into the archive's
# BUILD.txt, so what a shipped binary says it was built with cannot drift from
# what it was actually built with.
CONFIGURE_ARGS=(
    --prefix="$STAGE"
    --extra-version="cck$REV"
    --enable-shared --disable-static
    --disable-doc --disable-debug --disable-autodetect
    --disable-ffplay
    --enable-pthreads
    --enable-gpl --enable-version3
    --enable-zlib
    --enable-libx264
    --enable-videotoolbox
    --enable-avfoundation
    --enable-audiotoolbox
    --install-name-dir='@loader_path'
    --arch="$ARCH"
    --cc="$TARGET_CC"
    --extra-cflags="-I$STAGE/include"
    --extra-ldflags="-L$STAGE/lib"
)

# THE THREE FLAGS A CROSS-BUILD NEEDS, and only on a cross-build (DRAGON-700):
#
#   --enable-cross-compile  stops configure RUNNING its probe programs. Without
#                           it, an x86_64 test binary is executed on an arm64
#                           host: either Rosetta answers (making the result a
#                           statement about Rosetta) or it does not (and every
#                           feature probes as absent). Neither is a build.
#   --target-os=darwin      configure normally reads the target OS off `uname`,
#                           which describes the HOST. It is the same OS here, so
#                           this is only saying out loud what the probe can no
#                           longer discover for itself.
#   --host-cc=clang         the build's OWN helper programs, which run HERE. They
#                           must be native, or make stops to invoke an x86_64
#                           tool on an arm64 machine. `--cc` above is the TARGET
#                           compiler; these two are genuinely different jobs and
#                           ffmpeg is one of the few build systems that says so.
#
# On a native build all three are absent, so the arm64 configure line stays
# exactly what it was before this ticket.
if [[ $CROSS -eq 1 ]]; then
    CONFIGURE_ARGS+=(--enable-cross-compile --target-os=darwin --host-cc=clang)
fi

if [[ $CROSS -eq 1 ]]; then
    log "configuring ffmpeg $FFMPEG_VERSION (CROSS $HOST_ARCH -> $ARCH, shared, minos $MINOS)"
else
    log "configuring ffmpeg $FFMPEG_VERSION (native $ARCH, shared, minos $MINOS)"
fi
cd "ffmpeg-$FFMPEG_VERSION"
# PKG_CONFIG_LIBDIR, not PKG_CONFIG_PATH: the former REPLACES pkg-config's
# default search path instead of prepending to it, which is what keeps a
# Homebrew x264.pc from winning over the one just built into $STAGE.
PKG_CONFIG_LIBDIR="$STAGE/lib/pkgconfig" ./configure "${CONFIGURE_ARGS[@]}"

log "building (this is the long part)"
make -j"$(sysctl -n hw.ncpu)"
make install

# ── Verify, do not assume ────────────────────────────────────────────────────

BIN="$STAGE/bin"
LIB="$STAGE/lib"

# Both programs are load-bearing: ffmpeg records, ffprobe measures. A missing one
# is a customer-visible feature gone, so prove they arrived rather than trusting
# `make install`.
for exe in "${CCK_MAC_FFMPEG_EXES[@]}"; do
    [[ -x "$BIN/$exe" ]] || die "$exe was not built"
done
# ffplay is disabled above; if one ever appears here, the configure line changed
# under somebody and the archive would grow an SDL2 dependency we do not ship.
[[ ! -e "$BIN/ffplay" ]] || die "ffplay was built despite --disable-ffplay; the configure line changed"

# The exact dylib set the shared build must produce, from the shared list rather
# than globbed: the numbers ARE the ffmpeg generation (9.x), so a set that does
# not match means the source moved under the pin. They are the same seven the
# Windows archive carries.
for dylib in "${CCK_MAC_FFMPEG_DYLIBS[@]}"; do
    [[ -e "$LIB/$dylib" ]] || die "expected $dylib in the build output; got: $(cd "$LIB" && echo ./*.dylib)
If ffmpeg's soname generation moved, update scripts/mac-sidecars.sh to match."
done

# ── Stage the archive ────────────────────────────────────────────────────────
#
# FLAT, and everything DEREFERENCED. `make install` leaves three names per
# library (the real `libavcodec.63.19.100.dylib` plus `libavcodec.63.dylib` and
# `libavcodec.dylib` symlinks). Only the MAJOR name is ever loaded, because that
# is what `-install_name` recorded, so `cp -L` collapses each set to one real
# file under that name. Nothing downstream then has to reason about symlinks: not
# the bundle copy, not `codesign`, not notarization.
#
# The LICENCE and the build record travel WITH the binaries. ffmpeg here is built
# --enable-gpl --enable-version3, so what we hand a user is GPLv3 code, and the
# obligation that comes with it is to be able to say exactly which source and
# which configuration produced it. BUILD.txt is that answer, in the archive,
# rather than in a repo the recipient may not have.
PKG="$WORK/pkg/$NAME"
rm -rf "$WORK/pkg"
mkdir -p "$PKG/bin"
for exe in "${CCK_MAC_FFMPEG_EXES[@]}"; do
    cp "$BIN/$exe" "$PKG/bin/$exe"
done
for dylib in "${CCK_MAC_FFMPEG_DYLIBS[@]}"; do
    cp -L "$LIB/$dylib" "$PKG/bin/$dylib"
done
chmod 0755 "$PKG/bin/"*
cp COPYING.GPLv3 "$PKG/LICENSE"

# NO ABSOLUTE INSTALL NAME MAY SURVIVE, and NOTHING OUTSIDE THE BUNDLE MAY BE
# LINKED. This is the single most likely way a Mac build breaks, and it is
# invisible on the machine that produced it: a dylib still naming $STAGE, or an
# exe pulling libx264 out of /opt/homebrew, runs perfectly here and dies with a
# dyld error on every customer's Mac. `mac-package.sh::_check_relocatable`
# enforces the same rule at bundle time; this catches it three stages earlier,
# next to the build that caused it.
check_links() { # <mach-o>
    local f="$1" bad
    bad="$(otool -L "$f" | tail -n +2 | awk '{print $1}' \
        | grep -vE '^(/usr/lib/|/System/Library/|@loader_path/)' || true)"
    [[ -z "$bad" ]] || die "$(basename "$f") links libraries that will not exist on a customer's Mac:
$bad
Either --install-name-dir did not take, or configure resolved a dependency to
Homebrew despite --disable-autodetect."
}
for f in "$PKG/bin/"*; do check_links "$f"; done

# AND THE FLOOR IT IMPOSES ON THE USER. Every Mach-O carries LC_BUILD_VERSION, so
# unlike the Windows NVENC floor this one is readable from the file, and
# `fetch-mac-vendor.sh` asserts it again at fetch time. Checking it HERE is what
# makes MACOSX_DEPLOYMENT_TARGET a fact rather than a hope: a toolchain that
# ignored the variable would otherwise ship a binary demanding whatever the SDK
# defaults to, which on a current Xcode is far above the .app's floor.
for f in "$PKG/bin/"*; do
    got="$(vtool -show-build "$f" 2>/dev/null | awk '/^[[:space:]]*minos/ {print $2; exit}')"
    [[ "$got" == *.*.0 ]] && got="${got%.0}"
    [[ -n "$got" ]] || die "could not read the minimum macOS version out of $(basename "$f")"
    [[ "$got" == "$MINOS" ]] || die "$(basename "$f") declares minimum macOS $got, but pins.env says $MINOS.
MACOSX_DEPLOYMENT_TARGET did not reach every compiler invocation. A binary above
the .app's LSMinimumSystemVersion means a supported Mac installs the app and then
fails at launch with a dyld error."
done

# AND WHAT CPU IT IS FOR, on every file (DRAGON-700). This is THE cross-build
# check: nothing else here would notice a Mach-O built for the host instead of
# the target, because such a file passes every linkage and deployment-target
# check above and runs perfectly on this machine. It would then be published
# under an x86_64 asset name, bundled into the Intel dmg, and die with "Bad CPU
# type in executable" on the first customer's Mac.
#
# EXACTLY ONE architecture per file, asserted as equality rather than a substring
# match: `lipo -archs` prints a space-separated list for a fat binary, and a fat
# sidecar is the size regression this whole split exists to avoid.
for f in "$PKG/bin/"*; do
    got="$(lipo -archs "$f" 2>/dev/null || true)"
    [[ "$got" == "$ARCH" ]] || die "$(basename "$f") is '$got', not exactly $ARCH.
A file built for the wrong CPU, or a universal one. Either way it must not ship
under a $ARCH asset name."
done

log "linkage (for the record):"
for f in "$PKG/bin/"*; do
    printf '    %-24s %s\n' "$(basename "$f")" \
        "$(otool -L "$f" | tail -n +2 | awk '{print $1}' | sed 's|.*/||' | sort -u | tr '\n' ' ')"
done

# THE INPUT LIST IS `key=value`, NOT PROSE, and that shape is a contract:
# `fetch-mac-vendor.sh` reads this block back and refuses an archive whose inputs
# disagree with pins.env. The Windows manifest learned this the hard way, where a
# prose-scraping check happily matched a sentence and reported that ffmpeg had
# been "built from tarball". A block a machine parses should look like one.
cat > "$PKG/BUILD.txt" <<EOF
Cosmic Capture Kit's own ffmpeg build for macOS $ARCH.

Built by scripts/ffmpeg-mac/build.sh on an Apple Silicon Mac (arm64 natively,
x86_64 cross-compiled with clang -arch), from the pinned upstream sources below.
Every one is verified before use: the ffmpeg source tarball by the FFmpeg
project's GPG signature, x264 by SHA-256.

SHARED, not static: thin ffmpeg and ffprobe front-ends over one set of libav
dylibs, all in bin/, each dylib naming itself \`@loader_path/<name>\` so the
folder works wherever it is put. x264 is linked in statically and ships no dylib
of its own.

The macos-deployment-target line is the one that decides which Macs can run this
at all. It is asserted against LC_BUILD_VERSION at build time and again at fetch
time, and it must not exceed the .app's LSMinimumSystemVersion.

[build-inputs]
ffmpeg=$FFMPEG_VERSION
build-rev=cck$REV
x264=$X264_VERSION
macos-deployment-target=$MINOS
arch=$ARCH
[/build-inputs]

License: GPL version 3 (--enable-gpl --enable-version3). See LICENSE. The
corresponding ffmpeg source is the unmodified upstream release tarball:
$(echo $FFMPEG_SOURCE_URL | cut -d' ' -f1)

configure:
$(printf '  %s\n' "${CONFIGURE_ARGS[@]}")
EOF

# ── Pack and publish into target/artifacts/ ──────────────────────────────────
#
# `target/artifacts/` is the ONE build-output directory (DRAGON-590), and unlike
# the Windows build (which writes from inside a container, as root) this one runs
# as the calling user, so it can put the finished file there itself.
. "$REPO_ROOT/scripts/artifacts.sh"
cd "$REPO_ROOT"
OUT_DIR="$(cck_artifacts_dir)"

log "packing $NAME.tar.xz"
tar -C "$WORK/pkg" -cJf "$WORK/$NAME.tar.xz" "$NAME"
ARCHIVE="$(cck_publish_move "$WORK/$NAME.tar.xz")"
SHA="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
printf '%s  %s\n' "$SHA" "$NAME.tar.xz" > "$OUT_DIR/$NAME.tar.xz.sha256"

PAYLOAD="$(du -sk "$PKG/bin" | awk '{printf "%.1f MB", $1/1024}')"
SIZE="$(du -h "$ARCHIVE" | awk '{print $1}')"

# The pins.env key names this archive belongs in. Derived rather than spelled
# per arch at the printf below, so the instructions can never name the other
# architecture's keys, which is a mistake nothing downstream would catch until a
# customer's Mac refused to start the sidecar.
case "$ARCH" in
    arm64)  PIN_PREFIX=FFMPEG_MACOS_ARM64 ;;
    x86_64) PIN_PREFIX=FFMPEG_MACOS_X86_64 ;;
esac

# ── Report ───────────────────────────────────────────────────────────────────
log "ffmpeg:          $FFMPEG_VERSION (cck$REV)"
HOW="native"
if [[ $CROSS -eq 1 ]]; then
    HOW="cross-compiled on $HOST_ARCH"
fi
log "architecture:    $ARCH ($HOW)"
log "minimum macOS:   $MINOS (asserted on every Mach-O above)"
log "payload:         $PAYLOAD unpacked, the two exes plus seven dylibs"
log "archive:         $NAME.tar.xz  ($SIZE)"
log "sha256:          $SHA"

# The publish instructions, with the sha256 of the file that was JUST built.
#
# THE HASH IS RECORDED, NOT PREDICTED. This build is not byte-reproducible: the
# linker embeds a UUID derived from the inputs and timestamps, and the tar
# container stores mtimes, so two runs from identical sources produce different
# bytes. That is fine, because the sha256's job here is to pin the artifact we
# are ACTUALLY publishing, not to prove a determinism we do not have. It is the
# INPUTS that are reproducible: pinned, checksummed, and signed in ffmpeg's case.
# Never copy a hash from a previous build.
cat <<EOF

Next, publish it. Nothing else in this repo ever builds the macOS ffmpeg: CI,
\`just build\` and the dmg all FETCH this archive, so it has to exist on the
mirror first.

  gh release upload vendor-mirror \\
      "$ARCHIVE" \\
      "$OUT_DIR/$NAME.tar.xz.sha256" \\
      --repo Frosthaven/cosmic-capture-kit

Then set these two lines in scripts/pins.env, in the same commit as whatever pin
move prompted the rebuild:

${PIN_PREFIX}_URL=https://github.com/Frosthaven/cosmic-capture-kit/releases/download/vendor-mirror/$NAME.tar.xz
${PIN_PREFIX}_SHA256=$SHA

THIS IS ONE OF TWO ARCHIVES. The macOS dmg ships per architecture (DRAGON-700),
and both are built from the same pins, so publish the pair together:

  bash scripts/ffmpeg-mac/build.sh --arch arm64
  bash scripts/ffmpeg-mac/build.sh --arch x86_64

EOF

cck_say_artifact "$ARCHIVE"
