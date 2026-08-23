//! DRAGON-712: the owned stop tail, end to end, against the REAL shipped ffmpeg argv.
//!
//! ## Why this module exists
//!
//! A recording was lost to a stop that never finished, and every existing net looked
//! straight past it. `media_clock_e2e_tests` is `cfg(target_os = "linux")`, so macOS runs
//! none of it. `wedge_live_tests` build their OWN ffmpeg invocation (lavfi plus libx264,
//! no audio FIFOs), so they never touch what the recording muxer's argv actually says.
//! What was left covering the shipped command on macOS was one unit test on its argv
//! SHAPE, and the shape was fine: the defect was in how the pipeline BEHAVES when the two
//! sides of it wind down.
//!
//! So this drives the real thing: [`crate::encode::spawn_ffmpeg_media_clock`], the same
//! call the four owned workers make, against real FIFOs carrying real f32 audio, with the
//! audio fed at the render horizon the pump actually uses, through the real
//! [`super::owned::run_video_stop_tail`]. It is unix (Linux plus macOS) rather than
//! Linux-only on purpose, because "the regression net is Linux-only" is the reason this
//! bug shipped.
//!
//! ## The shape it pins
//!
//! `pump::RENDER_LAG_SECS` means the audio ffmpeg has been given always trails the video
//! by 1.5s, so a stop must push 1.5s of audio into a muxer whose video has already ended.
//! Measured headlessly (see `SessionVideoSink`'s doc): ffmpeg accepts about 0.2s of that
//! and then accepts NOTHING more, on every encoder, container and `-max_interleave_delta`
//! tried, until the video stdin gets its EOF. That is why the assertions here are about
//! the stop COMPLETING and about ffmpeg exiting cleanly, not about byte counts: the
//! failure mode is a stop that never ends.
//!
//! Skips LOUDLY without ffmpeg, like `av_sync_tests`.

use super::owned::{SessionVideoSink, run_video_stop_tail};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
/// Session length before the stop. Long enough that the render horizon is fully open (so
/// the stop really does have to push `RENDER_LAG_SECS` of audio past the video), short
/// enough to keep the suite quick.
const SESSION_SECS: f64 = 3.0;
/// What the pump's horizon holds back, restated here because the test has to feed audio
/// the way the pump does for the stop to have the shape the bug needed.
const RENDER_LAG_SECS: f64 = 1.5;
const SAMPLE_RATE: usize = 48_000;

fn have_ffmpeg() -> bool {
    let responds = |tool: PathBuf| {
        crate::util::quiet_command(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    responds(crate::util::ffmpeg_path())
}

/// Open a FIFO's write end the way `pump::open_fifo_write_end` does: retry the
/// non-blocking open until ffmpeg's read side shows up, then clear `O_NONBLOCK` so writes
/// block and carry natural backpressure.
fn open_fifo_write(path: &std::path::Path, budget: Duration) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if let Ok(f) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            use std::os::fd::AsFd;
            if let Ok(flags) = rustix::fs::fcntl_getfl(f.as_fd()) {
                let _ = rustix::fs::fcntl_setfl(f.as_fd(), flags - rustix::fs::OFlags::NONBLOCK);
            }
            return Some(f);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// One audio track fed at the pump's render horizon: an opening prime so ffmpeg's input
/// open never waits, then samples up to `now - RENDER_LAG_SECS`, then, once `stopping` is
/// set, everything up to the session's media end, then EOF by dropping the file.
fn feed_track(
    path: PathBuf,
    channels: usize,
    stopping: &AtomicBool,
    final_media: f64,
) -> Option<usize> {
    let mut f = open_fifo_write(&path, Duration::from_secs(20))?;
    let t0 = Instant::now();
    let mut written = 0usize; // samples PER CHANNEL
    let push = |f: &mut std::fs::File, written: &mut usize, want: usize| -> bool {
        while *written < want {
            let n = (want - *written).min(4096);
            let mut buf = Vec::with_capacity(n * channels * 4);
            for i in 0..(n * channels) {
                let s = 0.05f32 * ((i % 256) as f32 / 256.0 - 0.5);
                buf.extend_from_slice(&s.to_le_bytes());
            }
            if f.write_all(&buf).is_err() {
                return false;
            }
            *written += n;
        }
        true
    };
    // DRAGON-554's opening prime, so ffmpeg's input open never waits on the horizon.
    if !push(&mut f, &mut written, 4104) {
        return Some(written);
    }
    loop {
        if stopping.load(Ordering::Relaxed) {
            let want = (final_media * SAMPLE_RATE as f64) as usize;
            push(&mut f, &mut written, want);
            return Some(written);
        }
        let horizon = t0.elapsed().as_secs_f64() - RENDER_LAG_SECS;
        if horizon > 0.0 {
            let want = (horizon * SAMPLE_RATE as f64) as usize;
            if want > written && !push(&mut f, &mut written, want) {
                return Some(written);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// THE regression test. A whole owned-shaped session through the shipped argv, stopped the
/// way a user stops one, asserting the stop ENDS and ffmpeg reaps cleanly.
///
/// The bound is the thing being pinned. `progress::SESSION_STALL_SECS` (60) is what
/// declares a recording wedged, shows the user a failure alert and leaves them a
/// pre-finalize `-recovered.mkv`; a stop that needs anywhere near it has already failed
/// the user even if it eventually completes. The stop path's own worst case is the abandon
/// grace plus the covering budget plus the bounded reap, so anything past that is a
/// regression.
#[test]
fn owned_stop_tail_finishes_against_the_shipped_argv_e2e() {
    let _guard = super::recording_globals_lock().lock();
    if !have_ffmpeg() {
        eprintln!("SKIP owned_stop_tail_finishes_against_the_shipped_argv_e2e: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join(format!("cck-d712-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let temp = dir.join("take.mkv");
    let mic_fifo = dir.join("mic.pcm");
    let sys_fifo = dir.join("sys.pcm");
    let _ = std::fs::remove_file(&mic_fifo);
    let _ = std::fs::remove_file(&sys_fifo);
    assert!(super::owned::mkfifo(&mic_fifo), "mkfifo mic");
    assert!(super::owned::mkfifo(&sys_fifo), "mkfifo sys");

    let presets = crate::encode::Presets::default();
    let plan = crate::encode::EncodePlan::resolve("software", W, H, &presets);
    let mut child = crate::encode::spawn_ffmpeg_media_clock(
        W, H, W, H, FPS, &plan, 4000, &temp, &mic_fifo, &sys_fifo,
    )
    .expect("spawn the shipped recording muxer");
    let raw_stdin = child.stdin.take().expect("piped stdin");

    let stop = AtomicBool::new(false);
    let final_media = SESSION_SECS;
    let started = Instant::now();

    let outcome = std::thread::scope(|scope| {
        let mic_path = mic_fifo.clone();
        let sys_path = sys_fifo.clone();
        let mic = scope.spawn(|| feed_track(mic_path, 1, &stop, final_media));
        let sys = scope.spawn(|| feed_track(sys_path, 2, &stop, final_media));

        // The video side, shaped like every owned worker's capture loop: the abandonable
        // sink, CFR pacing, one frame per due tick.
        let mut stdin = SessionVideoSink::new(raw_stdin, &stop);
        let mut write_frame = super::owned::make_frame_writer(W, H, plan.nv12);
        let px = vec![0x40u8; (W * H * 4) as usize];
        let mut emitted: u32 = 0;
        let vt0 = Instant::now();
        while vt0.elapsed().as_secs_f64() < SESSION_SECS {
            let due = (vt0.elapsed().as_secs_f64() * FPS as f64) as u32;
            if due <= emitted {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            let owed = due - emitted;
            for _ in 0..owed {
                if !write_frame(W, H, &px, &mut stdin) {
                    break;
                }
                emitted += 1;
            }
        }
        // The user's stop. Both sides observe it through the same flag the workers use.
        stop.store(true, Ordering::Relaxed);
        let truncated = stdin.truncated();
        let stdin = stdin.into_stdin();
        // Cover to the audio's media end, exactly as the workers do.
        let want = (final_media * FPS as f64).ceil() as u32;
        let more = want.saturating_sub(emitted);
        let outcome = run_video_stop_tail(stdin, &mut child, &temp, false, truncated, more, |sd| {
            write_frame(W, H, &px, sd)
        });
        let _ = mic.join();
        let _ = sys.join();
        outcome
    });

    let took = started.elapsed();
    let _ = std::fs::remove_file(&mic_fifo);
    let _ = std::fs::remove_file(&sys_fifo);
    let size = std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(outcome.is_ok(), "the stop tail must salvage or succeed, got {outcome:?}");
    assert!(size > 4096, "the muxer must have written a real file, got {size} bytes");
    // The bound this ticket exists for. Before the fix a stop in this shape ran until
    // `progress::SESSION_STALL_SECS` (60) killed the session.
    let ceiling = Duration::from_secs_f64(SESSION_SECS)
        + super::owned::STOP_ABANDON_GRACE
        + super::owned::COVERING_WRITE_BUDGET
        + super::owned::STOP_TAIL_REAP;
    assert!(
        took < ceiling,
        "the session plus its whole stop path must fit inside {ceiling:?}, took {took:?}"
    );
    assert!(
        took.as_secs() < super::progress::SESSION_STALL_SECS,
        "a stop that reaches the session stall bound is reported to the user as a wedged \
         recording; took {took:?}"
    );
}
