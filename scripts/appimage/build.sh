#!/usr/bin/env bash
# Build the Linux AppImage. RUNS INSIDE the container built from the Dockerfile
# beside this file; `just appimage` is the only intended caller (on Linux, macOS
# and Windows alike, all three driving the same container).
#
# It expects:
#   /src    the repo, mounted READ-ONLY (nothing here writes to the checkout)
#   /work   a writable scratch dir, mounted from the host's `target-appimage/`
#
# NEVER the repo's own `target/`: that holds the live binaries the owner's
# PrintScreen shortcut launches, and a container writing there would replace them
# under a running daemon.
#
# ── What goes in the AppDir, and what deliberately does not ──────────────────
#
# BUNDLED, because the user may not have them and `util::locate_tool` finds a
# sidecar next to our own binary before it looks at PATH:
#
#   ffmpeg / ffprobe   pinned 8.1.2, built in this container (see the Dockerfile)
#   libav*.so + libx264  the same build's libraries, so the in-process GPU
#                      zero-copy encoder (`encode/gpu.rs` dlopens libavcodec /
#                      libavutil / libavfilter) works on a host that has no
#                      ffmpeg 8 at all. Debian-family users have never been able
#                      to have zero-copy; this is what gives it to them.
#   tesseract + tessdata  OCR, statically linked against leptonica and libpng so
#                      it needs nothing bundled beside it.
#
# NOT BUNDLED, on purpose: libgbm, Mesa, libdrm, libva and the Vulkan loader.
# They must come from the HOST so they match the user's GPU driver; a bundled
# copy breaks rendering wherever the driver differs from this build box.
# libpulse and libpipewire are left to the host for the same family of reason:
# they are clients of host daemons, and the app already hard-links libpulse, so
# the host demonstrably has one.
#
# ── Why RPATH and not LD_LIBRARY_PATH ────────────────────────────────────────
#
# The usual AppImage AppRun exports `LD_LIBRARY_PATH=$APPDIR/usr/lib`, which
# leaks into every CHILD process, and this app spawns host programs (the
# desktop's URI handler, the file manager, a browser for OAuth). Handing those
# our libav would be a way to crash somebody else's software. `patchelf` writes
# the search path INTO our own binaries instead, so it applies to exactly the
# processes it should and to nothing else. It also covers `dlopen`, because glibc
# consults the CALLING object's DT_RUNPATH.
set -euo pipefail

SRC=/src
WORK=/work

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ -f "$SRC/Cargo.toml" ]] || die "expected the repo mounted read-only at $SRC"
[[ -d "$WORK" ]] || die "expected a writable work dir at $WORK"

VERSION="$(grep -m1 '^version' "$SRC/Cargo.toml" | cut -d'"' -f2)"
[[ -n "$VERSION" ]] || die "could not read the version out of Cargo.toml"
APP=cosmic-capture-kit
ID=dev.frosthaven.CosmicCaptureKit

# The architecture is simply the one we are running on (DRAGON-529). Nothing here
# cross-compiles: an aarch64 artifact comes from running this same image on an
# aarch64 host. The name has to carry it because BOTH artifacts land on the same
# GitHub release, and because the app picks its update-manifest entry per arch
# (`update::linux_platform_key`) -- an aarch64 build fetching the x86_64 AppImage
# would install a binary its CPU cannot execute.
case "$(uname -m)" in
    x86_64)  ARCH=x86_64 ;;
    aarch64) ARCH=aarch64 ;;
    *) die "unsupported build architecture $(uname -m)" ;;
esac

# ARCH but NOT VERSION in the filename, and the asymmetry is deliberate.
#
# The arch has to be there: both artifacts land on the same GitHub release, and
# the app picks its update-manifest entry per arch (`update::linux_platform_key`),
# so an aarch64 build fetching the x86_64 AppImage would install a binary its CPU
# cannot execute.
#
# The VERSION must NOT be, because this file UPDATES ITSELF IN PLACE (DRAGON-532).
# `install_linux_appimage` renames the new release over `$APPIMAGE` and never
# touches the name, so that the user's capture shortcut, their autostart entry
# (`Exec=$APPIMAGE resident`) and the daemon's own respawn all keep working. A
# version in the name would therefore be WRONG the moment the first update lands:
# the file would still say 0.28.1 while containing 0.29.0, and a user who later
# downloaded the new version by hand would own two files, with their shortcut
# pointing at the one wearing the wrong number.
#
# This is where the AppImage differs from every other artifact we ship, and the
# rule follows from behaviour rather than taste. The dmg and msi are INSTALLERS,
# discarded once run. The zip is UNPACKED, and what comes out is named
# `cosmic-capture-kit` regardless. None of those three can go stale, so all three
# keep their version. This one is the download AND the installed program AND the
# thing that rewrites itself, so a stable name is the only honest one.
#
# It also makes `releases/latest/download/CosmicCaptureKit-<arch>.AppImage` a
# permanent URL, which the upstream AppImage guidance effectively asks for: its
# updater takes the destination name from the PUBLISHER's metadata, so publishers
# shipping versioned names are the ones whose users end up with renamed files and
# stray `.zs-old` copies.
OUT="$WORK/CosmicCaptureKit-$ARCH.AppImage"

# Cargo state lives in the work dir, so the container can run as the CALLING
# user and leave nothing root-owned on the host, and so an incremental rebuild
# is genuinely incremental across runs.
export CARGO_HOME="$WORK/cargo"
export CARGO_TARGET_DIR="$WORK/target"
export HOME="$WORK"

# DEFAULT FEATURES, so zero-copy is ON (DRAGON-530 settled this): libav is
# dlopened rather than linked, so the feature costs the binary nothing at launch,
# and the libraries it wants are bundled a few lines below.
log "building $APP $VERSION (release, default features)"
cd "$SRC"
cargo build --release

APPDIR="$WORK/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps"

# ── The app, its sidecars, and its libraries ─────────────────────────────────
#
# EXE-ADJACENT IS LOAD-BEARING: `util::locate_tool` probes the directory holding
# our own executable, so ffmpeg / ffprobe / tesseract must sit in `usr/bin` beside
# the app and nowhere else. `util::seed_tessdata` then reads `<tool dir>/tessdata`,
# which is why the language tree goes there too rather than into usr/share.
install -m 0755 "$CARGO_TARGET_DIR/release/$APP" "$APPDIR/usr/bin/$APP"
install -m 0755 /usr/local/bin/ffmpeg /usr/local/bin/ffprobe "$APPDIR/usr/bin/"
install -m 0755 /opt/tessdeps/bin/tesseract "$APPDIR/usr/bin/"

# The WHOLE tessdata tree, not just `*.traineddata`. `configs/tsv` is a config
# FILE tesseract loads by name when the scan runs `tesseract … tsv`; ship the
# language pack without it and tesseract prints `read_params_file: Can't open
# tsv`, silently falls back to plain text and exits 0, so OCR returns nothing
# with no error anywhere.
cp -a /opt/tessdeps/share/tessdata "$APPDIR/usr/bin/tessdata"
[[ -f "$APPDIR/usr/bin/tessdata/configs/tsv" ]] || die "tessdata/configs/tsv is missing"
[[ -f "$APPDIR/usr/bin/tessdata/eng.traineddata" ]] || die "eng.traineddata is missing"

# Real files, not the version symlinks, then re-create the sonames as symlinks.
# `cp -a` of the whole glob would copy both and cost nothing extra, but naming
# the two steps keeps it obvious that the payload is one copy per library.
for so in /usr/local/lib/lib{avcodec,avdevice,avfilter,avformat,avutil,swresample,swscale}.so.*.*.* \
          /usr/local/lib/libx264.so.*; do
    [[ -f "$so" && ! -L "$so" ]] || continue
    install -m 0644 "$so" "$APPDIR/usr/lib/"
done
for so in "$APPDIR"/usr/lib/*.so.*; do
    soname="$(objdump -p "$so" | awk '/SONAME/ {print $2}')"
    [[ -n "$soname" && "$soname" != "$(basename "$so")" ]] || continue
    ln -sf "$(basename "$so")" "$APPDIR/usr/lib/$soname"
done

# ── Where each binary looks for those libraries ──────────────────────────────
#
# `$ORIGIN` is expanded by the loader at run time, so this survives the AppImage
# mounting at a different path on every launch. The app binary needs the hop
# because it dlopens libavcodec from `usr/bin`; the libraries need `$ORIGIN`
# because DT_RUNPATH is NOT inherited by a dependency's own dependencies, so
# libavcodec has to be able to find libavutil and libx264 itself.
patchelf --set-rpath '$ORIGIN/../lib' "$APPDIR/usr/bin/$APP" "$APPDIR/usr/bin/ffmpeg" "$APPDIR/usr/bin/ffprobe"
for so in "$APPDIR"/usr/lib/*.so.*; do
    [[ -L "$so" ]] || patchelf --set-rpath '$ORIGIN' "$so"
done

# ── Desktop metadata ─────────────────────────────────────────────────────────
#
# The runtime execs `$APPDIR/AppRun`, and a SYMLINK straight to the binary is
# both the fastest form (no shell process per launch, which matters for a
# one-shot tool fired from PrintScreen) and the one that leaks no environment.
# It also leaves `current_exe()` resolving to the real file inside the mount,
# which is exactly what `util::locate_tool` needs to find the sidecars above.
ln -sf "usr/bin/$APP" "$APPDIR/AppRun"
install -m 0644 "$SRC/res/$ID.desktop" "$APPDIR/$ID.desktop"
install -m 0644 "$SRC/res/$ID.desktop" "$APPDIR/usr/share/applications/$ID.desktop"
install -m 0644 "$SRC/res/icons/$ID.svg" "$APPDIR/$ID.svg"
install -m 0644 "$SRC/res/icons/$ID.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/$ID.svg"
ln -sf "$ID.svg" "$APPDIR/.DirIcon"

# ── Pack it ──────────────────────────────────────────────────────────────────
#
# An AppImage is literally the runtime ELF followed by a squashfs image, which is
# all `appimagetool` does to produce one. Doing it directly keeps the pinned set
# to the runtime alone: appimagetool ships as an AppImage itself, needs
# `--appimage-extract-and-run` inside a container with no /dev/fuse, and by
# default DOWNLOADS a runtime from a moving `continuous` tag at build time, which
# is precisely the unpinned dependency this repo refuses to have.
#
# zstd because the payload is mounted lazily on every single launch: it decodes
# several times faster than xz for a size within a few percent. `-b 1M` keeps the
# block count (and so the seek count) low for the same reason.
log "packing the squashfs payload"
mksquashfs "$APPDIR" "$WORK/payload.squashfs" \
    -root-owned -noappend -no-xattrs -no-progress \
    -comp zstd -Xcompression-level 19 -b 1M >/dev/null

# ASSEMBLE ASIDE, THEN RENAME. Writing straight to `$OUT` fails with `Text file
# busy` whenever the previous AppImage is still running, which on the dev box is
# the normal state: `just appimage` leaves the resident daemon on the artifact it
# just built, so the very next run cannot open that file for writing. A rename is
# immune to it. The kernel only refuses to WRITE to a running executable; the
# directory entry can be replaced freely, and a process already running the old
# one keeps its inode (and its mount) until it exits. The same property is what
# will let a future in-app updater replace the AppImage under itself.
# A fixed name: the Dockerfile already picked the right runtime for this
# architecture when it built the image, so there is no arch logic to repeat here.
cat /opt/appimage/runtime "$WORK/payload.squashfs" > "$OUT.part"
chmod +x "$OUT.part"
mv -f "$OUT.part" "$OUT"
rm -f "$WORK/payload.squashfs"

# ── Report, rather than assume ───────────────────────────────────────────────
#
# The glibc floor is the ONE property an AppImage does not fix for itself: it is
# entirely the build host's, so a base-image change that raises it is invisible
# until a user on an older distro cannot start the app. That is exactly how the
# 2.39 regression in the zip went unnoticed.
FLOOR="$(objdump -T "$APPDIR/usr/bin/$APP" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
SIZE="$(du -h "$OUT" | cut -f1)"
log "glibc floor: $FLOOR"
log "size:        $SIZE"
log "bundled:     $(cd "$APPDIR/usr/lib" && ls *.so.*.*.* *.so.[0-9]* 2>/dev/null | sort -u | tr '\n' ' ')"
log "built:       $(basename "$OUT")"

# Machine-readable tail for the caller (the justfile recipe reads it back).
#
# The NAME, not the path: every path in this script is a path inside the
# container, and the caller is on the host, where the same file lives under
# `target-appimage/`. Handing over `/work/...` produced a recipe that reported
# success and then failed to launch anything, because it was trying to run a
# path that only exists inside a container that had already exited.
printf 'CCK_APPIMAGE_NAME=%s\nCCK_GLIBC_FLOOR=%s\nCCK_SIZE=%s\n' \
    "$(basename "$OUT")" "$FLOOR" "$SIZE" > "$WORK/appimage.env"
