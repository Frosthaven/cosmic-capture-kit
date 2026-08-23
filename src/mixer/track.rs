//! `Track`: one audio track's media-timeline accumulator (DRAGON-122 phase 3, chunk A).
//! `push` places a captured chunk at the media position of its
//! [`StreamTap::audible_time`](crate::audio::filters::StreamTap) — never `capture_time`,
//! which is a different consumer's base entirely (the future AEC far-end reference; see
//! that type's doc for the dual-timebase invariant) — sample-accurately:
//! `round(media_secs * SAMPLE_RATE)`. Data lives in a `VecDeque<f32>` from the track's
//! rendered horizon forward; frames before it have already been drained by `drain_to` and
//! are gone. No artificial cap in chunk A — chunk B's render pacing is what keeps this
//! bounded in practice (a render loop that stops being serviced grows the backlog, same
//! as any live pipeline that falls behind).
//!
//! This is where DRAGON-121's sample-continuity hazard is absorbed at the root: every
//! chunk either lands exactly, gets clipped against the horizon, gets dropped, or leaves
//! a gap, and every one of those outcomes is counted in [`MixerStats`] instead of
//! silently corrupting the timeline.
//!
//! ## Dropping a late chunk is real data loss, and says so (DRAGON-411)
//!
//! Counting is not the same as being heard. A chunk that lands entirely before the
//! rendered horizon cannot be written — those media positions are already downstream in
//! ffmpeg's hands, so there is nothing here to overwrite and the drop itself has to stay.
//! What must NOT stay is the silence around it: the drop is unrecoverable audio missing
//! from the user's recording, so the first one per track is logged loudly with its size,
//! and [`MixerStats::late_frames`] carries how much was lost for the session summary
//! (`record::pump`'s `log_audio_health`). The real defence is upstream — `record::pump`'s
//! `RENDER_LAG_SECS` is held far enough behind that a lagging source re-anchors and
//! recovers long before its audio can reach this path at all; when that margin was
//! absent (both constants 0.5s) a customer's 31s recording came back with 16.3% of the
//! mic track replaced by bit-exact zeros and nothing anywhere said why.
//!
//! ## ...except inside the opening prime's own span, which we gave away on purpose
//!
//! DRAGON-743. There is exactly ONE place the recorder renders media time the sources
//! have not delivered yet: DRAGON-554's opening prime, which renders media
//! `[0, record::pump::OPENING_PRIME_FRAMES)` (4104 frames, 85.5ms) the instant ffmpeg
//! exists, so ffmpeg 7.x's input-open never waits on the render horizon and the session
//! does not open on ~1.5s of frozen video. `OPENING_PRIME_FRAMES`' own doc has always
//! said what that costs: "what it gives up is the horizon's late-arrival protection for
//! the primed span ONLY ... bounded by this constant, and the trade is deliberate".
//!
//! This module never learned about the trade, so it reported the recorder's own
//! deliberate decision as destroyed audio, at `error`, on roughly two healthy sessions
//! in five. A typical one: the prime puts the horizon at +86ms, the system source's
//! first real chunk arrives covering -5ms to +20ms, it therefore ends 90ms behind the
//! horizon and drops, and its 10 to 23ms of post-zero tail is reported as loss. That
//! line has now been misdiagnosed three times, twice as a load artifact and once as a
//! growing regression, which is what a warning that fires on healthy runs buys.
//!
//! So the drop still happens, is still counted, and is still logged with its numbers,
//! but it is classified: [`late_drop_is_opening_prime`] separates it, the size goes to
//! [`MixerStats::opening_late_frames`] instead of [`MixerStats::late_frames`], and the
//! line drops to `debug`, beside the `opening prime rendered` line it is the consequence
//! of. DRAGON-411's net is untouched: a drop anywhere else is the same `error` carrying
//! the same seconds, and the two cases keep SEPARATE once-per-track latches so an
//! opening drop can never swallow a real one later in the same session.
//!
//! Dead ends, so nobody re-runs them:
//!
//! - **Downgrading the line outright** would take the DRAGON-411 net with it. That error
//!   is the only thing that would have named a customer's 16.3%-silent mic track.
//! - **`!started` (this track has landed no real sample yet) is NOT the predicate**, and
//!   it looks like it should be. It excludes a real instance of the opening case: when a
//!   chunk straddling media 0 lands its post-zero tail BEFORE the prime renders, the
//!   track has started, and the very next chunk still drops inside the primed span for
//!   exactly the same reason.
//! - **Reconciling the prime with the first capture** so nothing real is thrown away is
//!   a different and much larger question: it changes what lands in the file. This
//!   ticket only stops the log lying about it.

use std::collections::VecDeque;
use std::time::Duration;

use crate::audio::filters::StreamTap;

use super::clock::MediaClock;
use super::SAMPLE_RATE;

/// Per-track bookkeeping for the placement hazards `push`/`drain_to` absorb.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MixerStats {
    /// Chunks dropped entirely because they ended at or before the track's rendered
    /// horizon — nothing in them was still writable. A chunk that only PARTIALLY
    /// preceded the horizon is not counted here (see `push`'s doc): it wrote its usable
    /// tail instead of being dropped.
    pub(crate) late_chunks: u64,
    /// Frames of IN-RECORDING audio those dropped chunks carried (the part at media >=
    /// 0) — the size of the loss, which the chunk count alone does not give (chunk
    /// sizes vary by source and by platform, and a drop may be part pre-session).
    /// Reported in seconds by `record::pump`'s session summary, and the thing that
    /// decides whether a session is reported as healthy. Audio dropped inside the
    /// opening prime's own span is NOT here (see `opening_late_frames`).
    pub(crate) late_frames: u64,
    /// Frames of in-recording audio dropped inside the DRAGON-554 opening prime's span
    /// (see the module doc's second section): the same measurement as `late_frames`, on
    /// the one span the recorder renders ahead of delivery ON PURPOSE. Split out
    /// (DRAGON-743) for the same reason `pre_start_chunks` and `leading_gap_samples`
    /// were: it is present on healthy sessions, so leaving it in `late_frames` put an
    /// `error` on the drop AND on the session's one health line every time, which is a
    /// warning everybody learns to scroll past. Bounded by the prime itself, at the head
    /// of the file only, and still printed in the summary so it is never invisible.
    pub(crate) opening_late_frames: u64,
    /// Chunks dropped because the clock was paused at their `audible_time` (pause =
    /// not recorded, by construction).
    pub(crate) discarded_paused_chunks: u64,
    /// How many of the `late_chunks` lay WHOLLY before media 0 — audio captured before
    /// the session's timeline began (the sources run before the recorder anchors its
    /// clock). Expected and harmless: it predates the recording, so nothing is missing
    /// from the file. Split out because the loss report must not cry wolf over it —
    /// a chunk that is merely EARLY keeps its post-0 tail under DRAGON-417's signed
    /// placement, and one with no tail at all cost the user nothing (DRAGON-411).
    pub(crate) pre_start_chunks: u64,
    /// Frames of silence inserted MID-STREAM, in either of two places that are really
    /// the same hazard found at two different times: between two real chunks
    /// (discovered when a new chunk arrives after a gap) and past the last pushed
    /// sample when `drain_to` is asked to render further than data has arrived
    /// (discovered at render time). Once real audio has started, either one means a
    /// source stopped delivering and silence went into the user's recording in its
    /// place — the health signal.
    pub(crate) gap_samples: u64,
    /// Frames of silence inserted BEFORE this track's first real sample — the lead-in
    /// while the source spins up. Split out (DRAGON-411) because it is present on every
    /// healthy session, at a scale that swamps the signal: measured at 54-136ms across
    /// all seven E2E sessions, versus a mid-stream stall of ~0. Lumping the two together
    /// is what made the obvious `gap_samples > 0` predicate report every recording as a
    /// loss. Not a defect: nothing is missing, the source simply had not spoken yet.
    pub(crate) leading_gap_samples: u64,
}

/// Pure, unit-tested: does a late drop lie inside the DRAGON-554 opening prime's own
/// span, the one place this recorder renders media time the sources have not delivered
/// yet (DRAGON-743, see the module doc)?
///
/// `horizon` is the track's write horizon at the drop and `primed_through` the frame the
/// prime rendered to ([`Track::mark_opening_prime`]; 0 when nothing primed, which is
/// every consumer other than `record::pump`). A chunk is only dropped when it ends at or
/// before `horizon`, so `horizon <= primed_through` BOUNDS the lost media positions to
/// `[0, primed_through)`: the exact span the prime traded away, and never one frame more.
/// That bound is what makes the reclassification safe rather than a blanket downgrade.
///
/// It is also self-limiting in time, with no clock reading and no session state: the
/// render horizon only moves forward (`Track::drain_to` advances `base_frame`, and
/// `truncate_to` refuses to go behind it), so this can only be true during the session's
/// first `primed_through` frames of media, and is false for the whole rest of the run.
/// With no prime it reduces to `horizon <= 0`, where a drop has no post-zero part at all
/// and never reaches this question.
fn late_drop_is_opening_prime(horizon: i64, primed_through: u64) -> bool {
    horizon <= primed_through as i64
}

/// One track's accumulated, media-placed samples.
pub(crate) struct Track {
    channels: u8,
    /// This track's name in diagnostics ("mic" / "system") — see [`super::TrackSpec`].
    label: &'static str,
    /// Interleaved samples from `base_frame` (inclusive) forward.
    samples: VecDeque<f32>,
    /// Frame index of `samples[0]` — the track's rendered horizon. Frames before this
    /// have been drained (emitted by a prior `drain_to`); a chunk landing before it is
    /// late.
    base_frame: u64,
    stats: MixerStats,
    /// Whether the loud first-loss line has already been emitted (DRAGON-411): the
    /// drop can repeat ~100×/second on a stalled source, and `push` runs on the pump's
    /// CONTROL thread, which must never be pinned by I/O — one detailed line per track
    /// plus the session summary's totals says everything without becoming the stall.
    late_logged: bool,
    /// The same latch for the OPENING-PRIME drop (DRAGON-743), and separate from
    /// `late_logged` on purpose: sharing one would let a session's expected opening drop
    /// swallow the loud line for a real mid-session loss, which is DRAGON-411's whole
    /// net. Each kind logs once per track; both keep counting after that.
    opening_logged: bool,
    /// How far DRAGON-554's opening prime rendered on this track, in frames (0 until
    /// [`mark_opening_prime`](Self::mark_opening_prime), which only `record::pump`
    /// calls). Read by [`late_drop_is_opening_prime`]; see the module doc.
    primed_through: u64,
    /// Whether any REAL (non-silence) frame has landed yet — the split between
    /// `MixerStats::leading_gap_samples` and `MixerStats::gap_samples`. Before the
    /// first real sample, a hole is the source spinning up; after it, a hole is a
    /// source that stopped delivering (DRAGON-411).
    started: bool,
}

impl Track {
    /// `channels` is fixed for the track's lifetime (1 = mono, 2 = stereo); `label`
    /// names it in diagnostics.
    pub(crate) fn new(channels: u8, label: &'static str) -> Self {
        debug_assert!(channels == 1 || channels == 2, "tracks are mono or stereo");
        Self {
            channels,
            label,
            samples: VecDeque::new(),
            base_frame: 0,
            stats: MixerStats::default(),
            late_logged: false,
            opening_logged: false,
            primed_through: 0,
            started: false,
        }
    }

    /// Record where the DRAGON-554 opening prime left the RENDER horizon, which is what
    /// [`late_drop_is_opening_prime`] measures a later drop against. `record::pump` calls
    /// this once, immediately after priming; a track that is never marked behaves exactly
    /// as it did before DRAGON-743.
    ///
    /// It takes `base_frame`, the render horizon, and NOT `write_horizon`: the prime's
    /// reach is what it rendered, and a track that already had samples placed further
    /// ahead did not get them from the prime. Claiming those frames would extend the
    /// downgrade past the span the prime actually bought.
    pub(crate) fn mark_opening_prime(&mut self) {
        self.primed_through = self.base_frame;
    }

    pub(crate) fn channels(&self) -> u8 {
        self.channels
    }

    pub(crate) fn stats(&self) -> MixerStats {
        self.stats
    }

    /// The position right after the last written frame (real or gap-filled) — what the
    /// NEXT pushed chunk is compared against for late/gap/overlap.
    ///
    /// It is also this track's DELIVERED-THROUGH position, and `record::pump` renders
    /// against it (DRAGON-717). Sources stamp contiguously, so everything below this
    /// frame has arrived (or has already been silence-filled and counted) and nothing
    /// below it can still arrive: it is exactly how far the render horizon may advance
    /// without any chance of destroying captured audio.
    pub(crate) fn write_horizon(&self) -> u64 {
        self.base_frame + (self.samples.len() / self.channels as usize) as u64
    }

    /// Place `tap`'s samples at the media position of its audible time.
    ///
    /// - Entirely before the write horizon (its end <= horizon): dropped, counted
    ///   (`late_chunks`, plus `late_frames` for the part of it that was inside the
    ///   recording) and LOGGED — that is captured audio being destroyed, not
    ///   bookkeeping (see the module doc's DRAGON-411 section). A chunk lying wholly
    ///   before media 0 predates the session, so it loses nothing: counted
    ///   `pre_start_chunks`, silently. A drop inside the opening prime's span is real
    ///   audio but an expected one: counted `opening_late_frames` and logged at `debug`
    ///   (DRAGON-743, the module doc's second section).
    /// - Starting before the horizon but extending past it: only the unrendered tail
    ///   (from the horizon onward) is written — NOT counted as `late_chunks` (some of
    ///   it landed) and no gap-fill (it's contiguous with what's already there).
    /// - Starting past the horizon: the gap between is silence-filled and counted
    ///   (`gap_samples`), then the whole chunk is appended.
    /// - Crossing INTO a pause: clipped at the freeze (see below).
    /// - `tap.samples.len()` not a whole multiple of `channels`: the short trailing
    ///   partial frame is silently dropped (defensive; not expected in practice).
    pub(crate) fn push(&mut self, tap: StreamTap, clock: &MediaClock) {
        if clock.is_paused_at(tap.audible_time) {
            self.stats.discarded_paused_chunks += 1;
            return;
        }
        let ch = self.channels as usize;
        let chunk_frames = tap.samples.len() / ch;
        if chunk_frames == 0 {
            return;
        }
        // SIGNED (see `MediaClock::media_at_signed`): a chunk captured before media 0
        // must read as before media 0, not clamp onto frame 0 where every such chunk
        // collides and all but the first are dropped whole (DRAGON-417).
        let media = clock.media_at_signed(tap.audible_time);
        let start_frame = (media * SAMPLE_RATE as f64).round() as i64;
        let horizon = self.write_horizon() as i64;
        // Clip a chunk that STRADDLES a pause (DRAGON-411). Sources stamp contiguously
        // and their stamps legitimately run ahead of wall time (arrival jitter is a
        // monitor, never placed — `audio::capture`'s model), so the chunk in flight when
        // a pause lands can carry samples whose audible time is past the freeze. Media
        // time stopped there, so writing them smears pre-pause audio over media
        // positions that belong to whatever RESUMES — and the resumed audio then arrives
        // for positions already taken and is discarded as late. Both halves of that are
        // wrong: the pause must not be recorded, and the resume must not be robbed.
        // Measuring the usable length through the SAME signed clock handles it in one
        // line, with no special case for the pre-media-0 span (`media_at_signed` is
        // continuous across 0, so a straddling chunk still measures its full length):
        // while running, media advances 1:1 with wall time, so this is EXACTLY
        // `chunk_frames` and the normal path is untouched; only across a freeze is it
        // shorter.
        let end =
            tap.audible_time + Duration::from_secs_f64(chunk_frames as f64 / SAMPLE_RATE as f64);
        let usable_frames = (((clock.media_at_signed(end) - media) * SAMPLE_RATE as f64).round()
            as i64)
            .clamp(0, chunk_frames as i64) as usize;
        if usable_frames == 0 {
            // The freeze lands at this chunk's own start: nothing of it belongs to the
            // recording (the same outcome `is_paused_at` gives one chunk later).
            self.stats.discarded_paused_chunks += 1;
            return;
        }
        let chunk_frames = usable_frames;

        if start_frame + chunk_frames as i64 <= horizon {
            self.stats.late_chunks += 1;
            // How much of the drop was audio that is IN the recording (media >= 0).
            // Everything before media 0 predates the session and is expected to drop
            // (DRAGON-417's signed placement keeps its post-0 tail; a chunk wholly
            // before 0 has no tail to keep) — reporting that as destroyed audio would
            // put an `error` on every healthy recording and make the real signal
            // worthless.
            let lost = (start_frame + chunk_frames as i64).max(0) - start_frame.max(0);
            if lost == 0 {
                self.stats.pre_start_chunks += 1;
                return;
            }
            // The DRAGON-554 opening prime's own span: real audio, really dropped, but
            // dropped by a decision this recorder took on purpose and bounded to the head
            // of the file (DRAGON-743, module doc). It keeps its own counter, its own
            // latch and its own level, so DRAGON-411's report below still means what it
            // has always meant.
            if late_drop_is_opening_prime(horizon, self.primed_through) {
                self.stats.opening_late_frames += lost as u64;
                if !self.opening_logged {
                    self.opening_logged = true;
                    log::debug!(
                        "mixer[{}]: the opening prime already rendered past a chunk: {:.0}ms \
                         of a {:.0}ms chunk for media {:.3}s arrived {:.0}ms behind the \
                         horizon, which the prime had put at {:.0}ms before any source \
                         delivered it (DRAGON-554's deliberate trade, bounded by the prime \
                         and only at a session's head; further opening drops on this track \
                         are counted, not logged)",
                        self.label,
                        lost as f64 * 1000.0 / SAMPLE_RATE as f64,
                        chunk_frames as f64 * 1000.0 / SAMPLE_RATE as f64,
                        media,
                        (horizon - start_frame) as f64 * 1000.0 / SAMPLE_RATE as f64,
                        self.primed_through as f64 * 1000.0 / SAMPLE_RATE as f64,
                    );
                }
                return;
            }
            self.stats.late_frames += lost as u64;
            if !self.late_logged {
                self.late_logged = true;
                log::error!(
                    "mixer[{}]: DISCARDING captured audio — {:.0}ms of a {:.0}ms chunk for \
                     media {:.3}s arrived {:.0}ms behind the rendered horizon, so it can no \
                     longer be written (that much audio is missing from the recording; \
                     further drops on this track are counted, not logged)",
                    self.label,
                    lost as f64 * 1000.0 / SAMPLE_RATE as f64,
                    chunk_frames as f64 * 1000.0 / SAMPLE_RATE as f64,
                    media,
                    (horizon - start_frame) as f64 * 1000.0 / SAMPLE_RATE as f64,
                );
            }
            return;
        }
        let (usable_from, skip_frames) = if start_frame < horizon {
            (horizon, (horizon - start_frame) as usize)
        } else {
            (start_frame, 0)
        };
        if usable_from > horizon {
            let gap_frames = (usable_from - horizon) as u64;
            for _ in 0..(gap_frames as usize * ch) {
                self.samples.push_back(0.0);
            }
            self.count_gap(gap_frames);
        }
        self.samples.extend(tap.samples[skip_frames * ch..chunk_frames * ch].iter().copied());
        self.started = true;
    }

    /// Book a silence fill to the right counter: lead-in before this track's first real
    /// sample, a mid-stream hole after it (see [`MixerStats::leading_gap_samples`]).
    fn count_gap(&mut self, frames: u64) {
        if self.started {
            self.stats.gap_samples += frames;
        } else {
            self.stats.leading_gap_samples += frames;
        }
    }

    /// Discard everything placed at or after `until_frame` (DRAGON-411's other half).
    ///
    /// `push`'s own clip only covers chunks pushed once the clock ALREADY knows about a
    /// pause. A source's contiguous stamps legitimately run ahead of wall time, so the
    /// chunks in flight when the user hits pause were placed against a clock with no
    /// pause in it yet, at media positions past where the freeze then lands. Those
    /// samples were captured after the pause began — they must not be in the file, and
    /// leaving them there also robs the RESUME of the positions it is about to fill (its
    /// audio arrives for taken ground and is discarded as late, which is how this was
    /// found: an `error`-level loss report on every paused recording). `record::pump`
    /// calls this the instant it applies a pause, which is always far ahead of the
    /// render horizon (`RENDER_LAG_SECS`), so nothing already rendered is affected —
    /// the guard below makes that structural rather than assumed.
    pub(crate) fn truncate_to(&mut self, until_frame: u64) {
        if until_frame <= self.base_frame {
            return; // already rendered and gone: nothing here to take back
        }
        let keep = (until_frame - self.base_frame) as usize * self.channels as usize;
        if keep < self.samples.len() {
            self.samples.truncate(keep);
        }
    }

    /// Drain interleaved samples for `[base_frame, until_frame)`, silence-padding (and
    /// counting in `gap_samples`) any tail beyond what's actually been pushed yet.
    /// Always returns exactly `(until_frame - base_frame) * channels` samples (empty if
    /// `until_frame <= base_frame`), so callers rendering multiple tracks in lockstep
    /// get equal-length output regardless of which tracks have caught-up data.
    pub(crate) fn drain_to(&mut self, until_frame: u64) -> Vec<f32> {
        let ch = self.channels as usize;
        if until_frame <= self.base_frame {
            return Vec::new();
        }
        let needed = (until_frame - self.base_frame) as usize;
        let available = self.samples.len() / ch;
        let take = needed.min(available);
        let mut out: Vec<f32> = self.samples.drain(0..take * ch).collect();
        let pad = needed - take;
        if pad > 0 {
            out.resize(out.len() + pad * ch, 0.0);
            self.count_gap(pad as u64);
        }
        self.base_frame = until_frame;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> std::time::Instant {
        std::time::Instant::now()
    }
    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn tap_at(audible: std::time::Instant, n: usize, value: f32) -> StreamTap {
        StreamTap::new(vec![value; n], audible, audible)
    }

    #[test]
    fn push_at_media_zero_lands_at_frame_zero_with_no_gap() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 4, 0.25), &clock);
        assert_eq!(track.base_frame, 0);
        assert_eq!(Vec::from(track.samples.clone()), vec![0.25; 4]);
        assert_eq!(track.stats(), MixerStats::default());
    }

    #[test]
    fn push_places_a_chunk_at_the_exact_rounded_sample_position() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        // 0.5s at 48kHz -> exactly frame 24_000; the gap-fill count pins the rounding
        // down to zero error (24_000 exactly, not 23_999 or 24_001). Nothing real has
        // landed yet, so the fill is this track's LEAD-IN (DRAGON-411's split) — the
        // rounding this pins is the same either way.
        track.push(tap_at(t0 + secs(0.5), 10, 1.0), &clock);
        assert_eq!(track.write_horizon(), 24_010);
        assert_eq!(track.stats().leading_gap_samples, 24_000);
        assert_eq!(track.stats().gap_samples, 0, "a lead-in is not a mid-stream hole");
    }

    #[test]
    fn lead_in_silence_and_a_mid_stream_hole_are_counted_apart() {
        // The split that makes the health report usable (DRAGON-411): silence before a
        // source's first sample is spin-up, silence after it is a source that stopped
        // delivering. Measured across all seven E2E sessions, lead-in is 55-81ms EVERY
        // time while a mid-stream hole is 0 on an unpaused session — so lumping them
        // together (the obvious `gap_samples > 0` predicate) reports every healthy
        // recording as a loss.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0 + secs(0.1), 10, 1.0), &clock); // 4_800 frames of lead-in
        assert_eq!(track.stats().leading_gap_samples, 4_800);
        assert_eq!(track.stats().gap_samples, 0);
        // Now that real audio has started, a hole is a hole.
        track.push(tap_at(t0 + secs(0.2), 10, 2.0), &clock);
        assert_eq!(track.stats().leading_gap_samples, 4_800, "lead-in is closed for good");
        assert_eq!(track.stats().gap_samples, 4_800 - 10);
        // ...including one discovered at render time rather than at push time.
        let before = track.stats().gap_samples;
        track.drain_to(track.write_horizon() + 100);
        assert_eq!(track.stats().gap_samples, before + 100);
    }

    #[test]
    fn a_render_pad_before_any_real_audio_is_lead_in_not_a_hole() {
        // `drain_to` padding an empty track is the render horizon running ahead of a
        // source that has not delivered yet — the same spin-up, found at render time.
        let t0 = base();
        let _ = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.drain_to(1_000);
        assert_eq!(track.stats().leading_gap_samples, 1_000);
        assert_eq!(track.stats().gap_samples, 0);
    }

    #[test]
    fn gap_between_chunks_is_filled_with_silence_and_counted() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 5, 1.0), &clock); // frames 0..5
        track.push(tap_at(t0 + secs(1.0), 3, 2.0), &clock); // frames 48_000..48_003
        assert_eq!(track.stats().gap_samples, 48_000 - 5);
        assert_eq!(track.write_horizon(), 48_003);
        let all: Vec<f32> = track.samples.iter().copied().collect();
        assert_eq!(&all[0..5], &[1.0; 5]);
        assert!(all[5..48_000].iter().all(|&s| s == 0.0));
        assert_eq!(&all[48_000..48_003], &[2.0; 3]);
    }

    #[test]
    fn late_chunk_entirely_before_the_horizon_is_dropped_and_counted() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0 + secs(1.0), 480, 1.0), &clock); // frames 48_000..48_480
        track.drain_to(48_480); // horizon now 48_480, base_frame moves with it
        track.push(tap_at(t0 + secs(0.5), 10, 9.0), &clock); // frames 24_000..24_010: fully before
        assert_eq!(track.stats().late_chunks, 1);
        // DRAGON-411: the SIZE of the loss is what a user-facing report needs — chunk
        // counts alone say nothing (a chunk is 10ms on one platform, 25ms on another).
        assert_eq!(track.stats().late_frames, 10);
        assert_eq!(track.samples.len(), 0);
        assert_eq!(track.base_frame, 48_480);
        // A second drop accumulates frames but does NOT re-log (the loud line is
        // once per track; `push` runs on the pump's control thread).
        assert!(track.late_logged, "the first discard must have logged");
        track.push(tap_at(t0 + secs(0.6), 20, 9.0), &clock);
        assert_eq!(track.stats().late_chunks, 2);
        assert_eq!(track.stats().late_frames, 30);
    }

    #[test]
    fn dropping_pre_recording_audio_is_not_reported_as_loss() {
        // The sources are already delivering when the recorder anchors its clock, so
        // the opening taps of EVERY session are stamped before media 0. DRAGON-417's
        // signed placement keeps whatever tail of them falls inside the recording and
        // drops the rest — correctly, and at no cost to the user, because that audio
        // predates the file. It still lands in `late_chunks` (it was dropped at the
        // horizon), so the loss report has to look at `late_frames`: calling this
        // destroyed audio would put an `error` on every single healthy recording and
        // make the real signal worthless (DRAGON-411).
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        // Two 10ms taps that end before media 0: nothing of either is in the recording.
        track.push(tap_at(t0 - secs(0.05), 480, 1.0), &clock);
        track.push(tap_at(t0 - secs(0.04), 480, 2.0), &clock);
        assert_eq!(track.stats().late_chunks, 2, "both were dropped at the horizon");
        assert_eq!(track.stats().pre_start_chunks, 2, "...and both predate the recording");
        assert_eq!(track.stats().late_frames, 0, "so NO in-recording audio was lost");
        assert!(!track.late_logged, "a pre-recording drop must never log a discard");
        assert_eq!(track.samples.len(), 0);
        // A tap that STRADDLES media 0 is a different animal: its post-0 tail lands
        // (DRAGON-417), so it is not a drop at all.
        track.push(tap_at(t0 - secs(0.005), 480, 3.0), &clock);
        assert_eq!(track.stats().late_chunks, 2, "the straddling tap partially landed");
        assert_eq!(track.samples.len(), 240, "its 5ms post-0 tail");
    }

    #[test]
    fn a_partial_overlap_is_not_counted_as_lost_audio() {
        // The counterpart of the above: a chunk that still had a writable tail lost
        // nothing, so it must not inflate the loss report (DRAGON-411).
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 100, 1.0), &clock);
        track.drain_to(100);
        track.push(tap_at(t0 + Duration::from_secs_f64(50.0 / 48_000.0), 60, 5.0), &clock);
        assert_eq!(track.stats().late_frames, 0);
        assert!(!track.late_logged, "a partial overlap must never log a discard");
    }

    #[test]
    fn partial_overlap_chunk_writes_only_the_unrendered_part() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 100, 1.0), &clock); // frames 0..100
        track.drain_to(100); // horizon now 100
        // Starts at frame 50 (before the horizon) but runs 60 samples, to frame 110:
        // only the unrendered 10 frames (100..110) should land.
        track.push(tap_at(t0 + Duration::from_secs_f64(50.0 / 48_000.0), 60, 5.0), &clock);
        assert_eq!(track.stats().late_chunks, 0, "a partial overlap is not a late drop");
        assert_eq!(track.stats().gap_samples, 0, "contiguous with the horizon: no gap");
        assert_eq!(track.samples.len(), 10);
        assert!(track.samples.iter().all(|&s| s == 5.0));
        assert_eq!(track.write_horizon(), 110);
    }

    // ---- Pre-start chunks (DRAGON-417) ----

    #[test]
    fn a_chunk_straddling_media_zero_contributes_only_its_post_zero_tail() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        // 100 frames starting 50 frames before media 0: the first 50 belong to before
        // the recording, the last 50 to frames 0..50.
        let before = Duration::from_secs_f64(50.0 / 48_000.0);
        track.push(tap_at(t0 - before, 100, 7.0), &clock);
        assert_eq!(track.stats().late_chunks, 0, "part of it landed: not a late drop");
        assert_eq!(track.stats().gap_samples, 0, "it starts at the horizon: no gap");
        assert_eq!(track.samples.len(), 50);
        assert_eq!(track.write_horizon(), 50);
    }

    #[test]
    fn a_chunk_wholly_before_media_zero_is_dropped_as_late() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0 - secs(0.5), 10, 3.0), &clock);
        assert_eq!(track.stats().late_chunks, 1);
        assert_eq!(track.samples.len(), 0);
    }

    #[test]
    fn consecutive_pre_start_chunks_do_not_collide_at_frame_zero() {
        // The failure this pins: with a CLAMPED media position every pre-start chunk
        // reads as starting at frame 0, so the first one lands and every later one is
        // "entirely before the horizon" and is thrown away — including the part of it
        // that belongs inside the recording. Three contiguous 40-frame chunks ending
        // exactly at media 0 + 40: only the last 40 frames are in the recording, and
        // they must be there.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        let f = |n: f64| Duration::from_secs_f64(n / 48_000.0);
        track.push(tap_at(t0 - f(80.0), 40, 1.0), &clock); // frames -80..-40: gone
        track.push(tap_at(t0 - f(40.0), 40, 2.0), &clock); // frames -40..0: gone
        track.push(tap_at(t0, 40, 3.0), &clock); // frames 0..40: kept, whole
        assert_eq!(track.stats().late_chunks, 2, "only the two pre-recording chunks drop");
        assert_eq!(track.stats().gap_samples, 0);
        assert_eq!(Vec::from(track.samples.clone()), vec![3.0; 40]);
    }

    #[test]
    fn paused_chunk_is_discarded_and_counted() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(1.0));
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0 + secs(1.5), 10, 1.0), &clock); // audible inside the pause
        assert_eq!(track.stats().discarded_paused_chunks, 1);
        assert_eq!(track.samples.len(), 0);
        assert_eq!(track.write_horizon(), 0);
    }

    #[test]
    fn a_chunk_straddling_a_pause_is_clipped_at_the_freeze() {
        // DRAGON-411, found by the new loud logging via `media_clock_early_long_pause_e2e`:
        // sources stamp CONTIGUOUSLY, so their audible times legitimately run ahead of
        // wall time, and the chunk in flight when a pause lands can carry samples from
        // past the freeze. Writing those smeared pre-pause audio over media positions
        // belonging to the RESUME, whose own audio then arrived for taken positions and
        // was thrown away as late — an `error`-worthy loss on every paused recording,
        // caused by the recorder itself.
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(1.0));
        let mut track = Track::new(1, "test");
        // A 100ms chunk starting 40ms before the pause: only those 40ms belong to the
        // recording (48_000 - 1920 = frame 46_080 .. 48_000).
        track.push(tap_at(t0 + secs(0.96), 4_800, 1.0), &clock);
        assert_eq!(track.write_horizon(), 48_000, "the write horizon stops at the freeze");
        let placed: Vec<f32> = track.samples.iter().copied().collect();
        assert_eq!(placed.len(), 48_000, "46_080 frames of lead-in gap + the usable 1_920");
        assert!(placed[..46_080].iter().all(|&s| s == 0.0));
        assert!(placed[46_080..].iter().all(|&s| s == 1.0), "40ms of the 100ms chunk lands");
        assert_eq!(track.stats().late_chunks, 0);
        assert_eq!(track.stats().discarded_paused_chunks, 0);
        // ...and the audio that resumes at that exact media position now LANDS, instead
        // of colliding with the smear and being discarded.
        clock.resume(t0 + secs(9.0));
        track.push(tap_at(t0 + secs(9.0), 480, 2.0), &clock);
        assert_eq!(track.stats().late_chunks, 0, "the resume must not be robbed");
        assert_eq!(track.write_horizon(), 48_480);
    }

    #[test]
    fn clipping_is_exact_so_an_unpaused_chunk_is_untouched() {
        // The clip measures the usable length through the clock, which advances 1:1 with
        // wall time while running — so on the normal path it must resolve to the chunk's
        // full length for every size, with no off-by-one from the f64 round trip.
        let t0 = base();
        let clock = MediaClock::new(t0);
        for &n in &[1usize, 2, 7, 480, 1_200, 4_801, 48_000] {
            let mut track = Track::new(1, "test");
            track.push(tap_at(t0 + secs(0.5), n, 1.0), &clock);
            assert_eq!(track.write_horizon(), 24_000 + n as u64, "chunk of {n} frames");
        }
        // Stereo too (frames, not samples, drive the clip).
        let mut track = Track::new(2, "test");
        track.push(tap_at(t0, 2_400, 1.0), &clock);
        assert_eq!(track.write_horizon(), 1_200);
    }

    #[test]
    fn drain_to_pads_missing_tail_with_silence_and_counts_gap() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(2, "test"); // stereo: 2 samples per frame
        track.push(tap_at(t0, 6, 1.0), &clock); // 3 real frames (6 samples / 2ch)
        let out = track.drain_to(5); // ask for 5 frames; only 3 are real
        assert_eq!(out.len(), 10); // 5 frames * 2 channels
        assert_eq!(&out[0..6], &[1.0; 6]);
        assert_eq!(&out[6..10], &[0.0; 4]);
        assert_eq!(track.stats().gap_samples, 2); // 2 missing frames
        assert_eq!(track.base_frame, 5);
    }

    #[test]
    fn truncate_to_takes_back_placed_samples_but_never_rendered_ground() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 480, 1.0), &clock);
        track.truncate_to(200);
        assert_eq!(track.write_horizon(), 200);
        assert_eq!(track.samples.len(), 200);
        // Past the end: nothing to take back.
        track.truncate_to(10_000);
        assert_eq!(track.write_horizon(), 200);
        // Behind the rendered horizon: refused — those frames are downstream already
        // (this is what makes `record::pump` calling it at a pause edge safe).
        track.drain_to(200);
        track.push(tap_at(t0 + secs(0.5), 480, 2.0), &clock);
        let before = track.write_horizon();
        track.truncate_to(10);
        assert_eq!(track.write_horizon(), before, "rendered ground is never taken back");
    }

    #[test]
    fn drain_to_before_or_at_the_horizon_is_a_no_op() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "test");
        track.push(tap_at(t0, 10, 1.0), &clock);
        track.drain_to(10);
        assert_eq!(track.drain_to(10), Vec::<f32>::new());
        assert_eq!(track.base_frame, 10);
    }
}

/// DRAGON-743: the opening prime's span, and the DRAGON-411 net it must not weaken.
/// Named apart from `tests` because it pins ONE decision, [`late_drop_is_opening_prime`],
/// and the two facts that decision has to keep straight.
#[cfg(test)]
mod opening_prime_tests {
    use super::*;

    /// The prime's own length, from `record::pump::OPENING_PRIME_FRAMES`. Restated here
    /// rather than imported: these tests are about what the mixer does with a mark of ANY
    /// size, and the pump's constant is `pub(super)` to its own module for a reason.
    const PRIMED: u64 = 4_104; // 85.5ms at 48kHz

    fn base() -> std::time::Instant {
        std::time::Instant::now()
    }
    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }
    fn frames(n: f64) -> Duration {
        Duration::from_secs_f64(n / SAMPLE_RATE as f64)
    }
    fn tap_at(audible: std::time::Instant, n: usize, value: f32) -> StreamTap {
        StreamTap::new(vec![value; n], audible, audible)
    }
    /// A track already primed to [`PRIMED`], the way `record::pump` leaves one: the
    /// opening render has run and the mark records its reach.
    fn primed_track(channels: u8, label: &'static str) -> Track {
        let mut track = Track::new(channels, label);
        track.drain_to(PRIMED);
        track.mark_opening_prime();
        track
    }

    #[test]
    fn a_mid_session_late_drop_is_still_a_loud_error() {
        // THE test of this ticket: the DRAGON-411 shape, on a session that DID prime. A
        // source falls behind mid-recording and its audio arrives for media positions
        // ffmpeg already has. That is the customer's 31s recording with 16.3% of its mic
        // track written as bit-exact zeros, and it must read exactly as it did before:
        // `late_frames`, the loud line, and nothing in the opening's counters.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = primed_track(1, "mic");
        track.push(tap_at(t0 + secs(1.0), 480, 1.0), &clock); // frames 48_000..48_480
        track.drain_to(48_480); // a second of the session has been rendered and is gone
        track.push(tap_at(t0 + secs(0.5), 480, 9.0), &clock); // arrives half a second late
        assert_eq!(track.stats().late_frames, 480, "half a second late is real lost audio");
        assert!(track.late_logged, "...and it logs the DRAGON-411 error");
        assert_eq!(track.stats().opening_late_frames, 0, "nothing to do with the opening");
        assert!(!track.opening_logged);
    }

    #[test]
    fn the_two_latches_are_separate_so_an_opening_drop_cannot_swallow_a_real_one() {
        // Why the opening case does not just reuse `late_logged`: the loud line is once
        // per track, so sharing the latch would mean a session that opened normally could
        // never report a genuine mid-session loss.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = primed_track(1, "mic");
        track.push(tap_at(t0 - frames(240.0), 1_200, 1.0), &clock); // straddles media 0
        assert!(track.opening_logged, "the opening drop logged, at debug");
        assert!(!track.late_logged, "and it did NOT consume the loud line");
        // Now the session runs on and a source really falls behind.
        track.push(tap_at(t0 + secs(2.0), 480, 2.0), &clock);
        track.drain_to(96_480);
        track.push(tap_at(t0 + secs(1.0), 480, 9.0), &clock);
        assert!(track.late_logged, "the real loss still gets the loud line");
        assert_eq!(track.stats().late_frames, 480);
        assert_eq!(track.stats().opening_late_frames, 960, "the opening's 20ms, unchanged");
    }

    #[test]
    fn the_owners_opening_session_is_reproduced_and_is_no_longer_a_loss() {
        // The exact session shape from the DRAGON-743 report, on the stereo system track:
        // three 25ms chunks wholly before media 0, the opening prime, then the first real
        // chunk straddling media 0 by 5ms. The owner's summary read
        // `sys(late=4 lost=0.020s pre_start=3 lead_in=0.086s)`, with a `DISCARDING captured
        // audio ... arrived 90ms behind the rendered horizon` error beside it.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(2, "system");
        for before in [0.080, 0.055, 0.030] {
            track.push(tap_at(t0 - secs(before), 2_400, 1.0), &clock); // 1_200 frames = 25ms
        }
        assert_eq!(track.stats().pre_start_chunks, 3, "all three predate the file");
        // The catch-up drain has placed everything captured; the prime renders the opening.
        track.drain_to(PRIMED);
        track.mark_opening_prime();
        assert_eq!(track.stats().leading_gap_samples, PRIMED, "the owner's lead_in=0.086s");
        // The source's first real chunk: media -5ms to +20ms, ending 90ms behind the
        // horizon the prime just put at +85.5ms.
        track.push(tap_at(t0 - frames(240.0), 2_400, 2.0), &clock);
        assert_eq!(track.stats().late_chunks, 4, "the owner's late=4");
        assert_eq!(track.stats().late_frames, 0, "and now lost=0.000s: nothing was destroyed");
        assert_eq!(track.stats().opening_late_frames, 960, "the 20ms is the prime's own trade");
        assert!(track.opening_logged, "still logged, with its numbers, at debug");
        assert!(!track.late_logged, "but never as DISCARDING captured audio");
    }

    #[test]
    fn a_track_that_already_started_still_reads_its_opening_drop_as_the_opening() {
        // The dead end recorded in the module doc: `!started` looks like the predicate and
        // is not. A chunk straddling media 0 that arrives BEFORE the prime lands its
        // post-zero tail, so the track has started; the very next chunk then drops inside
        // the primed span for exactly the same reason, and calling that a loss would put
        // the error back on a healthy session.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1, "system");
        track.push(tap_at(t0 - frames(240.0), 1_200, 1.0), &clock); // its last 20ms lands
        assert!(track.started, "real audio is in the track");
        assert_eq!(track.write_horizon(), 960);
        track.drain_to(PRIMED);
        track.mark_opening_prime();
        // Contiguous with the last one (sources stamp contiguously): media 20ms to 45ms.
        track.push(tap_at(t0 + frames(960.0), 1_200, 2.0), &clock);
        assert_eq!(track.stats().opening_late_frames, 1_200);
        assert_eq!(track.stats().late_frames, 0);
        assert!(!track.late_logged, "a started track's opening drop is still the opening");
    }

    #[test]
    fn an_unmarked_track_classifies_exactly_as_it_did_before() {
        // Every mixer consumer other than `record::pump` never primes and never marks, so
        // `primed_through` stays 0 and this whole ticket is inert for them.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(2, "system");
        track.drain_to(PRIMED); // rendered ahead, but nobody said it was a prime
        track.push(tap_at(t0 - frames(240.0), 2_400, 2.0), &clock);
        assert_eq!(track.stats().late_frames, 960, "unmarked: the historical accounting");
        assert_eq!(track.stats().opening_late_frames, 0);
        assert!(track.late_logged, "...and the historical error");
    }

    #[test]
    fn the_downgrade_stops_at_the_last_primed_frame() {
        // The band edge, through the real type: one frame past the prime's reach and the
        // drop is an ordinary DRAGON-411 loss again.
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut inside = primed_track(1, "system");
        inside.push(tap_at(t0, 1_000, 1.0), &clock);
        assert_eq!(inside.stats().opening_late_frames, 1_000, "horizon == the primed frame");
        assert_eq!(inside.stats().late_frames, 0);
        let mut past = primed_track(1, "system");
        past.drain_to(PRIMED + 1); // one render cycle has moved on
        past.push(tap_at(t0, 1_000, 1.0), &clock);
        assert_eq!(past.stats().opening_late_frames, 0, "one frame past: not the opening");
        assert_eq!(past.stats().late_frames, 1_000);
    }

    #[test]
    fn the_opening_downgrade_can_never_exceed_the_primed_span() {
        // The bound that makes this safe rather than a blanket downgrade: a drop needs the
        // chunk to END at or before the horizon, and the horizon is inside the prime, so
        // the audio it can hide lies inside media [0, PRIMED) and nothing more. Walked
        // with contiguous chunks of several sizes, each starting before media 0.
        let t0 = base();
        let clock = MediaClock::new(t0);
        for chunk in [240usize, 1_200, 2_048, 4_800] {
            let mut track = primed_track(1, "system");
            let mut at = t0 - frames(chunk as f64 / 2.0);
            for _ in 0..12 {
                track.push(tap_at(at, chunk, 1.0), &clock);
                at += frames(chunk as f64);
            }
            assert_eq!(track.stats().late_frames, 0, "chunk of {chunk} frames");
            assert!(
                track.stats().opening_late_frames < PRIMED,
                "chunk of {chunk} frames: {} lost inside a {PRIMED}-frame prime",
                track.stats().opening_late_frames
            );
        }
    }

    #[test]
    fn late_drop_is_opening_prime_is_a_horizon_test_and_nothing_else() {
        // The pure decision on its own. Nothing but the two numbers goes into it: no
        // clock, no chunk, no session state, so it reads the same on every platform.
        assert!(late_drop_is_opening_prime(0, PRIMED));
        assert!(late_drop_is_opening_prime(PRIMED as i64 - 1, PRIMED));
        assert!(late_drop_is_opening_prime(PRIMED as i64, PRIMED), "the prime's own reach");
        assert!(!late_drop_is_opening_prime(PRIMED as i64 + 1, PRIMED));
        assert!(!late_drop_is_opening_prime(48_000, PRIMED), "a second in: ordinary loss");
        // No prime: the question never changes an answer for a consumer that never marks,
        // and a drop at horizon 0 has no post-zero part to lose in the first place.
        assert!(!late_drop_is_opening_prime(1, 0));
        assert!(!late_drop_is_opening_prime(48_000, 0));
        assert!(late_drop_is_opening_prime(0, 0), "unreachable: such a drop loses nothing");
    }
}
