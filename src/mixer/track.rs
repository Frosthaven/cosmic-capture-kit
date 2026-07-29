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
    /// decides whether a session is reported as healthy.
    pub(crate) late_frames: u64,
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
            started: false,
        }
    }

    pub(crate) fn channels(&self) -> u8 {
        self.channels
    }

    pub(crate) fn stats(&self) -> MixerStats {
        self.stats
    }

    /// The position right after the last written frame (real or gap-filled) — what the
    /// NEXT pushed chunk is compared against for late/gap/overlap.
    fn write_horizon(&self) -> u64 {
        self.base_frame + (self.samples.len() / self.channels as usize) as u64
    }

    /// Place `tap`'s samples at the media position of its audible time.
    ///
    /// - Entirely before the write horizon (its end <= horizon): dropped, counted
    ///   (`late_chunks`, plus `late_frames` for the part of it that was inside the
    ///   recording) and LOGGED — that is captured audio being destroyed, not
    ///   bookkeeping (see the module doc's DRAGON-411 section). A chunk lying wholly
    ///   before media 0 predates the session, so it loses nothing: counted
    ///   `pre_start_chunks`, silently.
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
            self.stats.late_frames += lost as u64;
            if lost == 0 {
                self.stats.pre_start_chunks += 1;
                return;
            }
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
