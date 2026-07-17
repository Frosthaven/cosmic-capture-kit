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

use std::collections::VecDeque;

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
    /// Chunks dropped because the clock was paused at their `audible_time` (pause =
    /// not recorded, by construction).
    pub(crate) discarded_paused_chunks: u64,
    /// Frames of silence inserted, in either of two places that are really the same
    /// hazard found at two different times: between two real chunks (discovered when a
    /// new chunk arrives after a gap) and past the last pushed sample when `drain_to`
    /// is asked to render further than data has arrived (discovered at render time).
    pub(crate) gap_samples: u64,
}

/// One track's accumulated, media-placed samples.
pub(crate) struct Track {
    channels: u8,
    /// Interleaved samples from `base_frame` (inclusive) forward.
    samples: VecDeque<f32>,
    /// Frame index of `samples[0]` — the track's rendered horizon. Frames before this
    /// have been drained (emitted by a prior `drain_to`); a chunk landing before it is
    /// late.
    base_frame: u64,
    stats: MixerStats,
}

impl Track {
    /// `channels` is fixed for the track's lifetime (1 = mono, 2 = stereo).
    pub(crate) fn new(channels: u8) -> Self {
        debug_assert!(channels == 1 || channels == 2, "tracks are mono or stereo");
        Self { channels, samples: VecDeque::new(), base_frame: 0, stats: MixerStats::default() }
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
    ///   `late_chunks`.
    /// - Starting before the horizon but extending past it: only the unrendered tail
    ///   (from the horizon onward) is written — NOT counted as `late_chunks` (some of
    ///   it landed) and no gap-fill (it's contiguous with what's already there).
    /// - Starting past the horizon: the gap between is silence-filled and counted
    ///   (`gap_samples`), then the whole chunk is appended.
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
        let media = clock.media_at(tap.audible_time);
        let start_frame = (media * SAMPLE_RATE as f64).round() as u64;
        let horizon = self.write_horizon();

        if start_frame + chunk_frames as u64 <= horizon {
            self.stats.late_chunks += 1;
            return;
        }
        let (usable_from, skip_frames) = if start_frame < horizon {
            (horizon, (horizon - start_frame) as usize)
        } else {
            (start_frame, 0)
        };
        if usable_from > horizon {
            let gap_frames = usable_from - horizon;
            for _ in 0..(gap_frames as usize * ch) {
                self.samples.push_back(0.0);
            }
            self.stats.gap_samples += gap_frames;
        }
        self.samples.extend(tap.samples[skip_frames * ch..chunk_frames * ch].iter().copied());
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
            self.stats.gap_samples += pad as u64;
        }
        self.base_frame = until_frame;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        let mut track = Track::new(1);
        track.push(tap_at(t0, 4, 0.25), &clock);
        assert_eq!(track.base_frame, 0);
        assert_eq!(Vec::from(track.samples.clone()), vec![0.25; 4]);
        assert_eq!(track.stats(), MixerStats::default());
    }

    #[test]
    fn push_places_a_chunk_at_the_exact_rounded_sample_position() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1);
        // 0.5s at 48kHz -> exactly frame 24_000; the gap-fill count pins the rounding
        // down to zero error (24_000 exactly, not 23_999 or 24_001).
        track.push(tap_at(t0 + secs(0.5), 10, 1.0), &clock);
        assert_eq!(track.write_horizon(), 24_010);
        assert_eq!(track.stats().gap_samples, 24_000);
    }

    #[test]
    fn gap_between_chunks_is_filled_with_silence_and_counted() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1);
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
        let mut track = Track::new(1);
        track.push(tap_at(t0 + secs(1.0), 480, 1.0), &clock); // frames 48_000..48_480
        track.drain_to(48_480); // horizon now 48_480, base_frame moves with it
        track.push(tap_at(t0 + secs(0.5), 10, 9.0), &clock); // frames 24_000..24_010: fully before
        assert_eq!(track.stats().late_chunks, 1);
        assert_eq!(track.samples.len(), 0);
        assert_eq!(track.base_frame, 48_480);
    }

    #[test]
    fn partial_overlap_chunk_writes_only_the_unrendered_part() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1);
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

    #[test]
    fn paused_chunk_is_discarded_and_counted() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(1.0));
        let mut track = Track::new(1);
        track.push(tap_at(t0 + secs(1.5), 10, 1.0), &clock); // audible inside the pause
        assert_eq!(track.stats().discarded_paused_chunks, 1);
        assert_eq!(track.samples.len(), 0);
        assert_eq!(track.write_horizon(), 0);
    }

    #[test]
    fn drain_to_pads_missing_tail_with_silence_and_counts_gap() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(2); // stereo: 2 samples per frame
        track.push(tap_at(t0, 6, 1.0), &clock); // 3 real frames (6 samples / 2ch)
        let out = track.drain_to(5); // ask for 5 frames; only 3 are real
        assert_eq!(out.len(), 10); // 5 frames * 2 channels
        assert_eq!(&out[0..6], &[1.0; 6]);
        assert_eq!(&out[6..10], &[0.0; 4]);
        assert_eq!(track.stats().gap_samples, 2); // 2 missing frames
        assert_eq!(track.base_frame, 5);
    }

    #[test]
    fn drain_to_before_or_at_the_horizon_is_a_no_op() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        let mut track = Track::new(1);
        track.push(tap_at(t0, 10, 1.0), &clock);
        track.drain_to(10);
        assert_eq!(track.drain_to(10), Vec::<f32>::new());
        assert_eq!(track.base_frame, 10);
    }
}
