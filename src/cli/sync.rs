//! The `--make-sync-clip` / `--calibrate-sync` subcommands (DRAGON-119): a
//! one-time, end-to-end A/V-sync calibration the in-app auto-calibration cannot
//! do (it only sees frame→encoder lag, not the compositor's delivery lag).
//! Measurement + clip generation live in [`crate::record`] (`sync_probe`); this
//! module is the argument handling, the printout, and the settings write.

use std::path::{Path, PathBuf};

/// The 4-step user workflow, printed by `--help` and by both subcommands.
pub const SYNC_WORKFLOW: &str = "\
A/V sync check (recordings already compensate for the audio device's latency
automatically; this VERIFIES end-to-end sync and offers a manual override for
exotic setups):
    1. cosmic-capture-kit --make-sync-clip      write the reference clip
    2. Play the clip in any video player, with system audio audible
    3. Record it with a normal capture (region around the player; system audio ON)
    4. cosmic-capture-kit --calibrate-sync <recording.mp4> --apply   (manual override)";

/// `--make-sync-clip [PATH]`: write the flash/beep reference clip (default name
/// `cck-sync-reference.mp4` in the user's record dir, else the cwd), then verify
/// it by measuring our own output — a clip whose flashes and beeps don't measure
/// as simultaneous would calibrate garbage, so that's a hard error.
pub fn make_sync_clip(path_arg: Option<&str>) {
    let out = sync_clip_path(path_arg);
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = crate::record::write_sync_clip(&out) {
        eprintln!("--make-sync-clip: {e}");
        std::process::exit(1);
    }
    // Self-check: the generator and the analyzer must agree the clip is aligned.
    match crate::record::measure_av_offset(&out) {
        Ok(m) if m.confident && m.offset_secs.abs() <= 0.025 => {
            println!("Wrote the A/V-sync reference clip: {}", out.display());
            println!(
                "  self-check: flash/beep offset {:+.1} ms over {} pairs: OK",
                m.offset_secs * 1000.0,
                m.pairs
            );
            println!();
            println!("{SYNC_WORKFLOW}");
        }
        Ok(m) => {
            let _ = std::fs::remove_file(&out);
            eprintln!(
                "--make-sync-clip: self-check failed. The clip measured {:+.1} ms \
                 (spread {:.1} ms, {} pairs{}); removed it. This is a tool bug or a \
                 broken ffmpeg build, not something a re-run fixes.",
                m.offset_secs * 1000.0,
                m.spread_secs * 1000.0,
                m.pairs,
                if m.confident { "" } else { ", low confidence" },
            );
            std::process::exit(1);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            eprintln!("--make-sync-clip: self-check failed ({e}); removed the clip.");
            std::process::exit(1);
        }
    }
}

/// Where the reference clip goes: an explicit PATH (a directory gets the default
/// name inside it), else the user's record dir (created if needed), else the cwd.
fn sync_clip_path(arg: Option<&str>) -> PathBuf {
    if let Some(p) = arg {
        let p = crate::util::expand_tilde(p);
        return if p.is_dir() { p.join(crate::record::SYNC_CLIP_NAME) } else { p };
    }
    let dir = crate::util::expand_tilde(&crate::state::load().record_dir);
    let dir = if !dir.as_os_str().is_empty() && std::fs::create_dir_all(&dir).is_ok() {
        dir
    } else {
        PathBuf::from(".")
    };
    dir.join(crate::record::SYNC_CLIP_NAME)
}

/// `--calibrate-sync <file> [--apply]`: measure a recording of the reference clip
/// and print the true offset; with `--apply` (and a confident measurement),
/// persist it into the settings.
pub fn calibrate_sync(file: Option<&Path>, apply: bool) {
    let Some(file) = file else {
        eprintln!(
            "--calibrate-sync: expected a recording file\n\
             usage: cosmic-capture-kit --calibrate-sync <recording.mp4> [--apply]"
        );
        std::process::exit(2);
    };
    if !file.exists() {
        eprintln!("--calibrate-sync: file not found: {}", file.display());
        std::process::exit(1);
    }
    let m = match crate::record::measure_av_offset(file) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("--calibrate-sync: {e}");
            std::process::exit(1);
        }
    };
    let t_ms = (m.offset_secs * 1000.0).round() as i32;
    println!("A/V sync measurement for {}:", file.display());
    println!("  offset : {t_ms:+} ms  (positive = audio leads video = the compensation to store)");
    println!("  spread : {:.0} ms over {} flash/beep pairs", m.spread_secs * 1000.0, m.pairs);
    println!("  confidence: {}", if m.confident { "OK" } else { "LOW (spread > 120 ms)" });
    println!(
        "  note: recordings made from now on already compensate for the audio device's\n\
         \x20       latency automatically (DRAGON-119), so measuring an OLD recording and\n\
         \x20       applying an offset is usually unnecessary. Use this to VERIFY sync, and\n\
         \x20       --apply only for a stubborn residual on an exotic setup."
    );
    if !m.confident {
        eprintln!(
            "\nNot applying a low-confidence measurement. Re-record with the capture \
             region tight around the player and other sounds quiet, then try again."
        );
        std::process::exit(1);
    }
    let p = crate::state::load();
    let old_base = p.av_calibration_base_ms.clamp(-2000, 2000);
    let (new_offset, new_base) = apply_values(t_ms, p.audio_sync_offset_ms, old_base);
    if apply {
        let mut p = p;
        println!("\nApplied:");
        println!("  audio_sync_offset_ms  : {} -> {}", p.audio_sync_offset_ms, new_offset);
        println!("  av_calibration_base_ms: {old_base} -> {new_base}");
        if !p.audio_sync_auto {
            println!(
                "  note: auto A/V sync is OFF, so this offset stays fixed until you \
                 re-calibrate (or turn auto sync back on)."
            );
        }
        p.audio_sync_offset_ms = new_offset;
        p.av_calibration_base_ms = new_base;
        crate::state::save(&p);
    } else {
        println!(
            "\nWould store: audio_sync_offset_ms {} -> {}, av_calibration_base_ms {} -> {}",
            p.audio_sync_offset_ms, new_offset, old_base, new_base
        );
        println!("Re-run with --apply to persist.");
    }
}

/// The apply math. `t_ms` is the end-to-end truth this calibration measured. The
/// auto-calibration persists `raw_median + base` after each recording, so what it
/// last MEASURED is `audio_sync_offset_ms − av_calibration_base_ms`; the new base
/// is whatever tops that raw median up to the truth (`T − raw_median`), and the
/// offset itself becomes `T`. Future auto-cal runs then keep landing on ~T while
/// still tracking real changes in the app-visible latency.
fn apply_values(t_ms: i32, cur_offset_ms: i32, cur_base_ms: i32) -> (i32, i32) {
    let raw_median = cur_offset_ms - cur_base_ms;
    (t_ms.clamp(-1000, 1000), (t_ms - raw_median).clamp(-2000, 2000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_tops_the_auto_median_up_to_the_truth() {
        // Auto-cal had persisted 42 (base 0, so raw median 42); the clip measured
        // 302 → base becomes the unobservable 260, offset the full 302.
        assert_eq!(apply_values(302, 42, 0), (302, 260));
    }

    #[test]
    fn apply_is_idempotent_once_applied() {
        // Re-measuring right after an apply (nothing changed): same values back.
        assert_eq!(apply_values(302, 302, 260), (302, 260));
    }

    #[test]
    fn apply_handles_a_negative_truth() {
        // Audio LAGS video (flash before beep): offset goes negative.
        assert_eq!(apply_values(-300, 42, 0), (-300, -342));
    }

    #[test]
    fn apply_clamps_both_fields() {
        assert_eq!(apply_values(1500, 0, 0), (1000, 1500));
        assert_eq!(apply_values(-2500, 0, 0), (-1000, -2000));
    }
}
