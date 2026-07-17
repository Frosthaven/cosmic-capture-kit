//! Device enumeration for the Audio settings dropdowns. On Linux this lists the
//! system's input sources (mics) and output sinks (speakers) with COSMIC-style
//! `"<active port> - <device>"` labels matching cosmic-settings, shelling out to
//! `pactl`. On macOS it lists avfoundation audio-input devices (id = the device
//! NAME — avfoundation's `-i` matches non-numeric strings by exact name, and names
//! survive replugs where the enumeration INDEX shifts), parsed from `ffmpeg -f
//! avfoundation -list_devices true`; there are no output sinks (system audio comes
//! from ScreenCaptureKit, not a Pulse sink monitor).

use std::process::Command;

/// Whether the `pactl` binary is on `PATH` (Linux audio in/out device enumeration shells
/// out to it). Without it, only the system-default device is offered in Settings. Always
/// false on macOS — there is no PulseAudio; device enumeration uses avfoundation instead.
pub fn pactl_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|d| d.join("pactl").is_file()))
            .unwrap_or(false)
    }
}

/// Build a COSMIC-style device label `"<active port> - <device>"`, reproducing the
/// abbreviations cosmic-settings applies: "HDMI / DisplayPort" -> "HDMI / DP", "High
/// Definition Audio" -> "HD Audio", and a trailing " Controller" dropped. Falls back to
/// the block's own `fallback` description when the port or device name isn't known.
#[cfg(not(target_os = "macos"))]
fn cosmic_audio_label(port: Option<&str>, device: Option<&str>, fallback: &str) -> String {
    match (port, device) {
        (Some(p), Some(d)) => {
            let p = p.replace("HDMI / DisplayPort", "HDMI / DP");
            let d = d.replace("High Definition Audio", "HD Audio");
            let d = d.strip_suffix(" Controller").map(str::to_string).unwrap_or(d);
            format!("{p} - {d}")
        }
        _ => fallback.to_string(),
    }
}

/// One parsed `pactl list sinks|sources` block.
#[cfg(not(target_os = "macos"))]
#[derive(Default)]
struct PaDevice {
    name: Option<String>,
    description: Option<String>, // the block's own "Description:" (fallback label)
    device_desc: Option<String>, // device.description property
    active_port: Option<String>,
    ports: std::collections::HashMap<String, String>, // port name -> human description
    monitor: bool,
}

#[cfg(not(target_os = "macos"))]
impl PaDevice {
    /// Commit this block to `out` as `(name, COSMIC-style label)`, unless it's unnamed
    /// or (when `skip_monitors`) a monitor source. Resets for the next block.
    fn flush(&mut self, out: &mut Vec<(String, String)>, skip_monitors: bool) {
        if let Some(n) = self.name.take()
            && !(skip_monitors && self.monitor)
        {
            let port = self
                .active_port
                .as_deref()
                .and_then(|a| self.ports.get(a))
                .map(String::as_str);
            let fallback = self.description.as_deref().unwrap_or(&n);
            let label = cosmic_audio_label(port, self.device_desc.as_deref(), fallback);
            out.push((n, label));
        }
        *self = PaDevice::default();
    }
}

/// Parse `pactl list <object>` (object = "sinks" / "sources", `header` = "Sink #" /
/// "Source #") into `(name, label)` pairs with COSMIC-style labels. `skip_monitors`
/// drops monitor sources (used for the input picker; monitors feed the echo reference /
/// system-audio path instead). Empty on any failure — the UI then offers only its
/// "System (automatic)" default. Shells out to `pactl` (from pipewire-pulse).
#[cfg(not(target_os = "macos"))]
fn list_pa_devices(object: &str, header: &str, skip_monitors: bool) -> Vec<(String, String)> {
    // Review aid: with CCK_HEALTH_FORCE_WARN, behave as if pactl found nothing, so the
    // device pickers fall back to (and enforce) "System (automatic)" - matching the
    // forced-missing pactl dependency.
    if std::env::var_os("CCK_HEALTH_FORCE_WARN").is_some() {
        return Vec::new();
    }
    let Ok(out) = Command::new("pactl").args(["list", object]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut devices = Vec::new();
    let mut cur = PaDevice::default();
    let mut in_ports = false; // inside the "Ports:" block (collecting port descriptions)
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(header) {
            cur.flush(&mut devices, skip_monitors);
            in_ports = false;
        } else if let Some(v) = t.strip_prefix("Name: ") {
            cur.name = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Description: ") {
            cur.description = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Monitor of Sink: ") {
            cur.monitor = v != "n/a";
        } else if let Some(v) = t.strip_prefix("device.description = \"") {
            cur.device_desc = Some(v.trim_end_matches('"').to_string());
        } else if t == "Ports:" {
            in_ports = true;
        } else if let Some(v) = t.strip_prefix("Active Port: ") {
            cur.active_port = Some(v.to_string());
            in_ports = false;
        } else if in_ports {
            // "<port-name>: <Human Description> (type: ..., priority: ...)" — the human
            // text is everything before the " (type:" metadata (it may itself contain
            // parens, e.g. "Digital Output (S/PDIF)").
            if let Some((pn, rest)) = t.split_once(": ") {
                let desc = rest.split(" (type:").next().unwrap_or(rest).trim();
                if !desc.is_empty() {
                    cur.ports.insert(pn.to_string(), desc.to_string());
                }
            }
        }
    }
    cur.flush(&mut devices, skip_monitors);
    devices
}

/// Enumerate input sources (mics, line-in), EXCLUDING monitors, with COSMIC-style
/// labels matching cosmic-settings' "Input device" list. On macOS this returns the
/// avfoundation audio-input devices as `(name, name)` (see
/// [`list_avfoundation_inputs`]): the device NAME is both the persisted id
/// (`mic_source()`, captured via `-f avfoundation -i ":<name>"`) and the label.
/// A name is stable across replugs where the enumeration index is not, and a
/// STALE name fails the capture open loudly instead of silently grabbing
/// whatever device shifted into an old index.
pub fn list_input_sources() -> Vec<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        list_avfoundation_inputs()
    }
    #[cfg(not(target_os = "macos"))]
    {
        list_pa_devices("sources", "Source #", true)
    }
}

/// Enumerate output sinks (speakers, headphones, HDMI/DP) with COSMIC-style labels
/// matching cosmic-settings' "Output device" list. The echo-cancellation reference is
/// captured from each sink's `<name>.monitor` source. On macOS this is empty — there is
/// no Pulse sink monitor; system audio (and the AEC far-end) comes from ScreenCaptureKit.
pub fn list_output_sinks() -> Vec<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        Vec::new()
    }
    #[cfg(not(target_os = "macos"))]
    {
        list_pa_devices("sinks", "Sink #", false)
    }
}

/// Run `ffmpeg -f avfoundation -list_devices true -i ""` (which exits non-zero and
/// prints the device inventory on STDERR) and parse its AUDIO section, keeping the
/// device NAME as both id and label (the parse yields `(index, name)`; the index is
/// dropped because it shifts on replug). Empty on any failure — the UI then offers
/// only its "System (automatic)" default.
#[cfg(target_os = "macos")]
fn list_avfoundation_inputs() -> Vec<(String, String)> {
    let Ok(out) = Command::new(crate::util::ffmpeg_path())
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
    else {
        return Vec::new();
    };
    parse_avfoundation_audio_devices(&String::from_utf8_lossy(&out.stderr))
        .into_iter()
        .map(|(_, name)| (name.clone(), name))
        .collect()
}

/// Parse the stderr of `ffmpeg -f avfoundation -list_devices true` into the AUDIO
/// devices as `(index, name)` pairs. ffmpeg prints, tagged with an
/// `[AVFoundation indev @ 0x…]` prefix on EVERY line, a video section then an audio
/// section, each a `AVFoundation <kind> devices:` header followed by `[<i>] <name>`
/// entries. Only the audio section is collected; unrelated banner/warning lines (with
/// or without the indev tag) are ignored.
#[cfg(target_os = "macos")]
fn parse_avfoundation_audio_devices(stderr: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_audio = false;
    for line in stderr.lines() {
        // Strip the leading "[AVFoundation indev @ 0x…] " tag (split on its "] ").
        let rest = line.split_once("] ").map(|(_, r)| r).unwrap_or(line).trim();
        if rest.eq_ignore_ascii_case("AVFoundation audio devices:") {
            in_audio = true;
            continue;
        }
        // Any other section header ends the audio section (e.g. the video header, which
        // in ffmpeg's output precedes audio, but guard both orders).
        if rest.ends_with("devices:") {
            in_audio = false;
            continue;
        }
        if !in_audio {
            continue;
        }
        // Device entry: "[<index>] <name>".
        if let Some((idx, name)) = rest.strip_prefix('[').and_then(|r| r.split_once("] ")) {
            let (idx, name) = (idx.trim(), name.trim());
            if !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty() {
                out.push((idx.to_string(), name.to_string()));
            }
        }
    }
    out
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::parse_avfoundation_audio_devices;

    // Real stderr shape from `ffmpeg -f avfoundation -list_devices true -i ""` (ffmpeg
    // 8.1.2, Homebrew) on the dev machine — the indev tag prefixes every line.
    const REAL: &str = "\
[AVFoundation indev @ 0xb14c10140] AVFoundation video devices:
[AVFoundation indev @ 0xb14c10140] [0] FaceTime HD Camera
[AVFoundation indev @ 0xb14c10140] [1] C922 Pro Stream Webcam
[AVFoundation indev @ 0xb14c10140] [2] Capture screen 0
[AVFoundation indev @ 0xb14c10140] [3] Capture screen 1
[AVFoundation indev @ 0xb14c10140] AVFoundation audio devices:
[AVFoundation indev @ 0xb14c10140] [0] BlackHole 2ch
[AVFoundation indev @ 0xb14c10140] [1] C922 Pro Stream Webcam
[AVFoundation indev @ 0xb14c10140] [2] MacBook Pro Microphone
[AVFoundation indev @ 0xb14c10140] [3] CalDigit TS4 Audio - Rear
[AVFoundation indev @ 0xb14c10140] [4] Microsoft Teams Audio
[in#0 @ 0xb14c10000] Error opening input: Input/output error
Error opening input file .
Error opening input files: Input/output error";

    #[test]
    fn parses_real_output_audio_only() {
        let got = parse_avfoundation_audio_devices(REAL);
        assert_eq!(
            got,
            vec![
                ("0".to_string(), "BlackHole 2ch".to_string()),
                ("1".to_string(), "C922 Pro Stream Webcam".to_string()),
                ("2".to_string(), "MacBook Pro Microphone".to_string()),
                ("3".to_string(), "CalDigit TS4 Audio - Rear".to_string()),
                ("4".to_string(), "Microsoft Teams Audio".to_string()),
            ]
        );
    }

    #[test]
    fn single_audio_device() {
        let s = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone";
        assert_eq!(
            parse_avfoundation_audio_devices(s),
            vec![("0".to_string(), "MacBook Pro Microphone".to_string())]
        );
    }

    #[test]
    fn no_audio_devices() {
        let s = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] AVFoundation audio devices:";
        assert!(parse_avfoundation_audio_devices(s).is_empty());
    }

    #[test]
    fn video_only_no_audio_section() {
        let s = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] [1] Capture screen 0";
        assert!(parse_avfoundation_audio_devices(s).is_empty());
    }

    #[test]
    fn ignores_interleaved_warnings() {
        // Banner/warning lines (with and without the indev tag) inside and around the
        // audio section must never be mistaken for devices.
        let s = "\
ffmpeg version 8.1.2 Copyright (c) 2000-2026 the FFmpeg developers
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone
[swscaler @ 0x2] deprecated pixel format used
[AVFoundation indev @ 0x1] [1] BlackHole 2ch
[in#0 @ 0x3] Error opening input: Input/output error
Error opening input file .";
        assert_eq!(
            parse_avfoundation_audio_devices(s),
            vec![
                ("0".to_string(), "MacBook Pro Microphone".to_string()),
                ("1".to_string(), "BlackHole 2ch".to_string()),
            ]
        );
    }

    #[test]
    fn names_with_special_chars_kept_whole() {
        // A hyphen/space-laden name (and one that itself contains a bracket-like token)
        // must survive intact — only the leading "[<idx>] " is stripped.
        let s = "\
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] CalDigit TS4 Audio - Rear
[AVFoundation indev @ 0x1] [10] Aggregate [stereo] Device";
        assert_eq!(
            parse_avfoundation_audio_devices(s),
            vec![
                ("0".to_string(), "CalDigit TS4 Audio - Rear".to_string()),
                ("10".to_string(), "Aggregate [stereo] Device".to_string()),
            ]
        );
    }
}
