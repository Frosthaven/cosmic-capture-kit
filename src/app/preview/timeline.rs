//! The video preview's timeline editor (DRAGON-114): the transport strip's
//! seek bar grown into a three-lane timeline — the video track, the mixed
//! soundtrack's left and right channels — with a seeking arm, a razor tool
//! that splits the timeline into segments, and segment select/delete.
//!
//! The model is a list of KEPT source spans ([`Timeline::spans`]): the edited
//! video is their concatenation, so "delete a segment and everything slides
//! left" is inherent — segment x-positions derive from the cumulative kept
//! lengths, never stored. A razor cut only divides a span (the output is
//! unchanged until something is deleted); [`Timeline::edited`] is therefore
//! "some content is gone", which is what gates the export re-encode.
//!
//! Times are SOURCE seconds everywhere in the model and the app (the playhead,
//! scrubs, and single-frame decodes all address the unedited file on disk);
//! the widget maps to/from EDITED seconds only to draw and hit-test. Undo/redo
//! snapshots are plain `Vec<Span>` clones pushed through the preview's shared
//! edit history (see `edit::EditOp`).

use super::*;
use std::path::Path;

/// Minimum kept-segment length (seconds): a razor cut this close to a segment
/// edge is ignored (a sliver segment can't be meaningfully selected or played).
pub const MIN_SEG_SECS: f32 = 0.05;

/// The drawn height of the lane stack (video + L + R, with the inter-lane gaps).
/// `PreviewSurface::transport_h` reserves this, so every sizing path follows.
pub const LANES_H: f32 = 168.0;

/// The measurement ruler above the lanes (DRAGON-116): the seek arm's ball
/// head rides the top band, the time-code labels sit below it, and the tick
/// marks hang to the baseline — tall enough that the ball clears the codes.
/// Part of the same canvas (and the same transport bar), stacked over the lanes.
pub const RULER_H: f32 = 34.0;

/// Peak-bucket count for the waveform lanes — enough columns for a wide monitor,
/// small enough that a long recording's decode still bins instantly.
pub const WAVE_BUCKETS: usize = 1024;

/// A kept span of the SOURCE recording, in seconds. Half-open in spirit
/// (`start..end`); always `start < end` by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f32,
    pub end: f32,
}

impl Span {
    pub fn len(self) -> f32 {
        (self.end - self.start).max(0.0)
    }
}

/// Where a playing stream sits relative to the kept spans — drives the
/// playback gap-skip (`App::playback_tick`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayPos {
    /// Inside kept content: keep presenting frames.
    Inside,
    /// In a deleted gap: restart the stream at this source time.
    Jump(f32),
    /// Past the last kept span: playback is done.
    Ended,
}

/// The cut/delete edit state of one recording: ordered, non-overlapping kept
/// spans plus the transient UI state (selection, razor mode) that never enters
/// undo history.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    /// The source duration the spans were carved from (fixed at probe time).
    pub duration: f32,
    /// Kept source spans, ascending. Never empty.
    pub spans: Vec<Span>,
    /// The selected segments (indices into `spans`) — multi-select via
    /// ctrl/shift click and box select.
    pub selected: std::collections::BTreeSet<usize>,
    /// The last plainly-clicked/toggled segment — shift-click ranges span
    /// from here to the click.
    pub anchor: Option<usize>,
    /// The razor (cut) tool is armed: LANE clicks split instead of selecting
    /// (the ruler above always seeks, whatever tool is armed).
    pub razor: bool,
    /// The right-click context menu, when open: the clicked SOURCE time (what
    /// "Cut here" splits at) and the widget-local click point (anchors the
    /// popover). Transient UI state, like `selected`/`razor` — never in undo.
    pub menu: Option<(f32, f32, f32)>,
}

/// Tolerance for "the playhead reached a span edge" — under a frame at any
/// realistic fps, so a gap jump can't skip real content.
const EDGE_EPS: f32 = 0.02;

impl Timeline {
    /// A fresh, uncut timeline covering the whole recording.
    pub fn new(duration: f32) -> Self {
        Self {
            duration: duration.max(0.0),
            spans: vec![Span { start: 0.0, end: duration.max(0.0) }],
            selected: std::collections::BTreeSet::new(),
            anchor: None,
            razor: false,
            menu: None,
        }
    }

    /// Whether any content has been DELETED (razor cuts alone don't change the
    /// output — the concatenation of adjacent halves is the original).
    pub fn edited(&self) -> bool {
        self.edited_duration() + 0.001 < self.duration
    }

    /// Total kept length — the edited video's duration.
    pub fn edited_duration(&self) -> f32 {
        self.spans.iter().map(|s| s.len()).sum()
    }

    /// First kept instant (source seconds) — where Play restarts from the top.
    pub fn first_start(&self) -> f32 {
        self.spans.first().map(|s| s.start).unwrap_or(0.0)
    }

    /// Last kept instant (source seconds) — the edited video's end on the
    /// source timeline.
    pub fn end(&self) -> f32 {
        self.spans.last().map(|s| s.end).unwrap_or(0.0)
    }

    /// The segment containing source time `t`, if `t` sits in kept content.
    pub fn span_at_source(&self, t: f32) -> Option<usize> {
        self.spans.iter().position(|s| t >= s.start && t < s.end)
    }

    /// Map a source time onto the edited timeline: inside a span it lands
    /// proportionally; in a deleted gap it collapses to the seam; past the end
    /// it clamps to the edited duration.
    pub fn source_to_edited(&self, t: f32) -> f32 {
        let mut acc = 0.0;
        for s in &self.spans {
            if t < s.start {
                return acc;
            }
            if t < s.end {
                return acc + (t - s.start);
            }
            acc += s.len();
        }
        acc
    }

    /// Map an edited time back to the source timeline (clamped to kept
    /// content). A seam time belongs to the LATER span (its start), so the
    /// inverse of `source_to_edited`'s gap collapse lands after the cut.
    pub fn edited_to_source(&self, t: f32) -> f32 {
        let mut acc = 0.0;
        for s in &self.spans {
            if t < acc + s.len() {
                return s.start + (t - acc).max(0.0);
            }
            acc += s.len();
        }
        self.end()
    }

    /// Split the segment containing source time `t` in two. Ignored (returns
    /// `false`) when `t` is in a gap or within [`MIN_SEG_SECS`] of an edge —
    /// the caller only pushes undo history on `true`.
    pub fn cut_at_source(&mut self, t: f32) -> bool {
        let Some(i) = self.span_at_source(t) else {
            return false;
        };
        let s = self.spans[i];
        if t - s.start < MIN_SEG_SECS || s.end - t < MIN_SEG_SECS {
            return false;
        }
        self.spans[i] = Span { start: s.start, end: t };
        self.spans.insert(i + 1, Span { start: t, end: s.end });
        // A cut inside a selected segment leaves the FIRST half selected;
        // segments after the cut shift up by one (the anchor follows).
        self.selected = self
            .selected
            .iter()
            .map(|&sel| if sel > i { sel + 1 } else { sel })
            .collect();
        if let Some(a) = self.anchor
            && a > i
        {
            self.anchor = Some(a + 1);
        }
        true
    }

    /// Delete segment `i`; later segments slide left inherently (edited
    /// positions are cumulative). Refused for the only remaining segment —
    /// an empty timeline has nothing to preview or save.
    pub fn delete(&mut self, i: usize) -> bool {
        if self.spans.len() <= 1 || i >= self.spans.len() {
            return false;
        }
        self.spans.remove(i);
        self.selected = self
            .selected
            .iter()
            .filter(|&&sel| sel != i)
            .map(|&sel| if sel > i { sel - 1 } else { sel })
            .collect();
        self.anchor = match self.anchor {
            Some(a) if a == i => None,
            Some(a) if a > i => Some(a - 1),
            keep => keep,
        };
        true
    }

    /// Delete EVERY selected segment in one edit (the caller pushes one undo
    /// snapshot). Refused when nothing is selected or the selection covers
    /// the whole timeline — something must remain to preview or save.
    pub fn delete_selected(&mut self) -> bool {
        if self.selected.is_empty() || self.selected.len() >= self.spans.len() {
            return false;
        }
        // Descending, so earlier removals can't shift the later indices; the
        // pre-check above means [`Self::delete`] can never hit its
        // last-segment refusal partway through.
        let picked: Vec<usize> = std::mem::take(&mut self.selected).into_iter().rev().collect();
        for i in picked {
            self.delete(i);
        }
        true
    }

    /// Plain click: select only `i` (`None` — a click away from any segment —
    /// deselects all). The click becomes the shift-range anchor.
    pub fn select_only(&mut self, i: Option<usize>) {
        self.selected.clear();
        self.selected.extend(i);
        self.anchor = i;
    }

    /// Ctrl-click: toggle `i` in/out of the selection; it becomes the new
    /// range anchor either way.
    pub fn select_toggle(&mut self, i: usize) {
        if !self.selected.remove(&i) {
            self.selected.insert(i);
        }
        self.anchor = Some(i);
    }

    /// Shift-click: select the contiguous run from the anchor (the last plain
    /// click/toggle; the click itself when there is none) to `i`, replacing
    /// the selection. The anchor stays put so a further shift-click re-ranges.
    pub fn select_range_to(&mut self, i: usize) {
        let a = self.anchor.unwrap_or(i);
        self.selected = (a.min(i)..=a.max(i)).collect();
        self.anchor = Some(a);
    }

    /// Box select: every segment whose EDITED span intersects `a..b` seconds.
    /// `additive` (ctrl/shift held) keeps the existing selection.
    pub fn select_edited_range(&mut self, a: f32, b: f32, additive: bool) {
        let (a, b) = (a.min(b), a.max(b));
        if !additive {
            self.selected.clear();
        }
        let mut acc = 0.0;
        for (i, s) in self.spans.iter().enumerate() {
            let (s0, s1) = (acc, acc + s.len());
            acc = s1;
            if s1 > a && s0 < b {
                self.selected.insert(i);
            }
        }
    }

    /// Restore spans from an undo/redo snapshot. Selection (and any open
    /// context menu) clears — the indices may no longer name what the user
    /// had selected.
    pub fn restore(&mut self, spans: Vec<Span>) {
        self.spans = spans;
        self.selected.clear();
        self.anchor = None;
        self.menu = None;
    }

    /// Classify a playing position against the kept spans (see [`PlayPos`]).
    pub fn play_pos(&self, t: f32) -> PlayPos {
        for s in &self.spans {
            if t < s.end - EDGE_EPS {
                return if t >= s.start - EDGE_EPS {
                    PlayPos::Inside
                } else {
                    PlayPos::Jump(s.start)
                };
            }
        }
        PlayPos::Ended
    }

    /// A playable source position for starting playback at `t`: inside kept
    /// content it's `t`; in a gap it snaps to the next kept span; at/past the
    /// edited end it wraps to the first span (the "play again" behavior).
    pub fn play_start(&self, t: f32) -> f32 {
        match self.play_pos(t) {
            PlayPos::Inside => t,
            PlayPos::Jump(next) => next,
            PlayPos::Ended => self.first_start(),
        }
    }
}

/// Decode the soundtrack (first audio stream, downmixed/split to stereo at a
/// coarse rate) and bin it into [`WAVE_BUCKETS`] per-channel peak buckets over
/// the SOURCE duration. `None` when the file has no decodable audio. Runs
/// off-thread (spawned from the `PosterReady` handler).
pub(super) fn extract_waveform(path: &Path) -> Option<Vec<(f32, f32)>> {
    let out = crate::util::ffmpeg_command()
        .args(["-v", "error"])
        .arg("-i")
        .arg(path)
        // 8 kHz stereo s16 is plenty for peak buckets and keeps a long
        // recording's decode tiny (~2 MB/min).
        .args(["-map", "0:a:0", "-ac", "2", "-ar", "8000", "-f", "s16le", "pipe:1"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let samples: Vec<i16> = out
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    Some(bucket_peaks(&samples, WAVE_BUCKETS))
}

/// Bin interleaved L/R samples into `buckets` per-channel peaks (0..=1).
/// Short input still yields `buckets` entries (trailing buckets flat) so the
/// draw code never rescales.
pub(super) fn bucket_peaks(interleaved: &[i16], buckets: usize) -> Vec<(f32, f32)> {
    let frames = interleaved.len() / 2;
    let mut out = vec![(0.0f32, 0.0f32); buckets.max(1)];
    if frames == 0 {
        return out;
    }
    let per = (frames as f32 / out.len() as f32).max(1.0);
    for f in 0..frames {
        let b = ((f as f32 / per) as usize).min(out.len() - 1);
        let l = (interleaved[2 * f] as f32 / 32768.0).abs();
        let r = (interleaved[2 * f + 1] as f32 / 32768.0).abs();
        out[b].0 = out[b].0.max(l);
        out[b].1 = out[b].1.max(r);
    }
    out
}

// ---------------------------------------------------------------------------
// The canvas widget: the measurement ruler + lanes + segments + waveform +
// seek arm, and the mouse handling (seek-drag, select, razor cut with
// soft-snap). Pure drawing/hit-testing over the model — every edit goes out
// as a message.
// ---------------------------------------------------------------------------

/// Lane geometry below the ruler (video on top, then L and R audio) — widget
/// y-offsets, so each is [`RULER_H`] plus its position within [`LANES_H`].
const VIDEO_LANE: (f32, f32) = (RULER_H, 78.0);
const L_LANE: (f32, f32) = (RULER_H + 87.0, 36.0);
const R_LANE: (f32, f32) = (RULER_H + 132.0, 36.0);

/// Soft-snap distance (px): a razor hover/click this close to the seek arm
/// lands exactly ON the playhead (DRAGON-116).
const SNAP_PX: f32 = 8.0;

/// The seek head's ball radius. The timeline's content is inset horizontally
/// by exactly this much on each side, so the ball never overhangs the
/// widget/window edge at either extreme of the strip.
const BALL_R: f32 = 5.5;

/// The horizontal content span at widget width `w`: `(left, width)` of the
/// [`BALL_R`]-inset area every time↔x mapping (segments, ticks, arm,
/// hit-tests) works in.
fn content_span(w: f32) -> (f32, f32) {
    (BALL_R, (w - 2.0 * BALL_R).max(1.0))
}

/// The ruler's MAJOR tick interval (edited seconds) at width `w`: the smallest
/// "nice" clock step keeping labelled ticks ≥ ~90px apart (a full
/// `00:00:00:00` timecode plus breathing room). Past an hour per label it
/// falls back to whole-hour multiples.
pub(super) fn ruler_step(duration: f32, w: f32) -> f32 {
    const STEPS: [f32; 16] = [
        0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0,
        1800.0, 3600.0,
    ];
    let min_step = 90.0 * duration.max(0.001) / w.max(1.0);
    STEPS
        .iter()
        .copied()
        .find(|s| *s >= min_step)
        .unwrap_or_else(|| (min_step / 3600.0).ceil() * 3600.0)
}

/// The ONE timecode format every readout in the editor uses (DRAGON-116):
/// SMPTE-style fixed-width `HH:MM:SS:FF`, the last group frames at `fps`.
pub(super) fn fmt_timecode(secs: f32, fps: f32) -> String {
    let fps = fps.max(1.0).round() as u64;
    let total = (secs.max(0.0) as f64 * fps as f64).round() as u64;
    let (whole, frames) = (total / fps, total % fps);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        whole / 3600,
        (whole % 3600) / 60,
        whole % 60,
        frames
    )
}

/// The timeline strip, borrowed from the preview state for one view pass.
pub(super) struct TimelineCanvas<'a> {
    pub timeline: &'a Timeline,
    /// Playhead in SOURCE seconds (the arm draws at its edited mapping).
    pub position: f32,
    /// Source frame rate — the ruler timecodes' frames field.
    pub fps: f32,
    /// Per-bucket L/R peaks over the SOURCE duration, once extracted.
    pub waveform: Option<&'a [(f32, f32)]>,
}

/// Per-widget interaction state: the hover point (drives the razor's cut
/// indicator), whether a ruler seek-drag is in progress, the live modifier
/// keys, and a pending lane press that becomes a click select on release or
/// a box select once dragged past the slop.
#[derive(Default)]
pub(super) struct TlState {
    hover: Option<cosmic::iced::Point>,
    dragging: bool,
    /// Modifier keys, tracked from the keyboard events the canvas receives.
    mods: cosmic::iced::keyboard::Modifiers,
    /// A pointer press in the lanes: `(press point, ctrl, shift)` — the
    /// modifiers captured AT the press, like most editors.
    press: Option<(cosmic::iced::Point, bool, bool)>,
    /// The press has dragged past the slop: a box select is live.
    boxing: bool,
    /// The box's moving corner (raw widget-local — may run past the bounds
    /// so a drag can sweep beyond an edge; the draw clamps it).
    box_end: Option<cosmic::iced::Point>,
}

/// The pixels a lane press may wander before it stops being a click and
/// becomes a box select.
const BOX_SLOP: f32 = 4.0;

/// Whether a widget-local point sits ON segment content — within one of the
/// three lane bands (the inter-lane gaps and the ruler are "away from any
/// segment": a plain click there deselects).
fn lane_hit(p: cosmic::iced::Point) -> bool {
    [VIDEO_LANE, L_LANE, R_LANE]
        .iter()
        .any(|&(y, h)| p.y >= y && p.y < y + h)
}

impl TimelineCanvas<'_> {
    /// Map a widget x to EDITED seconds (through the ball-radius inset).
    fn x_to_edited(&self, x: f32, w: f32) -> f32 {
        let (left, cw) = content_span(w);
        let ed = self.timeline.edited_duration();
        ((x - left) / cw).clamp(0.0, 1.0) * ed
    }

    /// Map a widget x to SOURCE seconds (what every message carries).
    fn x_to_source(&self, x: f32, w: f32) -> f32 {
        self.timeline.edited_to_source(self.x_to_edited(x, w))
    }

    /// The seek arm's widget x for the current playhead.
    fn arm_x(&self, w: f32) -> f32 {
        let (left, cw) = content_span(w);
        let ed = self.timeline.edited_duration().max(0.001);
        left + (self.timeline.source_to_edited(self.position) / ed * cw).clamp(0.0, cw)
    }

    /// The razor's cut instant for a click at `x`: soft-snapped onto the
    /// playhead when within [`SNAP_PX`] of the seek arm, else the click's own
    /// time. The snap cuts at the arm's KEPT source instant (a gap-stranded
    /// playhead draws at the seam — cut where the arm actually is).
    fn razor_time(&self, x: f32, w: f32) -> f32 {
        if (x - self.arm_x(w)).abs() <= SNAP_PX {
            self.timeline
                .edited_to_source(self.timeline.source_to_edited(self.position))
        } else {
            self.x_to_source(x, w)
        }
    }
}

impl cosmic::widget::canvas::Program<Msg, cosmic::Theme, cosmic::Renderer> for TimelineCanvas<'_> {
    type State = TlState;

    fn update(
        &self,
        state: &mut TlState,
        event: &cosmic::widget::canvas::Event,
        bounds: cosmic::iced::Rectangle,
        cursor: cosmic::iced::mouse::Cursor,
    ) -> Option<cosmic::widget::canvas::Action<Msg>> {
        use cosmic::iced::{keyboard, mouse};
        use cosmic::widget::canvas::{Action, Event};
        let ev = match event {
            Event::Mouse(ev) => ev,
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                // Keep the live modifiers for the press handler below.
                state.mods = *m;
                return None;
            }
            _ => return None,
        };
        let pos = cursor.position_in(bounds);
        // The raw widget-local point, defined even outside the bounds — the
        // scrub drag and the box select both follow the cursor past an edge.
        let raw = cursor
            .position()
            .map(|p| cosmic::iced::Point::new(p.x - bounds.x, p.y - bounds.y));
        match ev {
            mouse::Event::CursorMoved { .. } => {
                state.hover = pos;
                if state.dragging {
                    // Seek-drag: follow the cursor even slightly outside the
                    // bounds (clamp), like a slider grab.
                    let x = raw.map(|p| p.x).unwrap_or_default();
                    let t = self.x_to_source(x, bounds.width);
                    return Some(
                        Action::publish(Msg::Preview(PreviewMsg::TimelineSeek(t))).and_capture(),
                    );
                }
                if let Some((p0, ..)) = state.press {
                    // A live lane press: past the slop it's a box select.
                    state.box_end = raw;
                    if let Some(p1) = raw
                        && !state.boxing
                        && p1.distance(p0) > BOX_SLOP
                    {
                        state.boxing = true;
                    }
                    return Some(Action::request_redraw().and_capture());
                }
                // Repaint so the razor's hover line tracks the cursor.
                self.timeline.razor.then(Action::request_redraw)
            }
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let p = pos?;
                if p.y <= RULER_H {
                    // The RULER is the seek strip (whatever tool is armed):
                    // click jumps the head there and starts a scrub drag.
                    let t = self.x_to_source(p.x, bounds.width);
                    state.dragging = true;
                    return Some(
                        Action::publish(Msg::Preview(PreviewMsg::TimelineSeek(t))).and_capture(),
                    );
                }
                if self.timeline.razor {
                    // Cut where the preview line shows: soft-snapped to the arm.
                    let t = self.razor_time(p.x, bounds.width);
                    return Some(
                        Action::publish(Msg::Preview(PreviewMsg::TimelineCut(t))).and_capture(),
                    );
                }
                // Pointer in the LANES: a pending select — resolved at release
                // as a click (segment / away) or a box select once dragged.
                // The playhead never moves from here (that's the ruler's job).
                state.press = Some((p, state.mods.control(), state.mods.shift()));
                state.boxing = false;
                state.box_end = None;
                Some(Action::capture())
            }
            mouse::Event::ButtonPressed(mouse::Button::Right) => {
                // Context menu at the click: "Cut here" / "Delete segment".
                let p = pos?;
                let t = self.x_to_source(p.x, bounds.width);
                Some(
                    Action::publish(Msg::Preview(PreviewMsg::TimelineMenuOpen(t, p.x, p.y)))
                        .and_capture(),
                )
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                if state.dragging {
                    state.dragging = false;
                    return Some(Action::capture());
                }
                let (p0, ctrl, shift) = state.press.take()?;
                if state.boxing {
                    // Box select over the EDITED span the drag covered.
                    state.boxing = false;
                    let p1 = state.box_end.take().unwrap_or(p0);
                    let a = self.x_to_edited(p0.x.min(p1.x), bounds.width);
                    let b = self.x_to_edited(p0.x.max(p1.x), bounds.width);
                    return Some(
                        Action::publish(Msg::Preview(PreviewMsg::TimelineBoxSelect(
                            a,
                            b,
                            ctrl || shift,
                        )))
                        .and_capture(),
                    );
                }
                // A plain click: the segment under it — or None when the
                // click was away from any segment (an inter-lane gap).
                let t = lane_hit(p0).then(|| self.x_to_source(p0.x, bounds.width));
                Some(
                    Action::publish(Msg::Preview(PreviewMsg::TimelineSelect(t, ctrl, shift)))
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &TlState,
        bounds: cosmic::iced::Rectangle,
        cursor: cosmic::iced::mouse::Cursor,
    ) -> cosmic::iced::mouse::Interaction {
        use cosmic::iced::mouse::Interaction;
        let Some(p) = cursor.position_in(bounds) else {
            return Interaction::None;
        };
        if p.y <= RULER_H {
            // The ruler is the seek strip whatever tool is armed — the
            // regular cursor, never the razor's crosshair.
            Interaction::Idle
        } else if self.timeline.razor {
            // Over the LANES the cursor mirrors the armed tool (the
            // pointer/razor toggle): a crosshair for the razor…
            Interaction::Crosshair
        } else {
            // …and the plain arrow for the pointer.
            Interaction::Idle
        }
    }

    fn draw(
        &self,
        state: &TlState,
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::widget::canvas::Geometry<cosmic::Renderer>> {
        use cosmic::iced::{Color, Point, Size};
        use cosmic::widget::canvas::{Frame, Path, Stroke};
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let c = theme.cosmic();
        let accent = crate::app::theme::accent(theme);
        // The audio-lane bed is fully transparent (DRAGON-217): the tracks sit
        // directly on the transport strip, so a frosted window's glass — or the
        // plain body colour when unfrosted — carries straight through them instead
        // of an opaque gray component bed. Universal (all platforms) by design.
        let lane_bg = Color::TRANSPARENT;
        let video_fill = Color { a: 0.35, ..accent };
        let video_fill_sel = Color { a: 0.6, ..accent };
        let wave = Color { a: 0.9, ..accent };
        let ed_dur = self.timeline.edited_duration().max(0.001);
        let src_dur = self.timeline.duration.max(0.001);
        let (left, cw) = content_span(w);
        // The tracks follow the user's COSMIC rounding rule (round / slightly
        // round / square) at PANEL rounding (the small token — they're content
        // strips, not buttons); capped so a sliver segment can't out-round its
        // own box.
        let seg_r = crate::app::theme::rounding(theme).s1();

        // Segments: x-ranges from the cumulative kept lengths (ripple built in).
        let mut acc = 0.0f32;
        for (i, s) in self.timeline.spans.iter().enumerate() {
            let x0 = left + acc / ed_dur * cw;
            let x1 = left + (acc + s.len()) / ed_dur * cw;
            acc += s.len();
            // A hairline gap at each seam so cuts read without moving content.
            let (gx0, gx1) = (
                if i > 0 { x0 + 1.0 } else { x0 },
                if i + 1 < self.timeline.spans.len() { x1 - 1.0 } else { x1 },
            );
            let seg_w = (gx1 - gx0).max(1.0);
            let selected = self.timeline.selected.contains(&i);
            // Video lane.
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(gx0, VIDEO_LANE.0),
                    Size::new(seg_w, VIDEO_LANE.1),
                    seg_r.min(seg_w / 2.0).min(VIDEO_LANE.1 / 2.0).into(),
                ),
                if selected { video_fill_sel } else { video_fill },
            );
            // Audio lanes: bed + per-column channel peaks from the buckets.
            for (y, h) in [L_LANE, R_LANE] {
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(gx0, y),
                        Size::new(seg_w, h),
                        seg_r.min(seg_w / 2.0).min(h / 2.0).into(),
                    ),
                    lane_bg,
                );
            }
            if let Some(buckets) = self.waveform.filter(|b| !b.is_empty()) {
                let n = buckets.len();
                let mut x = gx0;
                while x < gx1 {
                    // This column's source time → bucket.
                    let frac = ((x - x0) / (x1 - x0).max(0.001)).clamp(0.0, 1.0);
                    let t = s.start + frac * s.len();
                    let b = ((t / src_dur * n as f32) as usize).min(n - 1);
                    let (l, r) = buckets[b];
                    for ((y, h), peak) in [(L_LANE, l), (R_LANE, r)] {
                        let mid = y + h / 2.0;
                        let half = (h / 2.0 - 1.0) * peak.clamp(0.0, 1.0);
                        frame.fill_rectangle(
                            Point::new(x, mid - half.max(0.5)),
                            Size::new(1.0, (2.0 * half).max(1.0)),
                            wave,
                        );
                    }
                    x += 2.0;
                }
            }
            if selected {
                // Selection ring around the whole segment column (lanes only —
                // the ruler above is shared ground, not segment content).
                frame.stroke(
                    &Path::rounded_rectangle(
                        Point::new(gx0 + 0.75, RULER_H + 0.75),
                        Size::new(seg_w - 1.5, LANES_H - 1.5),
                        seg_r.min((seg_w - 1.5) / 2.0).into(),
                    ),
                    Stroke::default().with_color(accent).with_width(1.5),
                );
            }
        }

        // The measurement ruler: a subdued baseline + ticks over the lanes,
        // with `HH:MM:SS:FF` timecodes (readable, near-foreground — the ticks
        // stay quiet, the codes must not) on the majors, in EDITED time — the
        // timeline the lanes draw.
        let subdued = crate::app::theme::subdued(theme);
        let label = {
            let on: Color = c.background.on.into();
            Color { a: 0.85, ..on }
        };
        frame.fill_rectangle(
            Point::new(left, RULER_H - 1.0),
            Size::new(cw, 1.0),
            Color { a: 0.5, ..subdued },
        );
        let step = ruler_step(ed_dur, cw);
        let minor = step / 5.0;
        let mut i = 0u32;
        loop {
            let t = i as f32 * minor;
            if t > ed_dur {
                break;
            }
            let x = left + (t / ed_dur * cw).min(cw - 0.5);
            let major = i.is_multiple_of(5);
            let h = if major { 7.0 } else { 4.0 };
            frame.fill_rectangle(
                Point::new(x - 0.5, RULER_H - 1.0 - h),
                Size::new(1.0, h),
                Color { a: if major { 0.8 } else { 0.45 }, ..subdued },
            );
            // Timecodes on the majors, on their own band BELOW the ball's
            // riding strip; the last label is skipped when it would run off
            // the right edge.
            if major && x + 3.0 < w - 64.0 {
                frame.fill_text(cosmic::widget::canvas::Text {
                    content: fmt_timecode(t, self.fps),
                    position: Point::new(x + 3.0, 2.0 * BALL_R + 2.0),
                    color: label,
                    size: 9.0.into(),
                    font: cosmic::font::mono(),
                    ..Default::default()
                });
            }
            i += 1;
        }

        // The seek arm's x at the playhead's EDITED spot (drawn last, below).
        let ax = self.arm_x(w);

        // The razor's would-cut indicator under the cursor, soft-snapping onto
        // the seek arm (matching what a click cuts — see `razor_time`). Only
        // while hovering the LANES: over the ruler a click seeks, not cuts.
        if self.timeline.razor
            && let Some(p) = state.hover.filter(|p| p.y > RULER_H)
        {
            let razor: Color = crate::app::theme::DANGER;
            let x = if (p.x - ax).abs() <= SNAP_PX {
                ax
            } else {
                p.x.clamp(left, left + cw)
            };
            frame.fill_rectangle(
                Point::new(x - 0.5, RULER_H),
                Size::new(1.0, LANES_H),
                Color { a: 0.85, ..razor },
            );
        }

        // The live box-select rectangle: the trim (accent) colour at 50%
        // transparency for the border, 75% for the fill, clamped into the
        // canvas (the drag may run past an edge).
        if state.boxing
            && let (Some((p0, ..)), Some(p1)) = (state.press, state.box_end)
        {
            let bottom = RULER_H + LANES_H;
            let (x0, x1) = (
                p0.x.min(p1.x).clamp(0.0, w),
                p0.x.max(p1.x).clamp(0.0, w),
            );
            let (y0, y1) = (
                p0.y.min(p1.y).clamp(0.0, bottom),
                p0.y.max(p1.y).clamp(0.0, bottom),
            );
            let (bw, bh) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
            frame.fill_rectangle(
                Point::new(x0, y0),
                Size::new(bw, bh),
                Color { a: 0.25, ..accent },
            );
            frame.stroke(
                &Path::rectangle(Point::new(x0, y0), Size::new(bw, bh)),
                Stroke::default()
                    .with_color(Color { a: 0.5, ..accent })
                    .with_width(1.0),
            );
        }

        // The seeking arm: a danger-red stem from the measurement bar's very
        // top edge down through the lanes, its ball head capping the top.
        let arm: Color = crate::app::theme::DANGER;
        frame.fill_rectangle(
            Point::new(ax - 1.5, 0.0),
            Size::new(3.0, RULER_H + LANES_H),
            arm,
        );
        frame.fill(&Path::circle(Point::new(ax, BALL_R), BALL_R), arm);

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl(duration: f32) -> Timeline {
        Timeline::new(duration)
    }

    #[test]
    fn a_fresh_timeline_is_one_full_span_and_not_edited() {
        let t = tl(10.0);
        assert_eq!(t.spans, vec![Span { start: 0.0, end: 10.0 }]);
        assert!(!t.edited());
        assert_eq!(t.edited_duration(), 10.0);
    }

    #[test]
    fn a_cut_divides_but_does_not_edit() {
        let mut t = tl(10.0);
        assert!(t.cut_at_source(4.0));
        assert_eq!(
            t.spans,
            vec![Span { start: 0.0, end: 4.0 }, Span { start: 4.0, end: 10.0 }]
        );
        // Nothing deleted: the concatenation is still the whole recording.
        assert!(!t.edited());
        assert_eq!(t.edited_duration(), 10.0);
    }

    #[test]
    fn cuts_too_close_to_an_edge_are_refused() {
        let mut t = tl(10.0);
        assert!(!t.cut_at_source(0.01), "sliver at the start");
        assert!(!t.cut_at_source(9.99), "sliver at the end");
        assert!(!t.cut_at_source(-1.0), "before the timeline");
        assert!(!t.cut_at_source(11.0), "past the timeline");
        assert_eq!(t.spans.len(), 1);
    }

    #[test]
    fn deleting_a_middle_segment_ripples_the_rest_left() {
        let mut t = tl(10.0);
        t.cut_at_source(3.0);
        t.cut_at_source(6.0);
        assert!(t.delete(1)); // drop 3..6
        assert_eq!(
            t.spans,
            vec![Span { start: 0.0, end: 3.0 }, Span { start: 6.0, end: 10.0 }]
        );
        assert!(t.edited());
        assert_eq!(t.edited_duration(), 7.0);
        // The second kept span starts at edited 3.0 — slid into the gap.
        assert_eq!(t.source_to_edited(6.0), 3.0);
        assert_eq!(t.edited_to_source(3.0), 6.0);
    }

    #[test]
    fn deleting_the_first_segment_slides_the_rest_to_the_front() {
        let mut t = tl(10.0);
        t.cut_at_source(2.0);
        assert!(t.delete(0));
        assert_eq!(t.spans, vec![Span { start: 2.0, end: 10.0 }]);
        // Edited time 0 is now source time 2.
        assert_eq!(t.edited_to_source(0.0), 2.0);
        assert_eq!(t.first_start(), 2.0);
    }

    #[test]
    fn the_last_remaining_segment_cannot_be_deleted() {
        let mut t = tl(10.0);
        assert!(!t.delete(0));
        t.cut_at_source(5.0);
        assert!(t.delete(0));
        assert!(!t.delete(0), "sole survivor must stay");
        assert_eq!(t.spans.len(), 1);
    }

    fn sel(t: &Timeline) -> Vec<usize> {
        t.selected.iter().copied().collect()
    }

    #[test]
    fn selection_tracks_deletes_and_cuts() {
        let mut t = tl(12.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0); // segments: 0..4, 4..8, 8..12
        t.select_only(Some(2));
        assert!(t.delete(0));
        assert_eq!(sel(&t), vec![1], "selection shifts down past a delete");
        assert!(t.delete(1));
        assert!(t.selected.is_empty(), "deleting the selection clears it");
        // A cut before the selection shifts it up.
        let mut t = tl(12.0);
        t.cut_at_source(8.0);
        t.select_only(Some(1));
        t.cut_at_source(4.0);
        assert_eq!(sel(&t), vec![2]);
    }

    #[test]
    fn plain_select_replaces_and_a_miss_deselects_all() {
        let mut t = tl(12.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0);
        t.select_only(Some(0));
        t.select_only(Some(2));
        assert_eq!(sel(&t), vec![2], "plain click replaces");
        t.select_only(None);
        assert!(t.selected.is_empty(), "clicking away deselects all");
        assert_eq!(t.anchor, None);
    }

    #[test]
    fn ctrl_toggle_grows_and_shrinks_the_selection() {
        let mut t = tl(12.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0);
        t.select_only(Some(0));
        t.select_toggle(2);
        assert_eq!(sel(&t), vec![0, 2]);
        t.select_toggle(0);
        assert_eq!(sel(&t), vec![2], "toggling a selected segment removes it");
    }

    #[test]
    fn shift_range_selects_from_the_anchor() {
        let mut t = tl(20.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0);
        t.cut_at_source(12.0);
        t.cut_at_source(16.0); // 5 segments
        t.select_only(Some(1));
        t.select_range_to(3);
        assert_eq!(sel(&t), vec![1, 2, 3]);
        // The anchor stays: re-ranging replaces the run, backwards too.
        t.select_range_to(0);
        assert_eq!(sel(&t), vec![0, 1]);
    }

    #[test]
    fn box_select_takes_intersecting_segments_additively_or_not() {
        let mut t = tl(12.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0); // edited spans: 0..4, 4..8, 8..12
        t.select_edited_range(3.0, 5.0, false);
        assert_eq!(sel(&t), vec![0, 1], "box takes everything it touches");
        t.select_edited_range(9.0, 10.0, true);
        assert_eq!(sel(&t), vec![0, 1, 2], "additive keeps the selection");
        t.select_edited_range(9.0, 10.0, false);
        assert_eq!(sel(&t), vec![2], "plain box replaces it");
    }

    #[test]
    fn delete_selected_removes_the_whole_set_but_never_everything() {
        let mut t = tl(12.0);
        t.cut_at_source(4.0);
        t.cut_at_source(8.0);
        t.select_edited_range(0.0, 12.0, false);
        assert!(!t.delete_selected(), "the full selection must be refused");
        t.select_only(Some(0));
        t.select_toggle(2);
        assert!(t.delete_selected());
        assert_eq!(t.spans, vec![Span { start: 4.0, end: 8.0 }]);
        assert!(t.selected.is_empty());
        assert!(!t.delete_selected(), "empty selection deletes nothing");
    }

    #[test]
    fn source_edited_mapping_round_trips_inside_kept_content() {
        let mut t = tl(10.0);
        t.cut_at_source(2.0);
        t.cut_at_source(5.0);
        t.delete(1); // keep 0..2 and 5..10
        for src in [0.0, 1.0, 1.9, 5.0, 7.5, 10.0] {
            let ed = t.source_to_edited(src);
            assert!(
                (t.edited_to_source(ed) - src).abs() < 1e-4,
                "round trip failed at {src}"
            );
        }
        // A gap time collapses onto the seam.
        assert_eq!(t.source_to_edited(3.5), 2.0);
    }

    #[test]
    fn play_pos_classifies_kept_gap_and_end() {
        let mut t = tl(10.0);
        t.cut_at_source(2.0);
        t.cut_at_source(5.0);
        t.delete(1); // keep 0..2 and 5..10
        assert_eq!(t.play_pos(1.0), PlayPos::Inside);
        assert_eq!(t.play_pos(3.0), PlayPos::Jump(5.0));
        assert_eq!(t.play_pos(7.0), PlayPos::Inside);
        assert_eq!(t.play_pos(9.995), PlayPos::Ended);
        // Reaching a deleted span's end (within the frame epsilon) jumps too.
        assert_eq!(t.play_pos(1.999), PlayPos::Jump(5.0));
    }

    #[test]
    fn play_start_snaps_gaps_and_wraps_the_end() {
        let mut t = tl(10.0);
        t.cut_at_source(2.0);
        t.cut_at_source(5.0);
        t.delete(0); // keep 2..5 and 5..10 (first two seconds gone)
        assert_eq!(t.play_start(0.0), 2.0, "pre-content start snaps forward");
        assert_eq!(t.play_start(6.0), 6.0);
        assert_eq!(t.play_start(10.0), 2.0, "at the end wraps to the top");
    }

    #[test]
    fn restore_swaps_spans_and_clears_selection() {
        let mut t = tl(10.0);
        t.cut_at_source(5.0);
        t.select_only(Some(1));
        let snapshot = vec![Span { start: 0.0, end: 10.0 }];
        t.restore(snapshot.clone());
        assert_eq!(t.spans, snapshot);
        assert!(t.selected.is_empty());
        assert_eq!(t.anchor, None);
    }

    #[test]
    fn bucket_peaks_bins_interleaved_channels_independently() {
        // 4 frames: L ramps up, R constant half-scale.
        let samples: Vec<i16> = vec![
            8192, 16384, //
            16384, 16384, //
            24576, 16384, //
            32767, 16384,
        ];
        let peaks = bucket_peaks(&samples, 2);
        assert_eq!(peaks.len(), 2);
        assert!((peaks[0].0 - 0.5).abs() < 0.01, "first bucket L peak");
        assert!((peaks[1].0 - 1.0).abs() < 0.01, "second bucket L peak");
        assert!((peaks[0].1 - 0.5).abs() < 0.01 && (peaks[1].1 - 0.5).abs() < 0.01);
    }

    #[test]
    fn ruler_step_picks_the_smallest_nice_step_that_fits() {
        // 10s over 1000px: 90px needs ≥0.9s → the next nice step is 1s.
        assert_eq!(ruler_step(10.0, 1000.0), 1.0);
        // 10 minutes over 800px: needs ≥67.5s → 120s.
        assert_eq!(ruler_step(600.0, 800.0), 120.0);
        // A short clip gets a clean sub-second step.
        assert_eq!(ruler_step(2.0, 1000.0), 0.2);
    }

    #[test]
    fn ruler_step_falls_back_to_hour_multiples_past_the_table() {
        // 10 hours over 400px: needs ≥8100s → 3 whole hours.
        assert_eq!(ruler_step(36_000.0, 400.0), 10_800.0);
    }

    #[test]
    fn fmt_timecode_is_fixed_width_hh_mm_ss_ff() {
        assert_eq!(fmt_timecode(0.0, 30.0), "00:00:00:00");
        assert_eq!(fmt_timecode(90.0, 30.0), "00:01:30:00");
        assert_eq!(fmt_timecode(3661.0, 30.0), "01:01:01:00");
        // The frames field counts at the source rate…
        assert_eq!(fmt_timecode(0.2, 30.0), "00:00:00:06");
        assert_eq!(fmt_timecode(1.5, 60.0), "00:00:01:30");
        // …and a frame count rounding up to a whole second carries over.
        assert_eq!(fmt_timecode(1.999, 30.0), "00:00:02:00");
    }

    #[test]
    fn razor_time_soft_snaps_onto_the_seek_arm() {
        let tl = tl(10.0);
        let c = TimelineCanvas { timeline: &tl, position: 5.0, fps: 30.0, waveform: None };
        // 1000px wide, content inset by the ball radius each side → the arm
        // sits exactly mid-strip at x=500. Within 8px a cut snaps to the
        // playhead exactly; farther away the click keeps its own time.
        assert_eq!(c.razor_time(505.0, 1000.0), 5.0);
        let expected = (520.0 - BALL_R) / (1000.0 - 2.0 * BALL_R) * 10.0;
        assert!((c.razor_time(520.0, 1000.0) - expected).abs() < 1e-4);
    }

    #[test]
    fn x_mapping_pins_the_content_edges_to_the_ball_inset() {
        let tl = tl(10.0);
        let c = TimelineCanvas { timeline: &tl, position: 0.0, fps: 30.0, waveform: None };
        // The inset edges map to the exact timeline extremes (clamped past them),
        // and the arm rides between them.
        assert_eq!(c.x_to_edited(BALL_R, 1000.0), 0.0);
        assert_eq!(c.x_to_edited(0.0, 1000.0), 0.0);
        assert_eq!(c.x_to_edited(1000.0 - BALL_R, 1000.0), 10.0);
        assert_eq!(c.arm_x(1000.0), BALL_R);
    }

    #[test]
    fn bucket_peaks_handles_empty_and_short_input() {
        assert_eq!(bucket_peaks(&[], 8), vec![(0.0, 0.0); 8]);
        // Fewer frames than buckets: leading buckets carry the data, the rest stay flat.
        let peaks = bucket_peaks(&[32767, 0, 32767, 0], 8);
        assert_eq!(peaks.len(), 8);
        assert!(peaks[0].0 > 0.9);
        assert_eq!(peaks[7], (0.0, 0.0));
    }
}
