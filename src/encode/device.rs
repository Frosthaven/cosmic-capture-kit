//! Encoder discovery: probe which hardware/software encoders this machine can
//! actually use and attach friendly, human-readable labels for the UI.

use std::process::Command;

use super::*;

/// A selectable encoder for the UI: stable `id` plus a friendly `label`.
#[derive(Clone)]
pub struct EncoderInfo {
    pub id: String,
    pub label: String,
}

impl AsRef<str> for EncoderInfo {
    fn as_ref(&self) -> &str {
        &self.label
    }
}

/// Probe the encoders usable on this machine, friendly-labelled, in preference
/// order (so `first()` is the best available): NVENC / VAAPI when the device +
/// ffmpeg encoder exist, then Software, which is always offered.
pub fn available_encoders() -> Vec<EncoderInfo> {
    let mut v = Vec::new();
    let enc = ffmpeg_encoders();
    // macOS: Apple VideoToolbox is the hardware tier — offered before Software, the
    // slot NVENC/VAAPI hold on Linux (both no-op here: no /dev/nvidia0, no /dev/dri).
    #[cfg(target_os = "macos")]
    if enc.contains("h264_videotoolbox") {
        v.push(EncoderInfo {
            id: "videotoolbox".into(),
            label: format!("{} (VideoToolbox)", chip_name()),
        });
    }
    if std::path::Path::new("/dev/nvidia0").exists()
        && (enc.contains("hevc_nvenc") || enc.contains("h264_nvenc"))
    {
        v.push(EncoderInfo {
            id: "nvenc".into(),
            label: format!("{} (NVENC)", nvidia_name()),
        });
    }
    if let Some((dev, _)) = vaapi_device()
        && (enc.contains("hevc_vaapi") || enc.contains("h264_vaapi")) {
            v.push(EncoderInfo {
                id: "vaapi".into(),
                label: format!("{} (VAAPI)", vaapi_name(&dev)),
            });
        }
    v.push(EncoderInfo {
        id: "software".into(),
        label: format!("{} (Software x264)", cpu_name()),
    });
    v
}

fn cpu_name() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|n| n.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "CPU".to_string())
}

/// Friendly Apple-Silicon/Intel chip name for the VideoToolbox label, via sysctl's
/// `machdep.cpu.brand_string` (e.g. "Apple M2 Pro") — the mac analogue of `cpu_name`'s
/// `/proc/cpuinfo` read on Linux.
#[cfg(target_os = "macos")]
fn chip_name() -> String {
    Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Apple Silicon".to_string())
}

/// One-shot `nvidia-smi` probe: the friendly GPU name when the driver stack answers,
/// or the post-driver-update NVML mismatch marker. Cached — the state only changes
/// with a reboot or package update, never within this one-shot process (and caching
/// keeps recording start from re-spawning nvidia-smi).
enum NvidiaProbe {
    /// nvidia-smi answered — or wasn't runnable at all, the historical
    /// generic-"NVIDIA GPU"-label case.
    Ready(String),
    /// nvidia-smi failed with "Failed to initialize NVML": the loaded kernel module
    /// no longer matches the userspace libraries on disk (a driver update since
    /// boot), and NVENC refuses to initialise right along with NVML.
    DriverMismatch,
}

fn nvidia_probe() -> &'static NvidiaProbe {
    static PROBE: std::sync::OnceLock<NvidiaProbe> = std::sync::OnceLock::new();
    PROBE.get_or_init(|| {
        match Command::new("nvidia-smi")
            .args(["--query-gpu=name", "--format=csv,noheader"])
            .output()
        {
            Ok(o) if nvml_mismatch(
                o.status.success(),
                &String::from_utf8_lossy(&o.stdout),
                &String::from_utf8_lossy(&o.stderr),
            ) =>
            {
                NvidiaProbe::DriverMismatch
            }
            Ok(o) => NvidiaProbe::Ready(
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "NVIDIA GPU".to_string()),
            ),
            Err(_) => NvidiaProbe::Ready("NVIDIA GPU".to_string()),
        }
    })
}

/// Pure classifier for the NVML mismatch state (unit-tested below): the probe must
/// have FAILED and its output must name the NVML initialisation failure. Checked on
/// BOTH streams — driver 610 prints it to stdout (exit 18), field reports of older
/// drivers say stderr. A clean run whose output happens to mention NVML, or a
/// failure for some other reason, is not a mismatch.
fn nvml_mismatch(success: bool, stdout: &str, stderr: &str) -> bool {
    const MARKER: &str = "Failed to initialize NVML";
    !success && (stdout.contains(MARKER) || stderr.contains(MARKER))
}

/// Whether the NVIDIA driver stack is in the post-update NVML "driver/library
/// version mismatch" state: a package update replaced the userspace libraries while
/// the previous kernel module is still loaded, so NVML — and NVENC with it — can't
/// initialise until the module reloads (in practice: a reboot). The Health page
/// shows this as a warning and `nvenc_plan` refuses NVENC while it holds (recordings
/// fall back to the next best encoder), but the encoder stays listed and the
/// persisted choice survives — the state is transient by nature.
pub fn nvenc_driver_mismatch() -> bool {
    std::path::Path::new("/dev/nvidia0").exists()
        && matches!(nvidia_probe(), NvidiaProbe::DriverMismatch)
}

fn nvidia_name() -> String {
    match nvidia_probe() {
        NvidiaProbe::Ready(name) => name.clone(),
        NvidiaProbe::DriverMismatch => "NVIDIA GPU".to_string(),
    }
}

/// Friendly name for the GPU behind a render node, via lspci on its PCI address.
fn vaapi_name(dev: &str) -> String {
    let node = dev.rsplit('/').next().unwrap_or("");
    let pci = std::fs::read_link(format!("/sys/class/drm/{node}/device"))
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    pci.and_then(|p| lspci_name(p.strip_prefix("0000:").unwrap_or(&p)))
        .unwrap_or_else(|| "GPU".to_string())
}

fn lspci_name(pci: &str) -> Option<String> {
    let out = Command::new("lspci").args(["-s", pci]).output().ok()?;
    let line = String::from_utf8(out.stdout).ok()?;
    let desc = line.split(": ").nth(1)?.trim();
    let desc = desc.split(" (rev").next().unwrap_or(desc).trim();
    let cleaned = desc
        .replace("Advanced Micro Devices, Inc. ", "AMD ")
        .replace("[AMD/ATI] ", "")
        .replace("Intel Corporation ", "Intel ")
        .replace("NVIDIA Corporation ", "");
    // Prefer a [Bracketed Model] (e.g. NVIDIA's "[GeForce RTX 4090]").
    if let (Some(a), Some(b)) = (cleaned.find('['), cleaned.find(']'))
        && b > a + 1 {
            return Some(cleaned[a + 1..b].trim().to_string());
        }
    Some(cleaned.trim().to_string())
}

/// Whether the resolved `ffmpeg` (override → sidecar → PATH; see
/// `util::ffmpeg_path`) is actually runnable. Recording requires it.
pub fn ffmpeg_available() -> bool {
    crate::util::tool_available(&crate::util::ffmpeg_path())
}

/// Whether the resolved `ffprobe` is runnable. It usually ships with ffmpeg but is
/// a separate binary (some distros split it); the video preview needs it to probe a
/// recording's metadata before playback.
pub fn ffprobe_available() -> bool {
    crate::util::tool_available(&crate::util::ffprobe_path())
}

/// `ffmpeg -encoders` output (empty string if ffmpeg can't be run), used to
/// detect which hardware encoders are actually built in.
pub(crate) fn ffmpeg_encoders() -> String {
    Command::new(crate::util::ffmpeg_path())
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The observed driver-610 shape: exit 18, the NVML line on STDOUT.
    #[test]
    fn nvml_mismatch_on_stdout_is_detected() {
        assert!(nvml_mismatch(
            false,
            "Failed to initialize NVML: Driver/library version mismatch\n\
             NVML library version: 610.43\n",
            ""
        ));
    }

    #[test]
    fn nvml_mismatch_on_stderr_is_detected() {
        assert!(nvml_mismatch(
            false,
            "",
            "Failed to initialize NVML: Driver/library version mismatch\n"
        ));
    }

    #[test]
    fn clean_run_is_not_a_mismatch() {
        assert!(!nvml_mismatch(true, "NVIDIA GeForce RTX 4090\n", ""));
    }

    #[test]
    fn failure_without_the_nvml_line_is_not_a_mismatch() {
        assert!(!nvml_mismatch(false, "", "No devices were found"));
    }

    #[test]
    fn nvml_text_on_a_clean_exit_is_not_a_mismatch() {
        assert!(!nvml_mismatch(true, "Failed to initialize NVML: transient", ""));
    }
}
