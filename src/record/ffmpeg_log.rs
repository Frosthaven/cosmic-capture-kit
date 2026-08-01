//! Keeping ffmpeg's last words (DRAGON-432).
//!
//! ## Why this exists
//!
//! Every recording in this app is muxed (and on the CPU paths encoded) by an `ffmpeg` child,
//! and ffmpeg says exactly what went wrong on **stderr** — "Unknown encoder", "No space left
//! on device", "Invalid argument", the codec that refused the pixel format. We threw all of
//! it away. When a recording failed, the reason we recorded was our own outside-in guess
//! ("muxer exited 1"), while the actual answer had been printed and discarded.
//!
//! ## The shape, and why it is this shape
//!
//! Piping a child's stderr is not free: the pipe holds ~64KB, and ffmpeg is CHATTY (it
//! prints a progress line per second). A piped-but-undrained stderr therefore fills and then
//! **blocks ffmpeg's writes**, which would wedge every recording longer than a few seconds —
//! a far worse bug than the silence being fixed. So the pipe is drained continuously, by a
//! thread of its own, for the child's whole life.
//!
//! That thread is also why this module is only a ring and a parser. The recording pipeline's
//! rules are strict and they are not negotiable here: the pump's control thread does NO
//! blocking I/O, and nothing in the record path waits unboundedly. A drain thread satisfies
//! both — it blocks on a read that nobody else is waiting for, and it ends by itself when the
//! pipe EOFs, which the child's exit guarantees.
//!
//! Memory is bounded twice over ([`StderrTail`]): by lines and by bytes. An ffmpeg that
//! prints a warning per frame for an hour must cost the same as one that prints nothing.
//!
//! ## What is done with it
//!
//! * A HEALTHY end drops the ring. Nothing is written anywhere, and a session that worked is
//!   byte-identical to before this ticket.
//! * An UNHEALTHY end (a nonzero exit, a watchdog kill, a stop-tail death that salvages)
//!   writes the tail verbatim to the DRAGON-419 debug log — that log is the sink, and this
//!   ticket adds no new file to disk.
//! * Where a reason string reaches the USER ([`crate::diag::Failure::RecordingFailed`]), it
//!   gets ONE line ([`likely_error_line`]), never the dump. A dialog full of ffmpeg banner
//!   text explains nothing to the person reading it.
//!
//! ## Privacy — read this before touching [`StderrTail::push_line`]
//!
//! The debug log is emailed to us, and `diag`'s rule is absolute: no paths, no filenames, no
//! usernames. ffmpeg's commonest fatal lines are made of exactly those — "Error opening output
//! file …", "… : Permission denied" — so a verbatim tail is the worst possible thing to write
//! there. Paths are therefore redacted at the ring's ENTRANCE
//! ([`crate::diag::redact_paths`]), not at its four exits: the ring simply never holds one,
//! so the log block, the extracted hint, and anything added later are all safe by
//! construction rather than by remembering.
//!
//! One consequence, stated so it is a choice and not an accident: the user's DIALOG shows the
//! redacted line too. It could legitimately keep the path (it never leaves their machine), but
//! there is exactly one reason string — `note_failure` records it and `fail_session` reads
//! that same recording back — and splitting it would mean a second failure detail channel,
//! which is the thing CLAUDE.md forbids. The path is the one part of a recording failure the
//! user already knows; the sentence is the part they do not.
//!
//! ## A known asymmetry
//!
//! The zero-copy worker inlines its own stop tail instead of calling
//! `owned::run_video_stop_tail`, so it gets the debug-log half (through `wait_or_kill`, which
//! every record-path ffmpeg is reaped by) but NOT the one-line enrichment of its reason
//! string. Deliberate: that path cannot be compiled on the machines this has been worked on,
//! and a blind edit there is worse than a documented gap.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// How many lines of stderr to keep. Generous enough that the real error is still in the
/// window after ffmpeg's startup banner (which is dozens of lines on a normal build), small
/// enough that it cannot matter.
pub const MAX_LINES: usize = 500;

/// A second, independent cap: total bytes retained. Lines have no length limit of their own —
/// a filter-graph error can be enormous — so the line count alone does not bound memory.
pub const MAX_BYTES: usize = 64 * 1024;

/// How many trailing lines go into the debug log on a failure. The tail is where ffmpeg's
/// verdict lives; the head is its banner.
pub const LOG_TAIL_LINES: usize = 40;

/// The last lines a child printed to stderr, bounded in both directions.
///
/// Oldest lines are dropped first, and the count of what was dropped is kept — a log that
/// silently shows the last 500 of 40,000 lines invites the wrong conclusion about what
/// ffmpeg was doing.
#[derive(Debug, Default)]
pub struct StderrTail {
    lines: VecDeque<String>,
    bytes: usize,
    dropped: u64,
}

impl StderrTail {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one line, evicting oldest lines until BOTH caps hold.
    ///
    /// A single line longer than [`MAX_BYTES`] is truncated rather than rejected: an enormous
    /// filter-graph error is precisely the case worth keeping, and dropping it entirely to
    /// honour a byte cap would throw away the answer to stay tidy.
    ///
    /// THIS IS THE PRIVACY BOUNDARY. Paths are redacted HERE, on the way in, so that nothing
    /// downstream can leak one by forgetting: [`Self::log_block`], [`Self::lines`] and
    /// [`likely_error_line`] all read a ring that never held a path in the first place. The
    /// ring has four sinks and one entrance, and one entrance is the only kind of guarantee
    /// worth having (see [`crate::diag::redact_paths`] for why ffmpeg makes this necessary).
    pub fn push_line(&mut self, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        let mut owned = crate::diag::redact_paths(line);
        if owned.len() > MAX_BYTES {
            // `String::truncate` PANICS unless the index is a char boundary, and a stderr line
            // is arbitrary UTF-8 — a non-ASCII filename in a filter error is all it takes. A
            // panic HERE kills the drain thread, which fills the stderr pipe, which blocks
            // ffmpeg's writes and deadlocks the recording; it also poisons the ring's mutex.
            // So walk back to a boundary rather than trusting the byte index.
            let mut cut = MAX_BYTES;
            while cut > 0 && !owned.is_char_boundary(cut) {
                cut -= 1;
            }
            owned.truncate(cut);
            owned.push_str(" …[truncated]");
        }
        self.bytes += owned.len();
        self.lines.push_back(owned);
        while self.lines.len() > MAX_LINES || (self.bytes > MAX_BYTES && self.lines.len() > 1) {
            if let Some(old) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.len());
                self.dropped += 1;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// How many lines were evicted to stay inside the caps. The production path reads the
    /// field directly in [`Self::log_block`]; this accessor exists for the tests that pin the
    /// eviction rules.
    #[cfg(test)]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<&str> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).map(String::as_str).collect()
    }

    /// The tail as one block for the debug log, prefixed with a note when lines were dropped
    /// so nobody reads a truncated window as the whole run.
    pub fn log_block(&self, n: usize) -> String {
        let mut s = String::new();
        if self.dropped > 0 {
            s.push_str(&format!("[{} earlier line(s) dropped]\n", self.dropped));
        }
        s.push_str(&self.tail(n).join("\n"));
        s
    }

    /// All retained lines, for [`likely_error_line`].
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }
}

/// Whether a stderr line is ffmpeg's per-second PROGRESS noise rather than something it is
/// telling us. These dominate the ring on a long recording and never explain anything.
fn is_progress_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("frame=") || t.starts_with("size=") || t.starts_with("video:")
}

/// Whether a line looks like ffmpeg reporting a real problem.
///
/// Deliberately a list of SHAPES rather than a grammar: ffmpeg's diagnostics come from
/// hundreds of components with no common format, and the only thing they share is vocabulary.
/// Matching too widely is the safe direction here — the worst case is a slightly wrong line in
/// a dialog that already says the recording failed.
fn looks_like_error(line: &str) -> bool {
    const NEEDLES: [&str; 14] = [
        "error",
        "invalid",
        "no such file",
        "permission denied",
        "unknown encoder",
        "unknown decoder",
        "not supported",
        "unsupported",
        "cannot ",
        "could not",
        "unable to",
        "failed",
        "no space left",
        "conversion failed",
    ];
    let lower = line.to_ascii_lowercase();
    NEEDLES.iter().any(|n| lower.contains(n))
}

/// The ONE line most likely to explain why this ffmpeg run failed, for the reason string a
/// user can end up reading.
///
/// The LAST error-shaped line wins: ffmpeg reports the proximate cause first and the
/// consequence last ("Conversion failed!"), but the consequence is generic while the specific
/// line above it is the answer — so a bare summary is skipped when something more specific
/// precedes it. Progress lines are never candidates.
///
/// `None` when nothing looked like an error, which is normal for a child killed by our own
/// watchdog: it was healthy and mid-sentence, and inventing a cause from its progress output
/// would be worse than saying nothing.
pub fn likely_error_line(lines: &[&str]) -> Option<String> {
    // Generic summaries ffmpeg ends with. Useful only when nothing better exists.
    const SUMMARIES: [&str; 2] = ["conversion failed", "error occurred"];
    let candidates: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !is_progress_line(l) && looks_like_error(l))
        .collect();
    let specific = candidates
        .iter()
        .rev()
        .find(|l| !SUMMARIES.iter().any(|s| l.to_ascii_lowercase().contains(s)));
    let chosen = specific.copied().or_else(|| candidates.last().copied())?;
    Some(tidy_error_line(chosen))
}

/// Trim an ffmpeg line down to what a person can use: drop the `[component @ 0x…]` prefix
/// (an address means nothing to a reader) and cap the length so a dialog stays a dialog.
fn tidy_error_line(line: &str) -> String {
    let mut s = line.trim();
    // `[libx264 @ 0x55f1c0] height not divisible by 2` → the message after the bracket.
    if s.starts_with('[')
        && let Some(end) = s.find("] ")
    {
        s = s[end + 2..].trim();
    }
    let mut out = s.to_string();
    const MAX: usize = 200;
    if out.chars().count() > MAX {
        out = out.chars().take(MAX).collect::<String>();
        out.push('…');
    }
    out
}

// ── The live side: draining a child's stderr without wedging it ──────────────
//
// Keyed by PID rather than threaded through every signature, and that is a deliberate
// trade. The alternative — returning a handle from `spawn_recording_muxer` — would change
// the shape of both recording muxers' spawn paths, one of which (`zero-copy`) cannot be
// compiled on the machine this was written on. A pid-keyed side table costs one lookup at
// two reporting sites and lets the feature-gated path inherit the whole mechanism with no
// blind edit. `record::recover::note_muxer` already registers the same child by pid a line
// away, so the idiom is the file's own.
//
// One recording runs at a time in this one-shot process, and entries are removed when read,
// so the table holds at most a handful and never grows.

/// Live stderr rings, by child pid.
static TAILS: Mutex<Option<HashMap<u32, Arc<Mutex<StderrTail>>>>> = Mutex::new(None);

/// The drain threads, by child pid, so [`report`] can let one finish before reading its ring.
///
/// Without this the killed-muxer case RACES its own evidence: `child.kill()` is asynchronous,
/// the drain is still reading the last lines out of the pipe, and reporting immediately prints
/// a ring that is missing exactly the words the kill was trying to explain.
static DRAINS: Mutex<Option<HashMap<u32, std::thread::JoinHandle<()>>>> = Mutex::new(None);

/// How long [`report`] will wait for a drain thread to finish before reading its ring.
///
/// BOUNDED, per DRAGON-118 — nothing in the record path waits unboundedly. A drain that is
/// still going after this is abandoned (the handle is dropped, the thread ends on its own at
/// pipe EOF) and we report whatever landed, which is strictly more than reporting instantly.
const DRAIN_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The one-line explanation [`report`] extracted, kept for the caller that builds the reason
/// string a user will read.
///
/// Two stages because two different places need the two halves, and neither can pass a value
/// to the other: `wait_or_kill` is where every reap happens (so it does the LOGGING), while
/// the stop tail a level up is where the `Result` that becomes the user's message is built.
/// Changing either signature would mean editing feature-gated callers blind, so the hint
/// waits here under the pid instead.
static HINTS: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

fn with_table<R>(f: impl FnOnce(&mut HashMap<u32, Arc<Mutex<StderrTail>>>) -> R) -> Option<R> {
    let mut g = TAILS.lock().ok()?;
    Some(f(g.get_or_insert_with(HashMap::new)))
}

/// Start draining `child`'s stderr into a bounded ring, on a thread of its own.
///
/// MUST be called for every child spawned with `Stdio::piped()` stderr, immediately after
/// the spawn. A piped-but-undrained stderr fills its ~64KB pipe and then BLOCKS ffmpeg's
/// writes — which, for a muxer we are simultaneously feeding frames to on stdin, is a
/// deadlock and would wedge every recording longer than about a minute. That is a far worse
/// failure than the silence this ticket removes, so the drain is not optional and is not
/// conditional.
///
/// The thread ends by itself when the pipe EOFs, which the child's exit guarantees, so it
/// can never outlive the reap and nothing here waits unboundedly. It holds no lock while
/// blocked on the read.
pub fn attach(child: &mut std::process::Child) {
    let Some(mut stderr) = child.stderr.take() else {
        return;
    };
    let pid = child.id();
    let ring = Arc::new(Mutex::new(StderrTail::new()));
    with_table(|t| t.insert(pid, ring.clone()));
    let spawned = std::thread::Builder::new()
        .name(format!("cck-ffmpeg-stderr-{pid}"))
        .spawn(move || {
            use std::io::{BufRead, BufReader};
            let mut reader = BufReader::new(&mut stderr);
            loop {
                // Bytes, not `read_line`: ffmpeg's progress output is not always valid
                // UTF-8 (a filename can be anything), and one bad byte must not end the
                // drain — ending it early is what re-introduces the wedge.
                //
                // BOUNDED, via `take`: plain `read_until` grows its buffer without limit, and
                // ffmpeg ends each progress update with `\r` and NO newline — so an hour of
                // recording is a single "line" to a newline-delimited reader, and the ring's
                // own caps would never get a chance to apply. Anything past the cap simply
                // becomes the next chunk, which the ring then evicts as usual.
                let mut raw = Vec::new();
                let mut limited = std::io::Read::take(&mut reader, MAX_BYTES as u64);
                match limited.read_until(b'\n', &mut raw) {
                    Ok(0) | Err(_) => break, // EOF, or the pipe broke: the child is gone
                    Ok(_) => {}
                }
                if let Ok(mut r) = ring.lock() {
                    r.push_line(&String::from_utf8_lossy(&raw));
                }
            }
        });
    match spawned {
        Ok(handle) => {
            let _ = DRAINS
                .lock()
                .map(|mut g| g.get_or_insert_with(HashMap::new).insert(pid, handle));
        }
        Err(_) => {
            // No drain thread means a pipe nobody empties, which WOULD wedge the muxer — so
            // this deliberately gives up the pipe instead. Dropping `stderr` (owned by the
            // closure that never ran) closes our read end, and ffmpeg then dies on SIGPIPE /
            // EPIPE at its next stderr write. That is a lost recording rather than a hung one,
            // which is the right way to lose: a wedged muxer takes the whole session down with
            // it and explains nothing. Failing to spawn a thread at all means the process is
            // already out of resources, so this is the end of the line either way.
            log::warn!(
                "DRAGON-432: could not start the ffmpeg stderr drain for pid {pid}; \
                 giving up the pipe (ffmpeg will fail on its next stderr write)"
            );
            with_table(|t| t.remove(&pid));
        }
    }
}

/// Give the drain thread for `pid` a BOUNDED moment to finish before its ring is read.
///
/// Called only on the reporting path. A drain that has not finished within [`DRAIN_SETTLE`] is
/// detached — dropping a `JoinHandle` does not stop the thread, and it ends by itself at pipe
/// EOF, so nothing leaks and nothing waits unboundedly.
fn settle(pid: u32) {
    let handle = DRAINS.lock().ok().and_then(|mut g| g.as_mut()?.remove(&pid));
    let Some(handle) = handle else { return };
    let deadline = std::time::Instant::now() + DRAIN_SETTLE;
    while !handle.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

/// Take a child's ring, removing it from the table. `None` when nothing was attached.
fn take(pid: u32) -> Option<Arc<Mutex<StderrTail>>> {
    with_table(|t| t.remove(&pid)).flatten()
}

/// Drop a child's ring without reading it — the HEALTHY end. Nothing is written anywhere.
pub fn discard(pid: u32) {
    // No settle: this end is not being explained, so waiting for the last few lines of a
    // healthy run would be time spent to throw them away. The handle is dropped (the thread
    // ends on its own at pipe EOF) so the table cannot grow.
    let _ = DRAINS.lock().map(|mut g| g.as_mut().map(|t| t.remove(&pid)));
    if let Some(ring) = take(pid)
        && let Ok(r) = ring.lock()
        && r.len() > 0
    {
        // Debug level only: a working recording says nothing, but the count is useful when
        // someone is reading a log to see the drain was actually running.
        log::debug!("DRAGON-432: ffmpeg pid {pid} ended healthy; dropping {} stderr line(s)", r.len());
    }
}

/// Report a child's stderr on an UNHEALTHY end: the tail verbatim into the DRAGON-419 debug
/// log (that log is the sink — this ticket adds no new file to disk), and the one line worth
/// showing a user returned for the caller's reason string.
///
/// Consumes the ring, so a second call after this one finds nothing and says nothing. Any
/// hint it extracts is left under the pid for [`take_hint`].
///
/// LEVEL: `warn`, not `error`. An unhealthy ffmpeg is not the same thing as a failed
/// recording — a stop-tail-only death whose temp file is sound gets SALVAGED, and the user
/// ends up with their recording. Logging the tail at `error` made a session that WORKED shout
/// in the customer's debug log. The verdict is `diag::note_failure`'s to record (and it warns
/// too); this line is the evidence behind it. Nothing is lost by the level: the file sink
/// takes this crate from `debug` up, and stderr's default filter is `warn`.
pub fn report(pid: u32, context: &str) {
    // Let the drain catch up first — see [`settle`]. The killed-muxer case otherwise reports a
    // ring that is missing the very lines the kill needs explaining.
    settle(pid);
    let Some(ring) = take(pid) else { return };
    let Ok(guard) = ring.lock() else { return };
    if guard.len() == 0 {
        return;
    }
    log::warn!(
        "DRAGON-432: ffmpeg ({context}, pid {pid}) stderr tail:\n{}",
        guard.log_block(LOG_TAIL_LINES)
    );
    let lines: Vec<&str> = guard.lines().collect();
    if let Some(hint) = likely_error_line(&lines) {
        let _ = HINTS
            .lock()
            .map(|mut g| g.get_or_insert_with(HashMap::new).insert(pid, hint));
    }
}

/// The one-line explanation [`report`] extracted for `pid`, if any. Consumed on read.
pub fn take_hint(pid: u32) -> Option<String> {
    HINTS.lock().ok()?.as_mut()?.remove(&pid)
}

/// Append an ffmpeg-supplied explanation to a reason string, when one could be extracted.
///
/// The reason strings this enriches (`"ffmpeg exited with {status}"`) reach the USER through
/// `Failure::RecordingFailed`, which is the one failure whose detail is shown verbatim. So it
/// gets ONE line, never the dump: a dialog full of ffmpeg banner text explains nothing.
///
/// No em-dash in the joiner: this lands in a dialog, and the house copy rule the
/// `alert_message` sweeps enforce applies to anything that gets there.
pub fn with_hint(reason: String, hint: Option<String>) -> String {
    match hint {
        Some(h) if !h.trim().is_empty() => format!("{reason}. ffmpeg reported: {}", h.trim()),
        _ => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail_of(lines: &[&str]) -> StderrTail {
        let mut t = StderrTail::new();
        for l in lines {
            t.push_line(l);
        }
        t
    }

    // ── The ring stays bounded ────────────────────────────────────────────────

    #[test]
    fn the_ring_keeps_only_the_last_lines_and_counts_what_it_dropped() {
        let mut t = StderrTail::new();
        for i in 0..(MAX_LINES + 250) {
            t.push_line(&format!("line {i}"));
        }
        assert_eq!(t.len(), MAX_LINES);
        assert_eq!(t.dropped(), 250);
        // The NEWEST lines survive — the verdict is at the end of an ffmpeg run.
        let last = t.tail(1);
        assert_eq!(last, vec![format!("line {}", MAX_LINES + 249)]);
    }

    /// The byte cap is the one that actually bounds memory: 500 lines of a megabyte each
    /// would otherwise be half a gigabyte. An ffmpeg that prints a huge warning per frame for
    /// an hour must cost the same as one that prints nothing.
    #[test]
    fn the_byte_cap_bounds_memory_independently_of_the_line_cap() {
        let mut t = StderrTail::new();
        let fat = "x".repeat(8 * 1024);
        for _ in 0..200 {
            t.push_line(&fat);
        }
        assert!(t.len() < MAX_LINES, "line cap alone would have kept all 200");
        let retained: usize = t.lines().map(str::len).sum();
        assert!(retained <= MAX_BYTES + fat.len(), "retained {retained} bytes");
        assert!(t.dropped() > 0);
    }

    /// One line bigger than the whole budget is truncated, never dropped — an enormous
    /// filter-graph error is exactly the line worth keeping.
    #[test]
    fn a_single_oversized_line_is_truncated_rather_than_lost() {
        let mut t = StderrTail::new();
        t.push_line(&"y".repeat(MAX_BYTES * 2));
        assert_eq!(t.len(), 1);
        assert!(t.tail(1)[0].ends_with("…[truncated]"));
    }

    /// THE PANIC TEST. `String::truncate` panics on a non-char-boundary index, and the test
    /// above cannot catch it because ASCII makes every index a boundary. A multi-byte line
    /// past the cap is not exotic — one accented character in a filter error is enough.
    ///
    /// The consequence was not a lost log line: the panic killed the DRAIN THREAD, so the
    /// stderr pipe filled, ffmpeg blocked on its next write, and the recording deadlocked
    /// against a muxer nobody was reading. It also poisoned the ring's mutex, so every later
    /// `lock()` failed silently.
    #[test]
    fn an_oversized_line_of_multibyte_text_does_not_panic() {
        // Every character is 3 bytes, so the cap lands mid-character by construction.
        let mut t = StderrTail::new();
        t.push_line(&"日".repeat(MAX_BYTES));
        assert_eq!(t.len(), 1);
        let kept = t.tail(1)[0];
        assert!(kept.ends_with("…[truncated]"), "{}", &kept[kept.len().saturating_sub(40)..]);
        // Truncated to a boundary means it is still valid UTF-8 and still readable.
        assert!(kept.starts_with('日'));
        assert!(kept.len() <= MAX_BYTES + " …[truncated]".len());

        // The same again with the boundary at every offset in a 4-byte character, so this
        // cannot pass by luck on one alignment.
        for pad in 0..4 {
            let mut t = StderrTail::new();
            let line = format!("{}{}", "a".repeat(pad), "🎬".repeat(MAX_BYTES / 4 + 8));
            t.push_line(&line);
            assert_eq!(t.len(), 1, "pad {pad}");
        }
    }

    /// THE PRIVACY TEST for the ring (DRAGON-432 follow-up). ffmpeg names the output file on
    /// its commonest fatal lines, and the ring feeds a log that customers email us — so a path
    /// must not survive the trip IN, which is the only place that can guarantee it.
    #[test]
    fn a_path_never_enters_the_ring() {
        let mut t = StderrTail::new();
        t.push_line("[out#0/mp4 @ 0x1] Error opening output file /home/jane/Capture/Q4 plan.mp4");
        t.push_line("/home/jane/Capture/Q4 plan.mp4: Permission denied");
        let block = t.log_block(LOG_TAIL_LINES);
        for secret in ["jane", "Capture", "Q4", "plan"] {
            assert!(!block.contains(secret), "leaked {secret:?}: {block}");
        }
        // And the line a USER would be shown comes from the same redacted content.
        let lines: Vec<&str> = t.lines().collect();
        let hint = likely_error_line(&lines).unwrap();
        assert!(!hint.contains("jane"), "{hint}");
        // The diagnosis itself survives — redaction must not cost the error vocabulary.
        assert!(block.contains("Permission denied"), "{block}");
        assert!(block.contains("Error opening output file"), "{block}");
    }

    #[test]
    fn line_endings_are_normalised_away() {
        let t = tail_of(&["plain", "windows\r\n", "unix\n"]);
        assert_eq!(t.tail(3), vec!["plain", "windows", "unix"]);
    }

    #[test]
    fn the_log_block_says_when_it_is_a_window_not_the_whole_run() {
        let mut t = StderrTail::new();
        for i in 0..(MAX_LINES + 5) {
            t.push_line(&format!("l{i}"));
        }
        let block = t.log_block(3);
        assert!(block.starts_with("[5 earlier line(s) dropped]"), "{block}");
        // A run that fit entirely gets no such note.
        let small = tail_of(&["a", "b"]);
        assert_eq!(small.log_block(10), "a\nb");
    }

    // ── Picking the line a human can use ──────────────────────────────────────

    #[test]
    fn the_specific_cause_beats_the_generic_summary() {
        // Real ffmpeg shape for a bad encoder parameter: the component's own complaint, then
        // per-second noise, then an "Error initializing…" line, then "Conversion failed!".
        // The summary is LAST but says nothing, so the specific line must win.
        let lines = [
            "ffmpeg version 6.1.1 Copyright (c) 2000-2023",
            "[libx264 @ 0x55f1c0] height not divisible by 2 (1920x1081)",
            "frame=  120 fps= 30 q=28.0 size=    512kB",
            "[vost#0:0/libx264 @ 0x55f200] Error initializing output stream: maybe incorrect \
             parameters such as bit_rate, rate, width or height",
            "Conversion failed!",
        ];
        let got = likely_error_line(&lines).unwrap();
        assert!(got.starts_with("Error initializing output stream"), "{got}");
        assert!(!got.contains("Conversion failed"), "{got}");
    }

    /// A KNOWN LIMIT of the heuristic, written down rather than left to be discovered.
    ///
    /// The picker keys on error VOCABULARY, so a cause line that happens to use none of it
    /// ("height not divisible by 2") is not a candidate, and the generic summary is all that
    /// is left for the one-line reason. That is an acceptable trade: the alternative is
    /// guessing from position, which would put arbitrary informational lines in a dialog.
    /// Nothing is lost either way — the FULL tail still goes to the debug log, where the
    /// specific line is right there.
    #[test]
    fn a_cause_with_no_error_vocabulary_falls_back_to_the_summary() {
        let lines = [
            "[libx264 @ 0x55f1c0] height not divisible by 2 (1920x1081)",
            "Conversion failed!",
        ];
        assert_eq!(likely_error_line(&lines).as_deref(), Some("Conversion failed!"));
    }

    #[test]
    fn a_summary_is_used_when_it_is_all_there_is() {
        let lines = ["frame=  10 fps= 30", "Conversion failed!"];
        assert_eq!(likely_error_line(&lines).as_deref(), Some("Conversion failed!"));
    }

    #[test]
    fn the_component_prefix_and_its_address_are_stripped() {
        // `0x55f1c0` is meaningless to a reader and changes every run, so it must never
        // reach a dialog.
        let lines = ["[out#0/mp4 @ 0x7f2a] Error opening output file /x/y.mp4"];
        assert_eq!(
            likely_error_line(&lines).as_deref(),
            Some("Error opening output file /x/y.mp4")
        );
    }

    #[test]
    fn common_real_ffmpeg_failures_are_recognised() {
        for (line, want) in [
            ("[NULL @ 0x1] Unknown encoder 'h264_nvenc'", "Unknown encoder 'h264_nvenc'"),
            ("av_interleaved_write_frame(): No space left on device", "av_interleaved_write_frame(): No space left on device"),
            ("/tmp/x.mp4: Permission denied", "/tmp/x.mp4: Permission denied"),
            ("[vost#0:0 @ 0x2] Could not open encoder before EOF", "Could not open encoder before EOF"),
        ] {
            assert_eq!(likely_error_line(&[line]).as_deref(), Some(want), "{line}");
        }
    }

    /// A child our own watchdog killed was healthy and mid-sentence. Inventing a cause from
    /// its progress output would be worse than saying nothing, so nothing is what it says.
    #[test]
    fn a_healthy_run_with_no_error_offers_no_line() {
        let lines = [
            "ffmpeg version 6.1.1 Copyright (c) 2000-2023",
            "  libavutil      58. 29.100",
            "frame=  900 fps= 30 q=28.0 size=   4096kB time=00:00:30.00",
        ];
        assert_eq!(likely_error_line(&lines), None);
        assert_eq!(likely_error_line(&[]), None);
        assert_eq!(likely_error_line(&["", "   "]), None);
    }

    /// Progress lines can contain the substring "error" in `-stats` output on some builds;
    /// they are still never the answer.
    #[test]
    fn progress_lines_are_never_chosen() {
        let lines = ["frame=  10 fps=30 q=-1.0 size=1kB error=0.00", "size=       2kB"];
        assert_eq!(likely_error_line(&lines), None);
    }

    // ── Enriching the reason string a user reads ──────────────────────────────

    #[test]
    fn a_hint_is_appended_only_when_there_is_one() {
        let base = "ffmpeg exited with exit status: 1".to_string();
        assert_eq!(with_hint(base.clone(), None), base);
        assert_eq!(with_hint(base.clone(), Some(String::new())), base);
        assert_eq!(with_hint(base.clone(), Some("   ".into())), base);
        assert_eq!(
            with_hint(base.clone(), Some("No space left on device".into())),
            "ffmpeg exited with exit status: 1. ffmpeg reported: No space left on device"
        );
    }

    /// This string reaches a DIALOG through `Failure::RecordingFailed`, whose detail is the
    /// one that is shown verbatim. The house rule the `alert_message` sweeps enforce applies
    /// to it, and those sweeps cannot see it because it is built out here.
    #[test]
    fn an_enriched_reason_obeys_the_dialog_copy_rules() {
        let out = with_hint(
            "ffmpeg exited with exit status: 1".into(),
            Some("[libx264 @ 0x1] Invalid argument".into()),
        );
        assert!(!out.contains('—'), "{out}");
        assert!(!out.contains('\n'), "a dialog reason stays one line: {out}");
    }

    #[test]
    fn an_enormous_error_line_is_capped_for_display() {
        let long = format!("[f @ 0x1] Invalid argument: {}", "z".repeat(1000));
        let got = likely_error_line(&[&long]).unwrap();
        assert!(got.chars().count() <= 201, "{} chars", got.chars().count());
        assert!(got.ends_with('…'));
        assert!(got.starts_with("Invalid argument:"));
    }
}
