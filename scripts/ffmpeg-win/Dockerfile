# The Windows ffmpeg's build environment (DRAGON-675): a mingw-w64 cross
# toolchain plus every library the shipped ffmpeg links.
#
# WHY WE BUILD THIS AT ALL. For two years the Windows ffmpeg was somebody
# else's binary: BtbN's, then gyan.dev's (DRAGON-631). That made one property of
# our product an INHERITED one: what the binary demands of the user's machine.
# gyan's 9.0.1 was compiled against nv-codec-headers 13.1 where its 9.0 used
# 13.0, so the minimum NVIDIA driver moved from ~570 to 610.00, every user below
# that lost NVENC silently, fell back to libx265, and their recordings died a
# few seconds in (DRAGON-671). Nothing in the shipped DLL reports that number,
# so no fetch-time assertion could have caught it: both halves of the message
# are formatted at runtime from constants compiled in from the headers.
#
# Building it ourselves turns that floor into a line in `scripts/pins.env`.
# NVCODEC_URL is the SAME pin the Linux AppImage's ffmpeg uses, so the two
# platforms now demand the same driver, and moving either is one visible edit.
#
# It also removes the durability problem underneath the whole chain: gyan.dev
# deletes the previous release build the moment it publishes the next, which is
# what 404'd our pinned 9.0 URL, which is what forced the bump that changed the
# floor.
#
# WHY DEBIAN AND NOT ROCKY 9, which is what every other container here uses:
# `scripts/appimage/Dockerfile` is on Rocky for its GLIBC floor, because that
# floor is a property of the ELF it produces. This container produces PE
# binaries for Windows. The host's libc is not linked into them and cannot leak
# into them, so the base distro is free, and Debian is the one that ships a
# complete, maintained mingw-w64 cross toolchain as ordinary packages.
#
# WHY AN IMAGE and a separate build script, same split as the AppImage: the
# libraries below are ten minutes of compilation that changes only when a pin
# changes, so docker's layer cache should pay for it once. ffmpeg itself lives
# in `build.sh`, which runs every time, because that is the part whose configure
# line we actually iterate on.
#
# EVERYTHING DOWNLOADED IS PINNED AND VERIFIED against `scripts/pins.env`, the
# one manifest the mac fetcher, the Windows fetcher and the AppImage all read.
# A checksum mismatch is fatal and never retried.
#
# Build context is `scripts/` (small), so this file is
#   docker build -f scripts/ffmpeg-win/Dockerfile -t cck-ffmpeg-win scripts
FROM debian:12

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

ENV DEBIAN_FRONTEND=noninteractive

# `mingw-w64` brings the gcc/g++ cross compilers, the CRT and the Windows
# headers. nasm is x264's and x265's x86 assembler; without it both fall back to
# C and lose most of their speed, which for a screen recorder is the difference
# between real time and not.
#
# `git` is not here to clone anything, and removing it as dead weight breaks the
# build in a way that names nothing: x265's Version.cmake reads its version out
# of the tarball's own `x265Version.txt`, but only inside a branch guarded by
# `find_package(Git)`. With no git it falls through to a mercurial branch,
# invokes an empty `HG_EXECUTABLE`, ends up with an EMPTY version string, and
# dies at `list GET given empty list` while configuring the resource compiler.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential mingw-w64 mingw-w64-tools \
      nasm cmake make pkg-config git \
      curl ca-certificates gnupg \
      xz-utils p7zip-full file patch python3 \
 && rm -rf /var/lib/apt/lists/*

# THE POSIX THREADING FLAVOUR, deliberately. Debian ships two mingw runtimes,
# win32 and posix, and only the posix one gives libstdc++ working `std::thread`
# / `std::mutex`, which x265 and oneVPL both use. The win32 flavour compiles
# them and then deadlocks or fails to link.
#
# The cost is that the posix flavour's output normally depends on
# libwinpthread-1.dll, libgcc_s_seh-1.dll and libstdc++-6.dll at RUNTIME. We do
# not ship those. Two of the three are handled by build.sh's link flags
# (-static-libgcc, -static-libstdc++) and the third needs the link-static
# directory below; `build.sh` REFUSES the build if any of them survives as an
# import. See its `check_imports`.
RUN update-alternatives --set x86_64-w64-mingw32-gcc /usr/bin/x86_64-w64-mingw32-gcc-posix \
 && update-alternatives --set x86_64-w64-mingw32-g++ /usr/bin/x86_64-w64-mingw32-g++-posix

# ONE cross prefix for everything we build, kept apart from the toolchain's own
# /usr/x86_64-w64-mingw32 so a pinned library can never be confused with a
# distro one.
ENV CROSS=x86_64-w64-mingw32
ENV W64=/opt/w64
# PKG_CONFIG_LIBDIR, not PKG_CONFIG_PATH: LIBDIR REPLACES the default search
# path instead of prepending to it. A cross build that can see the host's
# /usr/lib/pkgconfig will happily configure ffmpeg against a Linux x265 and then
# fail at link time with an error that names nothing useful.
ENV PKG_CONFIG_LIBDIR=/opt/w64/lib/pkgconfig
RUN mkdir -p "$W64"/{include,lib,bin,lib/pkgconfig}

# ── The static-pthread intercept, and why nothing simpler works ──────────────
#
# A directory holding ONLY the STATIC pthread archives, put FIRST on the link
# search path by build.sh. It exists to make `-lpthread` resolve to an archive
# no matter who asks for it or in what mode.
#
# The problem it solves, measured rather than reasoned: the posix mingw
# flavour's libstdc++ calls pthread_once / pthread_key_create / the mutex
# family, so any DLL linking a C++ static library (x265 into avcodec, oneVPL
# into avcodec and avfilter) ends up importing libwinpthread-1.dll, which we do
# not ship. The customer sees a missing-DLL dialog and nothing else.
#
# NOTHING ON FFMPEG'S COMMAND LINE CAN FIX IT, and all three obvious attempts
# were tried and rejected:
#
#   `--extra-libs="-Wl,-Bstatic -lwinpthread -Wl,-Bdynamic"` is too EARLY. gcc
#   appends its own libraries after everything the user passed, so libstdc++'s
#   pthread references are still undefined when the linker has already moved
#   past our static archive.
#
#   Leaving the linker in static mode (`-Wl,-Bstatic` with no restore) does not
#   survive: the g++ driver implements -static-libstdc++ as
#   `-Bstatic -lstdc++ -Bdynamic`, so it restores dynamic mode itself, right
#   before the implicit pthread.
#
#   Intercepting `-lwinpthread` alone misses it, because the name gcc's spec
#   actually appends is `-lpthread` (Debian ships libpthread.a as a copy of the
#   winpthread one). Both names are here for that reason.
#
# So the only lever left is RESOLUTION ORDER: a directory with the archive and
# no import library in it, searched first.
RUN mkdir -p "$W64/link-static" \
 && cp "$(x86_64-w64-mingw32-gcc -print-file-name=libpthread.a)" "$W64/link-static/" \
 && cp "$(x86_64-w64-mingw32-gcc -print-file-name=libwinpthread.a)" "$W64/link-static/"

# The toolchain file the three CMake projects below share.
#
# FIND_ROOT_PATH_MODE_PROGRAM=NEVER is the load-bearing line: without it CMake
# resolves helper programs out of the Windows sysroot and tries to execute PE
# binaries on Linux.
RUN printf '%s\n' \
  'set(CMAKE_SYSTEM_NAME Windows)' \
  'set(CMAKE_SYSTEM_PROCESSOR x86_64)' \
  'set(CMAKE_C_COMPILER   x86_64-w64-mingw32-gcc)' \
  'set(CMAKE_CXX_COMPILER x86_64-w64-mingw32-g++)' \
  'set(CMAKE_RC_COMPILER  x86_64-w64-mingw32-windres)' \
  'set(CMAKE_FIND_ROOT_PATH /usr/x86_64-w64-mingw32 /opt/w64)' \
  'set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)' \
  'set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)' \
  'set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)' \
  'set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)' \
  > /opt/mingw-w64.cmake

# The shared pin manifest. Everything below reads it, so a pin change
# invalidates every layer that follows, which is the point.
COPY pins.env /opt/pins.env

# ── zlib (static) ─────────────────────────────────────────────────────────────
#
# Built by hand rather than by `make install`: zlib's win32 makefile's default
# target also links example.exe and minigzip.exe, and its install target wants
# to place a DLL and an import library we would then have to delete again. Two
# copies is the whole job.
#
# STATIC ONLY, and no libz.dll.a anywhere, because mingw's linker prefers an
# import library over an archive for the same `-lz`: leave one behind and the
# DLLs come out depending on a zlib1.dll nobody ships.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o zlib.tar.gz "$ZLIB_URL" \
 && echo "$ZLIB_SHA256  zlib.tar.gz" | sha256sum -c - \
 && tar xzf zlib.tar.gz \
 && cd "zlib-$ZLIB_VERSION" \
 && make -f win32/Makefile.gcc -j"$(nproc)" PREFIX="$CROSS-" libz.a \
 && install -m 0644 libz.a "$W64/lib/" \
 && install -m 0644 zlib.h zconf.h "$W64/include/" \
 && cd /tmp && rm -rf "zlib-$ZLIB_VERSION" zlib.tar.gz

# ── x264 (static, the software H.264 tier) ───────────────────────────────────
#
# The SAME pin the AppImage builds, so the two platforms' software encoder is
# one version of one encoder.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o x264.tar.xz "$X264_URL" \
 && echo "$X264_SHA256  x264.tar.xz" | sha256sum -c - \
 && tar xJf x264.tar.xz \
 && cd "x264-$X264_VERSION" \
 && ./configure --host="$CROSS" --cross-prefix="$CROSS-" --prefix="$W64" \
      --enable-static --enable-pic --disable-cli --disable-opencl \
 && make -j"$(nproc)" && make install \
 && cd /tmp && rm -rf "x264-$X264_VERSION" x264.tar.xz

# ── x265 (static, 8-bit, the software HEVC tier) ─────────────────────────────
#
# EIGHT-BIT ONLY. The multilib builds other packagers ship add 10- and 12-bit
# depth by linking three copies of the encoder together, and we encode
# `yuv420p` 8-bit: `encode/plan.rs` names it explicitly on every software plan.
#
# MIRROR LIST, tried in order, first success wins, both gated by the same
# sha256. See the pin's own comment for why these two URLs are interchangeable.
#
# THE `sed` IS NOT COSMETIC. CMake writes x265's implicit link libraries into
# x265.pc as `Libs.private: -lstdc++ -lgcc_s -lgcc`, and `-lgcc_s` resolves to
# the import library for libgcc_s_seh-1.dll. ffmpeg reads that list (it
# configures with --pkg-config-flags=--static) while also linking
# -static-libgcc, so both copies of the unwinder arrive and the link dies with
# "multiple definition of `_Unwind_Resume'". We ship no libgcc DLL, so the
# shared one is never the answer here; dropping it from the file is.
RUN . /opt/pins.env \
 && cd /tmp \
 && for u in $X265_URL; do \
      curl -fsSL --retry 3 --retry-all-errors --speed-time 30 --speed-limit 1024 \
           -o x265.tar.gz "$u" && break; \
      echo "mirror failed, trying the next: $u" >&2; \
    done \
 && echo "$X265_SHA256  x265.tar.gz" | sha256sum -c - \
 && tar xzf x265.tar.gz \
 && cmake -S "x265_$X265_VERSION/source" -B b-x265 \
      -DCMAKE_TOOLCHAIN_FILE=/opt/mingw-w64.cmake \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$W64" \
      -DENABLE_SHARED=OFF -DENABLE_CLI=OFF -DENABLE_ASSEMBLY=ON \
 && cmake --build b-x265 -j"$(nproc)" --target install \
 && sed -i 's/ -lgcc_s//g' "$W64/lib/pkgconfig/x265.pc" \
 && cd /tmp && rm -rf b-x265 "x265_$X265_VERSION" x265.tar.gz

# ── SDL2 (static), for ffplay.exe and nothing else ───────────────────────────
#
# `ffplay_deps="avcodec avformat avfilter swscale swresample sdl2"` in ffmpeg's
# own configure: no SDL2, no ffplay, and ffplay is the preview editor's audio
# sink on Windows (DRAGON-285). It is not built for its video output; the app
# never asks ffplay to show anything.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o sdl2.tar.gz "$SDL2_URL" \
 && echo "$SDL2_SHA256  sdl2.tar.gz" | sha256sum -c - \
 && tar xzf sdl2.tar.gz \
 && cmake -S "SDL2-$SDL2_VERSION" -B b-sdl \
      -DCMAKE_TOOLCHAIN_FILE=/opt/mingw-w64.cmake \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$W64" \
      -DSDL_SHARED=OFF -DSDL_STATIC=ON -DSDL_TEST=OFF \
 && cmake --build b-sdl -j"$(nproc)" --target install \
 && cd /tmp && rm -rf b-sdl "SDL2-$SDL2_VERSION" sdl2.tar.gz

# ── nv-codec-headers: THE POINT OF THIS WHOLE CONTAINER ──────────────────────
#
# Headers only. They link nothing (ffmpeg dlopens `nvEncodeAPI64.dll` from the
# driver), and they are the ONLY thing that decides which NVIDIA driver version
# the shipped binary will accept. Read NVCODEC_URL's comment in pins.env before
# touching this: it is the same pin the Linux AppImage compiles against, and
# keeping the two together is the entire deliverable of DRAGON-675.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o nvcodec.tar.gz "$NVCODEC_URL" \
 && echo "$NVCODEC_SHA256  nvcodec.tar.gz" | sha256sum -c - \
 && tar xzf nvcodec.tar.gz \
 && cd "nv-codec-headers-$NVCODEC_VERSION" \
 && make PREFIX="$W64" install \
 && cd /tmp && rm -rf "nv-codec-headers-$NVCODEC_VERSION" nvcodec.tar.gz

# ── AMF headers (AMD's encoder interface) ────────────────────────────────────
#
# Headers only, same shape as nv-codec-headers: ffmpeg dlopens `amfrt64.dll`
# from the AMD driver, so nothing here is linked or shipped.
#
# ONLY `amf/public/include` is extracted. The tag archive is ~400 MB of SDK and
# samples, and it is deleted inside this same layer, so the image carries a few
# hundred kilobytes of headers rather than the download.
#
# THAT DOWNLOAD IS THE MOST EXPENSIVE THING IN THIS FILE, and docker invalidates
# every layer after the one you edit. Adding a new library step ABOVE this one
# therefore re-fetches 400 MB for a change that has nothing to do with AMF. Add
# new steps below it.
#
# It is also why editing scripts/pins.env AT ALL, even a comment, costs those
# 400 MB on the next build: `COPY pins.env` sits above every library layer, by
# design, because a pin change MUST invalidate them. If that becomes tiresome,
# the fix is to stop downloading the whole SDK: a sparse `git clone` of
# `amf/public/include` pinned to a COMMIT is a couple of megabytes and a
# stronger identity than a tarball hash. It was not done here only because it
# would be the one pin in pins.env that is not a URL plus a sha256.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o amf.tar.gz "$AMF_URL" \
 && echo "$AMF_SHA256  amf.tar.gz" | sha256sum -c - \
 && mkdir -p "$W64/include/AMF" \
 && tar xzf amf.tar.gz --wildcards --strip-components=4 -C "$W64/include/AMF" \
      "AMF-*/amf/public/include/*" \
 && test -f "$W64/include/AMF/core/Version.h" \
 && rm -f amf.tar.gz

# ── Intel oneVPL dispatcher (static), for h264_qsv / hevc_qsv ────────────────
#
# The DISPATCHER only: it finds and loads whatever VPL or Media SDK runtime the
# user's Intel driver installed, so nothing Intel-specific is shipped either.
# ffmpeg finds it through pkg-config as `vpl >= 2.6`.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -fsSL --retry 3 --retry-all-errors -o libvpl.tar.gz "$LIBVPL_URL" \
 && echo "$LIBVPL_SHA256  libvpl.tar.gz" | sha256sum -c - \
 && tar xzf libvpl.tar.gz \
 && cmake -S "libvpl-$LIBVPL_VERSION" -B b-vpl \
      -DCMAKE_TOOLCHAIN_FILE=/opt/mingw-w64.cmake \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$W64" \
      -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTS=OFF -DBUILD_EXAMPLES=OFF \
      -DINSTALL_EXAMPLES=OFF -DBUILD_TOOLS=OFF -DBUILD_PREVIEW=OFF \
 && cmake --build b-vpl -j"$(nproc)" --target install \
 && test -f "$W64/lib/pkgconfig/vpl.pc" \
 && cd /tmp && rm -rf b-vpl "libvpl-$LIBVPL_VERSION" libvpl.tar.gz
