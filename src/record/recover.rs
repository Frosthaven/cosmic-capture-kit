//! Crash RECOVERY for recording sessions (DRAGON-421), and giving up on a WEDGED one
//! (DRAGON-423).
//!
//! **What this file covers, stated plainly, because it was once read as more than it is:**
//! the layers below are about a session that DIED — a crash, a SIGKILL, a power cut. They
//! do nothing whatever for a session that is still alive and merely not getting anywhere.
//! The tether is a death-triggered mechanism; a hung process never triggers it, and none of
//! the in-process bounds can see a wedge either, because they live inside the part that has
//! gone wrong. That gap was found in the field within minutes of the tether shipping
//! (DRAGON-423) and it is closed by a bound at the SESSION level, [`super::progress`], whose
//! "give up" is [`abandon_session`] at the bottom of this file. Death and hangs are two
//! different failures; this file now holds the response to both, and neither implies the
//! other.
//!
//! A recording session that dies badly used to leave wreckage behind, and the wreckage
//! broke every LATER recording until someone found and killed it by hand:
//!
//! * an `ffmpeg` orphaned for minutes, still holding the `.recording.mkv` temp and parked
//!   forever in `open()` on an audio FIFO nobody would ever write to,
//! * stale `cosmic-capture-kit.<pid>.<n>.{mic,sys}mix.pcm` FIFOs from dead pids,
//! * abandoned `.<stamp>.recording.mkv` temps that were never finalized.
//!
//! Why the orphan hangs forever is worth stating once, because it explains why no
//! in-process watchdog could ever have caught it. The muxer opens its inputs in order:
//! stdin (video), then the mic FIFO, then the system FIFO. A POSIX `open(fifo, O_RDONLY)`
//! blocks until a WRITER appears — and the writer is [`super::pump`], inside our process.
//! When our process dies, ffmpeg is parked in that `open()`; it never gets as far as
//! reading stdin, so the EOF our death delivered on the video pipe is never observed. It
//! sits there holding the temp until someone kills it.
//!
//! **The DRAGON-118 finding, so nobody re-opens it:** this was NOT a gap in the bounded-wait
//! coverage, and not that coverage failing. Every wait in a LIVE session is bounded and each
//! one was working — [`super::MuxerWatchdog`] is armed before the session's first video write
//! (12s, pause-gated), the pump's FIFO-open rendezvous gives up after 15s and trips `stop`,
//! and the stop tail's reap is [`super::wait_or_kill`] at 30s. The problem is that all three
//! live INSIDE the process that had already died — the field report says so plainly ("its
//! parent capture child was already gone"). The scope was wrong, not the bounds. Widening any
//! of those numbers would have changed nothing, which is why the fix is structural instead.
//!
//! DRAGON-423 found the same sentence to be true for a different reason: those bounds also
//! live inside the part of a LIVE process that can be the thing that is stuck. Same
//! conclusion, same remedy — a bound at a scope that can still see — and again not a longer
//! timeout anywhere.
//!
//! Three layers, in the order they matter (all three are about a session that DIED — see
//! the note at the top, and [`abandon_session`] for the hang):
//!
//! 1. **The tether** ([`crate::util::spawn_tethered`]) — the muxer cannot outlive us in
//!    the first place. Kernel-enforced on Linux and Windows; see that function for the
//!    per-platform choice and what macOS can and cannot promise. It fires on our DEATH and
//!    only on our death: while we are alive it is inert, however stuck we are.
//! 2. **The ledger** ([`note_muxer`] / [`clear_session`]) — while a recording is live, its
//!    pid names its own wreckage-in-waiting (muxer pid, FIFOs, temp) in a per-pid sidecar
//!    alongside every other marker in [`crate::instance`]. That file is what lets a LATER
//!    session tell "wreckage from a corpse" apart from "a live session's working files".
//! 3. **The sweep** ([`sweep_wreckage`]) — at the start of every recording, reap what is
//!    PROVABLY dead and salvage what is worth keeping.
//!
//! ## The two ways this would be a disaster
//!
//! DRAGON-322/351 deliberately allow a recording and a fresh capture to coexist, so the
//! sweep runs while another instance may be recording for real. Two rules keep that safe,
//! and both are tested by causing the situation rather than reasoning about it:
//!
//! * **Never touch a live session's files.** Every FIFO carries its owner's pid in its
//!   NAME and every ledger carries it in its filename, so "is this dead?" is answered by
//!   [`crate::instance::pid_is_live`] and nothing else. A muxer pid is additionally
//!   cross-checked against every LIVE ledger before we would signal it, so a recycled pid
//!   can never be mistaken for a corpse's muxer, and confirmed to still BE our muxer
//!   ([`is_our_muxer`]) before the kill.
//! * **Never destroy a take.** A temp with real content in it is RENAMED into place as a
//!   recovered recording, never deleted — the same instinct as the stop tail's salvage
//!   (DRAGON-78/123). Only a temp that is provably dead AND empty (never outgrew the bare
//!   container header, [`super::muxer_alive`]) is removed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long an UNCLAIMED temp — one no ledger accounts for at all — must have gone
/// untouched before the sweep will consider it abandoned.
///
/// This is NOT a hang timeout being widened: a temp named by a live ledger is spared
/// outright, whatever its age. It exists only for wreckage left by a build that predates
/// the ledger (the two the owner found from earlier that day), where the only evidence
/// available is "nothing has written to this in a long time". A live recording's muxer
/// advances its temp at least once per Matroska cluster — 2MB or 5s of media, whichever
/// comes first, measured at up to ~6s in `super::wedge_live_tests` — so a minute of silence
/// on a temp that no live session claims is not a recording in progress. (`-flush_packets 1`
/// is why a cluster reaches the disk promptly; it is not why the file grows.) The unclaimed sweep is skipped
/// entirely while any other instance holds a recording marker, so even that inference is
/// never drawn with a real recording in the room.
const ORPHAN_TEMP_MIN_AGE: Duration = Duration::from_secs(60);

/// The suffix every recording temp carries ([`super::recording_temp_path`]).
const TEMP_SUFFIX: &str = ".recording.mkv";

// ── The ledger ───────────────────────────────────────────────────────────────

/// One recording session's wreckage-in-waiting, as recorded by its owner.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Ledger {
    /// Where the sidecar lives. Carried rather than re-derived, so the sweep drops the
    /// exact file it read instead of recomputing a path against the live runtime dir —
    /// which is also what makes the whole sweep exercisable against a temp directory.
    pub(crate) path: PathBuf,
    /// The pid that wrote it (from the sidecar's FILENAME, never its contents — a pid a
    /// file claims for itself is a pid that can lie).
    pub(crate) owner: u32,
    /// Every ffmpeg muxer this session spawned. More than one is normal: the zero-copy
    /// path falls back to the CPU path on a decline, and each attempt spawns its own.
    pub(crate) muxers: Vec<u32>,
    /// The `.recording.mkv` temp(s) the muxers write.
    pub(crate) temps: Vec<PathBuf>,
    /// The audio FIFOs the pump feeds them.
    pub(crate) fifos: Vec<PathBuf>,
}

/// Parse a ledger file's contents. Deliberately tolerant: an unknown or malformed line is
/// skipped rather than failing the whole read, because a ledger truncated mid-write by the
/// very crash we are recovering from is a case that MUST still yield whatever it did get
/// written. Pure, so the format has one definition and one test.
fn parse_ledger(owner: u32, text: &str) -> Ledger {
    let mut l = Ledger { owner, ..Default::default() };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        if value.is_empty() {
            continue;
        }
        match key {
            "muxer" => {
                if let Ok(pid) = value.parse::<u32>() {
                    l.muxers.push(pid);
                }
            }
            "temp" => l.temps.push(PathBuf::from(value)),
            "fifo" => l.fifos.push(PathBuf::from(value)),
            _ => {}
        }
    }
    l
}

/// The lines [`note_muxer`] appends for one muxer spawn. Pure, so [`parse_ledger`]'s
/// inverse is testable without touching the filesystem. Visible to the rest of `record` so
/// the live wedge proof can lay down a ledger the way a real session would, rather than
/// hand-writing the format into a second place.
pub(super) fn ledger_lines(muxer_pid: u32, temp: &Path, fifos: &[&Path]) -> String {
    let mut s = format!("muxer={muxer_pid}\ntemp={}\n", temp.to_string_lossy());
    for f in fifos {
        s.push_str(&format!("fifo={}\n", f.to_string_lossy()));
    }
    s
}

/// Record a freshly spawned recording muxer in THIS process's ledger.
///
/// Called from the one place every recording muxer is spawned
/// ([`crate::encode::spawn_ffmpeg_media_clock`] and its zero-copy sibling), so no worker
/// has to remember to do it. APPENDS rather than replaces: a session that spawns a second
/// muxer (the zero-copy decline → CPU fallback) must not erase the record of the first.
///
/// Best-effort throughout. A ledger we fail to write costs recovery for that one session;
/// it can never cost the recording itself, so nothing here is allowed to fail loudly.
pub(crate) fn note_muxer(muxer_pid: u32, temp: &Path, mic_fifo: &Path, sys_fifo: &Path) {
    use std::io::Write as _;
    let path = crate::instance::session_ledger_path(std::process::id());
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = f.write_all(ledger_lines(muxer_pid, temp, &[mic_fifo, sys_fifo]).as_bytes());
    let _ = f.flush();
}

/// Drop this process's ledger — its recording worker is over, so nothing it named is live
/// wreckage any more.
///
/// Called from [`super::DoneGuard`]'s `Drop`, which is THE "the worker is finished" seam
/// and fires on the panic/early-return paths too. A ledger left behind by a process that
/// then lives on into the preview is harmless (its owner is alive, so every sweep spares
/// it) but it would keep a corpse's-worth of paths on disk for no reason, and the next
/// session in this same process would append to it.
pub(crate) fn clear_session() {
    let _ = std::fs::remove_file(crate::instance::session_ledger_path(std::process::id()));
}

// ── The sweep ────────────────────────────────────────────────────────────────

/// What one sweep decided to do. Returned so the tests can assert on the decisions rather
/// than on log output, and so the caller has something to report.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Swept {
    /// Orphaned muxer pids that were killed.
    pub(crate) killed: Vec<u32>,
    /// Stale per-pid sidecars unlinked by [`crate::instance::sweep_stale_markers_in`] —
    /// the recorder's audio FIFOs above all, plus any other marker a dead pid left.
    pub(crate) sidecars: Vec<PathBuf>,
    /// Abandoned temps that had real content and were renamed into place.
    pub(crate) recovered: Vec<PathBuf>,
    /// Abandoned temps that were empty (never outgrew the container header) and removed.
    pub(crate) discarded: Vec<PathBuf>,
}

impl Swept {
    fn is_empty(&self) -> bool {
        self.killed.is_empty()
            && self.sidecars.is_empty()
            && self.recovered.is_empty()
            && self.discarded.is_empty()
    }
}

/// Reap what a crashed recording session left behind, and salvage what is worth keeping.
///
/// Call at the START of a recording, before the worker creates anything of its own —
/// that is the moment the guarantee is needed ("a new session never inherits old
/// wreckage") and the moment the evidence is freshest.
///
/// `capture_dir` is the folder the new recording will be written to; abandoned temps are
/// only ever looked for THERE, never anywhere else on disk.
pub fn sweep_wreckage(capture_dir: Option<&Path>) {
    // The unclaimed-temp pass draws an inference from staleness (see `ORPHAN_TEMP_MIN_AGE`)
    // rather than from proof, so it is skipped outright whenever another instance is
    // recording — the one situation where a wrong answer would cost someone their take.
    let unclaimed_ok = !crate::instance::any_other_recording();
    let swept = sweep_in(
        &crate::util::runtime_dir(),
        capture_dir,
        std::process::id(),
        unclaimed_ok,
        &crate::instance::pid_is_live,
    );
    report(&swept);
}

/// Log what a sweep did, through the ordinary `log` facade — which is what the DRAGON-419
/// debug log records, so this reaches the diagnostic channel that already exists rather
/// than inventing a second one. Silent when there was nothing to do, which is the normal
/// case and must not add a line to every recording.
fn report(swept: &Swept) {
    if swept.is_empty() {
        return;
    }
    log::warn!(
        "DRAGON-421: recovered from a crashed recording session — killed {} orphaned \
         muxer(s) {:?}, removed {} stale per-pid sidecar(s) (audio FIFOs and the like), \
         recovered {} abandoned temp(s), discarded {} empty one(s)",
        swept.killed.len(),
        swept.killed,
        swept.sidecars.len(),
        swept.recovered.len(),
        swept.discarded.len(),
    );
    for p in &swept.recovered {
        log::warn!(
            "DRAGON-421: an interrupted recording was recovered into {} — it is the \
             pre-finalize capture, so mic and system audio are two separate tracks in it",
            crate::diag::path_shape(p)
        );
    }
}

/// [`sweep_wreckage`]'s body against an EXPLICIT runtime dir, self pid and liveness
/// oracle, so every branch — including the ones that must NOT fire — is exercisable
/// without a live recording or a real crash.
fn sweep_in(
    runtime_dir: &str,
    capture_dir: Option<&Path>,
    self_pid: u32,
    unclaimed_ok: bool,
    live: &dyn Fn(u32) -> bool,
) -> Swept {
    let mut swept = Swept::default();
    let (live_ledgers, dead_ledgers) = read_ledgers(runtime_dir, self_pid, live);

    // Everything a LIVE session named is off limits, by path and by pid. This is the guard
    // that stands between the sweep and a real recording in progress.
    let live_muxers: HashSet<u32> = live_ledgers.iter().flat_map(|l| l.muxers.iter().copied()).collect();
    let mut live_paths: HashSet<PathBuf> = HashSet::new();
    for l in &live_ledgers {
        live_paths.extend(l.fifos.iter().cloned());
        live_paths.extend(l.temps.iter().cloned());
    }

    for l in &dead_ledgers {
        // The muxer first: it must stop writing before its temp is moved, and it is the
        // whole reason a later recording could not start.
        for &pid in &l.muxers {
            if !live(pid) || live_muxers.contains(&pid) {
                continue;
            }
            if !is_our_muxer(pid, &l.temps) {
                continue;
            }
            if kill_pid(pid) {
                log::warn!(
                    "DRAGON-421: killing ffmpeg pid {pid}, orphaned by dead recording \
                     session pid {} (it was holding the recording temp open)",
                    l.owner
                );
                swept.killed.push(pid);
            }
        }
        // NOTE the FIFOs a dead ledger names are NOT unlinked here. Every one of them
        // carries its creator's pid in its own FILENAME (`try_start_owned_audio` builds
        // them from `std::process::id()`), so the marker sweep below reaps them by exactly
        // the same dead-owner rule that reaps every other per-pid sidecar. The ledger's
        // `fifo=` lines stay because they are worth having in a diagnosis, and because
        // they let a LIVE session's paths be spared explicitly rather than incidentally.
        for t in &l.temps {
            if !live_paths.contains(t) {
                recover_temp(t, &mut swept);
            }
        }
        let _ = std::fs::remove_file(&l.path);
    }

    // Stale per-pid sidecars — the recorder's audio FIFOs above all, including ones no
    // ledger accounts for (wreckage from a build that predates the ledger, or from a
    // session that died between `mkfifo` and its muxer spawn). This is `instance`'s
    // EXISTING sweeper, extended to cover the FIFO suffixes, rather than a second one
    // here: the rule is identical — the owner pid is in the filename, so a live owner's
    // files are never even visited — and one implementation cannot drift from itself.
    swept.sidecars = crate::instance::sweep_stale_markers_in(runtime_dir, self_pid, live);

    if unclaimed_ok && let Some(dir) = capture_dir {
        sweep_unclaimed_temps(dir, &live_paths, &mut swept);
    }
    swept
}

/// Read every session ledger in `runtime_dir`, split by whether its owner is still alive.
/// Our OWN pid counts as live — this process is about to start a recording, so anything it
/// left is either current or about to be replaced, never wreckage to reap.
fn read_ledgers(runtime_dir: &str, self_pid: u32, live: &dyn Fn(u32) -> bool) -> (Vec<Ledger>, Vec<Ledger>) {
    let (mut alive, mut dead) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return (alive, dead);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((pid, suffix)) = crate::instance::parse_marker_name(name) else {
            continue;
        };
        if suffix != crate::instance::SESSION_MARKER {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let ledger = Ledger { path: entry.path(), ..parse_ledger(pid, &text) };
        if pid == self_pid || live(pid) {
            alive.push(ledger);
        } else {
            dead.push(ledger);
        }
    }
    (alive, dead)
}

/// Whether `name` is a recording temp ([`super::recording_temp_path`]'s shape: a HIDDEN
/// `.{stem}.recording.mkv` beside the recording it will become).
fn is_recording_temp(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(TEMP_SUFFIX) && name.len() > TEMP_SUFFIX.len() + 1
}

/// Look for abandoned temps in `capture_dir` that NO ledger accounts for — the wreckage of
/// a build that predates the ledger. See [`ORPHAN_TEMP_MIN_AGE`] for why staleness is
/// admissible evidence here and where the line is drawn.
fn sweep_unclaimed_temps(capture_dir: &Path, live_paths: &HashSet<PathBuf>, swept: &mut Swept) {
    let Ok(entries) = std::fs::read_dir(capture_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_recording_temp(name) {
            continue;
        }
        let path = entry.path();
        if live_paths.contains(&path) {
            continue;
        }
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|t| t.elapsed().map_err(std::io::Error::other))
            .map(|age| age < ORPHAN_TEMP_MIN_AGE)
            .unwrap_or(true); // unreadable mtime -> treat as fresh, i.e. leave it alone
        if fresh {
            continue;
        }
        recover_temp(&path, swept);
    }
}

/// Deal with one abandoned temp: SALVAGE it if it holds real content, remove it only if it
/// is provably empty.
///
/// Salvage is a rename, not a re-encode. Matroska needs no trailer and the recorder writes
/// it packet-granular (`-flush_packets 1`), so what is on disk plays as-is; a rename also
/// costs nothing at recording start and cannot itself fail halfway. The recovered file
/// keeps the `.mkv` extension it actually is, rather than the `.mp4` the finished
/// recording would have been.
///
/// What the user gets is therefore the PRE-finalize file: two SEPARATE audio tracks (mic and
/// system, titled as such) instead of the single mixed one `finalize` would have produced,
/// so a player opens it on the mic track and the system track has to be selected. That is a
/// deliberate trade. Running the real finalize here would mean an ffmpeg re-encode on the UI
/// thread at the very moment the user is starting a recording, and it could not be faithful
/// anyway — the channel-toggle automation that drives finalize's mute gates died with the
/// session that recorded it. A slightly awkward file the user still has beats a correct file
/// they never get, and it beats the alternative this replaces, which was no file at all.
fn recover_temp(temp: &Path, swept: &mut Swept) {
    if !super::muxer_alive(temp) {
        // Never outgrew the bare container header: there is no take in here to lose.
        if std::fs::remove_file(temp).is_ok() {
            swept.discarded.push(temp.to_path_buf());
        }
        return;
    }
    let Some(dest) = recovered_path(temp) else { return };
    if std::fs::rename(temp, &dest).is_ok() {
        swept.recovered.push(dest);
    }
}

/// Where an abandoned temp is recovered TO: `.2026-07-29-16-11-53-493.recording.mkv`
/// becomes `2026-07-29-16-11-53-493-recovered.mkv` beside it. Never overwrites — a
/// previously recovered file with the same name gets a counter — because the whole point
/// of this path is that it does not destroy recordings.
fn recovered_path(temp: &Path) -> Option<PathBuf> {
    let dir = temp.parent()?;
    let name = temp.file_name()?.to_str()?;
    let stem = name.strip_prefix('.')?.strip_suffix(TEMP_SUFFIX)?;
    for n in 0..1000 {
        let candidate = match n {
            0 => dir.join(format!("{stem}-recovered.mkv")),
            _ => dir.join(format!("{stem}-recovered-{n}.mkv")),
        };
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// ── Giving up on a WEDGED session (DRAGON-423) ───────────────────────────────

/// End THIS process's recording session completely, and salvage what it recorded.
///
/// The sweep above answers "what did a session that DIED leave behind?". This answers the
/// other half: "we are alive, this session is going nowhere, and we are giving up on it" —
/// the response [`super::progress`] calls for once its session-level bound expires.
///
/// It works off the same ledger, which is exactly why the ledger is worth having: it is the
/// only complete list of what this session started. Partial teardown is how the owner ended
/// up with two mic ffmpegs and a growing orphan temp, so everything the session named is
/// dealt with in one pass:
///
/// 1. **Every muxer it spawned** — including the second one a zero-copy decline started,
///    which is the one nobody was left watching. Killed first, so nothing is still writing
///    to a temp we are about to move. The same `is_our_muxer` proof the sweep uses stands
///    between this and an innocent process on a recycled pid: these are pids WE spawned, but
///    a muxer already reaped by a stop tail could have had its pid reused, and a wrong kill
///    is not a mistake worth risking to save a few seconds.
/// 2. **Every FIFO it created** — unlinked now rather than left for a later sweep. Unlinking
///    a FIFO an ffmpeg still holds open is harmless; it only removes the name.
/// 3. **Every temp it wrote** — through the SAME [`recover_temp`] the sweep uses, so a take
///    with real content in it is renamed to `<stamp>-recovered.mkv` and never deleted. There
///    is one salvage path in this codebase and this is it.
///
/// Returns what it did, so the caller can tell the user where their take went.
///
/// On Windows step 1 is a no-op by design (`is_our_muxer` there is `false`; a pid is all we
/// would have to go on, and the job-object tether kills the muxers as this process exits
/// moments later anyway). A temp still held open by one may therefore fail to rename; it is
/// then recovered by the next recording's sweep, which is the whole point of the sweep
/// existing.
pub(crate) fn abandon_session() -> Swept {
    let path = crate::instance::session_ledger_path(std::process::id());
    let swept = abandon_ledger(Path::new(&path), std::process::id());
    report_abandon(&swept);
    swept
}

/// [`abandon_session`]'s body against an EXPLICIT ledger path, so the whole teardown can be
/// exercised against a temp directory and a stand-in muxer instead of a real wedged session
/// — and, in `record::wedge_live_tests`, against real ffmpeg processes that really are stuck.
/// The explicit path is also what keeps a test from ever reading THIS process's own ledger,
/// which other tests in the same binary write to.
pub(super) fn abandon_ledger(path: &Path, owner: u32) -> Swept {
    let mut swept = Swept::default();
    let ledger = parse_ledger(owner, &std::fs::read_to_string(path).unwrap_or_default());
    for &pid in &ledger.muxers {
        if is_our_muxer(pid, &ledger.temps) && kill_pid(pid) {
            swept.killed.push(pid);
        }
    }
    for f in &ledger.fifos {
        if std::fs::remove_file(f).is_ok() {
            swept.sidecars.push(f.clone());
        }
    }
    for t in &ledger.temps {
        recover_temp(t, &mut swept);
    }
    // The ledger has been acted on in full; nothing it names is outstanding any more.
    let _ = std::fs::remove_file(path);
    swept
}

/// Log an abandonment through the ordinary `log` facade — the DRAGON-419 debug log records
/// it, and the user-facing half is the alert `progress::wedge_detail` writes. Never silent:
/// unlike the sweep, reaching here at all means something went wrong.
fn report_abandon(swept: &Swept) {
    log::error!(
        "DRAGON-423: gave up on a wedged recording session — killed {} of its muxer(s) {:?}, \
         removed {} of its FIFO(s), recovered {} take(s), discarded {} empty one(s)",
        swept.killed.len(),
        swept.killed,
        swept.sidecars.len(),
        swept.recovered.len(),
        swept.discarded.len(),
    );
    for p in &swept.recovered {
        log::error!(
            "DRAGON-423: the wedged session's take was recovered into {} — it is the \
             pre-finalize capture, so mic and system audio are two separate tracks in it",
            crate::diag::path_shape(p)
        );
    }
}

// ── Proving a pid is the corpse's muxer, per platform ────────────────────────

/// Whether live pid `pid` really is the ffmpeg a dead session spawned to write one of
/// `temps` — the last check before we signal it.
///
/// Killing a pid read out of a file is the one operation here that could hurt an innocent
/// process, so the caller has already established that the pid's owner is dead and that no
/// LIVE ledger claims it. This adds the final, platform-specific confirmation.
///
/// Answering `false` when it should have been `true` is the SAFE direction and costs almost
/// nothing: the temp and the stale sidecars are cleaned up regardless, and the next
/// recording sweeps again.
#[cfg(target_os = "linux")]
fn is_our_muxer(pid: u32, temps: &[PathBuf]) -> bool {
    // Airtight: the muxer's argv literally contains the temp path it was spawned to write,
    // and that path carries a millisecond timestamp. A recycled pid cannot match it.
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args = String::from_utf8_lossy(&raw);
    args.split('\0').any(|arg| temps.iter().any(|t| t.as_os_str() == arg))
}

/// macOS has no `/proc`, so the confirmation is the process's executable path
/// (`proc_pidpath`): it must still BE an ffmpeg. Weaker than the Linux argv match — it
/// cannot distinguish one ffmpeg from another — which is exactly why the caller's
/// "not claimed by any live ledger" check is what actually protects a concurrent
/// recording's muxer, and this only rules out an unrelated program on a recycled pid.
#[cfg(target_os = "macos")]
fn is_our_muxer(pid: u32, temps: &[PathBuf]) -> bool {
    let _ = temps;
    let mut buf = vec![0u8; 4096];
    // SAFETY: `proc_pidpath` writes at most `buf.len()` bytes into the buffer we own and
    // returns how many; it reads nothing else.
    let n = unsafe {
        libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32)
    };
    if n <= 0 {
        return false;
    }
    buf.truncate(n as usize);
    let path = String::from_utf8_lossy(&buf);
    Path::new(path.as_ref())
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f == "ffmpeg" || f == "ffmpeg.exe")
}

/// Windows never reaches the kill path: its muxers are tethered to a job object that the
/// kernel tears down when our process handle closes, however it died (see
/// [`crate::util::spawn_tethered`]), so an orphan cannot exist to be reaped. Answering
/// `false` keeps the sweep from signalling anything by pid on the one platform where a pid
/// is all we would have to go on.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_our_muxer(pid: u32, temps: &[PathBuf]) -> bool {
    let _ = (pid, temps);
    false
}

/// SIGKILL `pid`. SIGTERM would be politer, but the process this is aimed at is parked in
/// an uninterruptible-looking FIFO `open()` with nobody left to write to it; there is
/// nothing to shut down gracefully.
#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => rustix::process::kill_process(p, rustix::process::Signal::KILL).is_ok(),
        None => false,
    }
}

#[cfg(not(unix))]
fn kill_pid(pid: u32) -> bool {
    let _ = pid;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::time::Instant;

    /// A directory of our own, so a test never reads the live runtime dir. Tagged with the
    /// pid AND a per-call counter so parallel tests (and repeat runs) never share one.
    fn tmp(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("cck-d421-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_ledger(dir: &Path, owner: u32, body: &str) -> PathBuf {
        let p = dir.join(format!("cosmic-capture-kit.{owner}.session"));
        std::fs::write(&p, body).unwrap();
        p
    }

    /// A file big enough to read as a real take (`muxer_alive`).
    fn grown(path: &Path) {
        std::fs::write(path, vec![0u8; 64 * 1024]).unwrap();
    }

    // ── The ledger format ───────────────────────────────────────────────────

    #[test]
    fn ledger_lines_round_trip_through_the_parser() {
        let text = ledger_lines(
            4242,
            Path::new("/home/u/Capture/.stamp.recording.mkv"),
            &[Path::new("/run/mic.pcm"), Path::new("/run/sys.pcm")],
        );
        let l = parse_ledger(7, &text);
        assert_eq!(l.owner, 7, "the owner comes from the FILENAME, never the contents");
        assert_eq!(l.muxers, vec![4242]);
        assert_eq!(l.temps, vec![PathBuf::from("/home/u/Capture/.stamp.recording.mkv")]);
        assert_eq!(l.fifos.len(), 2);
    }

    #[test]
    fn a_ledger_truncated_by_the_crash_still_yields_what_it_got() {
        // The exact case this file exists for: the writer died mid-line.
        let l = parse_ledger(9, "muxer=100\ntemp=/tmp/a.recording.mkv\nfif");
        assert_eq!(l.muxers, vec![100]);
        assert_eq!(l.temps, vec![PathBuf::from("/tmp/a.recording.mkv")]);
        assert!(l.fifos.is_empty());
        // Junk and empty values are skipped, never guessed at.
        let l = parse_ledger(9, "muxer=notapid\nnonsense\ntemp=\nfifo=/tmp/f");
        assert!(l.muxers.is_empty() && l.temps.is_empty());
        assert_eq!(l.fifos, vec![PathBuf::from("/tmp/f")]);
    }

    #[test]
    fn a_session_appends_every_muxer_it_spawns() {
        // The zero-copy decline → CPU fallback spawns a second muxer; recording only the
        // last one would leave the first orphaned forever.
        let mut text = ledger_lines(11, Path::new("/tmp/a.recording.mkv"), &[]);
        text.push_str(&ledger_lines(22, Path::new("/tmp/a.recording.mkv"), &[]));
        assert_eq!(parse_ledger(1, &text).muxers, vec![11, 22]);
    }

    // ── Naming contracts ────────────────────────────────────────────────────

    #[test]
    fn recording_temps_are_recognised_by_the_shape_recording_temp_path_mints() {
        let real = super::super::recording_temp_path(Path::new("/x/2026-07-29-16-11-53-493.mp4"));
        let name = real.file_name().unwrap().to_str().unwrap();
        assert!(is_recording_temp(name), "{name}");
        for other in [
            "2026-07-29.mp4",              // a finished recording
            "2026-07-29-recovered.mkv",    // something we already recovered
            ".recording.mkv",              // no stem at all
            ".hidden.mkv",
            "notes.recording.mkv.bak",
        ] {
            assert!(!is_recording_temp(other), "must not claim {other}");
        }
    }

    #[test]
    fn a_recovered_name_never_overwrites_an_existing_file() {
        let dir = tmp("recovered-name");
        let temp = dir.join(".2026-07-29-16-11-53-493.recording.mkv");
        let first = recovered_path(&temp).unwrap();
        assert_eq!(first.file_name().unwrap(), "2026-07-29-16-11-53-493-recovered.mkv");
        std::fs::write(&first, b"already here").unwrap();
        let second = recovered_path(&temp).unwrap();
        assert_ne!(first, second);
        assert!(second.file_name().unwrap().to_str().unwrap().contains("recovered-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── The sweep: reap the dead ────────────────────────────────────────────

    #[test]
    fn a_dead_sessions_fifos_and_empty_temp_are_reaped_and_its_ledger_dropped() {
        let run = tmp("dead-run");
        let cap = tmp("dead-cap");
        let (dead, live_pid) = (4242u32, 4243u32);
        let mic = run.join("cosmic-capture-kit.4242.0.micmix.pcm");
        let sys = run.join("cosmic-capture-kit.4242.0.sysmix.pcm");
        let temp = cap.join(".stamp.recording.mkv");
        std::fs::write(&mic, b"").unwrap();
        std::fs::write(&sys, b"").unwrap();
        std::fs::write(&temp, vec![0u8; 900]).unwrap(); // bare header — nothing to lose
        let ledger = write_ledger(
            &run,
            dead,
            &ledger_lines(999_999_123, &temp, &[&mic, &sys]),
        );

        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), live_pid, true, &|p| p != dead);

        assert_eq!(swept.sidecars.len(), 2, "both stale FIFOs go");
        assert_eq!(swept.discarded, vec![temp.clone()], "an empty temp is discarded");
        assert!(swept.recovered.is_empty());
        assert!(!mic.exists() && !sys.exists() && !temp.exists());
        assert!(!ledger.exists(), "the ledger is dropped LAST, once acted on");
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    #[test]
    fn an_abandoned_temp_with_real_content_is_recovered_never_deleted() {
        let run = tmp("salvage-run");
        let cap = tmp("salvage-cap");
        let temp = cap.join(".2026-07-29-16-11-53-493.recording.mkv");
        grown(&temp);
        write_ledger(&run, 4242, &ledger_lines(999_999_124, &temp, &[]));

        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &|_| false);

        assert!(swept.discarded.is_empty(), "a take is NEVER discarded");
        assert_eq!(swept.recovered.len(), 1);
        assert!(!temp.exists(), "the hidden temp is gone…");
        let out = cap.join("2026-07-29-16-11-53-493-recovered.mkv");
        assert!(out.exists(), "…because it became a visible recovered recording");
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 64 * 1024, "byte-for-byte");
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    #[test]
    fn fifos_from_a_dead_pid_with_no_ledger_are_still_swept() {
        // Wreckage from a build that predates the ledger — the shape the owner actually hit.
        let run = tmp("legacy-fifo");
        let stale = run.join("cosmic-capture-kit.4242.0.micmix.pcm");
        std::fs::write(&stale, b"").unwrap();
        let swept = sweep_in(run.to_str().unwrap(), None, 1, true, &|_| false);
        assert_eq!(swept.sidecars, vec![stale.clone()]);
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(&run);
    }

    // ── The sweep: spare the living. These are the tests that matter most. ──

    #[test]
    fn a_live_sessions_fifos_and_temp_are_never_touched() {
        // DRAGON-322/351: a recording and a capture coexist, so this sweep runs WHILE
        // someone else is recording for real. Getting this wrong costs a user their take.
        let run = tmp("live-spare-run");
        let cap = tmp("live-spare-cap");
        let live_pid = 4242u32;
        let mic = run.join("cosmic-capture-kit.4242.0.micmix.pcm");
        let sys = run.join("cosmic-capture-kit.4242.0.sysmix.pcm");
        let temp = cap.join(".live-take.recording.mkv");
        std::fs::write(&mic, b"").unwrap();
        std::fs::write(&sys, b"").unwrap();
        grown(&temp);
        let ledger = write_ledger(&run, live_pid, &ledger_lines(777, &temp, &[&mic, &sys]));
        // Make the temp look ANCIENT, so only the ledger stands between it and the
        // unclaimed-temp pass: a paused recording writes nothing for as long as it likes.
        // Write access: Windows' SetFileTime needs FILE_WRITE_ATTRIBUTES, which a
        // read-only handle is denied.
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &|p| p == live_pid);

        assert_eq!(swept, Swept::default(), "nothing may be touched: {swept:?}");
        assert!(mic.exists() && sys.exists(), "a live session keeps its FIFOs");
        assert!(temp.exists(), "a live session keeps its temp");
        assert!(ledger.exists(), "and its ledger");
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    #[test]
    fn our_own_pids_files_are_never_swept() {
        let run = tmp("self-spare");
        let mine = run.join(format!("cosmic-capture-kit.{}.0.micmix.pcm", 4242));
        std::fs::write(&mine, b"").unwrap();
        // Self counts as live even when the oracle says otherwise.
        let swept = sweep_in(run.to_str().unwrap(), None, 4242, true, &|_| false);
        assert!(swept.sidecars.is_empty());
        assert!(mine.exists());
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_fresh_unclaimed_temp_is_left_alone() {
        // No ledger names it and nobody is provably recording, but it was written seconds
        // ago: not our business. Only a long-untouched one is treated as abandoned.
        let run = tmp("fresh-run");
        let cap = tmp("fresh-cap");
        let temp = cap.join(".just-started.recording.mkv");
        grown(&temp);
        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &|_| false);
        assert_eq!(swept, Swept::default());
        assert!(temp.exists());
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    #[test]
    fn the_unclaimed_pass_is_skipped_while_anyone_else_records() {
        let run = tmp("noclaim-run");
        let cap = tmp("noclaim-cap");
        let temp = cap.join(".old-take.recording.mkv");
        grown(&temp);
        // Write access: see a_live_sessions_fifos_and_temp_are_never_touched.
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // Stale AND unclaimed — but another instance holds a recording marker, so the
        // inference is refused outright.
        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, false, &|_| false);
        assert_eq!(swept, Swept::default());
        assert!(temp.exists());
        // With nobody recording, the SAME file is recovered — proving the guard is what
        // spared it, not some other accident of the setup.
        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &|_| false);
        assert_eq!(swept.recovered.len(), 1);
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    #[test]
    fn a_muxer_pid_claimed_by_a_live_ledger_is_never_signalled() {
        // The pid-recycling case: a dead session's ledger names a pid that a LIVE session
        // now names too. Killing it would kill the live recording's muxer.
        let run = tmp("recycled");
        let dead = 4242u32;
        let live_pid = 4243u32;
        let shared_muxer = std::process::id(); // certainly alive, and certainly not ffmpeg
        write_ledger(&run, dead, &ledger_lines(shared_muxer, Path::new("/tmp/dead.recording.mkv"), &[]));
        write_ledger(&run, live_pid, &ledger_lines(shared_muxer, Path::new("/tmp/live.recording.mkv"), &[]));
        let swept = sweep_in(run.to_str().unwrap(), None, 1, true, &|p| p != dead);
        assert!(swept.killed.is_empty(), "a live ledger's muxer is untouchable");
        let _ = std::fs::remove_dir_all(&run);
    }

    // ── Causing the failure: a real orphan, really killed ───────────────────

    /// The headline case, reproduced rather than reasoned about: a process holding a
    /// recording temp survives its owner, and the next session reaps it.
    ///
    /// The stand-in is spawned with the temp path in its argv, which is exactly what
    /// [`is_our_muxer`] matches on for the real ffmpeg. Linux-only because that argv check
    /// is `/proc`-based; macOS's weaker check wants a real ffmpeg on disk to be meaningful.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_orphaned_muxer_is_killed_and_its_take_recovered() {
        let run = tmp("orphan-run");
        let cap = tmp("orphan-cap");
        let temp = cap.join(".orphan-take.recording.mkv");
        grown(&temp);
        // A process that will outlive its "owner", holding the temp path in its argv —
        // exactly the shape `is_our_muxer` matches on for the real ffmpeg. `tail -f` is the
        // stand-in because it takes the path as an argument, never exits, and forks nothing
        // (a shell wrapper would either exec its argv away or leave a grandchild behind).
        let mut orphan = std::process::Command::new("tail")
            .arg("-f")
            .arg("/dev/null")
            .arg(&temp)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // `spawn` returns once the fork is away, BEFORE the exec has replaced the child's
        // argv — until then `/proc/<pid>/cmdline` is empty or still the harness's, and
        // `is_our_muxer` would (correctly, and safely) decline to match. Wait for the
        // fixture to actually BE the process this test is about. Real wreckage is never in
        // this state: a muxer worth reaping exec'd long before the session that spawned it
        // died.
        let ready = Instant::now() + Duration::from_secs(10);
        while !is_our_muxer(orphan.id(), std::slice::from_ref(&temp)) {
            assert!(Instant::now() < ready, "the orphan stand-in never exec'd");
            std::thread::sleep(Duration::from_millis(10));
        }
        // An owner pid that is genuinely dead (spawned and reaped), not merely made up.
        let dead_owner = {
            let mut c = std::process::Command::new("true").spawn().unwrap();
            c.wait().unwrap();
            c.id()
        };
        write_ledger(&run, dead_owner, &ledger_lines(orphan.id(), &temp, &[]));

        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &crate::instance::pid_is_live);

        // Reap the stand-in BEFORE asserting, whatever the sweep decided: a failed assert
        // that left it running would leak a process holding this test's inherited pipes.
        let status = super::super::wait_or_kill(&mut orphan, Duration::from_secs(5)).unwrap();
        let _ = std::fs::remove_dir_all(&run);

        assert_eq!(swept.killed, vec![orphan.id()], "the orphan must be killed");
        assert!(!status.success(), "it really died");
        assert_eq!(swept.recovered.len(), 1, "and its take survived as a recovered file");
        let _ = std::fs::remove_dir_all(&cap);
    }

    // ── Giving up on a wedged session (DRAGON-423) ──────────────────────────

    /// The teardown the session-level bound calls for, on the shape the owner actually hit:
    /// a session holding TWO muxers (the zero-copy attempt and the fallback nobody was
    /// watching), its FIFOs, an empty temp and one with a real take in it. Everything must
    /// go, and the take must survive.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wedged_session_is_torn_down_completely_and_its_take_recovered() {
        let run = tmp("abandon-run");
        let cap = tmp("abandon-cap");
        let empty = cap.join(".abandoned-empty.recording.mkv");
        let take = cap.join(".abandoned-take.recording.mkv");
        std::fs::write(&empty, vec![0u8; 900]).unwrap(); // bare header, nothing to lose
        grown(&take); // 1.4MB of real content, in the field's words
        let mic = run.join("cosmic-capture-kit.4242.0.micmix.pcm");
        let sys = run.join("cosmic-capture-kit.4242.0.sysmix.pcm");
        std::fs::write(&mic, b"").unwrap();
        std::fs::write(&sys, b"").unwrap();
        // Two muxers, as a zero-copy decline leaves behind — each with its temp in its argv,
        // which is what `is_our_muxer` matches on for the real ffmpeg.
        let spawn_stand_in = |temp: &Path| {
            let child = std::process::Command::new("tail")
                .arg("-f")
                .arg("/dev/null")
                .arg(temp)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            // `spawn` returns before the exec replaces the argv (see the orphan test).
            let ready = Instant::now() + Duration::from_secs(10);
            while !is_our_muxer(child.id(), std::slice::from_ref(&temp.to_path_buf())) {
                assert!(Instant::now() < ready, "the muxer stand-in never exec'd");
                std::thread::sleep(Duration::from_millis(10));
            }
            child
        };
        let mut first = spawn_stand_in(&empty);
        let mut second = spawn_stand_in(&take);
        let mut body = ledger_lines(first.id(), &empty, &[&mic, &sys]);
        body.push_str(&ledger_lines(second.id(), &take, &[&mic, &sys]));
        let ledger = write_ledger(&run, 4242, &body);

        let swept = abandon_ledger(&ledger, 4242);

        // Reap the stand-ins BEFORE asserting, so a failed assert cannot leak a process
        // holding the harness's inherited pipes.
        let first_status = super::super::wait_or_kill(&mut first, Duration::from_secs(5)).unwrap();
        let second_status = super::super::wait_or_kill(&mut second, Duration::from_secs(5)).unwrap();

        assert_eq!(swept.killed.len(), 2, "BOTH muxers go — a partial teardown is the bug");
        assert!(!first_status.success() && !second_status.success(), "they really died");
        assert!(!mic.exists() && !sys.exists(), "the session's FIFOs go with it");
        assert_eq!(swept.recovered.len(), 1, "the take is recovered, never deleted");
        assert!(swept.recovered[0].file_name().unwrap().to_str().unwrap().ends_with("-recovered.mkv"));
        assert!(swept.recovered[0].exists(), "and it is really there");
        assert_eq!(swept.discarded, vec![empty], "the header-only temp is discarded");
        assert!(!ledger.exists(), "the ledger names nothing outstanding any more");
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }

    /// Abandoning is best-effort and must never panic on a session that left nothing —
    /// a wedge before the first muxer spawn is exactly when the ledger is empty or absent.
    #[test]
    fn abandoning_a_session_that_named_nothing_is_a_no_op() {
        let run = tmp("abandon-empty");
        let swept = abandon_ledger(&run.join("cosmic-capture-kit.1.session"), 1);
        assert_eq!(swept, Swept::default());
        let _ = std::fs::remove_dir_all(&run);
    }

    /// The mirror image, and the one that would cost a user their work: an UNRELATED live
    /// process whose pid a dead ledger happens to name must not be signalled.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_live_process_that_is_not_our_muxer_is_never_killed() {
        let run = tmp("innocent-run");
        let cap = tmp("innocent-cap");
        let temp = cap.join(".someone-elses.recording.mkv");
        grown(&temp);
        // Same shape as the orphan above, except its argv has nothing to do with our temp
        // — i.e. a recycled pid.
        let mut innocent = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let dead_owner = {
            let mut c = std::process::Command::new("true").spawn().unwrap();
            c.wait().unwrap();
            c.id()
        };
        write_ledger(&run, dead_owner, &ledger_lines(innocent.id(), &temp, &[]));

        let swept = sweep_in(run.to_str().unwrap(), Some(&cap), 1, true, &crate::instance::pid_is_live);

        let survived = innocent.try_wait().unwrap().is_none();
        let _ = innocent.kill();
        let _ = innocent.wait();
        assert!(swept.killed.is_empty(), "an unrelated process must survive");
        assert!(survived, "it is still running");
        let _ = std::fs::remove_dir_all(&run);
        let _ = std::fs::remove_dir_all(&cap);
    }
}
