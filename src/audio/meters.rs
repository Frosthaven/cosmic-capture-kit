//! Per-channel level-meter sidecars: long-lived metering ffmpeg processes that publish a
//! channel's RMS level (~10 Hz) to a per-pid file under the runtime dir, plus the reader
//! that maps the last reading onto the 0..1 meter scale for the UI's on-button meters.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::record::AudioChannel;

/// File a channel's live level meter is published to (XDG_RUNTIME_DIR / tmpfs, so
/// the ~10Hz writes never touch the user's disk). Per-pid so multiple instances
/// don't collide.
fn meter_level_path(chan: AudioChannel) -> PathBuf {
    let dir = crate::util::runtime_dir();
    let c = match chan {
        AudioChannel::Mic => "mic",
        AudioChannel::Sys => "sys",
    };
    std::path::Path::new(&dir).join(format!(
        "cosmic-capture-kit.{}.{c}.level",
        std::process::id()
    ))
}

/// Spawn a metering ffmpeg that reads a channel's audio device and appends its RMS
/// level (dBFS, ~10Hz) to the channel's level file, for the UI's on-button volume
/// meters. The meter runs whenever a channel is "armed" (green), recording or not,
/// so it's a long-lived sibling process — `PR_SET_PDEATHSIG` ensures it's killed if
/// we exit or are signalled, so it can never orphan.
pub fn spawn_meter(chan: AudioChannel) -> Option<Child> {
    // macOS has no pulse monitors, so there is NO standalone capture to meter the
    // system channel from — and starting a second SCK audio stream just for metering
    // would fight the recording's own capture. Its level is instead PUBLISHED by the
    // owned recording capture (see [`publish_sys_level`] / `audio::capture`'s sck
    // module); while merely armed (not recording) the meter honestly stays flat.
    #[cfg(target_os = "macos")]
    if chan == AudioChannel::Sys {
        return None;
    }
    let source: String = match chan {
        AudioChannel::Mic => crate::audio::config::mic_source(),
        AudioChannel::Sys => "@DEFAULT_MONITOR@".to_string(),
    };
    let path = meter_level_path(chan);
    let _ = std::fs::remove_file(&path); // clear any stale level from a prior run
    // Batch to 0.1s frames → one RMS reading every 100ms; print just that key,
    // unbuffered, to the level file.
    let af = format!(
        "asetnsamples=n=4800:p=0,astats=metadata=1:reset=1,\
         ametadata=mode=print:key=lavfi.astats.Overall.RMS_level:file={}:direct=1",
        path.display()
    );
    let mut cmd = Command::new(crate::util::ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    // Capture input: PulseAudio on Linux; on macOS the mic is an avfoundation device
    // name (mirroring `clean_mic::spawn_pulse_pcm`'s arm — the leading colon selects
    // an audio-only device).
    #[cfg(not(target_os = "macos"))]
    cmd.args(["-f", "pulse", "-i", source.as_str()]);
    #[cfg(target_os = "macos")]
    cmd.args(["-f", "avfoundation", "-i", &format!(":{source}")]);
    cmd.args(["-af", &af, "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Kill the meter child if we die (PR_SET_PDEATHSIG). Linux-only; macOS has no
    // equivalent, so there the explicit stop_meter reaping handles cleanup.
    // SAFETY: only an async-signal-safe syscall in the forked child before exec.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            let _ = rustix::process::set_parent_process_death_signal(Some(
                rustix::process::Signal::KILL,
            ));
            Ok(())
        });
    }
    cmd.spawn().ok()
}

/// Stop a running meter process and drop its level file.
pub fn stop_meter(chan: AudioChannel, child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(meter_level_path(chan));
}

/// macOS: the system channel's live level (0..1 meter scale, f32 bits) as published
/// by the owned recording capture — the process-local stand-in for the Linux
/// sidecar's level file. 0 whenever no capture is running.
#[cfg(target_os = "macos")]
static SYS_LEVEL_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// macOS: publish the system channel's current meter level (0..1). Called by the
/// owned SCK system-audio capture (`audio::capture`'s sck module) per 0.1s RMS
/// window while a recording runs, and with 0.0 when the capture stops — there is no
/// pulse monitor to run a metering sidecar against on macOS, and a SECOND SCK audio
/// stream just for metering would fight the recording's own.
#[cfg(target_os = "macos")]
pub(crate) fn publish_sys_level(level: f32) {
    SYS_LEVEL_BITS.store(
        level.clamp(0.0, 1.0).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Map an RMS amplitude (linear, 0..1 full scale) onto the meters' 0..1 scale —
/// the same ~-60..0 dBFS window [`read_meter_level`] maps the Linux sidecar's
/// `RMS_level` readings through.
#[cfg(target_os = "macos")]
pub(crate) fn level_from_rms(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Current perceived level for a channel as 0..1, read from its meter file (the
/// last RMS_level line, mapped from a ~-60..0 dBFS range). `None` when there's no
/// reading yet (returns 0 so the meter is empty).
pub fn read_meter_level(chan: AudioChannel) -> f32 {
    // macOS: no sidecar/level file for the system channel — read the level the owned
    // SCK capture publishes while a recording runs (see [`publish_sys_level`]).
    #[cfg(target_os = "macos")]
    if chan == AudioChannel::Sys {
        return f32::from_bits(SYS_LEVEL_BITS.load(std::sync::atomic::Ordering::Relaxed));
    }
    let path = meter_level_path(chan);
    let Some(tail) = read_tail(&path, 256) else {
        return 0.0;
    };
    let text = String::from_utf8_lossy(&tail);
    let Some(line) = text.rsplit('\n').find(|l| l.contains("RMS_level=")) else {
        return 0.0;
    };
    let Some(val) = line.split("RMS_level=").nth(1).map(str::trim) else {
        return 0.0;
    };
    if val.starts_with("-inf") {
        return 0.0;
    }
    let Ok(db) = val.parse::<f32>() else {
        return 0.0;
    };
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Read up to the last `n` bytes of a file (for cheaply tailing the growing level
/// files without re-reading the whole thing each poll).
fn read_tail(path: &std::path::Path, n: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(n);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(n as usize);
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;

    #[test]
    fn level_from_rms_maps_the_same_dbfs_window_as_the_linux_sidecar() {
        // Full scale (0 dBFS) pegs the meter; silence and negatives are empty.
        assert_eq!(level_from_rms(1.0), 1.0);
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(-0.5), 0.0);
        // -20 dBFS (rms 0.1) → (−20+60)/60 ≈ 0.667, matching read_meter_level's map.
        let l = level_from_rms(0.1);
        assert!((l - 2.0 / 3.0).abs() < 1e-4, "level = {l}");
        // Below the -60 dBFS floor → clamped empty.
        assert_eq!(level_from_rms(0.0001), 0.0);
    }

    #[test]
    fn published_sys_level_round_trips_and_clamps() {
        publish_sys_level(0.42);
        let l = read_meter_level(AudioChannel::Sys);
        assert!((l - 0.42).abs() < 1e-6, "level = {l}");
        publish_sys_level(7.0); // clamped into the meter scale
        assert_eq!(read_meter_level(AudioChannel::Sys), 1.0);
        publish_sys_level(0.0);
        assert_eq!(read_meter_level(AudioChannel::Sys), 0.0);
    }
}
