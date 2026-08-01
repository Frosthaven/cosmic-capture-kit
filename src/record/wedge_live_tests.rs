//! LIVE proof of the session-level bound (DRAGON-423): the wedge is CAUSED here, with real
//! ffmpeg processes and real FIFOs, and then the whole response is run against it.
//!
//! The unit tests in [`super::progress`] and [`super::recover`] cover the decision and the
//! teardown separately, at test speed, with stand-ins. They cannot show the thing the ticket
//! is actually about: that a session which has genuinely stopped getting anywhere is
//! recognised as such and really does end, with nothing of it left running. So this module
//! reproduces the field failure from `~/.cck-evidence/wedge-1741` as closely as a test can:
//!
//! * an ffmpeg that never gets past opening its mic FIFO, because nobody ever writes to it —
//!   the exact wedge the evidence names ("mic FIFO write end never opened (wedged ffmpeg?)"),
//!   and the one whose in-process bounds all fired correctly while the session hung anyway;
//! * a SECOND ffmpeg still muxing away into the take, which is what the fallback left behind
//!   after it cleared the user's stop — the reason "the temp is growing" is not the same
//!   thing as "the session is doing what it was told";
//! * the ledger both of them are recorded in, which is what makes a complete teardown
//!   possible at all.
//!
//! Then: the bound is fed real observations of those real files until it declares the
//! session wedged, the teardown is run, and the four things the ticket asks for are checked
//! by looking at the system rather than at our own return values — the processes are gone,
//! the FIFOs are gone, the take is on disk under a name the user can find, and nothing is
//! still writing.
//!
//! ffmpeg is the one tool CLAUDE.md's "no ffmpeg in tests" rule excludes for verification
//! that can only be done by causing the thing (as `av_sync_tests` already is). This module
//! LOUDLY skips when ffmpeg is absent — never a silent pass — and runs wherever it is
//! present. Unix-only: it is built on FIFOs and signals.

use super::progress::{Phase, Sample, SessionProgress, SESSION_STALL_SECS};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// The short budget the bound runs on here, so a test does not sit through the real 60s.
/// Everything else about the run is real: real files, real clock, real processes. Only used
/// where the granularity of the signal cannot matter — a stop nothing is progressing
/// towards, and a pause whose budget is frozen outright.
const TEST_BUDGET: Duration = Duration::from_secs(2);

/// How long a live recording is watched in the must-not-fire proof, and the budget the bound
/// runs on while it is. Both are a QUARTER of the shipped figures (`SESSION_STALL_SECS` and
/// a 60s watch), so the test keeps the real ratio between "how long we wait" and "how coarsely
/// the file advances" without taking a minute to run.
const WATCH_BUDGET: Duration = Duration::from_secs(SESSION_STALL_SECS / 4);
const WATCH_WINDOW: Duration = Duration::from_secs(15);

/// Whether `ffmpeg` responds to `-version`.
fn have_ffmpeg() -> bool {
    crate::util::quiet_command(crate::util::ffmpeg_path())
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A LOUD skip: say why nothing was verified rather than passing quietly.
macro_rules! require_ffmpeg {
    ($name:literal) => {
        if !have_ffmpeg() {
            eprintln!(
                "SKIPPED (loud): {} needs ffmpeg on PATH — the live wedge was not reproduced",
                $name
            );
            return;
        }
    };
}

/// A directory of our own, never the live runtime or capture dir.
fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cck-d423-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A real FIFO, exactly as the audio pre-flight makes them.
/// rustix's `mkfifoat` is unavailable on Apple targets (see `owned::mkfifo`'s doc);
/// this mirrors that split rather than introducing a second one.
fn mkfifo(path: &Path) {
    #[cfg(not(target_os = "macos"))]
    {
        rustix::fs::mkfifoat(rustix::fs::CWD, path, rustix::fs::Mode::from_bits_truncate(0o600))
            .expect("mkfifo");
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("mkfifo path");
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0, "mkfifo");
    }
}

/// THE WEDGE. An ffmpeg with a video source and an audio FIFO nobody will ever write to.
///
/// ffmpeg opens its inputs in order, and a POSIX `open(fifo, O_RDONLY)` blocks until a
/// WRITER appears. So this process parks there forever: it never reaches the video input, it
/// never writes a byte of output, and it never exits. It is alive and it is not a recording.
fn spawn_wedged_muxer(fifo: &Path, temp: &Path) -> Child {
    crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=160x120:rate=30"])
        .args(["-f", "f32le", "-ar", "48000", "-ac", "1"])
        .arg("-i")
        .arg(fifo)
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .arg(temp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg spawn")
}

/// The session nobody was left watching: a real encode, paced to realtime and flushed per
/// packet exactly like the recorder's muxer, writing into the take temp indefinitely.
fn spawn_runaway_muxer(temp: &Path) -> Child {
    spawn_muxer(temp, "testsrc=size=320x240:rate=30", &["-crf", "30"])
}

/// The same thing at the shape and bitrate a real capture has (`-b:v 8000k`, as
/// `encode::plan` picks), which is what makes the growth granularity it exhibits a fair
/// stand-in for a user's recording.
fn spawn_recorder_shaped_muxer(temp: &Path) -> Child {
    spawn_muxer(
        temp,
        "testsrc=size=1280x720:rate=30",
        &["-b:v", "8000k", "-maxrate", "8000k", "-bufsize", "16000k"],
    )
}

fn spawn_muxer(temp: &Path, source: &str, rate: &[&str]) -> Child {
    crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-re", "-f", "lavfi", "-i", source])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(rate)
        // The recorder's own muxer flags: packet-granular flushing into Matroska.
        .args(["-flush_packets", "1", "-f", "matroska"])
        .arg(temp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg spawn")
}

/// Wait (bounded) for `cond`; `false` if it never came true.
fn wait_for(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Whether `child` exited of its own accord within `secs` — i.e. something else killed it.
/// Never kills it itself, because "did the teardown really do this?" is the question.
fn exited_within(child: &mut Child, secs: u64) -> bool {
    wait_for(secs, || child.try_wait().ok().flatten().is_some())
}

/// Reap a stand-in whatever state it is in. Called BEFORE the assertions so a failed one
/// cannot leak an ffmpeg holding the harness's inherited pipes.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The headline case, caused rather than reasoned about.
///
/// A recording whose worker has stopped getting anywhere — one muxer wedged in a FIFO open,
/// another still recording after the user asked to stop — is declared wedged by the
/// session-level bound, and giving up on it leaves nothing running, nothing behind, and the
/// take on disk.
#[test]
fn a_wedged_session_is_ended_its_processes_gone_its_fifos_gone_and_its_take_recovered() {
    require_ffmpeg!("the live wedge proof");
    let dir = tmpdir("live");
    let mic = dir.join("cosmic-capture-kit.4242.0.micmix.pcm");
    let sys = dir.join("cosmic-capture-kit.4242.0.sysmix.pcm");
    mkfifo(&mic);
    mkfifo(&sys);
    let wedged_temp = dir.join(".2026-07-29-17-40-29-056.recording.mkv");
    let take_temp = dir.join(".2026-07-29-17-40-37-000.recording.mkv");
    let out_path = dir.join("2026-07-29-17-40-37-000.mp4"); // finalize would write here

    let mut wedged = spawn_wedged_muxer(&mic, &wedged_temp);
    let mut runaway = spawn_runaway_muxer(&take_temp);

    // Only proceed once the runaway is really a live muxer with a real take in it — this
    // test is worthless if the "recovered" file was never a recording.
    let growing = wait_for(30, || super::muxer_alive(&take_temp));
    // The wedged one, meanwhile, has written nothing at all: it never got past the FIFO.
    let wedged_wrote_nothing = !wedged_temp.exists() || !super::muxer_alive(&wedged_temp);
    // The ledger, in the shape `note_muxer` appends one line-group per muxer spawn.
    let mut body = super::recover::ledger_lines(wedged.id(), &wedged_temp, &[&mic, &sys]);
    body.push_str(&super::recover::ledger_lines(runaway.id(), &take_temp, &[&mic, &sys]));
    let ledger = dir.join("cosmic-capture-kit.4242.session");
    std::fs::write(&ledger, &body).unwrap();

    // ── The bound, fed real observations of those real files ────────────────
    // The user has asked to stop. The take keeps growing, because the session that is
    // writing it never heard about that. Busy is not progress.
    let mut guard = SessionProgress::with_budget(Instant::now(), TEST_BUDGET);
    let started = Instant::now();
    let mut stall = None;
    while started.elapsed() < Duration::from_secs(30) {
        stall = guard.observe(
            Instant::now(),
            Phase::Stopping,
            Sample::read(Some(&take_temp), Some(&out_path)),
        );
        if stall.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // ── Giving up ───────────────────────────────────────────────────────────
    let swept = stall.map(|_| super::recover::abandon_ledger(&ledger, 4242));

    // Did they die because the teardown killed them, or are they still going?
    let wedged_gone = exited_within(&mut wedged, 5);
    let runaway_gone = exited_within(&mut runaway, 5);
    reap(&mut wedged);
    reap(&mut runaway);

    // Whatever the take ended up as, nothing may still be writing to it.
    let recovered = swept.as_ref().and_then(|s| s.recovered.first().cloned());
    let settled = recovered.as_ref().map(|p| {
        let a = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        std::thread::sleep(Duration::from_millis(400));
        let b = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        (a, b)
    });

    let fifos_gone = !mic.exists() && !sys.exists();
    let wedged_temp_left = wedged_temp.exists();
    let take_temp_left = take_temp.exists();
    let _ = std::fs::remove_dir_all(&dir);

    // ── What the ticket asks for ────────────────────────────────────────────
    assert!(growing, "the runaway muxer never produced a real take — the fixture is broken");
    assert!(
        wedged_wrote_nothing,
        "the wedged muxer was supposed to be stuck opening its FIFO, not encoding"
    );
    let stall = stall.expect(
        "the session-level bound must declare a session wedged when it goes on recording \
         after the user asked it to stop",
    );
    assert_eq!(stall.phase, Phase::Stopping);
    let swept = swept.expect("a stall must be given up on");

    assert!(wedged_gone, "the wedged muxer must be gone — it is what holds the temp open");
    assert!(
        runaway_gone,
        "the muxer nobody was watching must be gone too — a partial teardown is how the \
         owner ended up with two ffmpegs and a growing orphan"
    );
    assert_eq!(swept.killed.len(), 2, "both were ours to kill, and both were killed");
    assert!(fifos_gone, "the session's FIFOs must go with it");

    let take = recovered.expect("a take with real content in it must be salvaged, never lost");
    assert!(
        take.file_name().unwrap().to_str().unwrap().ends_with("-recovered.mkv"),
        "salvage goes through the same path a crashed session's take does: {take:?}"
    );
    let (a, b) = settled.unwrap();
    assert!(a > 4096, "the recovered take must hold the recording, not a bare header");
    assert_eq!(a, b, "nothing may still be writing to it once the session has been given up on");
    assert!(!take_temp_left, "the temp was renamed, not copied");
    assert!(!wedged_temp_left, "the wedged muxer's empty temp must not be left behind");

    // And the user is told, in the one sentence both channels carry.
    let detail = super::progress::wedge_detail(&stall, &swept.recovered);
    assert!(detail.contains("asked to stop"), "{detail}");
    assert!(
        detail.contains(take.file_name().unwrap().to_str().unwrap()),
        "the message must name the file the user can go and open: {detail}"
    );
}

/// The other half of the ticket, and the half that would cost someone a take they cannot
/// record again: a session that is genuinely recording must never be torn down for being
/// slow. Proven against a REAL muxer, because the thing that can be wrong here is not the
/// state machine (the unit tests cover that) but the BUDGET — whether it is generous enough
/// for the way a real recording's file actually advances.
///
/// This test is how that granularity stopped being an assumption. The first version of this
/// module ran the bound on a 2-second budget against a live muxer and it FIRED, which is the
/// disaster case, in a test, before shipping. The cause is that `-flush_packets 1` does not
/// make a Matroska file grow byte by byte: the muxer starts a new CLUSTER every 2 MB or 5
/// seconds of media, so a live temp advances in steps. So the gap is measured here on every
/// run and checked against the real [`SESSION_STALL_SECS`], rather than a scaled-down budget
/// being checked against a scaled-down fixture — which would have proved nothing about the
/// number we actually ship.
#[test]
fn a_real_recordings_temp_advances_far_inside_the_session_budget() {
    require_ffmpeg!("the live growth-granularity proof");
    let dir = tmpdir("live-slow");
    let temp = dir.join(".long-take.recording.mkv");
    let out_path = dir.join("long-take.mp4");
    let mut muxer = spawn_recorder_shaped_muxer(&temp);
    let growing = wait_for(30, || super::muxer_alive(&temp));

    // Watch a real recording for a good few clusters, tracking the longest silence AND
    // feeding the bound on a budget scaled to the real one the same way this window is
    // scaled to it.
    let mut guard = SessionProgress::with_budget(Instant::now(), WATCH_BUDGET);
    let mut fired_while_running = None;
    let mut longest_gap = Duration::ZERO;
    let mut last_seen = Sample::default();
    let mut last_change = Instant::now();
    let until = Instant::now() + WATCH_WINDOW;
    while Instant::now() < until {
        let sample = Sample::read(Some(&temp), Some(&out_path));
        if sample != last_seen {
            longest_gap = longest_gap.max(last_change.elapsed());
            last_change = Instant::now();
            last_seen = sample;
        }
        if let Some(s) = guard.observe(Instant::now(), Phase::Running, sample) {
            fired_while_running = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    longest_gap = longest_gap.max(last_change.elapsed());

    // Now pause it for real. SIGSTOP is the closest thing to the media clock freezing: the
    // process is alive, the file stops growing, and nothing is captured — by design. A
    // paused session's budget is FROZEN rather than merely generous, so a short budget run
    // out many times over is a fair test of that.
    let paused_ok = pause_process(muxer.id());
    let mut guard = SessionProgress::with_budget(Instant::now(), TEST_BUDGET);
    let mut fired_while_paused = None;
    let until = Instant::now() + TEST_BUDGET * 5;
    while Instant::now() < until {
        if let Some(s) =
            guard.observe(Instant::now(), Phase::Paused, Sample::read(Some(&temp), Some(&out_path)))
        {
            fired_while_paused = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = resume_process(muxer.id());
    reap(&mut muxer);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(growing, "the muxer never produced a take — the fixture is broken");
    eprintln!(
        "DRAGON-423: a live recorder-shaped muxer advanced its temp at worst every {:.1}s \
         (the session budget is {SESSION_STALL_SECS}s)",
        longest_gap.as_secs_f64()
    );
    assert!(
        fired_while_running.is_none(),
        "a recording that is still writing must never be given up on: {fired_while_running:?}"
    );
    assert!(
        longest_gap * 4 < Duration::from_secs(SESSION_STALL_SECS),
        "a live recording advanced only every {:.1}s — the session budget of \
         {SESSION_STALL_SECS}s no longer has the margin it claims, and shrinking it further \
         would start tearing down real takes",
        longest_gap.as_secs_f64()
    );
    assert!(paused_ok, "could not pause the stand-in muxer");
    assert!(
        fired_while_paused.is_none(),
        "a paused session writes nothing BY DESIGN and must never read as a stall: \
         {fired_while_paused:?}"
    );
}

fn pause_process(pid: u32) -> bool {
    signal(pid, rustix::process::Signal::STOP)
}

fn resume_process(pid: u32) -> bool {
    signal(pid, rustix::process::Signal::CONT)
}

fn signal(pid: u32, sig: rustix::process::Signal) -> bool {
    match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => rustix::process::kill_process(p, sig).is_ok(),
        None => false,
    }
}
