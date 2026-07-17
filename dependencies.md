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
| **libxkbcommon** (`libxkbcommon.so.0`) | Keyboard handling (Escape to cancel, etc.). | The only extra system library linked directly into the binary. |
| **libwayland-client** | Wayland client transport. | `dlopen`ed at runtime by the Wayland client stack. |
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

## 4. Audio capture — optional (recordings with sound)

| Dependency | Why | Without it |
|---|---|---|
| **PulseAudio** or **PipeWire** (with `pipewire-pulse`) | Microphone and system-audio capture for recordings; ffmpeg reads via `-f pulse`. | The mic / system-audio toggles produce no audio; recordings are video-only. |
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

This whole path is a compile-time cargo feature — **`zero-copy`**, on by default.
Build with **`--no-default-features`** to drop `ffmpeg-next` + `libgbm` entirely;
the app then builds on distros without ffmpeg 8 (Debian/Ubuntu/Pop!_OS LTS) and
recording uses only the external `ffmpeg` binary (no in-process zero-copy).

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
