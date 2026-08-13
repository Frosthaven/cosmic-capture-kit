# Dependencies

This documents everything `cosmic-capture-kit` needs **outside the compiled Rust
binary** — system shared libraries, runtime services, Wayland protocols, D-Bus
interfaces, and external command-line tools. Rust crate dependencies are in
`Cargo.toml` and are statically compiled in; they are not repeated here.

The tool degrades gracefully: anything not marked **Required** is probed at
runtime, and the related feature simply disables itself (often with a hint in the
UI) when its dependency is missing.

---

## 1. Display & graphics — **Required**

| Dependency | Why | Notes |
|---|---|---|
| **Wayland compositor (COSMIC)** | The whole app is a native COSMIC overlay. | See protocols below. Native capture is keyed on the protocols, not the desktop name, so it runs wherever those globals exist. Where they are missing or hidden (GNOME, sandboxed builds) capture falls back to the xdg-desktop-portal ScreenCast path (§2), with fewer extras. |
| **Vulkan-capable GPU + driver** | The overlay is rendered with `wgpu` (via libcosmic/iced), whose primary Linux backend is Vulkan. | Needs the Vulkan loader (`libvulkan.so.1`) and an ICD — NVIDIA's driver, or Mesa (RADV/ANV). Loaded at runtime (not shown by `ldd`). |
| **libxkbcommon** (`libxkbcommon.so.0`) | Keyboard handling (Escape to cancel, etc.). | **Linked** into the binary, so building needs its dev package too. See §7. |
| **libpulse** (`libpulse.so.0`) | The shared PulseAudio client FFI (`src/audio/pulse_ffi.rs`): the device-latency probe and the system-audio monitor capture. | **Linked** into the binary (`#[link(name = "pulse")]`, not `dlopen`ed), so it is a build requirement of every Linux build, including one that will never record. See §7. |
| **libpipewire** (`libpipewire-0.3.so.0`) | The xdg-portal ScreenCast capture path (the `pipewire` crate binds it). | **Linked** into the binary, and an unconditional Linux dependency rather than a feature-gated one. See §7. |
| **libwayland-client** | Wayland client transport. | `dlopen`ed at runtime by the Wayland client stack, so no dev package is needed to build. |
| **libgbm** (`libgbm.so.1`) | Allocates the GPU buffer for **zero-copy recording** (via the `gbm` crate): the compositor copies each frame straight into it. | Part of Mesa; present on any GPU desktop. Used only when GPU zero-copy is enabled. |
| **libavcodec / libavutil** | **In-process** hardware video encoding for the zero-copy path (via `ffmpeg-next`), distinct from the external `ffmpeg` binary. | Linked at build time, version-matched to ffmpeg 9.0. Used only for GPU zero-copy. |
| **DRM render node** (`/dev/dri/renderD*`) | The GPU the compositor renders on — zero-copy allocates its capture buffer and runs the in-process encoder on this same device. | Requires membership in the `render` / `video` group. Zero-copy only. |

### Wayland protocols the compositor must implement

Pixels are captured **natively** (no `grim`); each of these is bound directly:

| Protocol | Used for |
|---|---|
| **ext-image-copy-capture** (bound through `cosmic-client-toolkit`) | All native pixel capture: monitor, region, and per-window. The upstream `ext-*` protocol family, so any compositor implementing these globals runs the native backend; a session without them takes the portal ScreenCast fallback (§2) instead. |
| **wlr-layer-shell** (`zwlr_layer_shell_v1`) | The per-output overlay surfaces (selection UI, toolbar). |
| **ext-foreign-toplevel-list** + **COSMIC toplevel-info / toplevel-management** | Enumerating windows and capturing a specific (even occluded) window by handle. |
| **ext-workspace** | Restricting window capture to the active workspace. |
| **wlr-data-control** (`zwlr_data_control_manager_v1`) | Writing the capture to the clipboard (via the `wl-clipboard-rs` crate). |
| **linux-dmabuf** (`zwp_linux_dmabuf_v1`) | Wrapping a `gbm`-allocated GPU buffer as a `wl_buffer` for the compositor to copy frames into — **zero-copy recording** (COSMIC screencopy path). Optional. |

---

## 2. D-Bus session bus — **Required for sharing & the folder picker**

A running session bus is needed for the post-capture actions and the settings
folder picker.

| Interface / service | Why | Fallback |
|---|---|---|
| `org.freedesktop.Notifications` | "Copied / Saved" toast after a capture. | Silent no-op if unavailable. |
| `org.freedesktop.FileManager1` (`ShowItems`) | "Show in file manager" reveal. | Falls back to the portal's fd-based `OpenURI.OpenDirectory` on the containing folder, with the entry selected (DRAGON-556). |
| `org.freedesktop.portal.OpenURI` | Opening a URL decoded from a QR code (`OpenURI`), opening a saved local file with its default app (the fd-based `OpenFile`; `OpenURI` itself rejects `file://` URIs, DRAGON-556), and the file-manager reveal fallback (`OpenDirectory`). Replaces shelling out to `xdg-open`. | Provided by the base xdg-desktop-portal; silent no-op if absent. |
| **xdg-desktop-portal** + a backend (**xdg-desktop-portal-cosmic**) | Folder pickers in Settings (screenshot/recording save dirs) via `org.freedesktop.portal.FileChooser` (the `ashpd` crate); and, on sessions where the native capture globals are missing or hidden (GNOME, sandboxed builds), all capture via `org.freedesktop.portal.ScreenCast` + PipeWire. | Picker won't open (the dir can still be typed/edited and is persisted); portal-fallback capture is unavailable. |

---

## 3. External command-line tools

Each is found on `PATH` at runtime; the feature turns itself off when the tool is
absent.

### Feature tools

| Binary | Package (Arch) | Feature | Without it |
|---|---|---|---|
| **ffmpeg** | `ffmpeg` | Screen recording. Raw frames are piped to ffmpeg (`-f rawvideo`) and encoded. | The Recordings feature is disabled and the UI warns. |
| **tesseract** | `tesseract` + a language pack (e.g. `tesseract-data-eng`) | OCR text detection ("Scan text (OCR) in region mode"). The region is handed to `tesseract … tsv`. Found via `CCK_TESSERACT`, then a sidecar beside our binary, then `PATH` (DRAGON-527), so a packaged build can carry its own. **The macOS and Windows packages now DO** (DRAGON-531): they ship tesseract plus `tessdata/eng.traineddata` beside the binary, and the Settings language dropdown lists whatever is in that folder, so a user can drop more `.traineddata` files in. Linux keeps using the distro's tesseract and its own `/usr/share/tessdata`, deliberately: pointing it elsewhere would HIDE every `tesseract-data-*` pack the user installed.
| **proton-drive** | none in any distro's own repos; download the standalone binary from [proton.me/support/drive-cli](https://proton.me/support/drive-cli), or on Arch the AUR package `proton-drive-cli-bin` (Proton's official checksummed build). Linux also needs `libsecret` and a running keyring (GNOME Keyring, KWallet). | **Proton Drive cloud accounts only** (DRAGON-485). Proton has no third-party API, so this provider goes through Proton's own official CLI: sign-in (`auth login`, which opens your browser), uploads, folder listing and share links. Found via `CCK_PROTON_DRIVE`, then a sidecar beside our binary, then `PATH`. It is a ~118 MB self-contained binary and is deliberately **not bundled by the direct builds**; the lab Flatpak bundles it at `/app/bin` (DRAGON-566), because a sandboxed app cannot see a host install and the store updates the bundled copy with the app. | The Proton Drive entry stays visible in the add-account picker with an "Install proton-drive CLI" line, and selecting it opens Proton's download page instead of starting a sign-in. Every other cloud provider, and all capture, recording and OCR, are unaffected. |
| **curl** | `curl` | **Two features.** (1) The in-app **update check and install** (`src/update.rs`): one `curl -fsSL --max-time 10` per settings launch to fetch `update.json`, plus, when running as an **AppImage**, the download of the new `.AppImage` itself (DRAGON-532). (2) **Cloud accounts** (`src/cloud/http.rs`, DRAGON-482): every request to a connected drive, with the credentials fed through a stdin `--config -` rather than argv. | The update check reports "Could not run curl to check for updates."; a one-click update reports "Could not run curl to download the update." and changes nothing on disk; connecting or uploading to a cloud account fails with a named reason. Capture, recording and OCR are unaffected. |
| **sha256sum** | `coreutils` (already on every distro) | Verifying a downloaded update before it replaces the running program (`update::file_sha256`, the AppImage install path only). macOS uses `shasum` from its base system for the same step. | A one-click AppImage update stops with "Could not run the checksum tool to verify the download." and leaves the existing file untouched. An unverified update is never installed. Nothing else uses it. |

> URL opening and the file-manager reveal fallback go through the portal
> `OpenURI` D-Bus call (see §2), so **no `xdg-utils` / `xdg-open` is needed**. The
> `hvc1` mp4 tag is applied from the chosen encoder, so **no `ffprobe` call** is
> made either.

### GPU-probe tools (optional, cosmetic)

Used only to put a friendly GPU name on the hardware-encoder options. Missing
either just yields a generic label; encoding is unaffected.

| Binary | Package (Arch) | Purpose |
|---|---|---|
| **nvidia-smi** | `nvidia-utils` | Names the NVIDIA GPU for the NVENC option. |
| **lspci** | `pciutils` | Names the GPU from its PCI address (used for the VAAPI render node). |

---

## 4. Audio capture: **required for recording**, at build time and run time

Two things here are stronger than "optional", and both used to be mis-stated in
this file:

- **libpulse is LINKED into the binary**, not `dlopen`ed (§1). Its dev package is
  needed to compile every Linux build, even one that will never record.
- **A recording FAILS if the audio pre-flight cannot start.** It does not fall
  back to a silent video. `record::owned::try_start_owned_audio` starts the mic
  chain and the system-audio monitor **unconditionally**, before any video, and
  returns an error if either will not come up. The mic / system toggles are gain
  automation applied later, so turning them off does not skip the capture. A
  recording on a machine with no Pulse-compatible server ends with a named
  failure instead of a file.

| Dependency | Why | Without it |
|---|---|---|
| **PulseAudio** or **PipeWire** (with `pipewire-pulse`) | Microphone and system-audio capture for recordings; ffmpeg reads via `-f pulse`. | **Recording fails**, with the reason surfaced (`system audio (pulse monitor) connection failed to start`). Screenshots are unaffected. |
| **`pactl`** (from `libpulse` / `pipewire-pulse`) | Enumerating input + output devices for the "Input device" / "Output device" pickers in Settings (labelled to match COSMIC's sound settings). | The pickers just offer "System (automatic)" (the default source / monitor). |

> **The whole mic input chain is built in — no external dependency.** The Audio
> settings (Input/Output device, Input Sensitivity, Noise Suppression, Echo
> cancellation, Automatic Gain Control, Advanced Voice Activity) run in-process on the
> captured mic via `src/audio_input.rs`, all **pure-Rust** with embedded models —
> **no plugin, model file, or manual install** (vs the usual EasyEffects / NoiseTorch /
> PipeWire-filter route). The libraries:
> - **`nnnoiseless`** — RNNoise noise suppression + a per-frame voice probability.
> - **`sonora`** — a pure-Rust WebRTC AudioProcessing port: AEC3 echo cancellation,
>   noise suppression, and AGC2 automatic gain control (one pass does all three).
> - **`earshot`** — a pure-Rust neural VAD (embedded ~75 KiB model) powering "Advanced
>   Voice Activity" for the voice gate.
>
> (Listed here, against the doc's crates-aren't-repeated rule, precisely because they
> *replace* what would otherwise be external dependencies.) Run `--audio-test` for a
> synthetic self-test of the chain.

---

## 5. Hardware video acceleration — optional

The encoder picker auto-detects what's usable and falls back to software
`libx264` when nothing else works.

| Path | Needs | ffmpeg encoders |
|---|---|---|
| **NVENC** | NVIDIA driver + ffmpeg built with NVENC | `h264_nvenc`, `hevc_nvenc` |
| **VAAPI** | `libva` + a VAAPI driver (Mesa for AMD/Intel; NVIDIA via its VAAPI bridge) + ffmpeg VAAPI | `h264_vaapi`, `hevc_vaapi` |
| **Software** (always available) | ffmpeg with libx264 | `libx264` |

### GPU zero-copy recording (optional)

When enabled with a hardware encoder, full-output (monitor) recordings can stay
**GPU-resident**: the compositor copies each frame into a `gbm`-allocated dmabuf on
its own render node, which is imported directly into an **in-process** encoder
(libavcodec / VAAPI) — no read-back to system RAM. It works when an encoder lives
on the **same** device as the captured buffer (e.g. VAAPI on an AMD/Intel iGPU
output). An NVIDIA-rendered output would need NVENC dmabuf import (not yet
implemented) and falls back to the read-back path (still hardware-encoded by the
external `ffmpeg`). Both capture backends support it — COSMIC **screencopy** (no
portal dialog) and the **PipeWire** portal.

Extra runtime needs for this path: **libgbm**, a **DRM render node**
(`/dev/dri/renderD*`, `render` group), the **`zwp_linux_dmabuf_v1`** protocol, and
the in-process **libavcodec / libavutil** (all listed in §1).

This whole path is a compile-time cargo feature, **`zero-copy`**, on by default.
Build with **`--no-default-features`** to drop `ffmpeg-next` + `libgbm` entirely;
the app then builds on distros without ffmpeg 9 (Debian/Ubuntu/Pop!_OS LTS) and
recording uses only the external `ffmpeg` binary (no in-process zero-copy). **See
§7** for when this is mandatory rather than optional.

---

## 6. Filesystem & OS integration

| Dependency | Why |
|---|---|
| **Linux `/proc`** | Single-instance lock and "close other overlays on capture" read `/proc/<pid>/exe`. |
| **`~/.config/cosmic/`** (theme + background config) | Read to match COSMIC's window corner radius and active-window border on window captures, and to composite the real wallpaper. Falls back to sane defaults when absent. |
| **XDG base dirs** | `XDG_RUNTIME_DIR` for short-lived handoff files (clipboard payload, OCR temp PNG); persisted settings are TOML at `~/.config/cosmic-capture-kit/config.toml` (a legacy `state.ron` under the XDG state dir is still read and migrated). |
| **System fonts** | UI text rendering (cosmic-text). Uses installed fonts via the system font database. |
| **`dev.thedragon.CosmicCaptureKit.desktop`** (desktop entry) | Matches the app's `app_id` so the desktop and xdg-desktop-portal resolve its name (**"Cosmic Capture Kit"**) instead of a generic / wrong fallback in the screencast picker. Shipped in `res/`; install to `~/.local/share/applications/`. |

---

## 7. Building from source

Everything above is a RUNTIME dependency unless its row says otherwise. This
section is the BUILD side: a missing entry here fails the compile, it does not
degrade a feature.

### The linked libraries

Three system libraries are linked directly into the binary, so building needs
their development packages (the headers, and the unversioned `.so` symlink that
only the dev package ships). A desktop system normally has the runtime library
and not the dev package, which is what makes a first build fail.

| Build dependency | Why it is needed to build | Arch / CachyOS | Debian / Ubuntu / Mint / Pop!_OS |
|---|---|---|---|
| **libxkbcommon** | `smithay-client-toolkit`'s build script pkg-configs it. | `libxkbcommon` | `libxkbcommon-dev` |
| **libpulse** | `src/audio/pulse_ffi.rs` declares `#[link(name = "pulse")]` (§4). | `libpulse` | `libpulse-dev` |
| **libpipewire** | The `pipewire` crate, an unconditional Linux dependency. | `libpipewire` | `libpipewire-0.3-dev` |
| **libclang** | Not linked, but `libspa-sys` generates the PipeWire bindings with bindgen. | `clang` | `libclang-dev` |
| a C toolchain, pkg-config | Build scripts and the final link step. | `base-devel` | `build-essential`, `pkg-config` |

Two package-name traps, both verified rather than guessed:

- On Arch the headers and the `libpipewire-0.3.pc` / `libspa-0.2.pc` files are in
  **`libpipewire`**, the client-library package. The `pipewire` package is the
  daemon and ships no headers and no `.pc` files, so installing it alone leaves
  the build failing.
- On Debian the SPA headers are a separate package, but `libpipewire-0.3-dev`
  depends on `libspa-0.2-dev`, so installing the one pulls the other. Both are
  needed: `libspa-sys` pkg-configs `libspa-0.2`.

```sh
# Debian / Ubuntu / Linux Mint / Pop!_OS
sudo apt install build-essential pkg-config libclang-dev \
                 libxkbcommon-dev libpulse-dev libpipewire-0.3-dev
```

`libwayland` is deliberately absent: the Wayland client stack is `dlopen`ed
(§1), so no dev package is required for it.

### `zero-copy` needs ffmpeg 9, which LTS distros do not have

The default `zero-copy` feature links the system libavcodec/libavutil through
`ffmpeg-next`, which binds the **ffmpeg 9.0** headers. Rolling distros (Arch,
CachyOS, recent Fedora) have that. The LTS distros do not: Ubuntu 24.04 (and so
Mint 22) ships ffmpeg **6.1.1**, and no `-dev` package there can satisfy an
ffmpeg 9 binding at any version, so the build stops inside `ffmpeg-sys-next`.

On those distros, build with the feature off:

```sh
cargo build --release --no-default-features
```

Two things to know about that flag:

1. It is needed on **every** cargo invocation (`build`, `test`, `run`,
   `install`), not just the first. Leaving it off restores the default feature
   set and the same failure.
2. Cargo has no way to disable one feature by name, only `--features` to add
   one, so `--no-default-features` is the whole lever. `zero-copy` is this
   crate's only feature, so the two mean the same thing here.

Nothing is lost but the in-process GPU zero-copy path (§5). Recording still
works through the external `ffmpeg` binary, on ffmpeg 5+. That floor is
measured, not presumed (DRAGON-568): ffmpeg 4.4, 5.1, 6.1 and 7.1 all block a
raw-PCM FIFO input's open until real audio data arrives, and the shipped probe
flags plus the audio pump's opening prime bound that hunger to 4096 bytes,
cleared in about a tenth of a second, ffmpeg 4.4 included.

---

## 8. Pinned third-party artifacts (release builds)

Everything in this section is about what the RELEASE PIPELINE downloads, not
about what you need installed to run the app. A build from source, and the plain
Linux zip, still resolve `ffmpeg` and `tesseract` from the distro exactly as §3
describes.

The macOS `.app`, the Windows MSI and the Linux **AppImage** cannot do that: the
user's machine may have neither tool, so those packages **ship their own copies
beside the app binary**, which `util::locate_tool` finds before `PATH`. The Linux
zip build downloads one thing too, an ffmpeg source tarball, built inside its
container purely to supply the headers the zero-copy encoder compiles against
(§7).

### What the AppImage bundles, and what it deliberately does not

| Bundled | Why |
|---|---|
| `ffmpeg`, `ffprobe` | recording and probing, on a machine that may have no ffmpeg at all, or one older than 8 |
| `libavcodec`, `libavdevice`, `libavfilter`, `libavformat`, `libavutil`, `libswresample`, `libswscale`, `libx264` | the in-process **GPU zero-copy** encoder `dlopen`s the libav trio (§5). Bundling them is what gives zero-copy to Debian-family users, who cannot have it from their own repositories |
| `tesseract` + `tessdata` (`eng` plus tesseract's own `configs/`) | OCR. Statically linked against leptonica and libpng, so it needs nothing bundled beside it |

Everything else comes from the host, and that is a decision rather than an
omission. **Mesa, libgbm, libdrm, libva and the Vulkan loader must match the
user's GPU driver**; a bundled copy breaks rendering wherever the driver differs
from the build machine. libpulse and libpipewire are clients of host daemons and
are left to the host for the same family of reason. The app already hard-links
libpulse, libgbm and libpipewire (§1), so a machine that can start the app
demonstrably has them.

Two consequences worth knowing:

* The AppImage's glibc floor is **whatever its build base has**, and an AppImage
  does not fix that for itself. It is built on Rocky 9, so the floor is
  `GLIBC_2.34`, reaching Ubuntu 22.04, Mint 21, Pop!_OS 22.04, Debian 12 and
  everything newer. The build prints the measured floor so a base-image change
  cannot raise it quietly.
* The bundled ffmpeg has **no libx265**, so software HEVC encoding is
  unavailable there (the UI hides it). Hardware HEVC through VAAPI or NVENC is
  present, and H.264 software encoding through libx264 is present, so the
  fallback tier every machine relies on is intact.

The AppImage carries a **static-FUSE runtime** rather than the classic one.
Debian 12 and Ubuntu 24.04 dropped libfuse2, and the usual workaround
(`--appimage-extract-and-run`) unpacks the whole payload on every launch, which
for a one-shot tool bound to PrintScreen would mean an extraction per screenshot.
The runtime still needs the host's `fusermount3` helper and `/dev/fuse` to mount
itself, which the `fuse3` package provides and every current desktop already
has; `--appimage-extract-and-run` remains the escape hatch on a system without
it.

Every one of those artifacts is **pinned to an exact version and verified before
use**. Nothing follows a "latest" pointer, so a release built today and the same
release rebuilt later contain byte-identical third-party parts.

**`scripts/pins.env` is the single source of truth**: one plain `KEY=value`
manifest that every fetcher reads, so "the same ffmpeg and tesseract everywhere"
is true by construction rather than by vigilance. It exists because the pins
previously lived in each fetcher and drifted immediately: the Linux container
built ffmpeg 8.0 while the macOS package bundled 8.1.2, and nothing caught it but
a human reading both files. Its readers are:

| Reads `pins.env` | For |
|---|---|
| `scripts/fetch-mac-vendor.sh` | the macOS `.app` sidecars |
| `scripts/fetch-win-vendor.ps1` | the Windows MSI sidecars |
| `.github/workflows/release.yml` (`build-linux`) | the container's ffmpeg headers |
| `justfile` (the linux `dist` recipe) | the same container build |
| `scripts/appimage/Dockerfile` | everything the AppImage bundles |

The two fetch scripts populate the git-ignored `vendor/` directory, and both the
release workflow and `just build` call them, so a local package and a shipped one
cannot differ. They are idempotent: each vendor dir carries a `.pinned` stamp,
and a matching stamp skips the work. Change a pin in the manifest and the stamp
changes with it, which is what forces the re-fetch.

### What is pinned

| Component | Version | Where it comes from | Verified by |
|---|---|---|---|
| ffmpeg + ffprobe (macOS arm64) | **9.0** | martin-riedl.de, a permanent per-build URL with a published `.sha256` | SHA-256 |
| ffmpeg + ffprobe + ffplay (Windows x86_64) | **9.0** | **built by us** (DRAGON-675): cross-compiled with mingw-w64 in a container, `scripts/ffmpeg-win/`, from the same signed source tarball the Linux artifacts use, then hosted on our own `vendor-mirror` release. Previously gyan.dev's `full_build-shared`, and before that BtbN's | **GPG signature** on the source + SHA-256 on the archive |
| tesseract (macOS arm64) | **5.5.3** | built from source, with leptonica **1.87.0** and libpng **1.6.50** | SHA-256 |
| tesseract (Windows x86_64) | **5.5.3** | the official installer published as a GitHub release asset by the tesseract project, unpacked with 7-Zip (never executed) | SHA-256 |
| `eng.traineddata` | `tessdata_fast` tag **4.1.0** | the same file and hash on both platforms, so OCR results match | SHA-256 |
| ffmpeg **source** (Linux artifact) | **9.0** | ffmpeg.org, built inside the Rocky 9 container only to supply the headers `ffmpeg-sys-next` compiles against | **GPG signature** + SHA-256 |
| ffmpeg **source** (Linux AppImage) | **9.0** | the same tarball, built wider (x264, pulse, VAAPI, NVENC) because the AppImage *ships* the binary and its libraries rather than just compiling against the headers | **GPG signature** + SHA-256 |
| x264 (Linux AppImage) | git **0480cb0** (2025-09-10) | Debian's immutable pool tarball. x264 publishes no releases and no tags, videolan's snapshot directory stopped in 2019, and GitLab's `/-/archive/` tarballs are generated per request so their bytes can change under a hash | SHA-256 |
| nv-codec-headers (Linux AppImage) | **13.0.19.1** | header-only NVENC interface; links nothing, and decides only whether the bundled ffmpeg has `h264_nvenc` at all | SHA-256 |
| tesseract + leptonica + libpng (Linux AppImage) | **5.5.3** / **1.87.0** / **1.6.50** | the same sources and the same static-link recipe the macOS package uses | SHA-256 |
| AppImage runtime | tag **20251108** | AppImage/type2-runtime, the **dated** release rather than the moving `continuous` tag, which cannot carry a checksum that stays true | SHA-256 |

The Linux row is the odd one out in two ways, both deliberate. It is a *source*
tarball rather than a shipped binary, and it is the only artifact any upstream
here signs, so it is verified by **FFmpeg's detached GPG signature** with the
release key fingerprint `FCF986EA15E6E293A5644F10B4322F04D67658D8` pinned in the
build recipe and independently published on ffmpeg.org/download.html. That is
strictly stronger than a checksum: a hash we carry only proves the bytes still
match what we saw when the pin was made, while the signature proves the FFmpeg
project produced them. The build imports the key into a throwaway keyring and
requires the signature to be made by that exact fingerprint, so a compromised key
URL supplying both a key and a matching signature still fails.

The Windows binaries now inherit that guarantee (DRAGON-675). They used to be a
third-party build verified by a hash we computed ourselves; they are now OUR
build of that same signed tarball, so the signature covers the Windows package's
ffmpeg too, one step further up.

None of the other upstreams publish signatures (probed 2026-08: martin-riedl.de
offers `.sha256` only, and the libpng, leptonica, tesseract and tessdata
downloads none at all), so for those a pinned hash is the honest best available
rather than an equivalent guarantee.

**A PREBUILT binary's pin is never "just a refresh".** What a binary demands of
the user's machine is decided when it is compiled, and for someone else's build
we chose none of that and can read almost none of it back. This is not
hypothetical: a Windows ffmpeg pin moving 9.0 to 9.0.1 raised the minimum NVIDIA
driver from ~570 to 610.00, because the newer build had been compiled against
different nv-codec-headers, and no assertion on the file could have seen it
(DRAGON-671). Source we build ourselves is different in kind: every floor it
imposes is one of our own pins. `scripts/pins.env` carries the full rule.

**ffmpeg is held at 9.0 deliberately.** The recording pipeline is written against
ffmpeg's observed behaviour, including workarounds measured on ffmpeg 8 that
stay as bounded defenses on 9 (the `MuxerWatchdog`
exists because ffmpeg 8 could wedge on a session's first video write; a live
`-itsoffset` is banned because ≥ ~200 ms deterministically stalled ffmpeg 8's
scheduler). Moving up is a real change that needs a recording test pass behind
it, not something a version bump does quietly.

**Why macOS builds tesseract from source.** There is no self-contained arm64
macOS tesseract to download. Homebrew's links leptonica out of `/opt/homebrew`,
so copying it yields an app that runs on the build machine and dies everywhere
else. Building leptonica and libpng as static libraries and linking them into the
tesseract CLI leaves a single binary whose only dynamic dependencies are `/usr/lib`
system libraries, which is what `mac-package.sh`'s `_check_relocatable` guard
demands. It costs roughly two minutes on a hosted runner, once per pin change.

Windows takes the prebuilt official installer instead, and ships the DLLs beside
`tesseract.exe`.

### Licensing

The bundled ffmpeg is a GPL build. The app **spawns** it as a separate process
rather than linking it, which is the arm's-length case the GPL treats as mere
aggregation, so the obligation attaches to the bundled binary rather than to the
application. `mac-package.sh` therefore ships the GPLv3 text plus a `NOTICE`
naming the exact build and offering its source. Tesseract is Apache-2.0 and
leptonica BSD-2-Clause; both permit binary redistribution inside a proprietary
build as long as the notice travels with it, which `TESSERACT-LICENSE` does.

---

## Quick install (Arch / CachyOS)

```sh
# Required-ish + all features:
sudo pacman -S ffmpeg tesseract tesseract-data-eng xdg-desktop-portal-cosmic

# Optional hardware-encoder labelling / accel (install what matches your GPU):
sudo pacman -S pciutils                       # lspci (GPU naming)
sudo pacman -S nvidia-utils                   # NVENC + nvidia-smi (NVIDIA)
sudo pacman -S libva-mesa-driver libva-utils  # VAAPI (AMD/Intel)

# Audio in recordings (usually already present on a COSMIC desktop):
sudo pacman -S pipewire-pulse                 # or pulseaudio
```

> Wayland/Vulkan/D-Bus and the COSMIC compositor are assumed present on any COSMIC
> desktop, so they are not listed in the install command.

## Quick install (Debian / Ubuntu / Mint / Pop!_OS)

Runtime packages only. The build packages are in §7, and on these distros the
build also needs `--no-default-features`.

```sh
# Recording and OCR:
sudo apt install ffmpeg tesseract-ocr tesseract-ocr-eng

# Sound for recordings, which is REQUIRED for recording to work at all (§4).
# Usually already present, but confirm rather than assume:
sudo apt install pipewire-pulse pulseaudio-utils   # pactl lives in -utils

# Optional hardware-encoder labelling / accel (install what matches your GPU):
sudo apt install pciutils                          # lspci (GPU naming)
sudo apt install vainfo va-driver-all              # VAAPI (AMD/Intel)
```

> The xdg-desktop-portal BACKEND is desktop-specific and has no single package
> name here: COSMIC uses `xdg-desktop-portal-cosmic`, which these distros may not
> package. Without a backend the folder pickers in Settings will not open, though
> the directory can still be typed in.
>
> These distros are not a supported target yet. See the README's support table;
> what runs there beyond the build is a separate open question.
