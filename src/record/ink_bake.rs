//! Compositing live annotation ink into a WINDOW recording (DRAGON-691 amendment 2).
//!
//! # This is now the FALLBACK, not the normal path (DRAGON-724)
//!
//! [`crate::record::ink_live`] paints the same ink onto each frame as it is captured, which is
//! what every other capture mode already gets from the compositor. A recording that takes that
//! path stream-copies its video at finalize, exactly as an un-inked one does, and nothing in
//! this module runs.
//!
//! What is left for this module is the case the live composite's size gate refuses: a frame big
//! enough that per-tick image work would put the capture loop behind the screen. There the ink
//! is still data and this still replays it. Two things follow, and they are the reason to reach
//! for `ink_live` first whenever a choice exists:
//!
//! * The pass re-encodes the whole video, which measured 28.2s of spinner on a 3m36s take, about
//!   7.7x realtime on the HARDWARE tier after DRAGON-716's tier reuse.
//! * It replays the log against the VIDEO's frame indices, so it is only ever as right as the
//!   anchor the app stamped with. That anchor was WRONG when DRAGON-724 was written:
//!   `recording_started` latched `settled_at`, i.e. media 0 PLUS the worker's opening phase
//!   (489.8ms on the owner's session, against a 500ms measured lead in the delivered file,
//!   median of 21 samples), so every mark here landed 10 to 15 frames early. The compositing
//!   was index-correct; only the stamps were shifted. **DRAGON-725 fixed the anchor**, in the
//!   same batch: `recording_started` now latches `RecordHandle::media_zero_at`, which the
//!   worker fills inside `owned::media_zero` itself, so this path's stamps and its indices
//!   count the same file. One shared anchor was always the fix, rather than a correction
//!   applied in either compositor.
//!
//! # Why a bake exists at all
//!
//! Region and monitor recordings capture whatever is on the screen, so the ink surface is in
//! the file the moment it is drawn and nothing here runs. A WINDOW recording on macOS and
//! Windows captures the target window's OWN buffer (`SCContentFilter`'s desktop-independent
//! window, `GraphicsCaptureItem::CreateForWindow`), and no floating surface is part of another
//! window's buffer. That is exactly why window recording never had our toolbar in it, and it
//! is equally why it never had our ink.
//!
//! So on those two platforms the strokes are ALSO kept as data, in media time
//! ([`crate::ink::InkLog`]), and this module replays them over the finished video.
//!
//! Linux needs none of it: a window recording there is a fixed screen RECTANGLE (every mode
//! funnels through `spawn_screencopy_worker`'s region crop), so the overlay is captured live
//! like any other mode.
//!
//! # One pass, and the audio is not touched
//!
//! The bake is NOT a second encode bolted on after the recording. It joins the finalize pass
//! the recording already runs, as a second ffmpeg input:
//!
//! * the ink is fed in as `rawvideo` RGBA on the child's stdin and `overlay`ed onto the video;
//! * the AUDIO filtergraph, its two mute gates and its `amix` are byte-identical to a
//!   recording with no ink, and the audio is encoded exactly once, as it always was.
//!
//! That matters more than it looks. The obvious "bake only the inked spans and stream-copy the
//! rest" would have to CUT AND CONCAT, and a concat splices the audio too, where the mixer's
//! continuity guarantee is load-bearing (`src/mixer`, and CLAUDE.md's recording invariants: a
//! mid-file PTS gap in one audio stream desyncs under `amix`). Folding the overlay into the
//! existing pass buys most of the saving with none of that risk: the only thing that changes
//! is that the VIDEO is encoded instead of stream-copied, and only when there is ink.
//!
//! The cost is therefore one video encode of an inked window recording, and nothing at all for
//! every other recording. See `finalize::finalize_with_ink` for the exact argument diff.
//!
//! # What is baked, and where
//!
//! The ink is drawn in the ink surface's own POINTS, over the whole screen. The video shows
//! one window. So each frame's ink is mapped screen → video through the window's rect AT THAT
//! INSTANT ([`InkBake::rect_at`]), which is why the plan carries a rect TIMELINE rather than
//! one rectangle: a window can be moved and resized while it is being recorded, and the video
//! stays a fixed size throughout.
//!
//! Ink drawn OUTSIDE the window is clipped away, not squeezed in. That is the same rule the
//! live region clamp follows (`ink::clamp_into`): the recording is of the window, so the parts
//! of a stroke that were not over the window were never over anything the file contains. A
//! stroke that crosses the edge keeps the part that was inside, which is what the user drew
//! there.

/// Where the recorded window was, and when: `(media ms, (x, y, w, h))`.
///
/// The rect is in the platform's own capture space, which is the space `Selection` and
/// `OutputDesc` already use: physical virtual-screen pixels on Windows, global top-left points
/// on macOS. The mapping below is a ratio of two rects in that one space, so it never has to
/// know which.
pub(crate) type RectSample = (crate::ink::MediaMs, (i32, i32, i32, i32));

/// Everything the finalize pass needs to composite a session's ink.
///
/// Built by the app when the recording stops and published through `RecordHandle::ink`, the
/// same way `RecordHandle::events` carries the audio toggles: the UI records what happened and
/// the worker bakes it in at the end. `None` on that slot means "nothing to do", and finalize
/// then behaves exactly as it did before this existed.
#[derive(Debug, Clone)]
pub(crate) struct InkBake {
    /// The strokes, in media time.
    pub log: crate::ink::InkLog,
    /// Where the window was, over media time, oldest first.
    pub rects: Vec<RectSample>,
    /// The ink surface's output origin in capture space.
    pub origin: (i32, i32),
    /// Capture units per surface POINT for that output. `1.0` everywhere except a Windows
    /// monitor above 100%.
    pub point_scale: f32,
    /// The ink colour, straight alpha.
    pub color: [u8; 4],
}

impl InkBake {
    /// **Pure**, unit-tested: where the window was at media time `t_ms`.
    ///
    /// HOLD-LAST rather than interpolate. The samples come from a 100ms poll, and between two
    /// of them the honest answer is "wherever it was when we last looked": interpolating would
    /// invent a smooth glide across a jump the window made instantly, and place ink at a
    /// position it never occupied. Before the first sample the first one stands, which covers
    /// ink drawn in the moments before the first poll landed.
    pub fn rect_at(&self, t_ms: crate::ink::MediaMs) -> Option<(i32, i32, i32, i32)> {
        if self.rects.is_empty() {
            return None;
        }
        let i = self.rects.partition_point(|(at, _)| *at <= t_ms);
        Some(self.rects[i.saturating_sub(1)].1)
    }

    /// Is there anything at all to composite? An empty log, or a log whose ink never
    /// intersected the window, still costs nothing: finalize keeps its stream-copy.
    pub fn is_empty(&self) -> bool {
        self.log.clips.is_empty() || self.rects.is_empty()
    }
}

/// **Pure**, unit-tested: the map from ink-surface POINTS to VIDEO pixels, for one instant.
///
/// Three steps, folded into two multiplications:
///
/// 1. surface points to capture space, `origin + p * point_scale`;
/// 2. capture space to a fraction of the window, `(c - rect.origin) / rect.size`;
/// 3. that fraction to video pixels, `* video`.
///
/// The window's on-screen SIZE is what the video's full frame shows, whatever that size is at
/// the time, which is what makes a mid-recording resize come out right: the capture keeps
/// delivering the window's content at the fixed frame size, so a window that got bigger simply
/// means each of its points is worth fewer video pixels.
///
/// A degenerate rect (a window reported as zero-sized, which happens for a moment as one is
/// being closed) maps everything to the origin rather than dividing by zero. Nothing is drawn
/// from it in practice, because [`clip_to_video`] then rejects the whole stroke.
pub(crate) fn point_to_video(
    origin: (i32, i32),
    point_scale: f32,
    rect: (i32, i32, i32, i32),
    video: (u32, u32),
) -> impl Fn((f32, f32)) -> (f32, f32) + Copy {
    let s = if point_scale.is_finite() && point_scale > 0.0 { point_scale } else { 1.0 };
    let (rw, rh) = (rect.2.max(1) as f32, rect.3.max(1) as f32);
    let (kx, ky) = (video.0 as f32 / rw, video.1 as f32 / rh);
    let (ox, oy) = (origin.0 as f32, origin.1 as f32);
    let (rx, ry) = (rect.0 as f32, rect.1 as f32);
    move |p: (f32, f32)| {
        let (cx, cy) = (ox + p.0 * s, oy + p.1 * s);
        ((cx - rx) * kx, (cy - ry) * ky)
    }
}

/// **Pure**, unit-tested: the UNIFORM scale [`point_to_video`] carries, for the stroke widths
/// to ride.
///
/// The mapping can be very slightly anisotropic (a window whose aspect differs from the
/// video's by a rounding), and a stroke width is one number, so it takes the geometric mean.
/// That is the same "one uniform scale" contract `pen_stroke::stroke_fill_polygons` asks of
/// every caller.
pub(crate) fn video_scale(rect: (i32, i32, i32, i32), video: (u32, u32)) -> f32 {
    let (rw, rh) = (rect.2.max(1) as f32, rect.3.max(1) as f32);
    ((video.0 as f32 / rw) * (video.1 as f32 / rh)).max(0.0).sqrt()
}

/// **Pure**, unit-tested: does this polygon have any part inside the video frame?
///
/// The clip that implements "bake only the intersection". A stroke drawn beside the window is
/// dropped whole; one that crosses the edge is kept, and the rasterizer's own clipping cuts it
/// at the frame boundary. Bounds-only rather than a real polygon clip, because the rasterizer
/// clips exactly anyway and this only has to answer "is it worth drawing".
pub(crate) fn touches_video(poly: &[(f32, f32)], video: (u32, u32)) -> bool {
    let (vw, vh) = (video.0 as f32, video.1 as f32);
    let mut inside = false;
    let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for p in poly {
        if !p.0.is_finite() || !p.1.is_finite() {
            continue;
        }
        lo_x = lo_x.min(p.0);
        hi_x = hi_x.max(p.0);
        lo_y = lo_y.min(p.1);
        hi_y = hi_y.max(p.1);
        inside = true;
    }
    inside && hi_x >= 0.0 && lo_x <= vw && hi_y >= 0.0 && lo_y <= vh
}

/// **Pure**, unit-tested: how many video frames a bake has to feed.
///
/// From media 0 to the last instant anything is on screen, at the video's own rate. `None`
/// when the ink never ends (un-erased Marker ink is on screen for the rest of the recording),
/// which the caller reads as "to the end of the video".
///
/// It starts at 0 rather than at the first stroke, and that is a deliberate simplification
/// over offsetting the overlay input: the frames before the first stroke are transparent and
/// cost a memset and a pipe write each, where an offset would mean trusting `overlay`'s
/// framesync to hold the main input before its second stream begins. The bound that matters is
/// the one at the END, which is where an un-inked tail of a long recording is skipped
/// entirely.
pub(crate) fn frame_count(log: &crate::ink::InkLog, fps: u32) -> Option<u64> {
    let last = crate::ink::last_visible_ms(log)?;
    let fps = fps.max(1) as i64;
    // Round UP, and add one, so the final partially-covered frame is drawn.
    Some(((last.max(0) * fps) / 1000 + 1) as u64)
}

/// Rasterize one frame of ink into straight-alpha RGBA.
///
/// `runs` are `crate::ink::fill_runs_at`'s output, already mapped into video pixels. Each run
/// is filled as ONE path with the non-zero rule, which is the contract
/// `pen_stroke::stroke_fill_polygons` builds its polygons for: a scribble that crosses itself
/// unions instead of cancelling into holes, and a partially transparent run composites exactly
/// once instead of darkening at its own overlaps.
pub(crate) fn rasterize(
    runs: &[crate::ink::InkFillRun],
    color: [u8; 4],
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    use resvg::tiny_skia as sk;
    let mut pixmap = sk::Pixmap::new(w.max(1), h.max(1))?;
    let ident = sk::Transform::identity();
    for (alpha, polys) in runs {
        let mut pb = sk::PathBuilder::new();
        let mut any = false;
        for poly in polys {
            if !touches_video(poly, (w, h)) {
                continue;
            }
            let Some(first) = poly.first() else { continue };
            pb.move_to(first.0, first.1);
            for q in &poly[1..] {
                pb.line_to(q.0, q.1);
            }
            pb.close();
            any = true;
        }
        if !any {
            continue;
        }
        let Some(path) = pb.finish() else { continue };
        let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
        let a = (color[3] as f32 / 255.0 * alpha).clamp(0.0, 1.0);
        paint.set_color(sk::Color::from_rgba8(
            color[0],
            color[1],
            color[2],
            (a * 255.0).round() as u8,
        ));
        pixmap.fill_path(&path, &paint, sk::FillRule::Winding, ident, None);
    }
    // tiny-skia composites PREMULTIPLIED; ffmpeg's `rgba` is straight. Demultiplying here is
    // what keeps a half-faded stroke the right colour instead of a darkened one.
    Some(pixmap.take().chunks_exact(4).flat_map(demultiply).collect())
}

/// One premultiplied RGBA pixel as straight alpha.
fn demultiply(px: &[u8]) -> [u8; 4] {
    let a = px[3];
    if a == 0 || a == 255 {
        return [px[0], px[1], px[2], a];
    }
    let un = |v: u8| ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
    [un(px[0]), un(px[1]), un(px[2]), a]
}

/// Write `frames` frames of ink to `sink`, one per video frame from media 0, telling
/// `on_frame` the running count as each one lands.
///
/// Returns how many frames were written. Stops early and reports the short count if the sink
/// closes, which is what a dead ffmpeg looks like from here: the caller reaps it and reports
/// the real reason, so this must not turn a broken pipe into a panic.
///
/// `on_frame` is injected rather than reported from in here (DRAGON-728), so the counting
/// stays out of the raster loop's own business and the rule is testable with no finalize
/// around it. It counts frames HANDED OVER rather than frames encoded, and those are the same
/// thing to within ffmpeg's own queue: in production this sink is the child's stdin, the
/// writes block, and the pipe holds 64KB, which is a fraction of one frame at any size worth
/// measuring. See [`crate::record::finalize_progress`] for why that is the only honest signal
/// this pass has.
pub(crate) fn write_frames(
    bake: &InkBake,
    video: (u32, u32),
    fps: u32,
    frames: u64,
    sink: &mut impl std::io::Write,
    mut on_frame: impl FnMut(u64),
) -> u64 {
    let fps = fps.max(1);
    let blank = vec![0u8; (video.0 as usize) * (video.1 as usize) * 4];
    let mut written = 0u64;
    for i in 0..frames {
        let t_ms = (i as i64 * 1000) / fps as i64;
        let rgba = match bake.rect_at(t_ms) {
            Some(rect) if rect.2 > 0 && rect.3 > 0 => {
                let map = point_to_video(bake.origin, bake.point_scale, rect, video);
                let scale = video_scale(rect, video);
                let runs = crate::ink::fill_runs_at(&bake.log, t_ms, map, scale);
                if runs.is_empty() {
                    None
                } else {
                    rasterize(&runs, bake.color, video.0, video.1)
                }
            }
            _ => None,
        };
        let buf = rgba.as_deref().unwrap_or(&blank);
        if sink.write_all(buf).is_err() {
            break;
        }
        written += 1;
        on_frame(written);
    }
    let _ = sink.flush();
    written
}

#[cfg(test)]
mod rect_at_tests {
    use super::*;

    fn bake(rects: Vec<RectSample>) -> InkBake {
        InkBake {
            log: crate::ink::InkLog::default(),
            rects,
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        }
    }

    /// HOLD-LAST: between two samples the window was where we last saw it. Interpolating would
    /// invent a glide across a jump the window made instantly and put ink where it never was.
    #[test]
    fn a_rect_holds_until_the_next_sample() {
        let b = bake(vec![(0, (10, 10, 100, 100)), (1_000, (50, 10, 100, 100))]);
        assert_eq!(b.rect_at(0), Some((10, 10, 100, 100)));
        assert_eq!(b.rect_at(999), Some((10, 10, 100, 100)));
        assert_eq!(b.rect_at(1_000), Some((50, 10, 100, 100)));
        assert_eq!(b.rect_at(60_000), Some((50, 10, 100, 100)), "and past the last sample");
    }

    /// Before the first sample the first one stands, which covers ink drawn in the moments
    /// before the first poll landed rather than dropping it.
    #[test]
    fn a_query_before_the_first_sample_takes_it_anyway() {
        let b = bake(vec![(500, (10, 10, 100, 100))]);
        assert_eq!(b.rect_at(0), Some((10, 10, 100, 100)));
        assert_eq!(b.rect_at(-5_000), Some((10, 10, 100, 100)));
    }

    /// With no samples there is no mapping and nothing is baked, rather than everything being
    /// baked at the origin.
    #[test]
    fn no_samples_means_no_rect() {
        assert_eq!(bake(Vec::new()).rect_at(0), None);
        assert!(bake(Vec::new()).is_empty());
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    /// The window fills the video frame, so its corners map to the frame's corners and its
    /// centre to the centre. The everyday case, stated exactly.
    #[test]
    fn the_window_maps_onto_the_whole_frame() {
        let m = point_to_video((0, 0), 1.0, (100, 50, 800, 600), (1600, 1200));
        assert_eq!(m((100.0, 50.0)), (0.0, 0.0), "the window's top-left is the frame's");
        assert_eq!(m((900.0, 650.0)), (1600.0, 1200.0), "and its bottom-right");
        assert_eq!(m((500.0, 350.0)), (800.0, 600.0), "and its centre");
    }

    /// The output's ORIGIN and the surface's point scale both ride the same mapping, which is
    /// what lets ink recorded on a second monitor, or on a scaled Windows display, land in the
    /// right part of the frame.
    #[test]
    fn the_output_origin_and_point_scale_are_carried() {
        // A surface on a monitor starting at x=1920, with two capture units per point.
        let m = point_to_video((1920, 0), 2.0, (1920, 0, 800, 600), (800, 600));
        assert_eq!(m((0.0, 0.0)), (0.0, 0.0), "the surface origin is the window's corner");
        assert_eq!(m((400.0, 300.0)), (800.0, 600.0), "800 capture units across at 2x");
    }

    /// A window MOVED between two frames maps the same screen point to different video
    /// pixels, which is the whole reason the plan carries a timeline.
    #[test]
    fn moving_the_window_moves_the_mapping() {
        let before = point_to_video((0, 0), 1.0, (0, 0, 400, 300), (400, 300));
        let after = point_to_video((0, 0), 1.0, (100, 0, 400, 300), (400, 300));
        assert_eq!(before((200.0, 150.0)), (200.0, 150.0));
        assert_eq!(after((200.0, 150.0)), (100.0, 150.0), "the ink follows the window");
    }

    /// A window RESIZED mid-recording keeps filling the same frame, so each of its points is
    /// worth fewer video pixels. Without this a resize would scale the ink off the frame.
    #[test]
    fn resizing_the_window_rescales_the_mapping() {
        let small = point_to_video((0, 0), 1.0, (0, 0, 400, 300), (800, 600));
        let big = point_to_video((0, 0), 1.0, (0, 0, 800, 600), (800, 600));
        assert_eq!(small((400.0, 300.0)), (800.0, 600.0));
        assert_eq!(big((400.0, 300.0)), (400.0, 300.0));
        // And the stroke width rides the same change.
        assert_eq!(video_scale((0, 0, 400, 300), (800, 600)), 2.0);
        assert_eq!(video_scale((0, 0, 800, 600), (800, 600)), 1.0);
    }

    /// Degenerate input maps to something finite rather than dividing by zero. A window
    /// reported zero-sized for a moment while it closes must not produce NaN geometry.
    #[test]
    fn a_degenerate_rect_or_scale_stays_finite() {
        let m = point_to_video((0, 0), 0.0, (0, 0, 0, 0), (100, 100));
        let p = m((10.0, 10.0));
        assert!(p.0.is_finite() && p.1.is_finite(), "{p:?}");
        assert!(video_scale((0, 0, 0, 0), (100, 100)).is_finite());
        let m = point_to_video((0, 0), f32::NAN, (0, 0, 10, 10), (10, 10));
        assert!(m((1.0, 1.0)).0.is_finite(), "a nonsense scale degrades to 1");
    }
}

#[cfg(test)]
mod clip_tests {
    use super::*;

    /// Ink over the window is drawn.
    #[test]
    fn a_polygon_inside_the_frame_is_drawn() {
        assert!(touches_video(&[(10.0, 10.0), (20.0, 20.0)], (100, 100)));
    }

    /// Ink drawn BESIDE the window is dropped whole. The recording is of the window, so a
    /// stroke that was never over it was never over anything the file contains.
    #[test]
    fn a_polygon_outside_the_frame_is_dropped() {
        assert!(!touches_video(&[(200.0, 10.0), (250.0, 20.0)], (100, 100)));
        assert!(!touches_video(&[(-50.0, -50.0), (-10.0, -10.0)], (100, 100)));
    }

    /// A stroke that CROSSES the edge is kept, and the rasterizer cuts it at the boundary.
    /// That is "bake only the intersection", which is the same rule the live region clamp
    /// follows.
    #[test]
    fn a_polygon_crossing_the_edge_is_kept() {
        assert!(touches_video(&[(-20.0, 50.0), (20.0, 50.0)], (100, 100)));
        assert!(touches_video(&[(90.0, 50.0), (140.0, 50.0)], (100, 100)));
    }

    /// Degenerate geometry is dropped rather than drawn at an arbitrary place.
    #[test]
    fn empty_or_broken_geometry_is_dropped() {
        assert!(!touches_video(&[], (100, 100)));
        assert!(!touches_video(&[(f32::NAN, f32::NAN)], (100, 100)));
    }
}

#[cfg(test)]
mod frame_count_tests {
    use super::*;
    use crate::ink::{InkClip, InkGen, InkLog, InkTool, MediaMs};

    fn magic(at: MediaMs) -> InkLog {
        InkLog {
            clips: vec![InkClip {
                tool: InkTool::MagicMarker,
                points: vec![(0.0, 0.0), (1.0, 1.0)],
                speed: Vec::new(),
                point_ms: vec![at, at + 10],
                width: crate::ink::WIDTH_PT,
                generation: Some(0),
            }],
            gens: vec![InkGen { id: 0, last_activity_ms: at + 10 }],
            erases: Vec::new(),
            wipes: Vec::new(),
        }
    }

    /// The bake stops when the ink does, which is what keeps it proportional to the INKED
    /// span rather than to the recording's length: a five-second annotation at the start of a
    /// ten-minute take feeds a few hundred frames, not eighteen thousand.
    #[test]
    fn the_bake_stops_when_the_ink_does() {
        // Ink at 1s, gone 3s after the last stroke, so about 4s of frames at 30fps.
        let n = frame_count(&magic(1_000), 30).expect("magic ink ends");
        assert!((120..=125).contains(&n), "unexpected frame count {n}");
    }

    /// Marker ink that was never erased is on screen for the rest of the recording, so the
    /// bake has no end of its own and the caller uses the video's.
    #[test]
    fn un_erased_marker_ink_has_no_end() {
        let log = InkLog {
            clips: vec![InkClip {
                tool: InkTool::Marker,
                points: vec![(0.0, 0.0)],
                speed: Vec::new(),
                point_ms: vec![500],
                width: crate::ink::WIDTH_PT,
                generation: None,
            }],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        assert_eq!(frame_count(&log, 30), None);
    }

    /// An empty log bakes nothing at all.
    #[test]
    fn an_empty_log_bakes_no_frames() {
        assert_eq!(frame_count(&InkLog::default(), 30), Some(0).filter(|_| false));
    }

    /// The rate is honoured, and a nonsense rate does not divide by zero.
    #[test]
    fn the_frame_rate_is_honoured() {
        let a = frame_count(&magic(1_000), 60).expect("ends");
        let b = frame_count(&magic(1_000), 30).expect("ends");
        assert!(a > b, "60fps should need more frames than 30");
        assert!(frame_count(&magic(1_000), 0).is_some(), "a zero rate must not divide by zero");
    }
}

#[cfg(test)]
mod rasterize_tests {
    use super::*;

    /// The rasterizer draws where the geometry is, and leaves the rest transparent. The one
    /// thing a bake must get right: ink lands where the map put it.
    #[test]
    fn ink_lands_where_the_geometry_is() {
        // A solid square in the middle of a 40x40 frame.
        let square = vec![(15.0, 15.0), (25.0, 15.0), (25.0, 25.0), (15.0, 25.0)];
        let rgba = rasterize(&[(1.0, vec![square])], [255, 0, 0, 255], 40, 40).expect("raster");
        assert_eq!(rgba.len(), 40 * 40 * 4);
        let at = |x: usize, y: usize| rgba[(y * 40 + x) * 4 + 3];
        assert!(at(20, 20) > 250, "the middle of the square is inked");
        assert_eq!(at(2, 2), 0, "the corner of the frame is untouched");
        assert_eq!(at(35, 35), 0);
        // And it is the colour it was given, not a premultiplied darkening of it.
        let i = (20 * 40 + 20) * 4;
        assert_eq!(&rgba[i..i + 3], &[255, 0, 0]);
    }

    /// A run at partial alpha comes out at that alpha, with its colour intact. This is the
    /// dissolve reaching the file, and the demultiply is what keeps it from darkening.
    #[test]
    fn a_faded_run_keeps_its_colour() {
        let square = vec![(5.0, 5.0), (35.0, 5.0), (35.0, 35.0), (5.0, 35.0)];
        let rgba = rasterize(&[(0.5, vec![square])], [0, 255, 0, 255], 40, 40).expect("raster");
        let i = (20 * 40 + 20) * 4;
        assert!((rgba[i + 3] as i32 - 128).abs() <= 2, "alpha {}", rgba[i + 3]);
        assert!(rgba[i + 1] > 250, "green survived the demultiply: {}", rgba[i + 1]);
        assert_eq!(rgba[i], 0);
    }

    /// Geometry entirely outside the frame is dropped, which is the "bake only the
    /// intersection" rule at the raster.
    #[test]
    fn geometry_outside_the_frame_inks_nothing() {
        let far = vec![(500.0, 500.0), (510.0, 500.0), (510.0, 510.0)];
        let rgba = rasterize(&[(1.0, vec![far])], [255, 0, 0, 255], 40, 40).expect("raster");
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 0), "something inked off-frame");
    }

    /// A frame with no runs is fully transparent rather than absent, so the overlay has
    /// something to composite for every video frame.
    #[test]
    fn an_empty_frame_is_transparent() {
        let rgba = rasterize(&[], [255, 0, 0, 255], 8, 8).expect("raster");
        assert_eq!(rgba.len(), 8 * 8 * 4);
        assert!(rgba.iter().all(|b| *b == 0));
    }
}

#[cfg(test)]
mod write_frames_tests {
    use super::*;
    use crate::ink::{InkClip, InkLog, InkTool};

    /// The writer produces exactly one frame per video frame, at the frame size, and stops
    /// when asked. ffmpeg's rawvideo demuxer reads fixed-size frames, so a short or long write
    /// would desynchronise every frame after it.
    #[test]
    fn every_frame_is_written_at_the_frame_size() {
        let bake = InkBake {
            log: InkLog {
                clips: vec![InkClip {
                    tool: InkTool::Marker,
                    points: vec![(1.0, 1.0), (5.0, 5.0)],
                    speed: Vec::new(),
                    point_ms: vec![0, 10],
                    width: crate::ink::WIDTH_PT,
                    generation: None,
                }],
                gens: Vec::new(),
                erases: Vec::new(),
                wipes: Vec::new(),
            },
            rects: vec![(0, (0, 0, 10, 10))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        };
        let mut buf: Vec<u8> = Vec::new();
        let n = write_frames(&bake, (10, 10), 30, 5, &mut buf, |_| {});
        assert_eq!(n, 5);
        assert_eq!(buf.len(), 5 * 10 * 10 * 4, "one full frame each");
        // The ink is actually in there: the first frame has some opaque pixels.
        assert!(buf[..400].chunks_exact(4).any(|p| p[3] > 0), "the first frame has no ink");
    }

    /// A sink that closes (a dead ffmpeg) ends the write instead of panicking, and reports the
    /// short count so the caller can say what happened.
    #[test]
    fn a_closed_sink_stops_the_write() {
        struct Broken(usize);
        impl std::io::Write for Broken {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                self.0 -= 1;
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bake = InkBake {
            log: InkLog::default(),
            rects: vec![(0, (0, 0, 10, 10))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        };
        let n = write_frames(&bake, (4, 4), 30, 100, &mut Broken(2), |_| {});
        assert_eq!(n, 2, "stopped at the break rather than panicking");
    }

    /// DRAGON-728: the reporter is handed the running count, once per frame that really
    /// landed, and never for a frame the sink refused. The finalize spinner's percentage is
    /// this count, so a report for a write that failed would claim progress the encoder never
    /// got.
    #[test]
    fn the_reporter_counts_only_the_frames_that_landed() {
        struct Broken(usize);
        impl std::io::Write for Broken {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                self.0 -= 1;
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bake = InkBake {
            log: InkLog::default(),
            rects: vec![(0, (0, 0, 10, 10))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        };
        let mut seen: Vec<u64> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let n = write_frames(&bake, (4, 4), 30, 3, &mut buf, |c| seen.push(c));
        assert_eq!(n, 3);
        assert_eq!(seen, vec![1, 2, 3], "one report per frame, counting up");

        let mut seen: Vec<u64> = Vec::new();
        let n = write_frames(&bake, (4, 4), 30, 100, &mut Broken(2), |c| seen.push(c));
        assert_eq!(n, 2);
        assert_eq!(seen, vec![1, 2], "the refused frame is not reported");
    }
}
