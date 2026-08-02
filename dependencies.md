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
| **Wayland compositor (COSMIC)** | The whole app is a native COSMIC overlay. | See protocols below — capture relies on COSMIC's screencopy, so it does **not** work on non-COSMIC compositors. |
| **Vulkan-capable GPU + driver** | The overlay is rendered with `wgpu` (via libcosmic/iced), whose primary Linux backend is Vulkan. | Needs the Vulkan loader (`libvulkan.so.1`) and an ICD — NVIDIA's driver, or Mesa (RADV/ANV). Loaded at runtime (not shown by `ldd`). |
| **libxkbcommon** (`libxkbcommon.so.0`) | Keyboard handling (Escape to cancel, etc.). | **Linked** into the binary, so building needs its dev package too. See §7. |
| **libpulse** (`libpulse.so.0`) | The shared PulseAudio client FFI (`src/audio/pulse_ffi.rs`): the device-latency probe and the system-audio monitor capture. | **Linked** into the binary (`#[link(name = "pulse")]`, not `dlopen`ed), so it is a build requirement of every Linux build, including one that will never record. See §7. |
| **libpipewire** (`libpipewire-0.3.so.0`) | The xdg-portal ScreenCast capture path (the `pipewire` crate binds it). | **Linked** into the binary, and an unconditional Linux dependency rather than a feature-gated one. See §7. |
| **libwayland-client** | Wayland client transport. | `dlopen`ed at runtime by the Wayland client stack, so no dev package is needed to build. |
| **libgbm** (`libgbm.so.1`) | Allocates the GPU buffer for **zero-copy recording** (via the `gbm` crate): the compositor copies each frame straight into it. | Part of Mesa; present on any GPU desktop. Used only when GPU zero-copy is enabled. |
| **libavcodec / libavutil** | **In-process** hardware video encoding for the zero-copy path (via `ffmpeg-next`), distinct from the external `ffmpeg` binary. | Linked at build time, version-matched to ffmpeg 8.1. Used only for GPU zero-copy. |
| **DRM render node** (`/dev/dri/renderD*`) | The GPU the compositor renders on — zero-copy allocates its capture buffer and runs the in-process encoder on this same device. | Requires membership in the `render` / `video` group. Zero-copy only. |

### Wayland protocols the compositor must implement

Pixels are captured **natively** (no `grim`); each of these is bound directly:

| Protocol | Used for |
|---|---|
| **COSMIC screencopy** (`cosmic-protocols`, ext-image-copy-capture) | All pixel capture — monitor, region, and per-window. This is COSMIC-specific; it is why capture only works under COSMIC. |
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
| `org.freedesktop.FileManager1` (`ShowItems`) | "Show in file manager" reveal. | Falls back to the portal `OpenURI` on the containing directory. |
| `org.freedesktop.portal.OpenURI` | Opening a URL decoded from a QR code, and the file-manager reveal fallback (replaces shelling out to `xdg-open`). | Provided by the base xdg-desktop-portal; silent no-op if absent. |
| **xdg-desktop-portal** + a backend (**xdg-desktop-portal-cosmic**) | Folder pickers in Settings (screenshot/recording save dirs) via `org.freedesktop.portal.FileChooser` (the `ashpd` crate). | Picker won't open; the dir can still be typed/edited and is persisted. |

---

## 3. External command-line tools

Each is found on `PATH` at runtime; the feature turns itself off when the tool is
absent.

### Feature tools

| Binary | Package (Arch) | Feature | Without it |
|---|---|---|---|
| **ffmpeg** | `ffmpeg` | Screen recording. Raw frames are piped to ffmpeg (`-f rawvideo`) and encoded. | The Recordings feature is disabled and the UI warns. |
| **tesseract** | `tesseract` + a language pack (e.g. `tesseract-data-eng`) | OCR text detection ("Scan text (OCR) in region mode"). The region is handed to `tesseract … tsv`. | The toggle shows a "tesseract not found" hint and no-ops. |

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
the app then builds on distros without ffmpeg 8 (Debian/Ubuntu/Pop!_OS LTS) and
recording uses only the external `ffmpeg` binary (no in-process zero-copy). **See
§7** for when this is mandatory rather than optional.

---

## 6. Filesystem & OS integration

| Dependency | Why |
|---|---|
| **Linux `/proc`** | Single-instance lock and "close other overlays on capture" read `/proc/<pid>/exe`. |
| **`~/.config/cosmic/`** (theme + background config) | Read to match COSMIC's window corner radius and active-window border on window captures, and to composite the real wallpaper. Falls back to sane defaults when absent. |
| **XDG base dirs** | `XDG_RUNTIME_DIR` for short-lived handoff files (clipboard payload, OCR temp PNG); `XDG_STATE_HOME`/cache for persisted settings (`state.ron`). |
| **System fonts** | UI text rendering (cosmic-text). Uses installed fonts via the system font database. |
| **`dev.frosthaven.CosmicCaptureKit.desktop`** (desktop entry) | Matches the app's `app_id` so the desktop and xdg-desktop-portal resolve its name (**"Cosmic Capture Kit"**) instead of a generic / wrong fallback in the screencast picker. Shipped in `res/`; install to `~/.local/share/applications/`. |

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

### `zero-copy` needs ffmpeg 8, which LTS distros do not have

The default `zero-copy` feature links the system libavcodec/libavutil through
`ffmpeg-next`, which binds the **ffmpeg 8.1** headers. Rolling distros (Arch,
CachyOS, recent Fedora) have that. The LTS distros do not: Ubuntu 24.04 (and so
Mint 22) ships ffmpeg **6.1.1**, and no `-dev` package there can satisfy an
ffmpeg 8 binding at any version, so the build stops inside `ffmpeg-sys-next`.

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
works through the external `ffmpeg` binary, which is fine on ffmpeg 5+.

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
