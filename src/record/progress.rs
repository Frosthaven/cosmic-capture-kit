//! The SESSION-level bound on a recording (DRAGON-423): a session that stops making
//! progress ends itself instead of hanging forever.
//!
//! ## Why this exists when everything was already bounded
//!
//! DRAGON-118 bounded every individual wait in the record path, and DRAGON-421 confirmed
//! each one still fires: [`super::MuxerWatchdog`] (12s, pause-gated) before the first video
//! write, the pump's FIFO-open rendezvous at 15s, [`super::wait_or_kill`] at 30s in the stop
//! tail. DRAGON-421 then made a session that DIED recoverable — its ffmpeg is kernel-tethered
//! to us, its leftovers are swept.
//!
//! Neither covers a session that is alive and simply not getting anywhere. The owner hit
//! that within minutes of the tether shipping: a two-second capture still running at 58
//! seconds, the preview spinning on its loader, the capture child perfectly healthy. No wait
//! had expired, because the process was not waiting on anything — one worker had given up,
//! cleared the user's `stop` flag, and started a SECOND recording nobody was watching
//! (DRAGON-422). Every bound in the process was inside the part that had gone wrong.
//!
//! So the missing bound is not a longer timeout anywhere. It is a bound at a level that can
//! still observe when the parts stop adding up: **is this recording still advancing?**
//!
//! ## What counts as progress
//!
//! Media time advancing — read from OUTSIDE the worker, off the two files a session is
//! there to produce. [`super::recover`] already draws the same inference in the other
//! direction (a temp untouched for a minute is not a recording in progress).
//!
//! **How coarsely a live temp actually grows, because this was measured and not assumed.**
//! `-flush_packets 1` does not make the file grow byte by byte: Matroska writes in CLUSTERS,
//! and the muxer starts a new one at 2 MB or 5 seconds of media, whichever comes first. A
//! recorder-shaped 8 Mb/s capture was watched growing in steps up to ~6 seconds apart
//! (`wedge_live_tests` measures this on every run rather than trusting the number here). The
//! 5-second cluster limit is the part that matters: it is a bound on the gap that holds
//! however LOW the bitrate goes, and we own CFR pacing, so a session always has packets to
//! cluster. That is what makes "a whole minute of nothing" an honest test — a live recording
//! is an order of magnitude away from it — and it is why the budget cannot be shrunk toward
//! the granularity of the signal it reads.
//!
//! What "advancing" means depends on what the user has asked for, which is the whole point
//! — a session that is busy is not the same as a session that is doing what it was told:
//!
//! * [`Phase::Running`] — the `.recording.mkv` temp grows. Nothing else is expected yet.
//! * [`Phase::Paused`] — nothing at all, and that is correct. The budget is FROZEN, exactly
//!   as [`super::MuxerWatchdog::arm_gated`] freezes its own: the media clock is stopped by
//!   design (DRAGON-125) and a paused session can no more prove liveness here than it can
//!   there. This reuses the existing notion rather than inventing a second one.
//! * [`Phase::Stopping`] — the take moves toward the finished file: the output file appears,
//!   grows, or is merely WRITTEN to (see [`FileState`] — the `+faststart` pass rewrites it
//!   in place at a constant size, and that is a session finishing, not stalling), or the
//!   temp is consumed. **A temp that keeps growing after the user asked to stop is not
//!   progress** — it is new media being captured after the user said stop, which is
//!   precisely the shape that was observed in the field.
//!
//! ## Why the budget is what it is
//!
//! [`SESSION_STALL_SECS`] is deliberately far larger than any bound it backstops, because
//! the two mistakes here are not symmetrical. A hang costs the user a minute and a restart;
//! tearing down a slow-but-honest session costs them a take they cannot record again. So:
//!
//! * A RUNNING session's honest silence is one Matroska cluster — ~5 seconds at worst,
//!   measured above. A whole minute of nothing is not a slow machine, it is a dead pipeline.
//! * A STOPPING session's longest honest silence is the stop tail's own bounded reap
//!   ([`super::wait_or_kill`], 30s), during which neither file changes because ffmpeg is
//!   being waited on. The budget is double that, so a stop tail that is merely riding out
//!   its own worst case is never pre-empted — it gets to finish, salvage and finalize by
//!   itself, which produces a BETTER file than anything this module can.
//!
//! Nothing here lengthens an existing timeout, and nothing here is load-sensitive: the test
//! is "did any bytes move", not "how fast".

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a recording may go without progress before the session is declared wedged.
///
/// See the module doc for why this is 60 and not smaller. In short: the longest honest
/// silence in a stop tail is the 30s bounded reap, and in a running session it is one
/// Matroska cluster (~5s at worst, and measured on every test run) — so this is double the
/// former and an order of magnitude clear of the latter.
pub(crate) const SESSION_STALL_SECS: u64 = 60;

/// The longest honest silence inside a STOP tail: [`super::wait_or_kill`]'s bounded reap,
/// during which neither file changes and the session is behaving perfectly.
const STOP_TAIL_REAP_SECS: u64 = 30;

/// The longest honest silence inside a RUNNING session: one Matroska cluster, which the
/// muxer starts every 2 MB or 5 seconds of media. `wedge_live_tests` measures the real
/// figure on every run and fails if it ever comes near the budget.
const CLUSTER_SECS: u64 = 5;

// Pinned at COMPILE time, in the style of `pump`'s render-horizon assert (DRAGON-411) —
// these relationships are the whole argument for the number above, and a future edit that
// broke one would otherwise only show up as somebody losing a take.
const _: () = assert!(
    SESSION_STALL_SECS >= 2 * STOP_TAIL_REAP_SECS,
    "the session bound must stay at least double the stop tail's own bounded reap \
     (DRAGON-423): with less margin, a stop tail merely riding out its worst case gets \
     pre-empted, and the worker's own salvage — which produces a better file than this \
     bound ever can — never runs"
);

const _: () = assert!(
    SESSION_STALL_SECS >= 10 * CLUSTER_SECS,
    "the session bound must stay an order of magnitude clear of the cluster granularity of \
     the signal it reads (DRAGON-423): a budget near it tears down healthy recordings, \
     which is exactly what happened the first time this was run against a live muxer"
);

const _: () = assert!(
    SESSION_STALL_SECS > super::MUXER_LIVENESS_SECS,
    "the session bound is the OUTER ring: every in-process bound must get to fire first"
);

/// What the session has been asked to do, at the moment of one observation.
///
/// The app decides this from what the USER asked, never from the worker's `stop` flag —
/// a worker is free to clear that flag (the zero-copy decline does exactly that when it
/// falls back), and a session that erased the user's stop is one of the things this bound
/// exists to catch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Capturing. The temp should be growing.
    Running,
    /// Paused by the user: the media clock is frozen and the budget with it.
    Paused,
    /// The user asked to stop (or cancel). The take should be moving toward the file.
    Stopping,
}

/// One file as seen at one poll: how big it is and when it was last written.
///
/// SIZE alone is not enough, and the case that proves it is the one at the very end of a
/// long recording. `finalize` muxes with `-movflags +faststart`, whose final pass shifts the
/// whole file IN PLACE to put the index first — minutes of solid writing, on a big take and
/// a slow disk, during which the size never changes by a single byte. That is a session
/// working hard on exactly what the user asked for, and judging it by size would tear it
/// down at the last moment. So a WRITE counts, whether or not it made the file bigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileState {
    len: u64,
    written: std::time::SystemTime,
}

/// The two files a recording session produces, as seen at one poll. `None` means the file
/// does not exist (yet, or any more) — both of which are meaningful.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    /// The `.recording.mkv` temp the muxer writes.
    temp: Option<FileState>,
    /// The finished recording finalize produces from it.
    out: Option<FileState>,
}

impl Sample {
    /// Stat both files. A path we cannot stat is `None`, which is the same answer as "not
    /// there" — this is a progress signal, not a diagnosis. A filesystem with no mtime
    /// falls back to the epoch, which simply makes size the only signal, as it was before.
    pub(crate) fn read(temp: Option<&Path>, out: Option<&Path>) -> Self {
        let state = |p: Option<&Path>| {
            p.and_then(|p| std::fs::metadata(p).ok()).map(|m| FileState {
                len: m.len(),
                written: m.modified().unwrap_or(std::time::UNIX_EPOCH),
            })
        };
        Self { temp: state(temp), out: state(out) }
    }

    /// The sample a set of `(len, mtime)` pairs would produce — the tests' way in, so they
    /// can drive the state machine without touching a filesystem.
    #[cfg(test)]
    fn of(temp: Option<(u64, u64)>, out: Option<(u64, u64)>) -> Self {
        let state = |v: Option<(u64, u64)>| {
            v.map(|(len, secs)| FileState {
                len,
                written: std::time::UNIX_EPOCH + Duration::from_secs(secs),
            })
        };
        Self { temp: state(temp), out: state(out) }
    }
}

/// A session that has gone [`SESSION_STALL_SECS`] without progress — what phase it was in
/// and how long it has been stuck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stall {
    pub(crate) phase: Phase,
    pub(crate) secs: u64,
}

/// Whether a file appeared, grew, or was WRITTEN TO — any of which is work being done on it.
fn advanced(then: Option<FileState>, now: Option<FileState>) -> bool {
    match (then, now) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => b.len > a.len || b.written > a.written,
    }
}

/// Whether a file shrank or was removed — how a temp being CONSUMED by finalize looks.
fn consumed(then: Option<FileState>, now: Option<FileState>) -> bool {
    match (then, now) {
        (Some(_), None) => true,
        (Some(a), Some(b)) => b.len < a.len,
        _ => false,
    }
}

/// The session-level bound itself: fed one observation per poll, it answers "is this
/// recording still advancing?" and eventually "no, and it has been this long".
///
/// Deliberately a plain state machine over `(now, phase, sample)` with no clock, no
/// filesystem and no threads of its own, so every case below — including the ones that must
/// NOT fire — is exercisable in a unit test at test speed.
pub(crate) struct SessionProgress {
    budget: Duration,
    /// The last observation that counted as progress. The budget is spent from here.
    last_progress: Instant,
    /// What the files looked like then, to compare the next observation against.
    seen: Sample,
    /// The phase the previous observation was in; a change restarts the budget, because
    /// what counts as progress has just changed with it.
    phase: Phase,
}

impl SessionProgress {
    /// A guard for a session starting now.
    pub(crate) fn new(now: Instant) -> Self {
        Self::with_budget(now, Duration::from_secs(SESSION_STALL_SECS))
    }

    /// [`new`](Self::new) with an explicit budget — the tests use a short one, exactly as
    /// [`super::MuxerWatchdog::arm_for`] does, so the "must NOT fire" cases can be run out
    /// well past the deadline without a slow test.
    pub(super) fn with_budget(now: Instant, budget: Duration) -> Self {
        Self { budget, last_progress: now, seen: Sample::default(), phase: Phase::Running }
    }

    /// Feed one observation. `Some(stall)` means the session has gone the whole budget
    /// without progress and should be given up on; `None` means carry on.
    ///
    /// Once it has answered `Some` the caller is expected to end the session, so there is no
    /// latching here — a caller that kept polling would simply keep being told the same
    /// thing.
    pub(crate) fn observe(&mut self, now: Instant, phase: Phase, sample: Sample) -> Option<Stall> {
        if phase != self.phase {
            // The question itself just changed (a pause, a resume, a stop). Start the new
            // phase's budget from here rather than carrying over silence that was correct
            // under the old one.
            self.phase = phase;
            self.seen = sample;
            self.last_progress = now;
            return None;
        }
        let advancing = match phase {
            // Paused: frozen, not stalled. See the module doc.
            Phase::Paused => true,
            Phase::Running => {
                advanced(self.seen.temp, sample.temp) || advanced(self.seen.out, sample.out)
            }
            // Note what is NOT here: the temp advancing. A temp still being written after
            // the user asked to stop is the field defect, not progress.
            Phase::Stopping => {
                advanced(self.seen.out, sample.out) || consumed(self.seen.temp, sample.temp)
            }
        };
        self.seen = sample;
        if advancing {
            self.last_progress = now;
            return None;
        }
        let stuck = now.saturating_duration_since(self.last_progress);
        (stuck >= self.budget).then_some(Stall { phase, secs: stuck.as_secs() })
    }
}

/// The one sentence that goes to BOTH channels — DRAGON-419's debug log (via
/// `diag::note_failure`) and DRAGON-415's alert, which repeats a recording failure's reason
/// verbatim. There is no third channel and no second wording.
///
/// Pure, so the thing the user actually reads is unit-tested. `recovered` carries whatever
/// [`super::recover::abandon_session`] managed to salvage; only the file NAMES are used —
/// a timestamp we minted plus `-recovered.mkv`, which names no user content and so obeys
/// `diag`'s privacy rule while still telling the user where their take went.
pub(crate) fn wedge_detail(stall: &Stall, recovered: &[PathBuf]) -> String {
    let what = match stall.phase {
        Phase::Stopping => format!(
            "the recording did not finish in the {}s after it was asked to stop",
            stall.secs
        ),
        // A pause freezes the budget, so a stall can never be reported against one; both
        // remaining phases are "it stopped advancing while it was supposed to be capturing".
        Phase::Running | Phase::Paused => {
            format!("the recording stopped advancing for {}s", stall.secs)
        }
    };
    let take = match recovered.first().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        Some(name) => format!(
            "what had been recorded is saved as {name} in your capture folder (its mic and \
             system audio are two separate tracks, as this is the pre-finalize capture)"
        ),
        None => "nothing usable had been recorded, so nothing was saved".to_string(),
    };
    format!("{what}, so it was ended: {take}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard on a short budget, and a clock we advance by hand.
    fn guard(budget_ms: u64) -> (SessionProgress, Instant) {
        let t0 = Instant::now();
        (SessionProgress::with_budget(t0, Duration::from_millis(budget_ms)), t0)
    }

    /// A sample from plain sizes. Each distinct size is paired with its own mtime second,
    /// so "the file was written" and "the file grew" move together — the ordinary case. The
    /// two are pulled apart deliberately in `an_in_place_finalize_pass_is_progress`.
    fn sample(temp: Option<u64>, out: Option<u64>) -> Sample {
        Sample::of(temp.map(|n| (n, n)), out.map(|n| (n, n)))
    }

    // ── The wedge: a session that stops advancing ───────────────────────────

    #[test]
    fn a_running_session_whose_temp_stops_growing_is_declared_wedged() {
        let (mut g, t0) = guard(1000);
        // Two honest seconds of capture, then the pipeline dies where it stands.
        assert_eq!(g.observe(t0 + Duration::from_millis(100), Phase::Running, sample(Some(1_000), None)), None);
        assert_eq!(g.observe(t0 + Duration::from_millis(200), Phase::Running, sample(Some(9_000), None)), None);
        assert_eq!(g.observe(t0 + Duration::from_millis(800), Phase::Running, sample(Some(9_000), None)), None);
        let stall = g
            .observe(t0 + Duration::from_millis(1_300), Phase::Running, sample(Some(9_000), None))
            .expect("a minute of a muxer writing nothing is a dead pipeline");
        assert_eq!(stall.phase, Phase::Running);
    }

    /// The field shape (DRAGON-422 as observed in `~/.cck-evidence/wedge-1741`): the user
    /// stopped, a worker cleared the stop and started recording again, and the temp went on
    /// growing forever while the preview span its loader. Busy is not the same as stopping.
    #[test]
    fn a_session_still_recording_after_the_stop_is_declared_wedged() {
        let (mut g, t0) = guard(1000);
        let _ = g.observe(t0 + Duration::from_millis(100), Phase::Running, sample(Some(1_000), None));
        let mut len = 100_000u64;
        let mut last = None;
        for step in 2..20u64 {
            len += 50_000; // the abandoned second session, still muxing away
            last = g.observe(
                t0 + Duration::from_millis(100 * step),
                Phase::Stopping,
                sample(Some(len), None),
            );
        }
        let stall = last.expect("a temp still growing after the stop is not progress");
        assert_eq!(stall.phase, Phase::Stopping);
    }

    #[test]
    fn a_stop_that_never_produces_anything_is_declared_wedged() {
        // Nothing moves at all after the stop — the plainer half of the same failure.
        let (mut g, t0) = guard(1000);
        let _ = g.observe(t0 + Duration::from_millis(100), Phase::Running, sample(Some(50_000), None));
        let _ = g.observe(t0 + Duration::from_millis(200), Phase::Stopping, sample(Some(50_000), None));
        assert!(g
            .observe(t0 + Duration::from_millis(900), Phase::Stopping, sample(Some(50_000), None))
            .is_none());
        assert!(g
            .observe(t0 + Duration::from_millis(1_500), Phase::Stopping, sample(Some(50_000), None))
            .is_some());
    }

    // ── The disasters: sessions that must NEVER be torn down ────────────────

    /// A long recording. This is the case that would cost someone work they cannot redo, so
    /// it is run out to twenty times the budget.
    #[test]
    fn a_long_recording_is_never_declared_wedged() {
        let (mut g, t0) = guard(1000);
        let mut len = 0u64;
        for step in 1..200u64 {
            len += 4_000; // a steady muxer, whatever the wall clock says
            assert_eq!(
                g.observe(
                    t0 + Duration::from_millis(100 * step),
                    Phase::Running,
                    sample(Some(len), None)
                ),
                None,
                "a recording that is still writing must never be given up on"
            );
        }
    }

    /// A paused session writes NOTHING by design (DRAGON-125). It must be able to stay
    /// paused indefinitely — the budget is frozen, not merely generous.
    #[test]
    fn a_paused_session_is_never_declared_wedged() {
        let (mut g, t0) = guard(1000);
        let _ = g.observe(t0 + Duration::from_millis(100), Phase::Running, sample(Some(60_000), None));
        for step in 2..200u64 {
            assert_eq!(
                g.observe(
                    t0 + Duration::from_millis(100 * step),
                    Phase::Paused,
                    sample(Some(60_000), None)
                ),
                None,
                "a pause must never read as a stall"
            );
        }
        // And resuming does not inherit the pause's silence as a debt.
        assert_eq!(
            g.observe(t0 + Duration::from_millis(20_100), Phase::Running, sample(Some(60_000), None)),
            None
        );
    }

    /// A machine so loaded that the muxer only manages a packet every few polls is slow,
    /// not wedged. The test is "did bytes move", never "how fast".
    #[test]
    fn a_crawling_recording_on_a_loaded_machine_is_never_declared_wedged() {
        let (mut g, t0) = guard(1000);
        let mut len = 0u64;
        for step in 1..100u64 {
            // One byte of growth every 9th poll — 900ms apart, just inside the budget.
            if step % 9 == 0 {
                len += 1;
            }
            assert_eq!(
                g.observe(
                    t0 + Duration::from_millis(100 * step),
                    Phase::Running,
                    sample(Some(len), None)
                ),
                None
            );
        }
    }

    /// The honest stop tail: the covering ticks land, the temp is consumed, and finalize
    /// muxes the output. Every one of those is progress.
    #[test]
    fn an_honest_stop_tail_and_finalize_are_never_declared_wedged() {
        let (mut g, t0) = guard(1000);
        let t = |ms: u64| t0 + Duration::from_millis(ms);
        let _ = g.observe(t(100), Phase::Running, sample(Some(500_000), None));
        // The stop: the tail's final covering ticks still grow the temp. That is not
        // counted as progress, but the phase change gives the tail its own fresh budget…
        assert_eq!(g.observe(t(200), Phase::Stopping, sample(Some(520_000), None)), None);
        // …and a bounded reap can then sit silent for most of it without being pre-empted.
        assert_eq!(g.observe(t(1_100), Phase::Stopping, sample(Some(520_000), None)), None);
        // finalize consumes the temp and writes the output: both are progress.
        assert_eq!(g.observe(t(1_150), Phase::Stopping, sample(None, Some(4_096))), None);
        for step in 12..60u64 {
            assert_eq!(
                g.observe(t(100 * step), Phase::Stopping, sample(None, Some(4_096 * step))),
                None,
                "finalize writing its output is a session finishing, not stalling"
            );
        }
    }

    /// The last thing a long recording does is the `+faststart` shift, which rewrites the
    /// output IN PLACE: minutes of solid writing at an unchanging size. Judged on size alone
    /// that reads as a stall, and the take would be torn down at the very last moment.
    #[test]
    fn an_in_place_finalize_pass_is_progress_even_though_the_file_never_grows() {
        let (mut g, t0) = guard(1000);
        let _ = g.observe(t0 + Duration::from_millis(100), Phase::Running, sample(Some(500_000), None));
        // Stopped; finalize has written the whole output and is now shifting it.
        let _ = g.observe(
            t0 + Duration::from_millis(200),
            Phase::Stopping,
            Sample::of(None, Some((4_000_000_000, 100))),
        );
        for step in 3..40u64 {
            // Same size every time; only the mtime moves, because ffmpeg is writing.
            assert_eq!(
                g.observe(
                    t0 + Duration::from_millis(100 * step),
                    Phase::Stopping,
                    Sample::of(None, Some((4_000_000_000, 100 + step))),
                ),
                None,
                "a file being written must count as progress whether or not it grew"
            );
        }
        // And when the writing really does stop, the bound still fires.
        let stuck = Sample::of(None, Some((4_000_000_000, 140)));
        let _ = g.observe(t0 + Duration::from_millis(4_000), Phase::Stopping, stuck);
        assert!(g
            .observe(t0 + Duration::from_millis(5_100), Phase::Stopping, stuck)
            .is_some());
    }

    #[test]
    fn a_session_that_starts_before_its_temp_exists_is_given_the_budget() {
        // The audio pre-flight, the encoder plan and the ffmpeg spawn all happen before
        // any file exists. A missing temp is not growth, so the budget simply runs.
        let (mut g, t0) = guard(1000);
        assert_eq!(g.observe(t0 + Duration::from_millis(500), Phase::Running, sample(None, None)), None);
        assert!(g
            .observe(t0 + Duration::from_millis(1_100), Phase::Running, sample(None, None))
            .is_some());
    }

    // ── The message the user reads ──────────────────────────────────────────

    #[test]
    fn the_detail_names_the_recovered_take_when_there_is_one() {
        let stall = Stall { phase: Phase::Stopping, secs: 62 };
        let recovered =
            vec![PathBuf::from("/home/u/Capture/2026-07-29-17-40-29-056-recovered.mkv")];
        let d = wedge_detail(&stall, &recovered);
        assert!(d.contains("asked to stop"), "{d}");
        assert!(d.contains("2026-07-29-17-40-29-056-recovered.mkv"), "{d}");
        assert!(
            !d.contains("/home/u/"),
            "only the file NAME may be quoted — the detail is written to a log we ask \
             customers to send us: {d}"
        );
    }

    #[test]
    fn the_detail_says_plainly_when_nothing_was_saved() {
        let stall = Stall { phase: Phase::Running, secs: 61 };
        let d = wedge_detail(&stall, &[]);
        assert!(d.contains("stopped advancing"), "{d}");
        assert!(d.contains("nothing was saved"), "{d}");
    }
}
