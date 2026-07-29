//! LIVE proof that a recording keeps its OPENING, driven through the REAL worker
//! (DRAGON-421). `#[ignore]`-gated: it needs a real COSMIC/Wayland session, a
//! PulseAudio-compatible server, ffmpeg/ffprobe, `espeak-ng`, and a local
//! faster-whisper install. Run explicitly on the box:
//!
//! ```text
//! WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
//!   cargo test --release -- --ignored --nocapture --test-threads=1 live_recording_opening
//! ```
//!
//! ## Why this exists, when `media_clock_e2e_tests` already covers the opening
//!
//! `media_clock_recording_opening_survives_a_slow_start_e2e` drives
//! `run_owned_shaped_session` — a harness that reproduces the pump's SHAPE. It passed
//! green through a regression that made the app itself lose the opening again, because
//! the thing that broke was not the shape: it was what the real worker's real capture
//! sources do to the pump. A harness that mimics the subject cannot fail for the reasons
//! the subject fails. So this one drives `start_region_recording` — the same public
//! entry the app calls — and reads the finished file the way the user does: by
//! LISTENING to it.
//!
//! ## Verified by CONTENT, never by duration
//!
//! Duration, packet counts and RMS all stayed healthy through the DRAGON-417 regression
//! while the opening was being thrown away, and cost this project a day of chasing the
//! wrong thing. The only admissible evidence is what was SAID:
//!
//! - A spoken count ("one", "two", … ) is played into a disposable null sink, one word
//!   per fixed slot, starting at the instant the recording starts. Word *k* is therefore
//!   spoken during media second *k−1* — the word IS its own timestamp.
//! - The recording's system channel is pointed at that sink (`CCK_TEST_MONITOR_SOURCE`).
//! - The finished file is transcribed with word timestamps (faster-whisper) and the
//!   words are matched back to their slots.
//!
//! A lost opening reads as the leading words simply not being in the transcript — the
//! exact, unmistakable signature of the field failure. A recording that fails outright
//! reads as an empty transcript (or a worker error), which is the OTHER failure shape
//! this net has to catch.

use super::{RecordSettings, RegionRecordParams, start_region_recording};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The spoken count, index 0 = the word for media slot 0. Small numbers only: they are
/// short, phonetically distinct, and transcribe reliably at any model size.
const WORDS: [&str; 12] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve",
];

/// Seconds of media time each word owns. One second is comfortably longer than any of
/// the spoken words, so each lands cleanly inside its own slot with silence after it.
const SLOT_SECS: f64 = 1.0;

/// How many leading words make up "the opening" for the strict assertion. Comfortably
/// longer than any video-side startup measured on this box (0.4-1.4s), so a recording that
/// anchored media 0 at video-ready instead of audio-capture start loses at least one of
/// them — and four consecutive words cannot all be a mis-transcription.
const OPENING_WORDS: usize = 4;

/// Local faster-whisper interpreter (see the module doc). Absent = loud skip.
const WHISPER_PY: &str = "/home/frosthaven/.cck-tools/whisper-venv/bin/python";

/// Serializes live recordings against each other: they claim fixed-name pactl modules
/// and mutate `CCK_TEST_MONITOR_SOURCE`, which is process-global.
fn live_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Clears the process-global test overrides on drop, including during a panic's unwind.
struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: only constructed while holding `live_lock()`, held for this guard's
        // whole lifetime — no concurrent env access.
        unsafe {
            std::env::remove_var("CCK_TEST_MONITOR_SOURCE");
        }
        crate::audio::config::set_mic_source("");
    }
}

/// A pactl `module-null-sink`, unloaded on drop (mirrors `media_clock_e2e_tests`).
struct NullSink {
    module_id: String,
    name: String,
}

impl NullSink {
    fn load(name: &str) -> Option<Self> {
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={name}"),
                &format!("sink_properties=device.description={name}"),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let module_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if module_id.is_empty() {
            return None;
        }
        Some(Self { module_id, name: name.to_string() })
    }

    fn monitor_source(&self) -> String {
        format!("{}.monitor", self.name)
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl")
            .args(["unload-module", &self.module_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn tool_responds(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Every external dependency this proof needs, checked up front so a missing one skips
/// LOUDLY instead of failing as if the recorder were broken.
fn have_tools() -> Result<(), String> {
    let mut ff = crate::util::ffmpeg_command();
    ff.arg("-version");
    if !tool_responds(ff) {
        return Err("ffmpeg".into());
    }
    let mut fp = crate::util::ffprobe_command();
    fp.arg("-version");
    if !tool_responds(fp) {
        return Err("ffprobe".into());
    }
    let mut pactl = Command::new("pactl");
    pactl.arg("info");
    if !tool_responds(pactl) {
        return Err("pactl (no Pulse-compatible server)".into());
    }
    let mut espeak = Command::new("espeak-ng");
    espeak.arg("--version");
    if !tool_responds(espeak) {
        return Err("espeak-ng".into());
    }
    if !Path::new(WHISPER_PY).exists() {
        return Err(format!("faster-whisper interpreter at {WHISPER_PY}"));
    }
    Ok(())
}

/// Render [`WORDS`] to one 48 kHz mono wav in which word *k* starts at exactly
/// `k * SLOT_SECS` — so a transcript word's time IS the media second it was recorded
/// at, with no calibration step and nothing for a clock skew to corrupt.
fn build_count_wav(dir: &Path) -> Option<PathBuf> {
    let mut inputs: Vec<PathBuf> = Vec::new();
    for (i, w) in WORDS.iter().enumerate() {
        let raw = dir.join(format!("cck-word-{i}.wav"));
        let ok = Command::new("espeak-ng")
            .args(["-v", "en-us", "-s", "150", "-a", "200", "-w"])
            .arg(&raw)
            .arg(*w)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
        // Pad (or trim) the word to EXACTLY one slot, at the mixer's rate, so slot k
        // begins at k*SLOT_SECS by construction rather than by measurement.
        let slot = dir.join(format!("cck-slot-{i}.wav"));
        let ok = crate::util::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&raw)
            .args([
                "-af",
                &format!("aresample=48000,apad,atrim=0:{SLOT_SECS}"),
                "-ac", "1", "-ar", "48000",
            ])
            .arg(&slot)
            .stdin(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
        inputs.push(slot);
    }
    let list = dir.join("cck-count-list.txt");
    let body: String =
        inputs.iter().map(|p| format!("file '{}'\n", p.display())).collect::<String>();
    std::fs::write(&list, body).ok()?;
    let out = dir.join("cck-count.wav");
    let ok = crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-ac", "1", "-ar", "48000"])
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    Some(out)
}

/// Play `wav` into `sink` in real time, killed on drop.
struct SpeechPlayer(std::process::Child);
impl SpeechPlayer {
    fn start(wav: &Path, sink: &str) -> Option<Self> {
        let child = crate::util::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error", "-re", "-i"])
            .arg(wav)
            .args(["-f", "pulse", "-device", sink, "cck-opening-speech"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Self(child))
    }
}
impl Drop for SpeechPlayer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One transcribed word and the media second it was heard at.
#[derive(Debug, Clone)]
struct HeardWord {
    text: String,
    start: f64,
}

/// Transcribe `media`'s audio with word timestamps. Returns the words in order.
fn transcribe(media: &Path, dir: &Path) -> Vec<HeardWord> {
    // Decode to the 16 kHz mono wav whisper wants; done here (not in python) so the
    // helper stays dependency-free beyond faster-whisper itself.
    let wav = dir.join("cck-transcribe.wav");
    let ok = crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(media)
        .args(["-vn", "-ac", "1", "-ar", "16000"])
        .arg(&wav)
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "could not decode the recording's audio for transcription");
    let script = dir.join("cck-transcribe.py");
    std::fs::write(
        &script,
        r#"import sys
from faster_whisper import WhisperModel
m = WhisperModel("base.en", device="cpu", compute_type="int8")
segs, _ = m.transcribe(sys.argv[1], word_timestamps=True, beam_size=5)
for s in segs:
    for w in (s.words or []):
        print("%.3f\t%s" % (w.start, w.word.strip()))
"#,
    )
    .expect("write transcription helper");
    let out = Command::new(WHISPER_PY)
        .arg(&script)
        .arg(&wav)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .expect("faster-whisper failed to run");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (t, w) = l.split_once('\t')?;
            Some(HeardWord {
                start: t.trim().parse().ok()?,
                text: w.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase(),
            })
        })
        .filter(|w| !w.text.is_empty())
        .collect()
}

/// The first enumerated Wayland output's `(logical_pos, logical_size)`.
fn first_output() -> Option<((i32, i32), (i32, i32))> {
    let (_conn, _queue, data) = crate::screencopy::connect(false)?;
    crate::screencopy::outputs(&data).into_iter().next().map(|(_, _, pos, size)| (pos, size))
}

/// Which [`WORDS`] slots survived into the recording, as slot indices, in the order
/// they were heard. A word is credited to a slot only if the transcript actually says
/// it; timing is REPORTED but never used to decide presence (whisper's word times drift
/// by a few hundred ms and must not be load-bearing).
fn slots_heard(heard: &[HeardWord]) -> Vec<usize> {
    heard.iter().filter_map(|h| slot_of(&h.text)).collect()
}

/// Which slot a transcribed token names, if any. Whisper renders a spoken count as
/// DIGITS as readily as words ("1" and "one" both appear, sometimes in the same
/// transcript), so both spellings must count — a matcher that only knew the words would
/// read a perfectly intact recording as totally silent.
fn slot_of(token: &str) -> Option<usize> {
    if let Some(i) = WORDS.iter().position(|w| *w == token) {
        return Some(i);
    }
    token.parse::<usize>().ok().filter(|n| (1..=WORDS.len()).contains(n)).map(|n| n - 1)
}

/// Which capture channel the spoken count is played into. Both are recorded either way
/// (the app's normal configuration); only the channel under test carries the words, so a
/// missing word can be attributed to ONE track instead of a mix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CountOn {
    /// The mic chain — the deep path: `audio::input`'s DSP, the 2.5s-deep source
    /// channel, and the track whose real-world failure DRAGON-411 was about.
    Mic,
    /// The system-audio monitor — the shallow path, plus the device-latency latch that
    /// holds its opening chunks back until it resolves.
    System,
}

/// The recorder configuration a proof runs under. The defaults a user actually ships
/// with matter here: the GPU zero-copy path runs its OWN audio pre-flight alongside the
/// CPU path's, and mic-only sessions leave the system-latency latch to resolve with no
/// system audio flowing — both are startup shapes the CPU/both-channels default never
/// visits.
#[derive(Clone, Copy)]
struct Shape {
    zero_copy: bool,
    system_audio: bool,
    encoder: &'static str,
    /// The persisted manual A/V offset (ms). Non-zero shifts every tap's audible time,
    /// so it is part of the placement math the opening depends on.
    audio_offset_ms: i32,
    /// The persisted downscale cap (`(0, 0)` = none). A capped 1080p encode of a larger
    /// output is a different, heavier video-side startup than a raw passthrough.
    max_res: (u32, u32),
}

impl Shape {
    /// The suite's neutral shape: CPU software encode, both channels, no manual offset.
    fn plain() -> Self {
        Self {
            zero_copy: false,
            system_audio: true,
            encoder: "software",
            audio_offset_ms: 0,
            max_res: (0, 0),
        }
    }
}

/// THE net for the DRAGON-417 field failure and its DRAGON-421 return: the app's own
/// entry point records a spoken count that starts when the recording starts, and every
/// word must be in the file.
///
/// It fails in two distinct, self-describing ways:
///
/// - **Opening lost** — the leading numbers are missing from the transcript. This is the
///   regression: media 0 is anchored later than audio capture actually began, so the
///   recording starts partway through the count.
/// - **Recording failed outright** — the worker returns an error, or the transcript is
///   empty because no audio reached the file at all. Both were seen in the field
///   alongside orphaned ffmpeg processes.
fn run_opening_proof(count_on: CountOn, shape: Shape) {
    let _lock = live_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Install a logger, once, so the pump's recording-health line
    // (`late=/lost=/gap=`) reaches this test's output. A failure here should say what the
    // recorder itself thought happened, not just that words went missing. `env_logger`
    // directly rather than `crate::diag`, so this file also applies cleanly to trees from
    // before the debug log existed (bisecting is exactly what it is for).
    static LOGGER: OnceLock<()> = OnceLock::new();
    LOGGER.get_or_init(|| {
        let _ = env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("info,naga=warn,wgpu=warn,zbus=warn"),
        )
        .try_init();
    });
    if let Err(missing) = have_tools() {
        eprintln!(
            "SKIP (loud): {missing} unavailable — the live recording-opening proof did NOT run"
        );
        return;
    }
    let Some((pos, size)) = first_output() else {
        eprintln!(
            "SKIP (loud): no Wayland output reachable (set WAYLAND_DISPLAY + XDG_RUNTIME_DIR to \
             the live COSMIC session) — the live recording-opening proof did NOT run"
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!("cck-opening-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let Some(count_wav) = build_count_wav(&dir) else {
        eprintln!("SKIP (loud): could not synthesize the spoken count — the proof did NOT run");
        return;
    };
    // Two disposable sinks, so the channel NOT under test is captured from a device we
    // also control (and which is silent) rather than from the user's real hardware.
    let (Some(speech_sink), Some(quiet_sink)) =
        (NullSink::load("cck_opening_speech"), NullSink::load("cck_opening_quiet"))
    else {
        eprintln!("SKIP (loud): could not load the null sinks — the proof did NOT run");
        return;
    };
    let _env = EnvGuard;
    let (mic_src, sys_src) = match count_on {
        CountOn::Mic => (speech_sink.monitor_source(), quiet_sink.monitor_source()),
        CountOn::System => (quiet_sink.monitor_source(), speech_sink.monitor_source()),
    };
    // SAFETY: `live_lock()` is held for the whole test; nothing else reads env here.
    unsafe {
        std::env::set_var("CCK_TEST_MONITOR_SOURCE", &sys_src);
    }
    crate::audio::config::set_mic_source(&mic_src);

    let out_path = dir.join("opening.mkv");
    let _ = std::fs::remove_file(&out_path);
    // The WHOLE output, which is what a user records and the largest encoder load the
    // path faces. Recording a postage-stamp region instead makes the video side start
    // far faster than it really does, and the opening span is exactly what that startup
    // costs — a small region hides the very thing under test.
    let (rw, rh) = (size.0.max(2) as u32, size.1.max(2) as u32);
    let params = RegionRecordParams {
        x: pos.0,
        y: pos.1,
        w: rw,
        h: rh,
        cursor: false,
        settings: RecordSettings {
            fps: 30,
            preferred_encoder: shape.encoder.to_string(),
            presets: crate::encode::Presets::default(),
            zero_copy: shape.zero_copy,
            // The MIC is always on — it is the deep source path, and the app's default.
            // Only the channel under test carries speech (see [`CountOn`]).
            mic: true,
            system_audio: shape.system_audio,
            bitrate_kbps: 8000,
            audio_offset_ms: shape.audio_offset_ms,
            max_res: shape.max_res,
            auto_device_compensation: true,
            metadata: String::new(),
            out_path: out_path.clone(),
        },
    };

    // Start the recording, then the speech, in that order and back to back: media 0 is
    // the instant audio capture begins (inside the worker, within a millisecond of this
    // call), so the count begins a hair AFTER media 0 and every word belongs in the
    // file. Nothing here waits for a first frame — waiting for one would hide exactly
    // the video-side startup span the opening is lost in.
    let t_start = Instant::now();
    let handle = start_region_recording(params);
    let player = SpeechPlayer::start(&count_wav, &speech_sink.name);
    assert!(player.is_some(), "could not start the speech player");
    eprintln!(
        "LIVE opening[{count_on:?}]: recording started, speech playing ({} words)",
        WORDS.len()
    );

    let record_secs = WORDS.len() as f64 * SLOT_SECS + 1.5;
    let deadline = t_start + Duration::from_secs_f64(record_secs);
    while Instant::now() < deadline {
        if let Some(Err(e)) = handle.done.lock().unwrap().clone() {
            panic!(
                "recording FAILED before it finished: {e}\n(this is the outright-failure shape: \
                 no audio in the file, ffmpeg left blocked on FIFOs nobody writes)"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    handle.stop.store(true, Ordering::Relaxed);
    let reap = Instant::now() + Duration::from_secs(60);
    let result = loop {
        if let Some(r) = handle.done.lock().unwrap().clone() {
            break r;
        }
        assert!(Instant::now() < reap, "recording never finished (worker hung?)");
        std::thread::sleep(Duration::from_millis(200));
    };
    drop(player);
    let path = result.unwrap_or_else(|e| panic!("recording FAILED: {e}"));

    let heard = transcribe(&path, &dir);
    eprintln!(
        "LIVE opening[{count_on:?}]: transcript = {}",
        heard.iter().map(|h| format!("{:.2}:{}", h.start, h.text)).collect::<Vec<_>>().join(" ")
    );
    let slots = slots_heard(&heard);
    let _ = std::fs::remove_file(&path);

    assert!(
        !slots.is_empty(),
        "[{count_on:?}] the recording contains NO recognizable speech at all — that channel \
         captured nothing (recording failed outright, the second field shape)"
    );
    // THE claim: the recording opens where the count opens. A lost opening is a missing
    // leading RUN, so both halves are asserted — the very first word, and the whole opening
    // run behind it (one word could be a mis-transcription; four in a row cannot).
    assert_eq!(
        slots.first(),
        Some(&0),
        "[{count_on:?}] the recording starts at \"{}\", not \"{}\" — its opening was thrown \
         away (heard slots {slots:?}). This is the DRAGON-417 failure: media 0 anchored \
         after audio capture really began.",
        WORDS[*slots.first().unwrap_or(&0)],
        WORDS[0],
    );
    let missing_opening: Vec<&str> =
        (0..OPENING_WORDS).filter(|k| !slots.contains(k)).map(|k| WORDS[k]).collect();
    assert!(
        missing_opening.is_empty(),
        "[{count_on:?}] the recording's OPENING is incomplete — missing {missing_opening:?} \
         from the first {OPENING_WORDS} words (heard slots {slots:?})"
    );
    // And the rest of the recording has to be there too, or "the opening survived" would be
    // satisfied by a file that keeps its first second and drops everything after. One
    // tolerated absence, because the judge is a speech recogniser: a trailing word can be
    // clipped by where `stop` lands relative to the last syllable, and `base.en` does
    // occasionally drop one. Two or more missing is a real hole and fails — as does ANY
    // missing word inside the opening, which the assertions above have already covered.
    let missing: Vec<&str> =
        (0..WORDS.len()).filter(|k| !slots.contains(k)).map(|k| WORDS[k]).collect();
    assert!(
        missing.len() <= 1,
        "[{count_on:?}] {} of {} words are missing from the recording: {missing:?} (heard \
         slots {slots:?}) — that is a hole in the audio, not a transcription miss",
        missing.len(),
        WORDS.len(),
    );
    if let [only] = missing[..] {
        eprintln!("LIVE opening[{count_on:?}]: tolerated one missing word ({only})");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The opening proof on the MIC channel — the deep path (DSP chain, 2.5s source channel)
/// and the track DRAGON-411's field failure was measured on.
#[test]
#[ignore = "live: needs a COSMIC session, a Pulse server, ffmpeg/espeak-ng/faster-whisper"]
fn live_recording_opening_keeps_every_spoken_word_on_the_mic() {
    run_opening_proof(CountOn::Mic, Shape::plain());
}

/// The opening proof in the shape a REAL user records in (DRAGON-421): the GPU zero-copy
/// worker, a hardware encoder, and MIC-ONLY audio. That combination is not a stylistic
/// variation — it is a different startup: the zero-copy attempt runs a SECOND audio
/// pre-flight beside the CPU path's, and with no system audio selected the pump's
/// device-latency latch resolves on an idle monitor. The field regressions this file
/// exists for were both reported from exactly this configuration, and the plain shape
/// above passed straight through them.
#[test]
#[ignore = "live: needs a COSMIC session, a Pulse server, ffmpeg/espeak-ng/faster-whisper"]
fn live_recording_opening_survives_the_zero_copy_mic_only_shape() {
    run_opening_proof(CountOn::Mic, Shape {
        zero_copy: true,
        system_audio: false,
        encoder: "auto",
        audio_offset_ms: 42,
        max_res: (1920, 1080),
    });
}

/// The opening proof on the SYSTEM channel — the path whose opening chunks are held back
/// by the device-latency latch until it resolves.
#[test]
#[ignore = "live: needs a COSMIC session, a Pulse server, ffmpeg/espeak-ng/faster-whisper"]
fn live_recording_opening_keeps_every_spoken_word_on_system_audio() {
    run_opening_proof(CountOn::System, Shape::plain());
}
