# The Linux AppImage's build environment (DRAGON-510).
#
# WHY A CONTAINER AT ALL: an AppImage does not fix glibc. Its floor is entirely
# the build host's, so building on a dev machine (CachyOS here) would produce an
# artifact that only runs on other bleeding-edge distros and reaches nobody the
# plain zip does not already reach. Rocky 9 floors the binary at GLIBC_2.34,
# which covers Ubuntu 22.04, Mint 21, Pop!_OS 22.04, Debian 12 and everything
# newer. DRAGON-530 measured the whole ladder; this is the same base
# release.yml's `build-linux` job and the justfile's `dist` recipe already use.
#
# WHY AN IMAGE rather than one long `docker run` like `dist` does: this build
# needs a FULL ffmpeg (not just its headers), x264, and tesseract with leptonica,
# which is 10-15 minutes of third-party compilation. Baking that into an image
# means docker's layer cache pays it once and every later `just appimage` run
# only compiles our own code. `dist` builds ffmpeg inline because it needs
# nothing but the headers, and that takes seconds.
#
# EVERYTHING DOWNLOADED IS PINNED AND VERIFIED. The pins live in
# `scripts/pins.env`, the one manifest the mac and Windows fetchers read too, so
# the AppImage's ffmpeg can never drift from the ffmpeg the other platforms ship.
# A checksum mismatch is fatal and never retried: the only two causes are an
# upstream file changing under a URL that promised to be immutable, or a pin
# edited without its hash.
#
# Build context is `scripts/` (small), so this file is
#   docker build -f scripts/appimage/Dockerfile -t <tag> scripts
FROM rockylinux:9

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

# CRB carries nasm, which x264's and ffmpeg's x86 assembly need. EPEL carries
# cmake 3.31, patchelf and squashfs-tools' zstd-capable build.
#
# EVERY package below resolves for aarch64 too (checked against Rocky's and
# EPEL's own repo listings, DRAGON-529), so this one list serves both
# architectures and there is no arch-conditional install.
#
# `nasm` is the one that looks wrong and is not. It only assembles x86, so it is
# dead weight on an ARM build, but Rocky ships `nasm-2.15.03-7.el9.aarch64`, so
# asking for it there installs cleanly rather than failing the whole layer.
# x264 and ffmpeg use the GNU assembler for their NEON paths and never invoke it.
# Dropping it from the ARM leg would buy a few seconds and cost an
# arch-conditional install, which is the kind of split that drifts.
#
# The -devel packages are here for the APP's link line (libpulse, libgbm,
# pipewire, libxkbcommon) and ffmpeg's (libva, zlib). None of those libraries is
# bundled: they come from the host at runtime, deliberately, because a bundled
# Mesa/libdrm/libva would have to match the user's GPU driver and could not.
RUN dnf -y install epel-release dnf-plugins-core \
 && dnf config-manager --set-enabled crb \
 && dnf -y install \
      gcc gcc-c++ make cmake pkgconfig clang-devel \
      libxkbcommon-devel pulseaudio-libs-devel pipewire-devel \
      mesa-libgbm-devel libva-devel zlib-devel \
      git zip xz bzip2 nasm diffutils file which gnupg2 \
      squashfs-tools patchelf \
 && dnf clean all

# Rust into a SHARED prefix, readable by any uid, so the build can run as the
# calling user and still not need a writable toolchain (CARGO_HOME is per-run and
# lives in the mounted work dir).
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path \
 && chmod -R a+rX "$RUSTUP_HOME" "$CARGO_HOME"
ENV PATH=/usr/local/cargo/bin:$PATH

# The shared pin manifest. Everything below reads it, so a pin change
# invalidates every layer that follows, which is the point.
COPY pins.env /opt/pins.env

# ── x264 (the software H.264 encoder ffmpeg's fallback tier uses) ─────────────
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -sSL -o x264.tar.xz "$X264_URL" \
 && echo "$X264_SHA256  x264.tar.xz" | sha256sum -c - \
 && tar xJf x264.tar.xz \
 && cd "x264-$X264_VERSION" \
 && ./configure --prefix=/usr/local --enable-shared --disable-cli --disable-opencl \
 && make -j"$(nproc)" && make install \
 && cd /tmp && rm -rf "x264-$X264_VERSION" x264.tar.xz

# ── nv-codec-headers (NVENC interface; headers only, links nothing) ──────────
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -sSL -o nvcodec.tar.gz "$NVCODEC_URL" \
 && echo "$NVCODEC_SHA256  nvcodec.tar.gz" | sha256sum -c - \
 && tar xzf nvcodec.tar.gz \
 && cd "nv-codec-headers-$NVCODEC_VERSION" \
 && make PREFIX=/usr/local install \
 && cd /tmp && rm -rf "nv-codec-headers-$NVCODEC_VERSION" nvcodec.tar.gz

# ── ffmpeg 8.1.2, the binary AND the libav libraries the AppImage bundles ─────
#
# VERIFIED BY UPSTREAM GPG SIGNATURE, not just a checksum, exactly as
# release.yml's build-linux does: a hash we carry only proves the file did not
# change between our own two downloads, while the signature proves the FFmpeg
# project produced it. The fingerprint is pinned in pins.env and published on
# ffmpeg.org/download.html, and the signature must be made by THAT fingerprint,
# so a compromised key URL supplying both a key and a matching signature still
# fails. The sha256 runs first as a cheap gate.
#
# THIS BUILD IS WIDER THAN THE ZIP'S. The zip's container builds ffmpeg
# `--disable-everything` because it only wants headers. Here the artifacts ship,
# so the feature set is the app's actual runtime need:
#
#   libx264   the software encoder tier (`encode/plan.rs`)
#   libpulse  the preview soundtrack, which plays through `ffmpeg -f pulse`
#             (`app/preview/playback.rs`). Missing it would REGRESS a user who
#             has a working distro ffmpeg today, because a bundled sidecar wins
#             over PATH in `util::locate_tool`.
#   vaapi     the VAAPI encoder tier, and the zero-copy path's `h264_vaapi`
#   nvenc     the NVENC tier and the CUDA zero-copy path
#
# `--disable-bzlib --disable-lzma --disable-alsa --disable-sdl2 --disable-libxcb
# --disable-xlib` keep the shipped binaries' DT_NEEDED list short: every extra
# hard link is another library that must exist on the user's machine or ffmpeg
# will not start at all. What is left (libpulse, libva, libdrm, libz) is either
# already a hard requirement of the app itself or universal on a desktop.
# The three fetches walk a MIRROR LIST (DRAGON-529 follow-up): ffmpeg.org took a
# release build down by flapping its TLS, and every Linux job builds ffmpeg from
# source, so one host cost us all four artifacts at once. `--retry-all-errors`
# covers a blip, the second URL covers a sustained one, and the sha256 + pinned
# GPG fingerprint below gate whichever mirror answered.
RUN . /opt/pins.env \
 && cd /tmp \
 && fetch_any() { local out="$1"; shift; for u in "$@"; do \
        curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 \
             --speed-time 30 --speed-limit 1024 -o "$out" "$u" && return 0; \
        echo "mirror failed, trying the next: $u" >&2; \
    done; echo "every mirror failed for $out" >&2; return 1; } \
 && fetch_any ffmpeg.tar.xz $FFMPEG_SOURCE_URL \
 && fetch_any ffmpeg.tar.xz.asc $FFMPEG_SOURCE_ASC_URL \
 && fetch_any ffmpeg-devel.asc $FFMPEG_GPG_KEY_URL \
 && echo "$FFMPEG_SOURCE_SHA256  ffmpeg.tar.xz" | sha256sum -c - \
 && export GNUPGHOME=/tmp/cck-gnupg \
 && rm -rf "$GNUPGHOME" && mkdir -p "$GNUPGHOME" && chmod 700 "$GNUPGHOME" \
 && gpg --batch --quiet --import ffmpeg-devel.asc \
 && gpg --batch --with-colons --fingerprint | grep -E "^fpr:" | cut -d: -f10 \
      | grep -qx "$FFMPEG_GPG_FPR" \
 && gpg --batch --status-fd 1 --verify ffmpeg.tar.xz.asc ffmpeg.tar.xz \
      | grep -q "^\[GNUPG:\] VALIDSIG $FFMPEG_GPG_FPR " \
 && tar xJf ffmpeg.tar.xz \
 && cd "ffmpeg-$FFMPEG_VERSION" \
 && PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/local/lib64/pkgconfig \
    ./configure --prefix=/usr/local --enable-shared --disable-static \
      --disable-doc --disable-debug \
      --enable-gpl --enable-version3 \
      --enable-libx264 --enable-libpulse --enable-vaapi --enable-nvenc \
      --disable-bzlib --disable-lzma --disable-alsa \
      --disable-sdl2 --disable-libxcb --disable-xlib \
 && make -j"$(nproc)" && make install && ldconfig \
 && cd /tmp && rm -rf "ffmpeg-$FFMPEG_VERSION" ffmpeg.tar.xz ffmpeg.tar.xz.asc ffmpeg-devel.asc "$GNUPGHOME"

# ── tesseract, built the way the macOS package builds it ─────────────────────
#
# Same recipe as `scripts/fetch-mac-vendor.sh`, and for the same reason: link
# leptonica and libpng in STATICALLY so the resulting `tesseract` binary depends
# on nothing but libz, libstdc++ and libc, all of which come from the host. That
# leaves nothing to bundle beside it and nothing to relocate.
#
# TIFF stays OFF deliberately (the mac script records the upstream CMake bug that
# makes the alternative impossible); OCR feeds tesseract a PNG, so PNG + zlib is
# the whole requirement. GRAPHICS_DISABLED and OPENMP_BUILD=OFF drop dependencies
# a headless one-shot OCR call has no use for.
RUN . /opt/pins.env \
 && cd /tmp \
 && curl -sSL -o leptonica.tar.gz "$LEPTONICA_URL" \
 && echo "$LEPTONICA_SHA256  leptonica.tar.gz" | sha256sum -c - \
 && for url in $LIBPNG_URL; do \
        curl -fsSL --speed-time 20 --speed-limit 1024 -o libpng.tar.gz "$url" && break || true; \
    done \
 && echo "$LIBPNG_SHA256  libpng.tar.gz" | sha256sum -c - \
 && curl -sSL -o tesseract.tar.gz "$TESSERACT_SOURCE_URL" \
 && echo "$TESSERACT_SOURCE_SHA256  tesseract.tar.gz" | sha256sum -c - \
 && tar xzf leptonica.tar.gz && tar xzf libpng.tar.gz && tar xzf tesseract.tar.gz \
 && cmake -S "libpng-$LIBPNG_VERSION" -B b-png \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/opt/tessdeps \
      -DCMAKE_PREFIX_PATH=/opt/tessdeps -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_TESTS=OFF -DPNG_TOOLS=OFF \
 && cmake --build b-png -j"$(nproc)" --target install \
 && cmake -S "leptonica-$LEPTONICA_VERSION" -B b-lept \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/opt/tessdeps \
      -DCMAKE_PREFIX_PATH=/opt/tessdeps -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DBUILD_SHARED_LIBS=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DBUILD_PROG=OFF -DSW_BUILD=OFF \
      -DENABLE_ZLIB=ON -DENABLE_PNG=ON \
      -DENABLE_TIFF=OFF -DENABLE_GIF=OFF -DENABLE_JPEG=OFF -DENABLE_WEBP=OFF -DENABLE_OPENJPEG=OFF \
 && cmake --build b-lept -j"$(nproc)" --target install \
 && cmake -S "tesseract-$TESSERACT_VERSION" -B b-tess \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/opt/tessdeps \
      -DCMAKE_PREFIX_PATH=/opt/tessdeps -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DBUILD_SHARED_LIBS=OFF -DBUILD_TRAINING_TOOLS=OFF -DBUILD_TESTS=OFF \
      -DDISABLE_CURL=ON -DDISABLE_ARCHIVE=ON \
      -DGRAPHICS_DISABLED=ON -DOPENMP_BUILD=OFF -DENABLE_LTO=OFF -DENABLE_CCACHE=OFF \
 && cmake --build b-tess -j"$(nproc)" --target install \
 && cd /tmp && rm -rf b-png b-lept b-tess \
      "libpng-$LIBPNG_VERSION" "leptonica-$LEPTONICA_VERSION" "tesseract-$TESSERACT_VERSION" \
      leptonica.tar.gz libpng.tar.gz tesseract.tar.gz

# ── The language pack, and the `configs/` tesseract silently needs ───────────
#
# `configs/tsv` is NOT optional decoration. The scan runs `tesseract … tsv`,
# where `tsv` is a CONFIG FILE loaded from the tessdata directory. Ship only
# `*.traineddata` and tesseract prints `read_params_file: Can't open tsv`,
# quietly falls back to PLAIN TEXT and exits 0, so `parse_tsv_words` finds no
# columns and OCR returns nothing with no error anywhere. Verified both ways.
# The tesseract build above installs the stock `configs/` tree next to its own
# tessdata dir, so the language pack just joins it.
RUN . /opt/pins.env \
 && curl -sSL -o /opt/tessdeps/share/tessdata/eng.traineddata "$TESSDATA_ENG_URL" \
 && echo "$TESSDATA_ENG_SHA256  /opt/tessdeps/share/tessdata/eng.traineddata" | sha256sum -c - \
 && test -f /opt/tessdeps/share/tessdata/configs/tsv

# ── The AppImage runtime ─────────────────────────────────────────────────────
#
# Downloaded here rather than at package time so a `just appimage` run needs the
# network only when the image is (re)built.
#
# Picked by `uname -m` (DRAGON-529). There is NO cross-compilation anywhere in
# this file: an aarch64 artifact is built by running this same image on an
# aarch64 host, where `FROM rockylinux:9` resolves to the arm64 image by itself
# (the tag is a manifest list). So the architecture is simply whatever we are
# already running on, and the only thing that has to follow it is which runtime
# gets concatenated in front of the payload.
#
# Saved under a FIXED name, not an arch-suffixed one, so `build.sh` needs no arch
# logic of its own to find it. The arch is already decided here.
RUN . /opt/pins.env \
 && mkdir -p /opt/appimage \
 && case "$(uname -m)" in \
      x86_64)  RT_URL="$APPIMAGE_RUNTIME_URL";         RT_SHA="$APPIMAGE_RUNTIME_SHA256" ;; \
      aarch64) RT_URL="$APPIMAGE_RUNTIME_URL_AARCH64"; RT_SHA="$APPIMAGE_RUNTIME_SHA256_AARCH64" ;; \
      *) echo "no AppImage runtime pinned for $(uname -m)" >&2; exit 1 ;; \
    esac \
 && curl -sSL -o /opt/appimage/runtime "$RT_URL" \
 && echo "$RT_SHA  /opt/appimage/runtime" | sha256sum -c - \
 && chmod +x /opt/appimage/runtime

# RHEL's pkg-config does not search /usr/local, where ffmpeg and x264 installed,
# so `ffmpeg-sys-next` would not find libavutil.pc without this.
ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/local/lib64/pkgconfig \
    LD_LIBRARY_PATH=/usr/local/lib:/usr/local/lib64
