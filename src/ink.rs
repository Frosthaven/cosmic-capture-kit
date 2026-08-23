//! Live recording annotation (DRAGON-691): the pure model behind drawing ON the screen
//! while a recording runs, so the drawing lands IN the video.
//!
//! # What the feature is
//!
//! A three-way TOOL SELECTOR plus an Erase button, and the two drawing tools differ only in
//! what time does to their ink:
//!
//! * **Pointer**: the default, and the ordinary recording state. Nothing is armed, the ink
//!   surface takes no input, and the screen belongs to whatever is being recorded.
//! * **Marker**: the ink stays until the user presses Erase. Nothing here ticks for it.
//! * **Magic Marker**: the ink dissolves on its own, IN THE ORDER IT WAS DRAWN. The oldest
//!   point in the whole magic scene goes first and a fade FRONT sweeps forward through the
//!   ink, so an arrow drawn at a button vanishes from its tail toward its head. Never a whole
//!   line at once, which is what a per-stroke fade would give.
//!
//! # The dissolve is gated on ACTIVITY, not on each point's own age
//!
//! The clock is ONE instant per session, `last_activity`, reset by every Magic Marker input
//! sample. With `t = now - last_activity`: for `t` under [`MAGIC_HOLD_MS`] every magic point
//! is fully visible, and from there a single sweep runs so the WHOLE magic scene is gone at
//! [`MAGIC_GONE_MS`].
//!
//! The owner asked for a "time since drawing activity" timer, and the two readings are
//! genuinely different behaviours rather than two spellings of one. A per-point age fades the
//! start of a long explanation while its end is still being drawn, so ink the user is
//! actively building on top of is already going. An activity gate means DRAWING ANYTHING
//! HOLDS EVERYTHING: keep annotating and nothing moves, stop for two seconds and the board
//! clears itself. Draw order still decides the ORDER things leave in, which is why the
//! per-point timestamps below are still carried.
//!
//! # The support TIER, and what it changes (DRAGON-693)
//!
//! Not every session can host an always-present click-through ink surface. [`tier`] grades
//! each recording gold / silver / bronze from two facts it already knows, and the three
//! behaviours differ in exactly two places, both named on [`InkTier`]:
//!
//! * **Gold** (macOS, Windows, layer-shell Wayland) is everything below, unchanged: the ink
//!   is on its own surface, the controls stay reachable above it, and going back to Pointer
//!   leaves the marks up.
//! * **Silver** (no layer shell, region or monitor) arms ONE focused fullscreen toplevel,
//!   because there is no layer to put a click-through surface on. The portal's cast carries
//!   the ink live, so nothing is baked.
//! * **Bronze** (a window cast we cannot place on screen) draws on a FROZEN frame of the
//!   stream, in STREAM coordinates ([`fit_rect`] / [`to_stream`]), so it never needs to know
//!   where the recorded window is, and its bake is one rect sample covering the frame. That
//!   is a no-layer-shell session recording a window, and, since DRAGON-738, ALSO a session
//!   with layer shell whose portal window cast could not be matched to a toplevel: KDE,
//!   wlroots, and a COSMIC desktop with two windows of one size. Those used to be graded
//!   Gold, draw beautifully on screen, and deliver a video with no ink in it at all.
//!
//! Silver and Bronze cannot leave ink up after a disarm: the window it lives on has to go for
//! the user to reach the desktop again. Hiding it would be one frame with ink and the next
//! without, which the recording shows as a glitch, so a disarm records a FINAL SWEEP instead
//! ([`InkLog::wipe`], [`FINAL_SWEEP_MS`]) and everything lifts over 400ms. It is the SAME
//! curve the Magic Marker uses, with no hold and a much shorter travel.
//!
//! # Zero baking, and why that decides the whole design
//!
//! The ink is an ordinary composited on-screen surface, so a region or monitor recording
//! captures it the same way it captures everything else on the desktop. Nothing in
//! `src/record/`, `record::finalize` or the encoders changes, and there is no re-encode.
//! WINDOW-mode recording gets no ink in v1 for the same reason: those paths capture one
//! toplevel's own buffer, and a surface floating over it is not in that buffer. Baking ink
//! into a window recording is a separate ticket, which is why the stroke model below stores
//! a per-point TIMESTAMP it does not need today: a bake needs to know when each point
//! existed, and adding that later would mean remodelling every stroke.
//!
//! # Wall clock, not painted frames (and why that is not DRAGON-644 again)
//!
//! The idle time is measured against the wall clock. DRAGON-644 moved the dim's fade onto
//! PAINTED FRAMES because a ramp that spends itself while the main thread is stalled is an
//! animation nobody sees, and the dim's whole job is to be watched.
//!
//! This is the opposite situation, and the same reasoning gives the opposite answer. The
//! ink's deadline is a promise to the RECORDING: the video runs in real time, the user is
//! talking over it, and "gone about three seconds after I stopped drawing" has to be true in
//! the file. A frame-counted clock would stretch that deadline by exactly however long the
//! app stalled, so a stutter would leave ink on screen (and in the video) past the point the
//! narration has moved on. The dim's failure mode is an invisible animation; this one's is
//! a wrong deadline, so the two want different clocks.
//!
//! # Everything here is pure
//!
//! The surface, the pointer plumbing and the platform work live in `app::ink` and the
//! platform plugins. This module holds only decisions and geometry, so `cargo test` proves
//! them on every host: the fade curve, the visible span, the timestamp transfer, the tier
//! probe, the availability gate, the region clamp, the stream-space fit and the input-zone
//! rect subtraction.

/// A rectangle as `(x, y, w, h)`. Deliberately a bare tuple rather than an iced or
/// compositor type: this module is compiled on every host and must not drag a toolkit in
/// to state a rectangle.
pub type Rect = (f32, f32, f32, f32);

/// The ink tool selector: three mutually exclusive positions, no `Eraser` among them.
///
/// Erasing is an ACTION button beside the selector rather than a fourth position, because it
/// is a thing you do once and not a mode you are in: an eraser tool would leave the user
/// parked in a state where the pointer neither draws nor reaches the desktop.
///
/// [`InkTool::Pointer`] being a real position rather than "no tool" is the DRAGON-691
/// amendment. Two arm/disarm toggles say the same thing, but a selector says it in the one
/// shape a user already reads without being told: exactly one of these is on, and the first
/// one is the normal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InkTool {
    /// The default and the ordinary recording state: nothing draws, and the screen belongs to
    /// whatever is being recorded. Listed FIRST in the toolbar for that reason.
    #[default]
    Pointer,
    /// Ink that stays on screen until Erase is pressed.
    Marker,
    /// Ink that dissolves on its own, oldest point first ([`magic_alpha`]).
    MagicMarker,
}

impl InkTool {
    /// **Pure**, unit-tested: the tool's user-visible NAME, in every surface that names it: the
    /// tray's Live Annotations submenu (DRAGON-693), the armed window's selector, and the
    /// toolbar's tooltips. One source, so the three cannot drift; no em/en-dashes, project rule.
    pub fn label(self) -> &'static str {
        match self {
            InkTool::Pointer => "Pointer",
            InkTool::Marker => "Marker",
            InkTool::MagicMarker => "Magic Marker",
        }
    }

    /// The three tools in DISPLAY ORDER, Pointer first because it is the ordinary recording
    /// state. It WAS the index space a `ksni` radio group reported a pick back in, until
    /// DRAGON-757 gave the Linux trays four independent top-level rows; the Windows popup's
    /// contiguous command-id range is the index space left.
    pub const ORDER: [InkTool; 3] = [InkTool::Pointer, InkTool::Marker, InkTool::MagicMarker];

    /// **Pure**, unit-tested: the tool at a display-order index, `None` for an out-of-range one
    /// so a stray index can never arm a tool nobody picked.
    ///
    /// Dead off Windows since DRAGON-757. The two Linux ksni trays reached it through
    /// `recording_ui::ink_choice_at` while their tools were a radio group; flat rows carry
    /// their own `InkAction` and resolve no index at all, and the macOS menus mark an
    /// `NSMenuItem` directly. The Windows popup still walks `CMD_INK_FIRST..=CMD_INK_LAST`
    /// and turns a command id back into a tool by offset, so the body stays here rather than
    /// moving into `platform/windows/`: it is a decision, and the Linux gate tests it.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn at(index: usize) -> Option<Self> {
        Self::ORDER.get(index).copied()
    }

    /// Does this tool put ink on the screen? The one question the surface's input state and
    /// the stroke handlers ask, so neither has to match on the variant.
    pub fn draws(self) -> bool {
        matches!(self, InkTool::Marker | InkTool::MagicMarker)
    }

    /// Does ink drawn with this tool disappear on its own? The one place the two DRAWING
    /// tools differ.
    pub fn fades(self) -> bool {
        matches!(self, InkTool::MagicMarker)
    }
}

/// How long every magic point stays at FULL strength after the last Magic Marker sample.
///
/// The user's own framing is a "time since drawing activity" timer: keep drawing and nothing
/// moves; stop, and after this the board starts clearing itself. Two seconds is long enough
/// to lift the hand, point at something and take a breath without the ink going, and short
/// enough that it goes while the sentence it belongs to is still being said.
pub const MAGIC_HOLD_MS: u32 = 2_000;

/// How long the sweep itself takes, once the hold is over: the time between the oldest point
/// starting to lift and the newest point being gone.
///
/// A full second because the sweep has the WHOLE scene to cross, not one point's own fade.
/// The fade front is soft ([`SWEEP_EDGE`]) and has to be watchable as it travels, or the
/// dissolve reads as a wipe.
pub const MAGIC_SWEEP_MS: u32 = 1_000;

/// When the whole magic scene is gone, counted from the last Magic Marker sample.
pub const MAGIC_GONE_MS: u32 = MAGIC_HOLD_MS + MAGIC_SWEEP_MS;

/// How long the FINAL sweep takes: the fast dissolve Silver and Bronze run over everything on
/// screen when the user goes back to Pointer (DRAGON-693).
///
/// Those two tiers cannot leave ink up after a disarm, because it lives on a focused window
/// that has to go for the desktop to be reachable again. The naive implementation is to hide
/// the window, and the naive implementation is wrong IN THE VIDEO: the recording is running,
/// so a hide is one frame with ink and the next without, which reads as a glitch rather than
/// as the user putting the pen down.
///
/// So the disarm runs the same sweep the Magic Marker runs, with no hold and a much shorter
/// travel. 400ms is long enough to be about a dozen frames at 30fps (so the lift is legible
/// as motion, not as a cut) and short enough that the pointer is back before the user has
/// moved it.
pub const FINAL_SWEEP_MS: u32 = 400;

// The final sweep is the FAST one by definition: it exists because the user has already
// asked for the screen back, where the magic dissolve exists to be watched. A final sweep as
// slow as the magic one would hold a focused window over the desktop for a second after the
// user pressed Pointer, which is the complaint it is meant to prevent.
const _: () = assert!(
    FINAL_SWEEP_MS > 0 && FINAL_SWEEP_MS < MAGIC_SWEEP_MS,
    "DRAGON-693: the final sweep must exist (a hide is a one-frame blink in the video) and \
     must be faster than the magic dissolve (the user has already asked for the screen back)"
);

// The owner asked for exactly three seconds from the last stroke to an empty screen, with the
// hold taking two of them. Both constants are meant to be tunable by hand, so what is pinned
// is the RELATION rather than either number: a change that breaks one of these is a change to
// the behaviour that was asked for, and should be deliberate.
const _: () = assert!(
    MAGIC_GONE_MS == 3_000,
    "DRAGON-691: magic ink must be entirely gone three seconds after the last stroke"
);

// A hold with no sweep behind it is a wipe, which is the one thing this must not look like,
// and a sweep longer than the hold would spend more time leaving than being read.
const _: () = assert!(
    MAGIC_SWEEP_MS > 0 && MAGIC_HOLD_MS > MAGIC_SWEEP_MS,
    "DRAGON-691: the sweep must exist and must be shorter than the hold, or magic ink reads \
     as a wipe rather than as ink lifting"
);

/// How wide the fade FRONT is, in draw-order fraction of the scene.
///
/// The sweep is a soft edge travelling through the ink rather than a hard cut, and this is
/// how much of the scene is mid-dissolve at any instant. Too narrow and the front is a wipe;
/// too wide and everything fades at once, which is the "whole line at once" the tool exists
/// to avoid. About a third of the scene reads as ink lifting with a clear direction.
///
/// It is also what makes the ends exact: the front starts at `-SWEEP_EDGE` (so nothing has
/// begun to go at the moment the hold ends) and finishes at `1.0` (so nothing is left at the
/// moment the sweep does). See [`sweep_front`], which is that travel.
pub const SWEEP_EDGE: f32 = 0.35;

const _: () = assert!(
    SWEEP_EDGE > 0.0 && SWEEP_EDGE < 1.0,
    "DRAGON-691: a zero-width front is a wipe and a full-width one fades the whole scene at \
     once, which is the thing the sweep exists to avoid"
);

/// How many distinct alpha levels the dissolve is drawn with.
///
/// The ribbon is filled in RUNS (see [`crate::pen_stroke::stroke_fill_runs`]), one per level,
/// because a single fill can only carry one colour. Each run is a contiguous stretch of the
/// stroke, so the count is what decides whether the gradient reads as smooth or as banding.
///
/// 24 puts the step at ~4% alpha, which is under what the eye separates on a soft edge, and
/// keeps the run count (and so the fill count) small enough that a stroke costs a handful of
/// paths per frame rather than one per point. Raising it buys nothing visible and costs
/// fills; lowering it is visible as banding at 16ms cadence, which is the thing to check by
/// eye if this is ever retuned.
pub const ALPHA_STEPS: usize = 24;

/// **Pure**, unit-tested: ease-out-cubic, `0` at `t = 0` and `1` at `t = 1`.
///
/// The dissolve's curve. Deliberately EASE-OUT rather than the dim fade's ease-in-out
/// ([`crate::app::overlay`]): that one is a surface ARRIVING and wants a soft start, while
/// this is ink LEAVING and should let go quickly and then trail off, which is what makes the
/// last of a stroke read as lifting rather than as being switched off. Four lines, like its
/// sibling, rather than a crate for one curve.
fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let f = 1.0 - t;
    1.0 - f * f * f
}

/// **Pure**, unit-tested: where the fade FRONT has reached `idle_ms` into a sweep whose hold is
/// `hold_ms` and whose travel is `sweep_ms`, as a position in draw-order fraction.
///
/// ONE curve serves two sweeps (DRAGON-693). The Magic Marker's is the slow one every tier has
/// always had ([`MAGIC_HOLD_MS`] / [`MAGIC_SWEEP_MS`]); the FINAL sweep ([`FINAL_SWEEP_MS`]) is
/// the fast one Silver and Bronze run when the user goes back to Pointer. They differ only in
/// their two durations, so they are the same function called twice rather than two curves that
/// could drift: a second implementation would be a second thing to keep the bake and the live
/// overlay agreeing on. A zero `hold_ms` is legal and is exactly what the final sweep uses.
///
/// Ink at or below the front is gone; ink more than [`SWEEP_EDGE`] above it is untouched;
/// ink in between is mid-dissolve.
///
/// The travel is deliberately from `-SWEEP_EDGE` to `1.0`, not from `0.0` to `1.0`, and both
/// ends are the reason. Starting at `-SWEEP_EDGE` means that at the instant the hold ends the
/// front has not yet reached the oldest point, so the scene goes from "solid" to "beginning
/// to lift" continuously instead of snapping the oldest ink to half strength. Finishing at
/// exactly `1.0` means the newest point is gone at exactly the sweep's end, so the promise
/// "three seconds after you stop and the board is clear" is exact rather than approximate.
///
/// A zero `hold_ms` is legal and is exactly what the final sweep uses: the front is still at
/// `-SWEEP_EDGE` at `idle_ms == 0`, so the scene is whole at the instant the sweep is
/// triggered and starts lifting from the next frame rather than snapping.
pub fn sweep_front(idle_ms: u32, hold_ms: u32, sweep_ms: u32) -> f32 {
    if idle_ms <= hold_ms {
        return -SWEEP_EDGE;
    }
    let sweep_ms = sweep_ms.max(1);
    if idle_ms >= hold_ms.saturating_add(sweep_ms) {
        return 1.0;
    }
    let p = (idle_ms - hold_ms) as f32 / sweep_ms as f32;
    -SWEEP_EDGE + p * (1.0 + SWEEP_EDGE)
}

/// **Pure**, unit-tested: the opacity of a magic point at draw-order fraction `q`, `idle_ms`
/// after the last Magic Marker sample.
///
/// `q` is `0.0` for the OLDEST point in the whole magic scene and `1.0` for the newest, so
/// this is what makes the dissolve run in draw order across every stroke at once rather than
/// stroke by stroke.
///
/// Monotonically non-DEcreasing in `q` and non-INcreasing in `idle_ms`, which together are
/// the whole behaviour: at any instant older ink is fainter than newer ink, and no ink ever
/// comes back on its own. (It CAN come back when the user draws again, which resets the clock
/// rather than the alpha; that reset lives in `app::ink`, and points already at zero are
/// dropped before it so they stay gone.)
pub fn magic_alpha(q: f32, idle_ms: u32) -> f32 {
    sweep_alpha(q, idle_ms, MAGIC_HOLD_MS, MAGIC_SWEEP_MS)
}

/// **Pure**, unit-tested: [`magic_alpha`] with the hold and the sweep spelled out, so the
/// Magic Marker's slow dissolve and the Silver/Bronze FINAL sweep share one curve.
pub fn sweep_alpha(q: f32, idle_ms: u32, hold_ms: u32, sweep_ms: u32) -> f32 {
    if idle_ms <= hold_ms {
        return 1.0;
    }
    if idle_ms >= hold_ms.saturating_add(sweep_ms.max(1)) {
        return 0.0;
    }
    let front = sweep_front(idle_ms, hold_ms, sweep_ms);
    ease_out_cubic((q.clamp(0.0, 1.0) - front) / SWEEP_EDGE)
}

/// **Pure**, unit-tested: a point's position in the magic scene's draw order, `0.0` for the
/// oldest and `1.0` for the newest.
///
/// `ordinal` counts every drawn point of every magic stroke, oldest first; `total` is how many
/// there are. A scene of one point is all one position, `0.0`, so it holds and then goes as a
/// unit instead of dividing by zero.
pub fn draw_fraction(ordinal: u32, total: u32) -> f32 {
    if total <= 1 {
        return 0.0;
    }
    (ordinal as f32 / (total - 1) as f32).clamp(0.0, 1.0)
}

/// **Pure**, unit-tested: the lowest ordinal in a generation of `total` points that is still
/// visible at `idle_ms`. `total` means the whole generation has gone.
///
/// The vanished part of a generation is always a PREFIX in draw order, because
/// [`magic_alpha`] rises with the ordinal and falls with time. That is what lets a dissolving
/// stroke be drawn by SLICING rather than by filtering point by point, and it is what "the
/// erasure follows the stroke" means in code.
///
/// Computed by walking rather than by inverting the curve, because the curve is eased and the
/// inverse would be a second expression of it that could drift. A generation is at most a few
/// thousand points and this runs once per clip per frame.
pub fn first_live_ordinal(total: u32, idle_ms: u32) -> u32 {
    first_live_ordinal_with(total, idle_ms, MAGIC_HOLD_MS, MAGIC_SWEEP_MS)
}

/// **Pure**, unit-tested: [`first_live_ordinal`] with the hold and the sweep spelled out.
pub fn first_live_ordinal_with(total: u32, idle_ms: u32, hold_ms: u32, sweep_ms: u32) -> u32 {
    if idle_ms <= hold_ms {
        return 0;
    }
    if idle_ms >= hold_ms.saturating_add(sweep_ms.max(1)) {
        return total;
    }
    (0..total)
        .find(|&o| sweep_alpha(draw_fraction(o, total), idle_ms, hold_ms, sweep_ms) > 0.0)
        .unwrap_or(total)
}

/// **Pure**, unit-tested: which alpha RUN a given opacity belongs to, `0` (invisible)
/// through [`ALPHA_STEPS`] (solid). See [`ALPHA_STEPS`] for why the gradient is quantized at
/// all, and [`run_alpha`] for the value a run is actually filled with.
pub fn alpha_run(alpha: f32) -> usize {
    let a = alpha.clamp(0.0, 1.0);
    (a * ALPHA_STEPS as f32).round() as usize
}

/// **Pure**, unit-tested: the opacity run `n` is filled with. The exact inverse of
/// [`alpha_run`] at the run's own level, so a solid run is `1.0` and not `0.999`.
pub fn run_alpha(n: usize) -> f32 {
    (n.min(ALPHA_STEPS) as f32) / ALPHA_STEPS as f32
}

/// **Pure**, unit-tested: transfer the RAW trail's per-sample timestamps onto the SMOOTHED
/// centerline, by absolute arc length, one value per point of `fit`.
///
/// The exact shape of [`crate::pen_stroke::pressure_along`], and deliberately so: the
/// smoothing pipeline throws the raw spacing away, so anything the raw trail knows has to be
/// re-expressed against the curve that is actually drawn, and doing it the same way as the
/// pressure signal means the two cannot land in different places on the same stroke.
///
/// ABSOLUTE arc length rather than a fraction, for the reason `pressure_along` gives:
/// a fraction re-maps the whole stroke every time the far end grows, and the settled prefix
/// must not move. Since a live stroke is re-fitted on every sample, a fraction would make
/// every existing point's timestamp drift backwards as the user keeps drawing, which for
/// magic ink means the dissolve stops where the hand stops.
///
/// The interpolation is LINEAR between the two raw samples the arc position falls between,
/// picked over nearest-sample because the fit is resampled at a finer step than the trail:
/// nearest would quantize the ages into visible plateaus and the run boundaries would sit on
/// them. Empty or mismatched input answers all-zero, which reads as "drawn at the stroke's
/// start" and so fades first: the safe direction for ink whose timing we could not recover.
pub fn ages_along(raw: &[(f32, f32)], raw_ms: &[u32], fit: &[(f32, f32)]) -> Vec<u32> {
    if fit.is_empty() {
        return Vec::new();
    }
    if raw.len() != raw_ms.len() || raw.len() < 2 {
        return vec![raw_ms.first().copied().unwrap_or(0); fit.len()];
    }
    let s_raw = arc_lengths(raw);
    let s_fit = arc_lengths(fit);
    let mut j = 0usize;
    s_fit
        .iter()
        .map(|&s| {
            while j + 2 < s_raw.len() && s_raw[j + 1] < s {
                j += 1;
            }
            let (a, b) = (s_raw[j], s_raw[j + 1]);
            let t = if b > a { ((s - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
            let (ma, mb) = (raw_ms[j] as f32, raw_ms[j + 1] as f32);
            (ma + (mb - ma) * t).max(0.0).round() as u32
        })
        .collect()
}

/// **Pure**, unit-tested: each point's position along a polyline as a fraction of its total
/// arc length, `0.0` at the first point and `1.0` at the last.
///
/// The bridge between a stroke's per-point data and
/// [`crate::pen_stroke::stroke_fill_runs`], which tags every finished piece of the ribbon
/// with exactly this quantity and knows nothing else about the stroke. A polyline with no
/// length at all (a tap, or one that never moved) is all one position, `0.0`, which keeps it
/// in a single run instead of dividing by zero.
pub fn arc_fractions(pts: &[(f32, f32)]) -> Vec<f32> {
    let s = arc_lengths(pts);
    let total = s.last().copied().unwrap_or(0.0);
    if total > 0.0 {
        s.iter().map(|v| (v / total).clamp(0.0, 1.0)).collect()
    } else {
        vec![0.0; pts.len()]
    }
}

/// **Pure**, unit-tested: read a per-point series at an arc FRACTION, linearly interpolated
/// between the two points it falls between.
///
/// `fracs` is [`arc_fractions`] of the same polyline the `values` belong to, so the two are
/// parallel and `fracs` is non-decreasing. Used to answer "how old is the ink HERE" for a
/// ribbon piece that only knows where along the stroke it sits.
///
/// Empty or mismatched input answers `0`, which reads as "drawn at the stroke's start" and so
/// fades first: losing the timing must never mean losing the deadline.
pub fn at_fraction(fracs: &[f32], values: &[u32], f: f32) -> u32 {
    if fracs.is_empty() || fracs.len() != values.len() {
        return 0;
    }
    let f = f.clamp(0.0, 1.0);
    let hi = match fracs.iter().position(|&v| v >= f) {
        // NO fraction reaches `f`. A well-formed series from [`arc_fractions`] ends at exactly
        // `1.0` and `f` is clamped to it, so the only way here is a DEGENERATE polyline, whose
        // fractions are all `0.0`. Its points are coincident, so "how far along" has no answer
        // and the value that matters is the OLDEST: taking the newest instead would let a
        // stroke that never moved outlive its own deadline.
        None => return values[0],
        // At or before the first point.
        Some(0) => return values[0],
        Some(i) => i,
    };
    let (a, b) = (fracs[hi - 1], fracs[hi]);
    let t = if b > a { ((f - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
    let (va, vb) = (values[hi - 1] as f32, values[hi] as f32);
    (va + (vb - va) * t).max(0.0).round() as u32
}

/// Cumulative arc length along a polyline, one entry per point, starting at `0`.
///
/// A local copy rather than a reach into `pen_stroke`: that module keeps its own private and
/// this one is three lines, so sharing it would mean widening someone else's surface for no
/// gain. Both compute the same quantity the same way.
fn arc_lengths(pts: &[(f32, f32)]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pts.len());
    let mut acc = 0.0f32;
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            let q = pts[i - 1];
            acc += (p.0 - q.0).hypot(p.1 - q.1);
        }
        out.push(acc);
    }
    out
}

/// **Pure**, unit-tested: are the live-annotation tools offered at all?
///
/// ONE term since DRAGON-693, and the two that went are the whole of that ticket's gating
/// rework:
///
/// * `recording`: there is nothing to draw ON otherwise. Ink is a recording feature; the
///   idle overlay has the whole preview editor for annotating a still. This one is not a
///   preference and never will be.
/// * **`toolbar_hidden` is gone.** It was honest while the in-frame toolbar buttons were the
///   only way to arm a tool: with the toolbar hidden the feature existed with no door. The
///   tray's "Live Annotations" submenu is that door now, on every platform and in every
///   tier, so a hidden toolbar is no longer a reason to withhold the tools.
/// * **`portal_fallback` is gone as a REFUSAL and stayed as a CHOICE.** It used to mean "no
///   layer shell, so there is no surface to host ink and no way to mint one the portal
///   stream would carry", which was true of the surfaces we had. [`InkTier`] is the answer
///   instead: that session gets Silver or Bronze, which mint their own armed window rather
///   than relying on a surface the recording tore down.
///
/// **WINDOW mode is no longer a term** (DRAGON-691 amendment 2, item 1), and the reason it
/// was one is worth keeping: a window recording captures that toplevel's own buffer, so a
/// surface floating over it is not in the file and the ink would be on screen and absent from
/// the video. Two different answers removed it. On Linux a window recording is a fixed screen
/// RECTANGLE (`spawn_screencopy_worker` funnels every mode into a region crop), so the
/// overlay is captured live like any other mode. On macOS and Windows it really is the
/// window's own buffer, and the ink is composited in at finalize from this module's replay
/// ([`visible_at`]) instead. Both are honest; neither needs a gate.
pub fn tools_available(recording: bool) -> bool {
    recording
}

// ── The support TIER (DRAGON-693) ────────────────────────────────────────────────────────

/// How well this session can host live annotation: the owner's gold / silver / bronze
/// grading, decided once per recording and read everywhere the three behaviours differ.
///
/// The UX is IDENTICAL across all three: the same selector, the same fade clocks, the same
/// Clear Marks. What differs is only what the platform can physically do, and the two
/// differences are named on the variants. Encoding them here rather than as scattered `if`s
/// is the point: a reader asks one question ("what tier is this?") and every rule follows
/// from the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InkTier {
    /// **Gold**: annotate AND control at once, and the marks persist until they are cleared.
    ///
    /// macOS, Windows, and any Wayland session WITH `wlr-layer-shell`. The ink lives on its
    /// own always-present surface, the recording controls stay clickable above it, and going
    /// back to Pointer leaves the ink on screen, display-only, until Clear Marks or a Magic
    /// Marker dissolve takes it.
    ///
    /// The DEFAULT, because it is what every platform but one Linux configuration gets, and
    /// because a session with no tier decided yet is a session that is not recording, where no
    /// tier rule applies at all.
    #[default]
    Gold,
    /// **Silver**: annotate OR control, and going back to Pointer clears the marks.
    ///
    /// A no-layer-shell Wayland session (a GNOME session, or a Flatpak build cosmic-comp
    /// hides the global from) recording a REGION or a MONITOR. There is no layer to put a
    /// click-through surface on, so arming presents one ordinary fullscreen toplevel that is
    /// focused and owns the pointer, hosting both the canvas and the selector. The portal's
    /// screen cast carries it live, so nothing is baked. It cannot persist after disarm
    /// because the window it lives on has to go for the user to reach the desktop again,
    /// which is what [`InkTier::sweeps_on_disarm`] says.
    Silver,
    /// **Bronze**: annotate on a frozen snapshot, and going back to Pointer dismisses it.
    ///
    /// A WINDOW cast whose window we cannot place on screen. The cast streams that window's
    /// own pixels to us, so arming freezes the latest captured frame and shows it fitted in
    /// the armed window; strokes are born in STREAM coordinates and reach the file through
    /// the existing finalize bake with an identity mapping. The honest caveat is that the
    /// ink is placed against the frozen moment while the window keeps recording live
    /// underneath.
    ///
    /// TWO sessions reach it, and the second is DRAGON-738's:
    ///
    /// * the no-layer-shell session recording a window, which is where DRAGON-693 found it;
    /// * ANY session whose portal window cast could not be matched to a toplevel. Only
    ///   `zcosmic_toplevel_info_v1` carries a toplevel's geometry, so KDE and wlroots can
    ///   never resolve one, and a COSMIC desktop with two same-size windows may not either
    ///   (DRAGON-737). Those sessions have layer shell and were graded Gold, so the ink drew
    ///   perfectly on screen and the file got none of it, silently.
    Bronze,
}

impl InkTier {
    /// **Pure**, unit-tested: does ink stay on screen after the user goes back to Pointer?
    ///
    /// Gold only. Silver and Bronze run a fast final sweep instead ([`FINAL_SWEEP_MS`]), for
    /// the same reason in both: the ink lives on a focused, opaque-to-input window that has
    /// to go before the user can touch anything else, so "leave it up" is not on offer. The
    /// sweep is what stops that being a one-frame blink in the video.
    pub fn persists_after_disarm(self) -> bool {
        matches!(self, InkTier::Gold)
    }

    /// **Pure**, unit-tested: the exact complement of [`InkTier::persists_after_disarm`],
    /// named from the side the disarm handler asks from.
    pub fn sweeps_on_disarm(self) -> bool {
        !self.persists_after_disarm()
    }

    /// **Pure**, unit-tested: does arming mint the tier's OWN fullscreen window, rather than
    /// using the always-present ink surface?
    ///
    /// Silver and Bronze. Silver, and Bronze in its DRAGON-693 form, are no-layer-shell
    /// sessions where every surface of ours is torn down at recording start; the armed window
    /// is a deliberate exception to that teardown, minted mid-recording and taken down again
    /// on disarm. DRAGON-738's Bronze reaches the same window from the other side: that
    /// session HAS layer shell and could have hosted an always-present surface, but the ink it
    /// hosted would go nowhere, so it mints the armed window for the picture rather than for
    /// the want of a layer.
    pub fn arms_own_window(self) -> bool {
        !matches!(self, InkTier::Gold)
    }

    /// **Pure**, unit-tested: does arming freeze the last captured frame and draw on it?
    ///
    /// Bronze alone. It is the one tier whose recording is not of the screen the user is
    /// looking at, so a transparent window over the desktop would have the user drawing on
    /// pixels the file does not contain.
    pub fn shows_snapshot(self) -> bool {
        matches!(self, InkTier::Bronze)
    }

    /// The word the debug log and the diagnostics dump use. Lower case, one word.
    pub fn word(self) -> &'static str {
        match self {
            InkTier::Gold => "gold",
            InkTier::Silver => "silver",
            InkTier::Bronze => "bronze",
        }
    }
}

/// **Pure**, unit-tested: the one line the ARMED window prints under its selector, naming the
/// mode this session got and why (DRAGON-738), or `None` where nothing may be printed.
///
/// It exists because the DRAGON-738 downgrade is otherwise invisible in the only way that
/// matters. A KDE or wlroots user arms a tool, gets a frozen picture instead of their live
/// desktop, and has no way at all to tell "this is the mode you are in, for this reason" from
/// "the app has hung". `begin_ink_session` says it in the log, and a log reaches nobody who is
/// recording their screen.
///
/// **BRONZE is the only tier that may be told anything, and the reason is the RECORDING.**
///
/// * Gold has no armed window at all: its ink lives on an always-present surface over the
///   real desktop, which needs no explaining.
/// * SILVER's armed window IS IN THE FILE. That session is casting the screen, so its window
///   is captured along with everything else, which is exactly why its backdrop is a faint
///   scrim and not a caption. A sentence printed there would be burned into the user's
///   recording, permanently, and that is a worse outcome than the thing it explains.
/// * BRONZE's is not. That session is casting ONE window's own buffer, so nothing our armed
///   window draws can reach the frame, and the caption is free.
///
/// The two Bronze lines differ because the two causes do, and telling a user the wrong reason
/// is worse than telling them none. A no-layer-shell session gets a frozen frame because that
/// is the only picture of the recorded window it has; a session WITH layer shell gets one
/// because the compositor will not say where that window is, which is a fact about their
/// desktop rather than about this app.
pub fn armed_note(tier: InkTier, window_unlocated: bool) -> Option<&'static str> {
    match tier {
        InkTier::Gold | InkTier::Silver => None,
        InkTier::Bronze if window_unlocated => Some(
            "Drawing on a frozen frame: this desktop will not say where the recorded window \
             is, so the marks are placed on the picture instead of on the screen.",
        ),
        InkTier::Bronze => Some(
            "Drawing on a frozen frame of the recorded window, which keeps recording live \
             underneath.",
        ),
    }
}

/// **Pure**, unit-tested: THE tier probe. Which grade of live annotation this session gets.
///
/// THREE facts decide it, and all three are known before the first stroke:
///
/// * `portal_fallback`: this Linux session has no `wlr-layer-shell`, so every surface of
///   ours went at recording start (`App::overlay_fallback_active`). It is FALSE on macOS and
///   on Windows by construction, which is how the platform axis folds in without this
///   function naming a platform: those two always have their own window level to draw on.
/// * `window_mode`: the recording is of one window rather than of a region or a monitor.
/// * `window_unlocated` (DRAGON-738): this is a portal WINDOW cast and we could not find out
///   where the recorded window is. FALSE everywhere else, including on macOS and Windows,
///   where `platform::window_rect` answers from the window server all session long.
///
/// # Why the third fact had to join
///
/// A window cast delivers the toplevel's OWN buffer, so our ink surface is not in it and the
/// strokes have to be composited in by us, mapped from the screen into that buffer. The map
/// needs the window's origin, and on Linux only `zcosmic_toplevel_info_v1` carries a
/// toplevel's geometry: the portal names a PipeWire node and never a window, core Wayland
/// tells a client nothing about where anything is, and `ext_foreign_toplevel_list_v1` carries
/// identity alone. So KDE and wlroots can never answer, and COSMIC may not either when two
/// windows share a size (DRAGON-737 settles most of those by pixels; some genuinely tie).
///
/// Those sessions HAVE layer shell, so they were graded Gold. The ink drew perfectly, the
/// user watched it, and the file got none of it, with nothing on screen to say so. Bronze
/// already solves exactly that problem, needing no origin at all: it draws on a frozen frame
/// OF THE STREAM, in stream coordinates, and bakes through an identity mapping.
///
/// # The two terms are not interchangeable
///
/// `window_unlocated` only downgrades a WINDOW recording, which is why it is ANDed with
/// `window_mode` rather than tested alone. A region or monitor cast has the ink on the screen
/// it is casting, so no origin is involved and no downgrade is warranted. And `portal_fallback`
/// alone still means Silver, unchanged: that session's ink IS in the cast.
///
/// A native Linux window recording is untouched too, and this is where the distinction earns
/// its keep: that path crops a screen RECTANGLE, so the compositor captures the ink with
/// everything else and no origin is ever wanted. Its caller reports `window_unlocated` false
/// for that reason, not by accident.
pub fn tier(portal_fallback: bool, window_mode: bool, window_unlocated: bool) -> InkTier {
    if window_mode && (portal_fallback || window_unlocated) {
        return InkTier::Bronze;
    }
    if portal_fallback {
        InkTier::Silver
    } else {
        InkTier::Gold
    }
}

/// **Pure**, unit-tested: must WE put this session's ink into the recorded file, rather than
/// letting the compositor capture it along with everything else on the screen?
///
/// The decision `App::ink_needs_bake` reads, moved here in DRAGON-736 so the whole matrix is
/// provable on any host. Two routes, and they reach the same compositing from opposite ends:
///
/// * `window_mode && capture_excludes_overlays`: the recording is of ONE window and the
///   capture reads that window's own buffer, which no floating surface of ours is part of. So
///   the ink is on the screen and not in the file.
/// * `tier.shows_snapshot()`, i.e. BRONZE: the ink is not on the screen being cast at all. It
///   is drawn on a frozen still of the stream, in stream coordinates.
///
/// Everything else already has the ink in the file, because the compositor put it there.
///
/// **`capture_excludes_overlays` is a question about the BACKEND, not about the platform**,
/// and answering it with a `cfg!` was the whole of DRAGON-736. See
/// [`crate::platform::window_capture_excludes_overlays`], which is where that fact is decided,
/// and note that it is ANDed with `window_mode` here rather than inside: a region or monitor
/// capture of any kind sees the whole screen, so the window question does not arise for it.
pub fn needs_composite(window_mode: bool, capture_excludes_overlays: bool, tier: InkTier) -> bool {
    (window_mode && capture_excludes_overlays) || tier.shows_snapshot()
}

/// **Pure**, unit-tested: clamp a point into `rect`, so ink can never be drawn outside the
/// part of the screen the recording actually keeps.
///
/// `margin` is the ink's own half-width, so a stroke drawn hard against the edge has its
/// full weight inside rather than half of it hanging over. A rect too small to hold the
/// margin collapses to its centre on that axis instead of inverting, which is the same
/// "never let a clamp cross itself" rule `annot_gesture_to`'s margin clamp follows.
pub fn clamp_into(p: (f32, f32), rect: Rect, margin: f32) -> (f32, f32) {
    let (rx, ry, rw, rh) = rect;
    let m = margin.max(0.0);
    let clamp1 = |v: f32, lo: f32, len: f32| {
        let (a, b) = (lo + m, lo + len - m);
        if b <= a { (lo + len * 0.5).clamp(lo, lo + len) } else { v.clamp(a, b) }
    };
    (clamp1(p.0, rx, rw), clamp1(p.1, ry, rh))
}

// ── Bronze: drawing on the frozen stream (DRAGON-693) ────────────────────────────────────
//
// Bronze arms on a WINDOW cast, whose stream is that window's own pixels at the window's own
// size. The armed overlay shows the last captured frame FITTED inside a fullscreen window, so
// there are two spaces and exactly one conversion between them: the window's points, which the
// pointer arrives in, and the stream's pixels, which the strokes are recorded in.
//
// Recording in STREAM pixels is what makes Bronze need no window position, ever. The bake's
// mapping is then the identity (a single rect sample covering the whole frame), and moving or
// resizing the recorded window changes nothing about where the ink sits, because the ink is
// pinned to the picture rather than to the desk.

/// **Pure**, unit-tested: where a `content`-sized picture sits inside a `viewport`, fitted to
/// contain it and centred, as `(x, y, w, h)` in the viewport's own units.
///
/// CONTAIN, so the whole frame is reachable: a stroke aimed at the picture's corner must be
/// able to get there, and a cover fit would have cropped that corner off screen. It DOES
/// enlarge a small picture, unlike the capture overlay's letterbox bridge (`OverlayUnits::
/// letterbox` caps its scale at 1): that one is drawing a monitor's own capture at its own
/// size, where enlarging would be a lie, and this one is drawing a 400pt window's stream on a
/// 27-inch display, where refusing to enlarge would leave the user annotating a postage stamp.
///
/// Degenerate input answers the whole viewport rather than dividing by zero, which is the
/// harmless direction: the snapshot draws stretched for the frame or two before a real one
/// arrives.
pub fn fit_rect(content: (u32, u32), viewport: (f32, f32)) -> Rect {
    let (cw, ch) = (content.0 as f32, content.1 as f32);
    let (vw, vh) = (viewport.0.max(0.0), viewport.1.max(0.0));
    if cw <= 0.0 || ch <= 0.0 || vw <= 0.0 || vh <= 0.0 {
        return (0.0, 0.0, vw, vh);
    }
    let s = (vw / cw).min(vh / ch);
    let (w, h) = (cw * s, ch * s);
    (((vw - w) * 0.5).max(0.0), ((vh - h) * 0.5).max(0.0), w, h)
}

/// **Pure**, unit-tested: a viewport point as a STREAM pixel, given where the picture was
/// fitted.
///
/// The inverse of [`fit_rect`]'s placement, clamped into the frame so a stroke started in the
/// letterbox bars lands on the picture's edge rather than outside it. That clamp is the same
/// rule [`clamp_into`] applies for a region recording, for the same reason: a point the file
/// does not contain is a point the ink must not be recorded at.
///
/// `margin` is the ink's own half-width IN STREAM PIXELS, so a stroke drawn against the frame
/// edge keeps its full weight inside.
pub fn to_stream(p: (f32, f32), fit: Rect, content: (u32, u32), margin: f32) -> (f32, f32) {
    let (fx, fy, fw, fh) = fit;
    let (cw, ch) = (content.0 as f32, content.1 as f32);
    if fw <= 0.0 || fh <= 0.0 || cw <= 0.0 || ch <= 0.0 {
        return (0.0, 0.0);
    }
    let q = ((p.0 - fx) * (cw / fw), (p.1 - fy) * (ch / fh));
    clamp_into(q, (0.0, 0.0, cw, ch), margin)
}

/// **Pure**, unit-tested: `full` with `hole` cut out of it, as up to four rectangles.
///
/// This is the Linux input zone while a tool is armed: the ink surface takes the pointer
/// everywhere EXCEPT over the toolbar, so the recording controls stay clickable through it.
/// A `wlr-layer-shell` input region is a list of rectangles with no subtraction operator, so
/// the subtraction has to be done here.
///
/// The four bands are cut in a fixed order, above then below then left then right, with the left and
/// right bands limited to the hole's own vertical span, so no two overlap. Overlap would not
/// be wrong for an input region (the union is the same set) but it makes the result
/// impossible to reason about in a test, and the compositor is handed one rectangle more
/// than it needs for every stray band.
///
/// A hole that does not intersect `full` gives `full` back whole, and a hole that swallows
/// it gives nothing at all, which is the honest answer: a toolbar covering the output leaves
/// nowhere to draw.
///
/// Its one production caller is the Linux input zone, so it is honestly DEAD on macOS and
/// Windows, where a whole-window click-through flip carries the same decision. It stays in the
/// shared tree, and stays compiled everywhere, because the RULE is the sort of geometry that
/// has to be provable by `cargo test` on whatever host is in front of you, the same split
/// every `win_build_*` gate in `platform/mod.rs` follows.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn rect_subtract(full: Rect, hole: Rect) -> Vec<Rect> {
    let (fx, fy, fw, fh) = full;
    if fw <= 0.0 || fh <= 0.0 {
        return Vec::new();
    }
    let (fx1, fy1) = (fx + fw, fy + fh);
    let (hx, hy, hw, hh) = hole;
    let (hx0, hy0) = (hx.max(fx), hy.max(fy));
    let (hx1, hy1) = ((hx + hw).min(fx1), (hy + hh).min(fy1));
    if hx1 <= hx0 || hy1 <= hy0 {
        return vec![full];
    }
    let mut out = Vec::with_capacity(4);
    let mut push = |x: f32, y: f32, w: f32, h: f32| {
        if w > 0.0 && h > 0.0 {
            out.push((x, y, w, h));
        }
    };
    push(fx, fy, fw, hy0 - fy);
    push(fx, hy1, fw, fy1 - hy1);
    push(fx, hy0, hx0 - fx, hy1 - hy0);
    push(hx1, hy0, fx1 - hx1, hy1 - hy0);
    out
}

/// **Pure**, unit-tested: which of `outputs` a recording of `target` should host its ink
/// surface on, or `None` when the target touches none of them.
///
/// The rule is the largest OVERLAP, not the nearest origin or the first hit: a region drawn
/// across a monitor seam belongs to the display that holds most of it, which is where the
/// user is looking and where the toolbar anchored itself. A whole-monitor recording resolves
/// by NAME before this is consulted (there is nothing to measure), so this is the region
/// path's answer and the fallback for a monitor whose name no longer matches an output.
///
/// Ties go to the earlier output, which makes the answer stable across enumeration order for
/// the one case where it genuinely does not matter.
pub fn best_output(outputs: &[Rect], target: Rect) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, o) in outputs.iter().enumerate() {
        let area = overlap_area(*o, target);
        if area <= 0.0 {
            continue;
        }
        if best.is_none_or(|(_, b)| area > b) {
            best = Some((i, area));
        }
    }
    best.map(|(i, _)| i)
}

/// The area shared by two rectangles, `0` when they do not meet.
fn overlap_area(a: Rect, b: Rect) -> f32 {
    let w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
    let h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
    if w <= 0.0 || h <= 0.0 { 0.0 } else { w * h }
}

/// The ink's width, in the surface's own POINTS.
///
/// A single weight, no picker: v1 is a live pointing device, not the preview editor's
/// annotation set, and a width control on the in-frame toolbar would be one more thing in the
/// recorded frame. That constraint is a v1 shape and not a permanent one: a brush size and
/// colour UI is the planned successor (the same future `App::ink_color`'s note points at for
/// the colour), and until it lands this constant is the ONE place the answer lives.
///
/// **The value IS the preview editor's "Thin line" preset** (owner decision, 2026-08-17):
/// `app::preview::annotate::DEFAULT_ANNOT_STROKE`, 4 points, the third rung of that module's
/// `STROKE_WIDTHS` ladder and the one `preview::chrome`'s width flyout labels Thin line. Named
/// in prose rather than in code because that module is private to `app` and this one is not,
/// so `annotate`'s own `live_recording_ink_is_the_editors_thin_line_preset` test is what stops
/// the two numbers drifting apart (DRAGON-703). What it replaced was 6 points, chosen here in
/// isolation for being heavy enough to read on a full-screen video after the encoder has been
/// at it; the owner's answer is that the editor already settled that question for this app's
/// annotations and the live tool has no business disagreeing with it. 4 points still reads at
/// a glance and buries less of whatever it circles.
///
/// **Points, and no scale factor anywhere.** A first cut multiplied this by the output's
/// `point_scale`, which is wrong in both directions at once and is worth stating so it is not
/// re-added: the stroke is drawn in the surface's POINT space, which is the scale-independent
/// one on every platform, so 4 points is the same apparent size on a 100% and a 200% monitor
/// AND the same fraction of the recorded frame (the frame grows by exactly the factor the
/// point does). Scaling it would have made the ink twice as thick, visibly, on a scaled
/// Windows display.
pub const WIDTH_PT: f32 = 4.0;

// The width has to leave the pen's own sampling and taper room to work. Below the 1.5-point
// trail-thinning step (`app::preview::annotate::PEN_MIN_STEP`, named in prose because that
// module is private to `app` and this one is not) the ribbon is resampled more finely than it
// is wide and has nothing to taper; far above it the ink buries whatever it is pointing at,
// and far below it does not survive a video encoder. A compile-time assert rather than a test
// because both sides are constants (the house form, per CLAUDE.md).
const _: () = assert!(
    WIDTH_PT > 1.5 && WIDTH_PT >= 3.0 && WIDTH_PT <= 12.0,
    "DRAGON-691: live ink this thin has no ribbon to shape and does not survive an encoder; \
     this thick it buries what it points at"
);

// ── The recorded scene: generations, the log, and the ONE replay ──────────────────────────
//
// Everything above answers "what does one point look like". This answers "what is on the
// screen at media time t", and it is the piece a WINDOW recording cannot do without: those
// capture the target window's own buffer, so the ink never reaches the file live and has to be
// composited in afterwards. The bake replays; the live overlay replays; they call the SAME
// function, which is the only way display and bake can be guaranteed not to drift. It is the
// principle `pen_stroke` already embodies for geometry, applied to time.
//
// MEDIA time, not wall time, and that is what makes the replay possible at all. A pause
// freezes media time, so ink drawn during one lands at the frozen instant and the dissolve
// does not run while the file is not moving. When the recording resumes, the ink is where the
// user left it, which is also what they were looking at. A wall clock would have quietly
// erased everything during a long pause and the file would show it reappearing.

/// A media timestamp in milliseconds. SIGNED, like `MediaClock::media_at_signed`: a stroke can
/// legitimately predate media 0 (the pipeline settles a few hundred ms after the user is told
/// recording began), and clamping those to zero would pile them all onto the first frame.
pub type MediaMs = i64;

/// A GENERATION of Magic Marker ink: a batch that holds together and then fades together.
///
/// The generation is the DRAGON-691 amendment 2 fix, and the behaviour it replaces is worth
/// stating because it is the thing the owner tested and disliked. There used to be ONE clock
/// for all magic ink, so drawing anything reset it: start a new stroke while the previous
/// batch was halfway through dissolving and that dissolve stopped, went back to full strength
/// and started over. Ink the user had watched leave came back.
///
/// With generations, a batch's fade is committed the moment it BEGINS. While a generation is
/// still holding (nothing has faded yet) new drawing joins it and extends its hold, which is
/// what makes "keep drawing and nothing moves" work. Once its sweep starts it is CLOSED: new
/// drawing opens a new generation with its own hold, and the closed one runs to completion
/// underneath. Several can be fading at once, at their own rates, which is what a whiteboard
/// actually looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkGen {
    /// Dense index into [`InkLog::gens`], and what a clip carries to name its generation.
    pub id: u32,
    /// Media ms of the last drawing that joined this generation. Frozen once it closes, which
    /// is what makes the fade run to completion.
    pub last_activity_ms: MediaMs,
}

/// **Pure**, unit-tested: may drawing at `now_ms` JOIN a generation whose last activity was at
/// `last_activity_ms`, or does it need a new one?
///
/// The whole generation rule in one line: a generation accepts new ink for exactly as long as
/// it has not begun to fade. The boundary is the same [`MAGIC_HOLD_MS`] the fade starts at, so
/// there is no instant where a generation is both fading and still accepting.
pub fn gen_accepts(last_activity_ms: MediaMs, now_ms: MediaMs) -> bool {
    let idle = now_ms - last_activity_ms;
    (0..(MAGIC_HOLD_MS as MediaMs)).contains(&idle)
}

/// **Pure**, unit-tested: how long a generation has been idle at `now_ms`, saturating at zero.
///
/// Saturating rather than signed, because [`magic_alpha`] takes an unsigned idle: a query at a
/// media time BEFORE the generation's last activity (a replay scrubbing backwards, or a stroke
/// stamped a hair late) is "not idle at all", which is the answer that keeps the ink solid
/// rather than making it flash.
pub fn gen_idle_ms(last_activity_ms: MediaMs, now_ms: MediaMs) -> u32 {
    (now_ms - last_activity_ms).clamp(0, u32::MAX as MediaMs) as u32
}

/// One stroke, as BOTH the live renderer and the bake read it.
///
/// The points are in the ink surface's own POINTS. Turning those into window-buffer pixels is
/// the bake's job and needs the window's rect timeline, which is why it is not baked in here:
/// this type is what the user drew, not where it ended up.
#[derive(Debug, Clone)]
pub struct InkClip {
    /// Which tool drew it. Only [`InkTool::MagicMarker`] fades.
    pub tool: InkTool,
    /// The smoothed centerline: what is drawn.
    pub points: Vec<(f32, f32)>,
    /// The pseudo-pressure speed signal along `points`, or empty for neutral.
    pub speed: Vec<f32>,
    /// The media ms at which each point was drawn. Parallel to `points`, non-decreasing.
    pub point_ms: Vec<MediaMs>,
    /// Nominal width in surface points.
    pub width: f32,
    /// Which generation it belongs to; `None` for a Marker stroke, which never fades.
    /// (`generation` rather than `gen`, which edition 2024 reserves.)
    pub generation: Option<u32>,
}

impl InkClip {
    /// The media instant this stroke began, or `MediaMs::MAX` for an empty one (which is never
    /// visible, which is the right answer).
    pub fn started_ms(&self) -> MediaMs {
        self.point_ms.first().copied().unwrap_or(MediaMs::MAX)
    }
}

/// Everything the user drew during one recording, in the order they drew it.
///
/// This IS the live scene as well as the bake's input. There is no second model: the overlay
/// renders `visible_at(log, now)` and the bake renders `visible_at(log, t)` for each frame.
///
/// # APPEND-ONLY, and it is not a preference
///
/// **Nothing is ever REMOVED from this log while a recording runs.** Strokes are appended,
/// generations are appended, and an Erase is appended as the INSTANT it was pressed. What is on
/// screen is a question about a time, answered by [`visible_at`], and never a question about
/// what the log still contains.
///
/// This is the DRAGON-691 follow-up fix, and the bug it replaces is worth stating because a
/// reader will otherwise re-introduce it as an obvious optimization. `App::retire_spent_ink`
/// used to DROP clips the live scene could no longer show: a magic stroke whose generation had
/// finished its sweep, and anything an Erase had cleared. Every term of its justification was
/// true of the LIVE overlay, which only ever asks about now, and false of the window-mode BAKE,
/// which re-reads this log after the session and replays it from media 0. "Invisible now" is not
/// "invisible then". The owner drew three things, the middle one dissolved while the third was
/// being drawn, the tick that finished its sweep deleted it, and it was absent from the finished
/// video at every instant, including the seconds it had been on screen.
///
/// So a live renderer may SKIP ink it cannot see (that is what [`visible_at`] does, per frame,
/// for free) and may never FORGET it. The memory is a few thousand points per stroke and it is
/// gone when the session is; the drawing it protects cannot be recovered by anything.
#[derive(Debug, Clone, Default)]
pub struct InkLog {
    /// Every stroke, oldest first. Append-only; see the type's own doc.
    pub clips: Vec<InkClip>,
    /// Every magic generation, oldest first. `gens[i].id == i`. Append-only.
    pub gens: Vec<InkGen>,
    /// The media instant of each Erase press. An erase is RECORDED rather than applied, which
    /// is the same rule the strokes follow and the reason a bake can replay one.
    pub erases: Vec<MediaMs>,
    /// The media instant of each FINAL SWEEP (DRAGON-693): a Silver or Bronze disarm, where
    /// everything on screen lifts over [`FINAL_SWEEP_MS`] instead of staying.
    ///
    /// Recorded rather than applied, for the same reason [`InkLog::erases`] is: the bake
    /// replays this log from media 0, so it has to know the screen began clearing itself at
    /// 12.4 seconds, and it cannot know that from a shorter list of clips. An erase and a
    /// sweep are two different vocabulary words on purpose: an erase is instant and a sweep is
    /// a RAMP, and a video shows the difference.
    ///
    /// Gold never writes one. Its disarm leaves the ink up, which is the whole of what the
    /// medal buys.
    pub wipes: Vec<MediaMs>,
}

impl InkLog {
    /// **Pure**, unit-tested: begin a stroke at `now_ms`, returning its index in
    /// [`InkLog::clips`].
    ///
    /// The ONE way a stroke enters the log, so the generation rule below is applied in exactly
    /// one place and a pure test can drive a whole session through the code the app runs. `at`
    /// is the stroke's first point in the ink surface's own points, already clamped by the
    /// caller to the part of the screen the recording keeps.
    ///
    /// A tap is a stroke of one point, which `pen_stroke` renders as a firm-press dot, so
    /// nothing here special-cases it.
    pub fn begin_clip(
        &mut self,
        tool: InkTool,
        at: (f32, f32),
        width: f32,
        now_ms: MediaMs,
    ) -> usize {
        let generation = tool.fades().then(|| self.generation_for(now_ms));
        self.clips.push(InkClip {
            tool,
            points: vec![at],
            speed: Vec::new(),
            point_ms: vec![now_ms],
            width,
            generation,
        });
        self.clips.len() - 1
    }

    /// Which generation a Magic Marker sample at `now_ms` belongs to, opening a new one when the
    /// last has begun to fade.
    ///
    /// THE amendment-2 rule: a generation accepts new ink only while it is still holding. Once
    /// its sweep has started it is closed for good, so drawing during a dissolve starts a fresh
    /// batch and the dissolving one runs to completion underneath instead of being rescued.
    fn generation_for(&mut self, now_ms: MediaMs) -> u32 {
        if let Some(g) = self.gens.last_mut()
            && gen_accepts(g.last_activity_ms, now_ms)
        {
            g.last_activity_ms = now_ms;
            return g.id;
        }
        let id = self.gens.len() as u32;
        self.gens.push(InkGen { id, last_activity_ms: now_ms });
        id
    }

    /// **Pure**, unit-tested: extend the hold of clip `i`'s generation, if this sample still
    /// belongs to it.
    ///
    /// The mid-stroke half of [`InkLog::begin_clip`]: a stroke already has its generation, and
    /// every further sample of it keeps that generation's hold open. A sample that arrives after
    /// the generation has closed (a stroke drawn so slowly that its own batch began to fade
    /// under it) does NOT reopen it, which is the same rule from the other side. Marker ink,
    /// which has no generation, is a no-op.
    pub fn touch_clip_generation(&mut self, i: usize, now_ms: MediaMs) {
        let Some(id) = self.clips.get(i).and_then(|c| c.generation) else {
            return;
        };
        if let Some(g) = self.gens.get_mut(id as usize)
            && gen_accepts(g.last_activity_ms, now_ms)
        {
            g.last_activity_ms = now_ms;
        }
    }

    /// **Pure**, unit-tested: record an Erase at `at_ms`.
    ///
    /// The clips are NOT dropped, and that is the whole point: a window-mode bake has to know
    /// that the screen was cleared at 12.4 seconds, and it cannot know that from an empty list.
    /// See the type's own doc.
    pub fn erase(&mut self, at_ms: MediaMs) {
        self.erases.push(at_ms);
    }

    /// **Pure**, unit-tested: record a FINAL SWEEP at `at_ms` (DRAGON-693).
    ///
    /// The Silver / Bronze disarm. Everything that was on screen begins lifting in draw order
    /// and is gone [`FINAL_SWEEP_MS`] later; anything drawn AFTER it is untouched, exactly
    /// like an erase. Nothing is dropped, for the reason the type's own doc gives.
    pub fn wipe(&mut self, at_ms: MediaMs) {
        self.wipes.push(at_ms);
    }
}

/// Which final sweep each clip belongs to, and the draw-order space that sweep runs in.
///
/// `by_clip[i]` is `Some(w)` when clip `i` is taken by `log.wipes[w]`, `bases[i]` is the clip's
/// first ordinal within that sweep's scene, and `totals[w]` is how many points the sweep has to
/// cross. Parallel to `log.clips` / `log.wipes`.
struct WipeScenes {
    by_clip: Vec<Option<usize>>,
    bases: Vec<u32>,
    totals: Vec<u32>,
}

/// **Pure**, unit-tested (through [`visible_at`]): work out which clips each final sweep takes,
/// and their draw order within it.
///
/// A clip belongs to the FIRST sweep at or after it started, and only if it was still on screen
/// when that sweep landed. Two things can have taken it first, and both are checked because
/// both change the sweep's own ordinal space, which is what decides the ORDER things leave in:
///
/// * an Erase between the clip's start and the sweep: already gone, so it is not swept;
/// * a Magic Marker generation whose dissolve had already finished: likewise.
///
/// Counting a clip that was not on screen would stretch the scene the front has to cross and
/// make the remaining ink lift more slowly than it looks like it should.
fn wipe_scenes(log: &InkLog) -> WipeScenes {
    let mut by_clip = Vec::with_capacity(log.clips.len());
    let mut bases = Vec::with_capacity(log.clips.len());
    let mut totals = vec![0u32; log.wipes.len()];
    for c in &log.clips {
        let started = c.started_ms();
        let found = log
            .wipes
            .iter()
            .enumerate()
            .filter(|(_, w)| **w >= started)
            .min_by_key(|(_, w)| **w)
            .filter(|(_, w)| {
                // Still on screen when the sweep landed?
                let w = **w;
                let erased = log.erases.iter().any(|&e| e >= started && e <= w);
                let faded = c
                    .generation
                    .and_then(|g| log.gens.get(g as usize))
                    .is_some_and(|g| w >= g.last_activity_ms + MAGIC_GONE_MS as MediaMs);
                !erased && !faded
            })
            .map(|(i, _)| i);
        match found.and_then(|i| totals.get_mut(i).map(|t| (i, t))) {
            Some((i, total)) => {
                bases.push(*total);
                *total += c.points.len() as u32;
                by_clip.push(Some(i));
            }
            None => {
                bases.push(0);
                by_clip.push(None);
            }
        }
    }
    WipeScenes { by_clip, bases, totals }
}

/// How a visible span of magic ink is fading, at the instant it was asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkFade {
    /// The draw-order ordinal of this span's FIRST point, within its generation.
    pub ord_base: u32,
    /// How many points its generation holds.
    pub total: u32,
    /// How long that generation has been idle.
    pub idle_ms: u32,
}

impl InkFade {
    /// The opacity of the point `i` places into this span. The one call both renderers make.
    pub fn alpha_at(&self, i: u32) -> f32 {
        magic_alpha(draw_fraction(self.ord_base + i, self.total), self.idle_ms)
    }
}

/// How a visible span is being taken by the FINAL sweep (DRAGON-693), at the instant it was
/// asked for.
///
/// [`InkFade`]'s fast twin, and a separate type on purpose. They differ in one thing, the
/// curve's two durations, and they can apply to the SAME span at the same time: a Magic Marker
/// batch halfway through its own dissolve when the user presses Pointer is both fading and
/// being swept. Folding them into one struct would mean one of them silently winning; keeping
/// them apart lets the renderer take the lower of the two opacities, which is the only answer
/// that never makes ink brighten.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkWipe {
    /// The draw-order ordinal of this span's FIRST point, within the swept scene.
    pub ord_base: u32,
    /// How many points the swept scene holds.
    pub total: u32,
    /// How long ago the sweep was triggered.
    pub idle_ms: u32,
}

impl InkWipe {
    /// The opacity of the point `i` places into this span, under the fast sweep alone.
    pub fn alpha_at(&self, i: u32) -> f32 {
        sweep_alpha(
            draw_fraction(self.ord_base + i, self.total),
            self.idle_ms,
            // No hold: the user has asked for the screen back, so the lift starts at once.
            0,
            FINAL_SWEEP_MS,
        )
    }
}

/// One clip's contribution to a frame: which of its points are on screen, and how they fade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkSpan {
    /// Index into [`InkLog::clips`].
    pub clip: usize,
    /// First visible point. Non-zero when a dissolve has eaten this stroke's head.
    pub from: usize,
    /// One past the last point drawn by the queried instant. A stroke still being drawn is cut
    /// here, which is what makes a replay show it being drawn rather than appearing whole.
    pub to: usize,
    /// `None` for Marker ink, which is solid until erased.
    pub fade: Option<InkFade>,
    /// `Some` while a FINAL SWEEP (DRAGON-693) is taking this span. Independent of `fade`:
    /// both can apply at once, and the renderer takes the lower opacity.
    pub wipe: Option<InkWipe>,
}

impl InkSpan {
    /// How many points this span covers.
    pub fn len(&self) -> usize {
        self.to.saturating_sub(self.from)
    }

    /// Is there nothing to draw? A span is only ever returned non-empty, so this is for
    /// callers building their own.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Is anything at all thinning this span's ink? The one question the fade TICK asks: a
    /// span with neither a dissolve nor a sweep on it is static, and static ink needs no
    /// timer, because it only changes when the user does something that redraws anyway.
    pub fn animating(&self) -> bool {
        self.fade.is_some() || self.wipe.is_some()
    }

    /// The opacity of the point `i` places into this span: the LOWER of its dissolve and its
    /// final sweep, with an absent one reading as fully opaque.
    ///
    /// The lower rather than the product, because these are two ways of taking the same ink
    /// away rather than two layers of translucency: multiplying would make a stroke that is
    /// half gone under both come out at a quarter, which is darker than either says.
    pub fn alpha_at(&self, i: u32) -> f32 {
        let fade = self.fade.map_or(1.0, |f| f.alpha_at(i));
        let wipe = self.wipe.map_or(1.0, |w| w.alpha_at(i));
        fade.min(wipe)
    }
}

/// **Pure**, unit-tested: THE replay. What is on the screen at media time `t_ms`.
///
/// The one function the live overlay and the finalize bake both go through, so a window
/// recording's baked ink is the same drawing the user watched, by construction rather than by
/// two implementations agreeing. Spans come back in clip order, which is draw order, which is
/// the order they must be painted in.
///
/// The three rules it applies, in the order they can rule a stroke out:
///
/// 1. **Erase** removes everything that was on screen when it was pressed. A stroke is gone
///    from an erase onward if it had STARTED by then, which is every stroke on screen, since
///    pressing Erase ends any stroke in flight.
/// 2. **Not drawn yet.** Only points whose own media stamp has arrived are included, so a
///    replay watches a stroke being drawn at the speed it was drawn.
/// 3. **The dissolve**, for magic ink only, through its generation's idle time. A fully faded
///    generation contributes nothing at all.
pub fn visible_at(log: &InkLog, t_ms: MediaMs) -> Vec<InkSpan> {
    let mut out = Vec::new();
    spans_at(log, t_ms, |s| {
        out.push(s);
        true
    });
    out
}

/// **Pure**, unit-tested: is there anything on screen at all at `t_ms`?
///
/// [`visible_at`]'s question without its answer, and the one the ink SURFACE's lifetime asks:
/// the surface exists while a tool is armed or while ink the user has not lost is still showing,
/// and goes when neither is true. Asked rather than derived from the log's length because the
/// log keeps every stroke of the session (see [`InkLog`]), so "the log has clips" and "something
/// is on screen" are different questions and only the second one decides a surface.
///
/// Stops at the first visible span rather than building them all, since the answer cannot change
/// after that.
pub fn any_visible_at(log: &InkLog, t_ms: MediaMs) -> bool {
    let mut any = false;
    spans_at(log, t_ms, |_| {
        any = true;
        false
    });
    any
}

/// **Pure**, unit-tested: is any ink on screen at `t_ms` still MOVING, i.e. does anything have a
/// dissolve or a final sweep left to run?
///
/// The live overlay's fade-tick condition. Marker ink schedules no tick at all, which is right,
/// because it is static: it changes only when the user draws or presses Clear Marks, and both of
/// those are messages that redraw on their own. Magic ink has to be ticked for the WHOLE of its
/// clock, the hold included, because nothing else would produce the frame on which a sweep
/// starts. Since DRAGON-693 a Silver / Bronze FINAL SWEEP arms it too, and for the sharper
/// version of the same reason: that sweep is 400ms of motion the video will show, so a missing
/// tick is a visible jump rather than a slow start.
///
/// It answers `false` the instant the last of them ends, and false for ink an erase has already
/// cleared, so the tick stops as soon as there is nothing left for it to move.
///
/// (`magic_visible_at` until DRAGON-693, when the sweep gave it a second reason to be true.)
pub fn fading_at(log: &InkLog, t_ms: MediaMs) -> bool {
    let mut any = false;
    spans_at(log, t_ms, |s| {
        any = s.animating();
        !any
    });
    any
}

/// The replay itself, walked once and handed to `emit` in draw order. `emit` returns whether to
/// keep going, which is what lets the "is anything showing" questions above stop at the first
/// answer without a second copy of the rules.
fn spans_at(log: &InkLog, t_ms: MediaMs, emit: impl FnMut(InkSpan) -> bool) {
    spans_at_inner(log, t_ms, &wipe_scenes(log), emit);
}

/// [`spans_at`] with the sweep bookkeeping already computed, so a caller that walks many
/// instants over one log (the bake, once per video frame) pays for it once.
fn spans_at_inner(
    log: &InkLog,
    t_ms: MediaMs,
    scenes: &WipeScenes,
    mut emit: impl FnMut(InkSpan) -> bool,
) {
    // Per-generation totals and each clip's ordinal base within its own generation. Counted
    // over the WHOLE generation rather than over the part drawn by `t_ms`: while a generation
    // is still holding every point is solid so the total cannot matter, and once it starts
    // fading it is closed and no more points can arrive. Anything else would make the sweep's
    // own scale move under it.
    let mut totals: Vec<u32> = vec![0; log.gens.len()];
    let mut bases: Vec<u32> = Vec::with_capacity(log.clips.len());
    for c in &log.clips {
        match c.generation.and_then(|g| totals.get_mut(g as usize)) {
            Some(total) => {
                bases.push(*total);
                *total += c.points.len() as u32;
            }
            None => bases.push(0),
        }
    }
    for (i, c) in log.clips.iter().enumerate() {
        let started = c.started_ms();
        if started > t_ms {
            continue;
        }
        // Rule 1: an erase at or before `t_ms` that this stroke was already on screen for.
        if log.erases.iter().any(|&e| e <= t_ms && e >= started) {
            continue;
        }
        // Rule 2: the points that exist yet.
        let to = c.point_ms.partition_point(|&m| m <= t_ms);
        if to == 0 {
            continue;
        }
        // Rule 3: the dissolve.
        let (from, fade) = match c.generation.and_then(|g| log.gens.get(g as usize)) {
            Some(g) => {
                let idle = gen_idle_ms(g.last_activity_ms, t_ms);
                let total = totals[g.id as usize];
                let cut = first_live_ordinal(total, idle);
                let base = bases[i];
                // The cut is in generation ordinals; bring it into this clip's own indices.
                let from = (cut.saturating_sub(base) as usize).min(to);
                (from, Some(InkFade { ord_base: base, total, idle_ms: idle }))
            }
            None => (0, None),
        };
        // Rule 4 (DRAGON-693): the FINAL SWEEP, if one has landed on this clip. It eats the
        // stroke's head exactly the way a dissolve does, so the two cuts are combined by
        // taking the further one: whichever took more, took it.
        let mut from = from;
        let mut wipe = None;
        if let Some(w) = scenes.by_clip[i]
            && let Some(&at) = log.wipes.get(w)
            && at <= t_ms
        {
            let idle = gen_idle_ms(at, t_ms);
            if idle >= FINAL_SWEEP_MS {
                continue;
            }
            let total = scenes.totals[w];
            let base = scenes.bases[i];
            let cut = first_live_ordinal_with(total, idle, 0, FINAL_SWEEP_MS);
            from = from.max((cut.saturating_sub(base) as usize).min(to));
            wipe = Some(InkWipe { ord_base: base, total, idle_ms: idle });
        }
        if from >= to {
            continue;
        }
        if !emit(InkSpan { clip: i, from, to, fade, wipe }) {
            return;
        }
    }
}

/// One group of filled polygons at one opacity: `(alpha, polygons)`.
///
/// Deliberately an ALPHA rather than a colour, which is what lets this module stay
/// toolkit-free while serving two very different renderers: the live overlay turns it into an
/// `iced::Color` and the finalize bake turns it into a tiny-skia one. The ink's hue is one
/// value for the whole session, so carrying it through here would buy nothing.
pub type InkFillRun = (f32, Vec<Vec<(f32, f32)>>);

/// **Pure**, unit-tested: the whole scene at media time `t_ms`, as filled polygons with their
/// opacities, in draw order.
///
/// **This is THE renderer**, and the reason it lives here rather than in either caller: the
/// live overlay calls it with the current media time, and a window-mode bake calls it once per
/// video frame with that frame's media time. A baked recording is therefore the drawing the
/// user watched by construction, not because two implementations agree.
///
/// `map` takes a point in the ink surface's own POINTS to wherever it is being drawn (the
/// identity for the overlay, screen-to-video for the bake) and `scale` is that mapping's
/// uniform factor, which the stroke widths ride.
///
/// Marker ink is ONE group at full opacity. Magic ink is several, because a fill carries one
/// colour and a dissolving stroke is a gradient: it is split by
/// [`crate::pen_stroke::stroke_fill_runs`], which grades the FINISHED ribbon rather than
/// re-fitting each run (a per-run fit re-tapers, and the stroke would read as a chain of
/// beads).
pub fn fill_runs_at(
    log: &InkLog,
    t_ms: MediaMs,
    map: impl Fn((f32, f32)) -> (f32, f32) + Copy,
    scale: f32,
) -> Vec<InkFillRun> {
    let mut out = Vec::new();
    for span in visible_at(log, t_ms) {
        let Some(clip) = log.clips.get(span.clip) else { continue };
        let pts = &clip.points[span.from..span.to];
        if pts.is_empty() {
            continue;
        }
        // The speed signal is only used when it is exactly as long as the points, so a slice
        // that got out of step degrades to neutral pressure rather than desyncing.
        let speed: &[f32] = if clip.speed.len() == clip.points.len() {
            &clip.speed[span.from..span.to]
        } else {
            &[]
        };
        if span.is_empty() {
            continue;
        }
        if !span.animating() {
            out.push((1.0, crate::pen_stroke::stroke_fill_polygons(pts, clip.width, speed, map, scale)));
            continue;
        }
        let fracs = arc_fractions(pts);
        // Each slice point's index within its CLIP, which is what the fade is expressed
        // against.
        let idx: Vec<u32> = (span.from as u32..span.to as u32).collect();
        let runs = crate::pen_stroke::stroke_fill_runs(pts, clip.width, speed, map, scale, |f| {
            alpha_run(span.alpha_at(at_fraction(&fracs, &idx, f)))
        });
        for (run, polys) in runs {
            let alpha = run_alpha(run);
            if alpha > 0.0 {
                out.push((alpha, polys));
            }
        }
    }
    out
}

/// **Pure**, unit-tested: is there anything left to draw, ever, after `t_ms`?
///
/// The bake's stopping condition. Once every generation has finished its sweep and no marker
/// ink survives the last erase, no later frame can differ from the untouched video, so the
/// bake can stop feeding overlay frames and let the rest of the recording pass through
/// untouched. That bound is what keeps a bake proportional to the INKED span rather than to
/// the recording's length.
pub fn last_visible_ms(log: &InkLog) -> Option<MediaMs> {
    let mut last: Option<MediaMs> = None;
    for c in &log.clips {
        let started = c.started_ms();
        let drawn_to = match c.point_ms.last() {
            Some(&m) => m,
            None => continue,
        };
        // When this stroke stops being visible, whichever comes first: its generation's sweep
        // ending, the first erase after it was drawn, or the first FINAL SWEEP after it was
        // drawn running out. Any of them can be absent, and a stroke with none of them (marker
        // ink, never cleared) is on screen for the rest of the recording.
        let gone = c
            .generation
            .and_then(|g| log.gens.get(g as usize))
            .map(|g| g.last_activity_ms + MAGIC_GONE_MS as MediaMs);
        let erased = log.erases.iter().copied().filter(|&e| e >= started).min();
        let wiped = log
            .wipes
            .iter()
            .copied()
            .filter(|&w| w >= started)
            .min()
            .map(|w| w + FINAL_SWEEP_MS as MediaMs);
        let end = [gone, erased, wiped].into_iter().flatten().min();
        // An un-cleared stroke has no end at all, and neither does the scene.
        if gone.is_none() && erased.is_none() && wiped.is_none() {
            return None;
        }
        match end {
            None => return None,
            Some(end) => last = Some(last.map_or(end, |l: MediaMs| l.max(end)).max(drawn_to)),
        }
    }
    last
}

/// The Magic Marker's whole timing model: the activity gate, the sweep, and the draw order it
/// runs in.
#[cfg(test)]
mod magic_fade_tests {
    use super::*;

    /// The owner's requirement stated directly: keep drawing and nothing moves, stop and the
    /// board is clear three seconds later, with the first two of those untouched.
    #[test]
    fn the_scene_holds_for_two_seconds_and_is_clear_at_three() {
        assert_eq!(MAGIC_HOLD_MS, 2_000);
        assert_eq!(MAGIC_SWEEP_MS, 1_000);
        assert_eq!(MAGIC_GONE_MS, 3_000);
        // The whole scene, from its oldest point to its newest, through the hold.
        for q in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for idle in [0, 500, 1_500, MAGIC_HOLD_MS] {
                assert_eq!(magic_alpha(q, idle), 1.0, "q={q} idle={idle}");
            }
        }
        // And gone at the end, for good.
        for q in [0.0, 0.5, 1.0] {
            for idle in [MAGIC_GONE_MS, MAGIC_GONE_MS + 1, 60_000, u32::MAX] {
                assert_eq!(magic_alpha(q, idle), 0.0, "q={q} idle={idle}");
            }
        }
    }

    /// The dissolve runs in DRAW ORDER: at any instant during the sweep, older ink is fainter
    /// than newer ink. This is the requirement's "oldest point first" and it is what rules out
    /// a whole-scene fade.
    #[test]
    fn older_ink_is_always_fainter_than_newer_ink() {
        for idle in (MAGIC_HOLD_MS..=MAGIC_GONE_MS).step_by(25) {
            let mut last = -1.0f32;
            for i in 0..=40 {
                let a = magic_alpha(i as f32 / 40.0, idle);
                assert!(a >= last - 1e-6, "alpha fell with draw order at idle={idle}");
                assert!((0.0..=1.0).contains(&a), "alpha out of range: {a}");
                last = a;
            }
        }
    }

    /// Ink only ever gets fainter as the idle time grows. Without this a rounding slip in the
    /// front could make the scene flicker back up mid-sweep.
    #[test]
    fn ink_never_brightens_as_time_passes() {
        for i in 0..=20 {
            let q = i as f32 / 20.0;
            let mut last = f32::INFINITY;
            for idle in (0..4_000).step_by(20) {
                let a = magic_alpha(q, idle);
                assert!(a <= last + 1e-6, "alpha rose at q={q} idle={idle}");
                last = a;
            }
        }
    }

    /// The sweep is a MOVING FRONT, not a wipe and not a uniform fade: partway through, part
    /// of the scene is gone, part is untouched, and a band in between is mid-dissolve. All
    /// three have to be true at once or the dissolve does not read as ink lifting.
    #[test]
    fn the_sweep_is_a_moving_front() {
        let half = MAGIC_HOLD_MS + MAGIC_SWEEP_MS / 2;
        let alphas: Vec<f32> = (0..=40).map(|i| magic_alpha(i as f32 / 40.0, half)).collect();
        assert!(alphas.contains(&0.0), "nothing has gone yet");
        assert!(alphas.contains(&1.0), "everything is already going");
        assert!(
            alphas.iter().any(|&a| a > 0.05 && a < 0.95),
            "the front has no soft edge, so this is a wipe"
        );
        // And the front travels: later means more of the scene is gone.
        let gone_at = |idle: u32| (0..=40).filter(|i| magic_alpha(*i as f32 / 40.0, idle) == 0.0).count();
        assert!(gone_at(half) > gone_at(MAGIC_HOLD_MS + 100));
        assert!(gone_at(MAGIC_GONE_MS - 100) > gone_at(half));
    }

    /// The front's ends are exact, which is what makes both promises exact rather than
    /// approximate: nothing has begun to lift when the hold ends, and nothing is left when the
    /// sweep does.
    #[test]
    fn the_front_starts_before_the_scene_and_finishes_past_it() {
        let magic = |idle| sweep_front(idle, MAGIC_HOLD_MS, MAGIC_SWEEP_MS);
        assert_eq!(magic(0), -SWEEP_EDGE);
        assert_eq!(magic(MAGIC_HOLD_MS), -SWEEP_EDGE);
        assert_eq!(magic(MAGIC_GONE_MS), 1.0);
        assert_eq!(magic(u32::MAX), 1.0);
        // Monotone in between, so the front never doubles back.
        let mut last = -f32::INFINITY;
        for idle in (0..4_000).step_by(10) {
            let f = magic(idle);
            assert!(f >= last - 1e-6, "the front moved backwards at idle={idle}");
            last = f;
        }
    }

    /// A one-point scene has no draw order to sweep, so it holds as a unit and then goes as
    /// one. Dividing by a zero span would otherwise make a single tap NaN.
    #[test]
    fn a_single_point_scene_holds_then_goes() {
        assert_eq!(draw_fraction(0, 1), 0.0);
        assert_eq!(draw_fraction(0, 0), 0.0);
        assert_eq!(magic_alpha(draw_fraction(0, 1), MAGIC_HOLD_MS), 1.0);
        assert_eq!(magic_alpha(draw_fraction(0, 1), MAGIC_GONE_MS), 0.0);
    }

    /// Draw-order fractions span the scene end to end and rise with the ordinal, which is what
    /// [`first_live`]'s prefix cut rests on.
    #[test]
    fn draw_fractions_run_from_the_oldest_to_the_newest() {
        assert_eq!(draw_fraction(0, 5), 0.0);
        assert_eq!(draw_fraction(4, 5), 1.0);
        assert_eq!(draw_fraction(2, 5), 0.5);
        // Out of range cannot escape the scene.
        assert_eq!(draw_fraction(99, 5), 1.0);
    }
}

/// [`visible_at`]: THE replay, and the contract the live overlay and the window-mode bake both
/// rest on. If these hold, a baked recording shows what the user watched.
#[cfg(test)]
mod replay_tests {
    use super::*;

    /// A stroke of `n` points drawn from `at_ms`, one point every 10ms.
    fn clip(tool: InkTool, generation: Option<u32>, at_ms: MediaMs, n: usize) -> InkClip {
        InkClip {
            tool,
            points: (0..n).map(|i| (i as f32 * 5.0, 10.0)).collect(),
            speed: Vec::new(),
            point_ms: (0..n).map(|i| at_ms + i as MediaMs * 10).collect(),
            width: WIDTH_PT,
            generation,
        }
    }

    /// One magic stroke drawn at media 0, its generation last touched when the stroke ended.
    fn one_magic() -> InkLog {
        InkLog {
            clips: vec![clip(InkTool::MagicMarker, Some(0), 0, 10)],
            gens: vec![InkGen { id: 0, last_activity_ms: 90 }],
            erases: Vec::new(),
            wipes: Vec::new(),
        }
    }

    /// A replay watches a stroke being DRAWN: before it starts there is nothing, partway
    /// through there is a prefix, and after it there is all of it. Without this a baked
    /// recording would show finished strokes appearing whole.
    #[test]
    fn a_stroke_appears_point_by_point_as_it_was_drawn() {
        let log = one_magic();
        assert!(visible_at(&log, -1).is_empty(), "nothing before the first point");
        assert_eq!(visible_at(&log, 0)[0].to, 1, "the first point exists at its own stamp");
        assert_eq!(visible_at(&log, 45)[0].to, 5);
        assert_eq!(visible_at(&log, 90)[0].to, 10);
        assert_eq!(visible_at(&log, 500)[0].to, 10, "and no more than it drew");
        // Every span reports its own clip and starts whole while the generation holds.
        let s = visible_at(&log, 90)[0];
        assert_eq!(s.clip, 0);
        assert_eq!(s.from, 0);
        assert_eq!(s.len(), 10);
    }

    /// Marker ink never fades: no `fade`, and it is still there long after any sweep would
    /// have finished.
    #[test]
    fn marker_ink_is_solid_and_stays() {
        let log = InkLog {
            clips: vec![clip(InkTool::Marker, None, 0, 10)],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let s = visible_at(&log, 60_000);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].fade, None);
        assert_eq!(s[0].from, 0);
        assert_eq!(s[0].to, 10);
        assert_eq!(last_visible_ms(&log), None, "un-erased marker ink has no end");
    }

    /// Magic ink holds, then dissolves from its head, then is gone. The three phases of the
    /// tool, read through the replay rather than through the alpha curve directly.
    #[test]
    fn magic_ink_holds_then_dissolves_then_retires() {
        let log = one_magic();
        let last = log.gens[0].last_activity_ms;
        // Holding: whole, and at full strength.
        let s = visible_at(&log, last + MAGIC_HOLD_MS as MediaMs)[0];
        assert_eq!(s.from, 0);
        let f = s.fade.expect("magic ink fades");
        assert_eq!(f.alpha_at(0), 1.0);
        // Dissolving: the head has gone and what is left is graded.
        let mid = last + MAGIC_HOLD_MS as MediaMs + MAGIC_SWEEP_MS as MediaMs * 3 / 4;
        let s = visible_at(&log, mid)[0];
        assert!(s.from > 0, "the head should be eaten by now");
        let f = s.fade.expect("magic ink fades");
        assert!(f.alpha_at(s.from as u32) < f.alpha_at(s.to as u32 - 1), "not a gradient");
        // Gone.
        assert!(visible_at(&log, last + MAGIC_GONE_MS as MediaMs).is_empty());
        assert_eq!(last_visible_ms(&log), Some(last + MAGIC_GONE_MS as MediaMs));
    }

    /// THE amendment-2 property, as a replay test. A second batch drawn while the first is
    /// dissolving does NOT stop that dissolve: the first generation keeps going to zero on its
    /// own clock while the second holds at full strength.
    #[test]
    fn a_new_generation_does_not_rescue_the_one_that_is_fading() {
        // Generation 0 last touched at 90; its sweep starts at 2090. Generation 1 is drawn at
        // 2400, squarely inside generation 0's sweep.
        let log = InkLog {
            clips: vec![
                clip(InkTool::MagicMarker, Some(0), 0, 10),
                clip(InkTool::MagicMarker, Some(1), 2_400, 10),
            ],
            gens: vec![
                InkGen { id: 0, last_activity_ms: 90 },
                InkGen { id: 1, last_activity_ms: 2_490 },
            ],
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let at = 2_600;
        let spans = visible_at(&log, at);
        assert_eq!(spans.len(), 2, "both batches are on screen");
        let (old, new) = (spans[0], spans[1]);
        assert_eq!(old.clip, 0);
        assert_eq!(new.clip, 1);
        // The old one is mid-dissolve...
        assert!(old.from > 0, "the old batch should be losing its head");
        assert!(old.fade.expect("fades").alpha_at(old.from as u32) < 1.0);
        // ...and the new one is untouched.
        assert_eq!(new.from, 0);
        assert_eq!(new.fade.expect("fades").alpha_at(0), 1.0);
        // The old batch still finishes on ITS OWN clock, with the new one still holding.
        let done = 90 + MAGIC_GONE_MS as MediaMs;
        let spans = visible_at(&log, done);
        assert_eq!(spans.len(), 1, "the old batch is gone and only the new one is left");
        assert_eq!(spans[0].clip, 1);
    }

    /// Erase removes what was on screen when it was pressed, and nothing drawn after it.
    #[test]
    fn erase_clears_what_was_on_screen_and_nothing_later() {
        let log = InkLog {
            clips: vec![
                clip(InkTool::Marker, None, 0, 10),
                clip(InkTool::Marker, None, 1_000, 10),
            ],
            gens: Vec::new(),
            erases: vec![500],
            wipes: Vec::new(),
        };
        // Before the erase, the first stroke is there.
        assert_eq!(visible_at(&log, 400).len(), 1);
        // At and after it, gone for good.
        assert!(visible_at(&log, 500).is_empty());
        assert_eq!(visible_at(&log, 60_000).len(), 1, "only the later stroke survives");
        assert_eq!(visible_at(&log, 60_000)[0].clip, 1);
        // The later stroke was never erased, so the scene has no end.
        assert_eq!(last_visible_ms(&log), None);
        // Erase it too and the scene does end, at the erase.
        let mut log = log;
        log.erases.push(2_000);
        assert_eq!(last_visible_ms(&log), Some(2_000));
    }

    /// Spans come back in DRAW ORDER, because that is the order they must be painted in: a
    /// later stroke drawn over an earlier one is on top, live and baked alike.
    #[test]
    fn spans_come_back_in_draw_order() {
        let log = InkLog {
            clips: vec![
                clip(InkTool::Marker, None, 0, 4),
                clip(InkTool::Marker, None, 100, 4),
                clip(InkTool::Marker, None, 200, 4),
            ],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let spans = visible_at(&log, 1_000);
        assert_eq!(spans.iter().map(|s| s.clip).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    /// The dissolve is scoped to a GENERATION, not to the scene: two strokes in one generation
    /// share one sweep and lift in the order they were drawn, across the stroke boundary.
    #[test]
    fn one_generation_dissolves_across_its_strokes_in_order() {
        let log = InkLog {
            clips: vec![
                clip(InkTool::MagicMarker, Some(0), 0, 10),
                clip(InkTool::MagicMarker, Some(0), 200, 10),
            ],
            gens: vec![InkGen { id: 0, last_activity_ms: 290 }],
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let mid = 290 + MAGIC_HOLD_MS as MediaMs + MAGIC_SWEEP_MS as MediaMs / 2;
        let spans = visible_at(&log, mid);
        // The two strokes share one ordinal space, so the second's base is past the first.
        let f0 = spans[0].fade.expect("fades");
        let f1 = spans[1].fade.expect("fades");
        assert_eq!(f0.ord_base, 0);
        assert_eq!(f1.ord_base, 10);
        assert_eq!(f0.total, 20);
        assert_eq!(f1.total, 20);
        // And the older stroke is fainter than the newer one at the same instant.
        assert!(f0.alpha_at(0) < f1.alpha_at(9));
    }

    /// A replay at a media time inside a PAUSE reads the frozen instant, because every stamp
    /// in the log is media time and a pause contributes none of it. Nothing here is special
    /// cased for pause; the test says so.
    #[test]
    fn a_frozen_media_instant_replays_as_itself() {
        // Two strokes stamped at the SAME media ms, which is what a pause produces.
        let log = InkLog {
            clips: vec![
                clip(InkTool::Marker, None, 1_000, 3),
                InkClip { point_ms: vec![1_000; 3], ..clip(InkTool::Marker, None, 1_000, 3) },
            ],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let spans = visible_at(&log, 1_000);
        assert_eq!(spans.len(), 2, "both are on screen at the frozen instant");
        assert!(spans.iter().all(|s| s.to > 0));
    }

    /// Degenerate input answers nothing rather than panicking: an empty log, an empty clip, a
    /// clip naming a generation that is not there.
    #[test]
    fn a_broken_log_replays_as_empty() {
        assert!(visible_at(&InkLog::default(), 0).is_empty());
        assert_eq!(last_visible_ms(&InkLog::default()), Some(MediaMs::MIN).filter(|_| false));
        let log = InkLog {
            clips: vec![
                InkClip {
                    tool: InkTool::Marker,
                    points: Vec::new(),
                    speed: Vec::new(),
                    point_ms: Vec::new(),
                    width: WIDTH_PT,
                    generation: None,
                },
                // Names generation 7, which does not exist: it degrades to solid rather than
                // vanishing, so ink is never lost to a bookkeeping slip.
                clip(InkTool::MagicMarker, Some(7), 0, 4),
            ],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: Vec::new(),
        };
        let spans = visible_at(&log, 1_000);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].clip, 1);
        assert_eq!(spans[0].fade, None);
    }
}

/// The FINAL SWEEP (DRAGON-693): the fast lift Silver and Bronze run when the user goes back
/// to Pointer, replayed through [`visible_at`] exactly as the live overlay draws it.
///
/// These matter beyond the screen. The sweep is RECORDED on the log rather than applied, so
/// the bake replays it too, which is what makes a Bronze recording show the ink leaving
/// instead of the ink vanishing between two frames.
#[cfg(test)]
mod final_sweep_tests {
    use super::*;

    /// A stroke of `n` points drawn from `at_ms`, one point every 10ms.
    fn clip(tool: InkTool, generation: Option<u32>, at_ms: MediaMs, n: usize) -> InkClip {
        InkClip {
            tool,
            points: (0..n).map(|i| (i as f32 * 5.0, 10.0)).collect(),
            speed: Vec::new(),
            point_ms: (0..n).map(|i| at_ms + i as MediaMs * 10).collect(),
            width: WIDTH_PT,
            generation,
        }
    }

    /// One marker stroke, swept at 1000ms.
    fn swept() -> InkLog {
        InkLog {
            clips: vec![clip(InkTool::Marker, None, 0, 20)],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: vec![1_000],
        }
    }

    /// The whole of the promise: marker ink that would otherwise stay for the rest of the
    /// recording is solid at the instant of the disarm, lifting a moment later, and gone
    /// exactly [`FINAL_SWEEP_MS`] after it.
    #[test]
    fn a_sweep_lifts_the_scene_and_finishes_on_time() {
        let log = swept();
        // Before the disarm: untouched, no sweep on it at all.
        let s = visible_at(&log, 999)[0];
        assert_eq!(s.wipe, None);
        assert_eq!(s.from, 0);
        // At the disarm: whole, and still at full strength, so there is no snap.
        let s = visible_at(&log, 1_000)[0];
        let w = s.wipe.expect("the sweep has begun");
        assert_eq!(s.from, 0);
        assert_eq!(w.idle_ms, 0);
        assert_eq!(s.alpha_at(0), 1.0);
        // Gone at the deadline, and staying gone.
        assert!(visible_at(&log, 1_000 + FINAL_SWEEP_MS as MediaMs).is_empty());
        assert!(visible_at(&log, 60_000).is_empty());
        assert_eq!(last_visible_ms(&log), Some(1_000 + FINAL_SWEEP_MS as MediaMs));
    }

    /// It is a RAMP, not a cut, and that is the whole reason it exists: hiding the window
    /// would be one frame with ink and the next without, which a recording shows as a glitch.
    /// Partway through, part of the scene is gone, part is untouched, and a band is lifting.
    #[test]
    fn the_sweep_is_a_moving_front_rather_than_a_cut() {
        let log = swept();
        let mid = 1_000 + FINAL_SWEEP_MS as MediaMs / 2;
        let s = visible_at(&log, mid)[0];
        assert!(s.from > 0, "the head should have gone by half way");
        let alphas: Vec<f32> = (s.from as u32..s.to as u32).map(|i| s.alpha_at(i)).collect();
        assert!(alphas.iter().any(|&a| a > 0.0 && a < 1.0), "no soft edge: this is a wipe");
        assert!(alphas.contains(&1.0), "everything is already going");
        // And it only ever moves forward.
        let mut last = 0usize;
        for t in (1_000..1_000 + FINAL_SWEEP_MS as MediaMs).step_by(10) {
            let from = visible_at(&log, t).first().map(|s| s.from).unwrap_or(usize::MAX);
            assert!(from >= last, "the sweep went backwards at {t}ms");
            last = from.min(20);
        }
    }

    /// Ink drawn AFTER a sweep is untouched by it, exactly like an erase. A user who arms a
    /// tool again is starting a new scene, not rejoining one that is leaving.
    #[test]
    fn ink_drawn_after_a_sweep_is_untouched() {
        let log = InkLog {
            clips: vec![
                clip(InkTool::Marker, None, 0, 8),
                clip(InkTool::Marker, None, 2_000, 8),
            ],
            gens: Vec::new(),
            erases: Vec::new(),
            wipes: vec![1_000],
        };
        let spans = visible_at(&log, 60_000);
        assert_eq!(spans.len(), 1, "only the later stroke survives");
        assert_eq!(spans[0].clip, 1);
        assert_eq!(spans[0].wipe, None);
        assert_eq!(last_visible_ms(&log), None, "and it has no end of its own");
    }

    /// A MAGIC batch caught mid-dissolve by a sweep is taken by whichever is further along, so
    /// ink can never brighten when the user asks for the screen back. The alpha is the LOWER
    /// of the two rather than their product: they are two ways of taking the same ink away.
    #[test]
    fn a_sweep_never_brightens_ink_that_was_already_fading() {
        // Generation 0's last activity is 90, so its dissolve runs 2090..3090. The disarm
        // lands at 2600, squarely inside it.
        let log = InkLog {
            clips: vec![clip(InkTool::MagicMarker, Some(0), 0, 20)],
            gens: vec![InkGen { id: 0, last_activity_ms: 90 }],
            erases: Vec::new(),
            wipes: vec![2_600],
        };
        let before = visible_at(&log, 2_599)[0];
        let after = visible_at(&log, 2_600)[0];
        assert!(after.fade.is_some() && after.wipe.is_some(), "both apply at once");
        for i in after.from as u32..after.to as u32 {
            assert!(
                after.alpha_at(i) <= before.alpha_at(i) + 1e-6,
                "ink brightened at point {i} when the sweep began"
            );
        }
        // And nothing survives the earlier of the two deadlines.
        assert!(visible_at(&log, 2_600 + FINAL_SWEEP_MS as MediaMs).is_empty());
    }

    /// A clip an ERASE already took is not part of the swept scene, so it does not stretch the
    /// draw-order space the front has to cross. Without this the surviving ink would lift more
    /// slowly than it looks like it should.
    #[test]
    fn an_already_cleared_clip_is_not_part_of_the_swept_scene() {
        let log = InkLog {
            clips: vec![
                clip(InkTool::Marker, None, 0, 30),
                clip(InkTool::Marker, None, 600, 10),
            ],
            gens: Vec::new(),
            erases: vec![500],
            wipes: vec![1_000],
        };
        let spans = visible_at(&log, 1_000);
        assert_eq!(spans.len(), 1, "the erased stroke is gone");
        let w = spans[0].wipe.expect("swept");
        assert_eq!(w.total, 10, "the swept scene is the surviving stroke alone");
        assert_eq!(w.ord_base, 0);
    }

    /// The fade TICK has to be armed for a sweep: it is 400ms of motion the video will record,
    /// so a missing tick is a visible jump rather than a slow start.
    #[test]
    fn a_running_sweep_keeps_the_tick_alive() {
        let log = swept();
        assert!(!fading_at(&log, 999), "static marker ink needs no tick");
        assert!(fading_at(&log, 1_000), "the sweep does");
        assert!(fading_at(&log, 1_000 + FINAL_SWEEP_MS as MediaMs / 2));
        assert!(!fading_at(&log, 1_000 + FINAL_SWEEP_MS as MediaMs), "and it stops at the end");
    }

    /// The sweep is RECORDED, not applied: the log still holds every stroke afterwards, which
    /// is what lets a Bronze bake replay the lift instead of finding the ink gone.
    #[test]
    fn a_sweep_keeps_the_strokes_it_lifted() {
        let mut log = InkLog::default();
        log.begin_clip(InkTool::Marker, (0.0, 0.0), WIDTH_PT, 0);
        log.wipe(1_000);
        assert_eq!(log.clips.len(), 1, "a sweep must not delete history");
        assert_eq!(log.wipes, vec![1_000]);
        assert!(any_visible_at(&log, 999));
        assert!(!any_visible_at(&log, 1_000 + FINAL_SWEEP_MS as MediaMs));
    }

    /// The final sweep and the magic dissolve share ONE curve, called with different
    /// durations. This pins that they are the same shape rather than two implementations: the
    /// front travels from `-SWEEP_EDGE` to `1.0` in both, and a zero hold means the scene is
    /// whole at the instant the sweep is triggered.
    #[test]
    fn both_sweeps_are_the_same_curve_at_different_speeds() {
        assert_eq!(sweep_front(0, 0, FINAL_SWEEP_MS), -SWEEP_EDGE);
        assert_eq!(sweep_front(FINAL_SWEEP_MS, 0, FINAL_SWEEP_MS), 1.0);
        // Same fraction of the travel, same front, whatever the durations are.
        let a = sweep_front(FINAL_SWEEP_MS / 4, 0, FINAL_SWEEP_MS);
        let b = sweep_front(MAGIC_HOLD_MS + MAGIC_SWEEP_MS / 4, MAGIC_HOLD_MS, MAGIC_SWEEP_MS);
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        // And the final one really is the fast one (the module's own compile-time assert
        // says the same; this states it where a reader of the curve tests will see it).
        const { assert!(FINAL_SWEEP_MS < MAGIC_SWEEP_MS) };
    }
}

/// [`first_live_ordinal`]: how much of a generation the sweep has eaten.
#[cfg(test)]
mod first_live_tests {
    use super::*;

    /// Through the hold a generation is whole, which is what "drawing holds everything" looks
    /// like from the drawing side.
    #[test]
    fn a_held_generation_keeps_all_of_itself() {
        assert_eq!(first_live_ordinal(50, 0), 0);
        assert_eq!(first_live_ordinal(50, MAGIC_HOLD_MS), 0);
    }

    /// The cut sweeps forward and only forward. The exact ordinal at a given instant depends
    /// on the front's soft edge, so what is pinned is the shape.
    #[test]
    fn the_cut_only_ever_moves_forward() {
        let mut last = 0u32;
        let mut moved = false;
        for idle in (MAGIC_HOLD_MS..MAGIC_GONE_MS).step_by(10) {
            let cut = first_live_ordinal(50, idle);
            assert!(cut >= last, "the cut went backwards at idle={idle}");
            moved |= cut > last;
            last = cut;
        }
        assert!(moved, "the cut never advanced, so the generation went all at once");
    }

    /// Past the sweep the whole generation is gone, which is what retires it.
    #[test]
    fn a_swept_generation_is_entirely_gone() {
        assert_eq!(first_live_ordinal(50, MAGIC_GONE_MS), 50);
        assert_eq!(first_live_ordinal(50, 100_000), 50);
        assert_eq!(first_live_ordinal(0, MAGIC_HOLD_MS + 1), 0);
    }
}

/// The GENERATION rule (DRAGON-691 amendment 2, item 2): when new drawing joins the batch on
/// screen and when it starts a fresh one.
#[cfg(test)]
mod generation_tests {
    use super::*;

    /// While a generation is merely holding, new drawing JOINS it. That is what makes "keep
    /// drawing and nothing moves" work: one batch, one hold, extended by every stroke.
    #[test]
    fn drawing_during_the_hold_joins_the_generation() {
        assert!(gen_accepts(1_000, 1_000), "the same instant");
        assert!(gen_accepts(1_000, 1_500));
        assert!(gen_accepts(1_000, 1_000 + MAGIC_HOLD_MS as MediaMs - 1));
    }

    /// The moment its sweep begins it is CLOSED, and this is the whole of the fix the owner
    /// asked for: drawing during a dissolve must not stop that dissolve, so the new ink cannot
    /// be allowed to join the batch that is leaving.
    #[test]
    fn drawing_during_the_sweep_starts_a_new_generation() {
        let closed_at = 1_000 + MAGIC_HOLD_MS as MediaMs;
        assert!(!gen_accepts(1_000, closed_at), "exactly at the sweep's start");
        assert!(!gen_accepts(1_000, closed_at + 1));
        assert!(!gen_accepts(1_000, closed_at + MAGIC_SWEEP_MS as MediaMs / 2));
        // And long after it has finished.
        assert!(!gen_accepts(1_000, 1_000 + MAGIC_GONE_MS as MediaMs * 4));
    }

    /// A generation's own clock never runs backwards, however the query time arrives. A
    /// replay scrubbing backwards, or a stamp that lands a hair early, reads as "not idle"
    /// rather than as a negative that would wrap into an enormous one.
    #[test]
    fn a_query_before_the_generation_reads_as_not_idle() {
        assert_eq!(gen_idle_ms(5_000, 4_000), 0);
        assert_eq!(gen_idle_ms(5_000, 5_000), 0);
        assert_eq!(gen_idle_ms(5_000, 5_250), 250);
        assert!(!gen_accepts(5_000, 4_000), "and a backwards stamp joins nothing");
    }

    /// The two answers are exact complements at the boundary, so there is no instant where a
    /// generation is both fading and still accepting ink.
    #[test]
    fn accepting_and_fading_never_overlap() {
        for idle in 0..(MAGIC_GONE_MS as MediaMs + 500) {
            let accepts = gen_accepts(0, idle);
            let fading = magic_alpha(0.0, gen_idle_ms(0, idle)) < 1.0
                || idle >= MAGIC_HOLD_MS as MediaMs;
            assert_ne!(accepts, fading, "idle={idle}");
        }
    }
}

/// The SESSION as the app records it, driven through [`InkLog`]'s own API so these pin the code
/// that actually runs (`app::ink` calls exactly these three methods).
///
/// The headline here is the DRAGON-691 follow-up: the log keeps the whole session, so a window
/// mode bake replaying it afterwards sees every stroke at the instants it was on screen.
#[cfg(test)]
mod session_tests {
    use super::*;

    /// Draw a stroke of `n` points from `at_ms`, one every 10ms, exactly the way
    /// `App::ink_stroke_begin` / `ink_stroke_move` do: begin, then extend, touching the
    /// generation with every sample. Returns the clip's index.
    fn draw(log: &mut InkLog, tool: InkTool, at_ms: MediaMs, n: usize) -> usize {
        let i = log.begin_clip(tool, (0.0, 10.0), WIDTH_PT, at_ms);
        for k in 1..n {
            let t = at_ms + k as MediaMs * 10;
            let c = &mut log.clips[i];
            c.points.push((k as f32 * 5.0, 10.0));
            c.point_ms.push(t);
            log.touch_clip_generation(i, t);
        }
        i
    }

    /// **THE regression** (DRAGON-691 follow-up), in the shape the owner hit it: a Marker
    /// stroke, a Magic Marker stroke, and a second Magic Marker stroke begun while the first
    /// one's sweep was in flight.
    ///
    /// What broke was not the fade. It was that the live scene DELETED the middle stroke the
    /// moment its sweep finished, so the bake, which replays this log from media 0 after the
    /// session, had no record that it had ever existed and the finished video did not contain
    /// it at any instant, including the seconds it had been solid on screen.
    #[test]
    fn a_stroke_that_faded_mid_session_is_still_in_the_log_afterwards() {
        let mut log = InkLog::default();
        // 1. Marker at 500ms: never fades, never erased.
        let marker = draw(&mut log, InkTool::Marker, 500, 10);
        // 2. Magic at 2000ms, last sample 2090. It holds to 4090 and is gone at 5090.
        let a = draw(&mut log, InkTool::MagicMarker, 2_000, 10);
        // 3. Magic at 4500ms, squarely inside A's sweep.
        let b = draw(&mut log, InkTool::MagicMarker, 4_500, 10);

        // The generation rule: B could not join A, so it opened its own.
        assert_eq!(log.gens.len(), 2, "drawing mid-sweep must open a fresh generation");
        assert_eq!(log.clips[a].generation, Some(0));
        assert_eq!(log.clips[b].generation, Some(1));
        assert_eq!(log.gens[0].last_activity_ms, 2_090, "B must not have rescued A's clock");

        // Long after everything has finished both dissolves are spent (the marker stays, it is
        // the tool that does)...
        assert!(!fading_at(&log, 20_000), "both dissolves should have finished");
        // ...and the log still holds every stroke of it. This is the property the bake rests
        // on, and the one the deleted `retire_spent_ink` broke.
        assert_eq!(log.clips.len(), 3, "the session's history must survive the session");

        // Replayed from the far end: A is SOLID through its hold.
        for t in [2_090, 3_000, 4_089] {
            let spans = visible_at(&log, t);
            let s = spans.iter().find(|s| s.clip == a).unwrap_or_else(|| {
                panic!("the mid-session magic stroke is missing from the replay at {t}ms")
            });
            assert_eq!(s.from, 0, "solid ink should start at its first point at {t}ms");
            assert_eq!(s.fade.expect("magic ink fades").alpha_at(0), 1.0, "not solid at {t}ms");
        }

        // Mid-sweep, with B on screen: A is dissolving from its head on ITS OWN clock while B
        // is untouched, and the marker is unaffected by either.
        let spans = visible_at(&log, 4_600);
        assert_eq!(spans.len(), 3, "all three strokes are on screen at 4600ms");
        let sa = spans.iter().find(|s| s.clip == a).expect("A is still on screen");
        assert!(sa.from > 0, "A should be losing its head by 4600ms");
        let fa = sa.fade.expect("magic ink fades");
        assert!(fa.alpha_at(0) < 1.0, "A should be part way through its dissolve");
        let sb = spans.iter().find(|s| s.clip == b).expect("B is on screen");
        assert_eq!(sb.from, 0);
        assert_eq!(sb.fade.expect("magic ink fades").alpha_at(0), 1.0, "B must hold");

        // A finishes on its own clock, B and the marker stay.
        let after = visible_at(&log, 5_090);
        assert!(!after.iter().any(|s| s.clip == a), "A should be gone at its own deadline");
        assert!(after.iter().any(|s| s.clip == b));
        assert!(after.iter().any(|s| s.clip == marker));
        // And the marker outlives every dissolve there is.
        assert!(visible_at(&log, 60_000).iter().any(|s| s.clip == marker));
    }

    /// The other half of the generation rule, so the fix above cannot be read as "every stroke
    /// gets its own batch": drawing while a generation is merely HOLDING joins it and extends
    /// that hold, which is what "keep drawing and nothing moves" means.
    #[test]
    fn drawing_during_the_hold_extends_it_instead_of_opening_a_generation() {
        let mut log = InkLog::default();
        draw(&mut log, InkTool::MagicMarker, 0, 10);
        draw(&mut log, InkTool::MagicMarker, 1_000, 10);
        assert_eq!(log.gens.len(), 1, "a held generation accepts new ink");
        assert_eq!(log.gens[0].last_activity_ms, 1_090, "and the hold moved with it");
        // One batch, one clock: both are solid to 3090 and both are gone at 4090.
        assert!(any_visible_at(&log, 3_000));
        assert!(!any_visible_at(&log, 4_090));
    }

    /// A stroke drawn so slowly that its own batch began to fade under it does NOT reopen that
    /// batch, and Marker ink has no batch to touch at all.
    #[test]
    fn a_closed_generation_never_reopens() {
        let mut log = InkLog::default();
        let i = log.begin_clip(InkTool::MagicMarker, (0.0, 0.0), WIDTH_PT, 0);
        log.touch_clip_generation(i, 1_000);
        assert_eq!(log.gens[0].last_activity_ms, 1_000);
        log.touch_clip_generation(i, 5_000);
        assert_eq!(log.gens[0].last_activity_ms, 1_000, "a closed generation must stay closed");
        let m = log.begin_clip(InkTool::Marker, (0.0, 0.0), WIDTH_PT, 6_000);
        log.touch_clip_generation(m, 6_100);
        assert_eq!(log.gens.len(), 1, "marker ink opens no generation");
        assert_eq!(log.clips[m].generation, None);
    }

    /// Erase records an INSTANT and keeps everything it cleared, for the same reason the fades
    /// do: the bake has to be able to replay both sides of it.
    #[test]
    fn an_erase_keeps_the_strokes_it_cleared() {
        let mut log = InkLog::default();
        draw(&mut log, InkTool::Marker, 0, 10);
        draw(&mut log, InkTool::MagicMarker, 100, 10);
        log.erase(3_000);
        assert_eq!(log.clips.len(), 2, "an erase must not delete history");
        assert_eq!(log.erases, vec![3_000]);
        assert!(any_visible_at(&log, 1_000), "both were on screen before it");
        assert!(!any_visible_at(&log, 3_000), "and neither is after it");
    }

    /// The two cheap questions never disagree with the full replay, which is what lets the
    /// surface's lifetime and the fade tick ask them instead of building every span.
    #[test]
    fn the_showing_questions_agree_with_the_replay() {
        let mut log = InkLog::default();
        draw(&mut log, InkTool::Marker, 500, 8);
        draw(&mut log, InkTool::MagicMarker, 2_000, 8);
        draw(&mut log, InkTool::MagicMarker, 4_500, 8);
        log.erase(9_000);
        for t in (0..12_000).step_by(37) {
            let spans = visible_at(&log, t);
            assert_eq!(any_visible_at(&log, t), !spans.is_empty(), "at {t}ms");
            assert_eq!(
                fading_at(&log, t),
                spans.iter().any(|s| s.fade.is_some()),
                "at {t}ms"
            );
        }
    }
}

#[cfg(test)]
mod alpha_run_tests {
    use super::*;

    /// The two ends are exact: a solid run fills at 1.0 (so undissolved ink is not drawn at
    /// 96%) and an empty run at 0.0 (so nothing is drawn at all).
    #[test]
    fn the_ends_of_the_scale_are_exact() {
        assert_eq!(alpha_run(1.0), ALPHA_STEPS);
        assert_eq!(run_alpha(ALPHA_STEPS), 1.0);
        assert_eq!(alpha_run(0.0), 0);
        assert_eq!(run_alpha(0), 0.0);
    }

    /// Out-of-range input cannot produce an out-of-range run, so a caller handing in a
    /// stale or unclamped alpha indexes nothing surprising.
    #[test]
    fn the_scale_is_closed_at_both_ends() {
        assert_eq!(alpha_run(-5.0), 0);
        assert_eq!(alpha_run(9.0), ALPHA_STEPS);
        assert_eq!(run_alpha(usize::MAX), 1.0);
    }

    /// The quantization step is small enough that the gradient reads as smooth. Banding is
    /// the one visible failure mode of drawing a fade in runs, so the bound is pinned
    /// rather than left to whatever `ALPHA_STEPS` happens to be.
    #[test]
    fn the_step_is_too_small_to_band() {
        assert!(1.0 / ALPHA_STEPS as f32 <= 0.05, "an alpha step over 5% is visible banding");
    }

    /// Runs are ordered the same way alphas are, which is what lets the caller sort by run
    /// and get the draw order right.
    #[test]
    fn a_stronger_alpha_is_never_a_weaker_run() {
        let mut last = 0usize;
        for i in 0..=100 {
            let r = alpha_run(i as f32 / 100.0);
            assert!(r >= last);
            last = r;
        }
    }
}

#[cfg(test)]
mod ages_along_tests {
    use super::*;

    /// Six raw samples down a straight line, one every 100ms.
    fn raw() -> (Vec<(f32, f32)>, Vec<u32>) {
        let pts: Vec<(f32, f32)> = (0..6).map(|i| (i as f32 * 10.0, 0.0)).collect();
        let ms: Vec<u32> = (0..6).map(|i| i as u32 * 100).collect();
        (pts, ms)
    }

    /// The endpoints survive exactly: the stroke's first drawn instant and its last must
    /// land on the fit's first and last points, or the dissolve starts (or ends) at the
    /// wrong end of the ink.
    #[test]
    fn the_endpoints_keep_their_own_timestamps() {
        let (pts, ms) = raw();
        let fit: Vec<(f32, f32)> = (0..21).map(|i| (i as f32 * 2.5, 0.0)).collect();
        let out = ages_along(&pts, &ms, &fit);
        assert_eq!(out.len(), fit.len());
        assert_eq!(out[0], 0);
        assert_eq!(*out.last().unwrap(), 500);
    }

    /// Monotone in, monotone out. [`first_live`]'s prefix cut is only a prefix because of
    /// this, so it is pinned here rather than assumed there.
    #[test]
    fn the_transfer_preserves_order() {
        let (pts, ms) = raw();
        let fit: Vec<(f32, f32)> = (0..51).map(|i| (i as f32, 0.0)).collect();
        let out = ages_along(&pts, &ms, &fit);
        assert!(out.windows(2).all(|w| w[0] <= w[1]), "ages went backwards: {out:?}");
    }

    /// Halfway along the line is halfway through the drawing, because the transfer is by
    /// ARC LENGTH and this trail was drawn at a constant speed. This is what makes the
    /// erasure sweep at the speed the hand moved.
    #[test]
    fn the_middle_of_an_even_stroke_is_the_middle_of_its_time() {
        let (pts, ms) = raw();
        let fit: Vec<(f32, f32)> = (0..11).map(|i| (i as f32 * 5.0, 0.0)).collect();
        let out = ages_along(&pts, &ms, &fit);
        assert!(out[5].abs_diff(250) <= 1, "midpoint age {} is not ~250ms", out[5]);
    }

    /// Interpolation, not nearest-sample: a fit point BETWEEN two raw samples takes a value
    /// between their timestamps rather than snapping to one. Nearest would quantize the
    /// ages into plateaus and put the run boundaries on them.
    #[test]
    fn a_point_between_two_samples_lands_between_their_times() {
        let pts = vec![(0.0, 0.0), (100.0, 0.0)];
        let ms = vec![0u32, 1_000];
        let fit = vec![(0.0, 0.0), (25.0, 0.0), (50.0, 0.0), (100.0, 0.0)];
        let out = ages_along(&pts, &ms, &fit);
        assert_eq!(out, vec![0, 250, 500, 1_000]);
    }

    /// Degenerate input answers something usable rather than panicking or desyncing: one
    /// value per fit point, all at the stroke's start, so the ink fades FIRST rather than
    /// lingering. Losing the timing must never mean losing the deadline.
    #[test]
    fn broken_input_still_answers_one_age_per_point() {
        let fit = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)];
        assert_eq!(ages_along(&[], &[], &fit), vec![0, 0, 0]);
        // Mismatched lengths (a caller that got its parallel arrays out of step).
        assert_eq!(ages_along(&[(0.0, 0.0)], &[5, 6], &fit), vec![5, 5, 5]);
        // No fit at all: nothing to say.
        assert!(ages_along(&[(0.0, 0.0), (1.0, 0.0)], &[0, 1], &[]).is_empty());
    }

    /// A stroke that stopped moving (every sample in the same place) has no arc length to
    /// interpolate over. It must still answer one age per point instead of dividing by zero.
    #[test]
    fn a_stroke_that_never_moved_is_still_timed() {
        let pts = vec![(4.0, 4.0); 4];
        let ms = vec![0u32, 50, 100, 150];
        let out = ages_along(&pts, &ms, &[(4.0, 4.0), (4.0, 4.0)]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|&v| v <= 150));
    }
}

/// The tool's headline behaviour, end to end over the pure pipeline a real stroke goes
/// through: raw samples with their timestamps, transferred onto the smoothed centerline, read
/// back as alphas. Each piece is pinned on its own above; this is the one that says the pieces
/// together do what the user asked for.
/// The tool's headline behaviour end to end, over the pure pipeline a real scene goes
/// through: two strokes drawn one after the other, smoothed the way the live path smooths
/// them, then read back as alphas at a given idle time. Each piece is pinned on its own
/// above; this is the one that says the pieces together do what the owner asked for.
#[cfg(test)]
mod dissolve_order_tests {
    use super::*;

    /// A stroke of `n` samples along a straight line starting at `x0`, smoothed and given its
    /// per-point timestamps exactly as `app::ink` does.
    fn stroke(x0: f32, n: usize) -> Vec<(f32, f32)> {
        let raw: Vec<(f32, f32)> = (0..n).map(|i| (x0 + i as f32 * 8.0, 100.0)).collect();
        let raw_ms: Vec<u32> = (0..n).map(|i| i as u32 * 20).collect();
        let fit = crate::pen_stroke::smooth_path(&raw, WIDTH_PT);
        // The timestamps still ride along, which is what the future bake needs and what keeps
        // draw order meaningful inside one stroke.
        let ages = ages_along(&raw, &raw_ms, &fit);
        assert!(ages.windows(2).all(|w| w[0] <= w[1]), "timestamps must stay in draw order");
        fit
    }

    /// A whole scene: two strokes, the first drawn before the second, with the scene-wide
    /// ordinals `app::ink` assigns.
    fn scene() -> (Vec<u32>, Vec<u32>, u32) {
        let a = stroke(0.0, 20);
        let b = stroke(400.0, 20);
        let total = (a.len() + b.len()) as u32;
        let first: Vec<u32> = (0..a.len() as u32).collect();
        let second: Vec<u32> = (a.len() as u32..total).collect();
        (first, second, total)
    }

    /// Drawing holds EVERYTHING: through the hold, every point of every stroke is solid.
    #[test]
    fn nothing_lifts_while_the_hold_is_running() {
        let (first, second, total) = scene();
        for ords in [&first, &second] {
            for &o in ords.iter() {
                assert_eq!(magic_alpha(draw_fraction(o, total), MAGIC_HOLD_MS), 1.0);
            }
        }
    }

    /// Mid-sweep the scene is a gradient running from its oldest point to its newest, ACROSS
    /// both strokes. That is the requirement's "in draw order", and it is what a per-stroke
    /// fade would not give.
    #[test]
    fn the_scene_lifts_from_its_oldest_point_onward() {
        let (first, second, total) = scene();
        let idle = MAGIC_HOLD_MS + MAGIC_SWEEP_MS / 2;
        let all: Vec<f32> = first
            .iter()
            .chain(second.iter())
            .map(|&o| magic_alpha(draw_fraction(o, total), idle))
            .collect();
        assert!(
            all.windows(2).all(|w| w[0] <= w[1] + 1e-6),
            "the scene is not lifting in draw order: {all:?}"
        );
        assert!(all[0] < *all.last().unwrap(), "no gradient across the scene");
        // The FIRST stroke is further gone than the second, at the same instant.
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let a: Vec<f32> = all[..first.len()].to_vec();
        let b: Vec<f32> = all[first.len()..].to_vec();
        assert!(mean(&a) < mean(&b), "the older stroke is not leading the dissolve");
    }

    /// And the whole generation is gone at three seconds, both strokes together.
    #[test]
    fn the_whole_scene_retires_at_the_end_of_the_sweep() {
        let (first, second, total) = scene();
        // Just before the end: the older stroke is entirely eaten, the newer one is not.
        let cut = first_live_ordinal(total, MAGIC_GONE_MS - 50);
        assert!(cut > *first.last().expect("points"), "the older stroke should be gone");
        assert!(cut <= *second.last().expect("points"), "the newer one should not be");
        // And at the end, everything.
        assert_eq!(first_live_ordinal(total, MAGIC_GONE_MS), total);
    }
}

#[cfg(test)]
mod fraction_tests {
    use super::*;

    /// An even polyline's fractions are evenly spaced, and both ends are exact. The ribbon's
    /// pieces are tagged with this quantity, so an end that was 0.999 rather than 1.0 would
    /// put the last piece of a stroke in the wrong run.
    #[test]
    fn the_ends_are_exact_and_the_middle_is_even() {
        let pts: Vec<(f32, f32)> = (0..5).map(|i| (i as f32 * 10.0, 0.0)).collect();
        let f = arc_fractions(&pts);
        assert_eq!(f, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
    }

    /// A polyline with no length is all one position, so it stays in ONE run instead of
    /// dividing by zero and scattering across every run at once.
    #[test]
    fn a_polyline_with_no_length_is_one_position() {
        assert_eq!(arc_fractions(&[(2.0, 2.0); 3]), vec![0.0, 0.0, 0.0]);
        assert_eq!(arc_fractions(&[(2.0, 2.0)]), vec![0.0]);
        assert!(arc_fractions(&[]).is_empty());
    }

    /// The reading interpolates rather than snapping, and both ends read their own value.
    #[test]
    fn a_reading_between_two_points_is_interpolated() {
        let fracs = vec![0.0, 0.5, 1.0];
        let vals = vec![0u32, 100, 300];
        assert_eq!(at_fraction(&fracs, &vals, 0.0), 0);
        assert_eq!(at_fraction(&fracs, &vals, 0.25), 50);
        assert_eq!(at_fraction(&fracs, &vals, 0.5), 100);
        assert_eq!(at_fraction(&fracs, &vals, 0.75), 200);
        assert_eq!(at_fraction(&fracs, &vals, 1.0), 300);
    }

    /// Out-of-range and broken input answer something usable. A stray fraction must never
    /// index past the series, and a lost series must fade rather than linger.
    #[test]
    fn broken_or_out_of_range_input_still_answers() {
        let fracs = vec![0.0, 1.0];
        let vals = vec![10u32, 20];
        assert_eq!(at_fraction(&fracs, &vals, -3.0), 10);
        // Clamped to the far end, which a well-formed series always reaches.
        assert_eq!(at_fraction(&fracs, &vals, 9.0), 20);
        assert_eq!(at_fraction(&[], &[], 0.5), 0);
        assert_eq!(at_fraction(&fracs, &[1, 2, 3], 0.5), 0);
    }

    /// A degenerate stroke's fractions are all zero, so every reading is the FIRST value: the
    /// OLDEST. Answering with the newest instead would let a stroke that never moved outlive
    /// its own deadline, which on a recording is ink the user cannot account for.
    #[test]
    fn a_degenerate_stroke_reads_its_oldest_value() {
        for f in [0.0, 0.5, 1.0] {
            assert_eq!(at_fraction(&[0.0, 0.0, 0.0], &[7, 8, 9], f), 7, "f={f}");
        }
    }
}

#[cfg(test)]
mod tools_available_tests {
    use super::*;

    /// The shipped case: a live recording, in ANY capture mode, on ANY tier.
    #[test]
    fn a_live_recording_offers_the_tools() {
        assert!(tools_available(true));
    }

    /// Not recording is the commonest state of the app and offers nothing: the preview
    /// editor is where a still gets annotated.
    #[test]
    fn nothing_is_offered_outside_a_recording() {
        assert!(!tools_available(false));
    }

    /// The two terms DRAGON-693 removed, stated as the behaviour they now have, because both
    /// used to REFUSE and a reader will otherwise assume they still do.
    ///
    /// A hidden toolbar used to mean "no door", and the tray's Live Annotations submenu is
    /// that door on every platform now. The portal fallback used to mean "no surface", and
    /// [`tier`] answers Silver or Bronze instead, both of which mint their own.
    #[test]
    fn a_hidden_toolbar_and_the_portal_fallback_are_no_longer_refusals() {
        assert!(tools_available(true), "a hidden toolbar is armed from the tray now");
        assert_eq!(
            tier(true, false, false),
            InkTier::Silver,
            "the fallback is graded, not refused"
        );
        assert_eq!(tier(true, true, false), InkTier::Bronze);
    }

    /// The one remaining term refuses by itself, which is the whole of the gate.
    #[test]
    fn the_recording_term_is_the_whole_gate() {
        assert!(tools_available(true));
        assert!(!tools_available(false));
    }
}

/// The support TIER (DRAGON-693): the owner's gold / silver / bronze grading, and the three
/// rules that follow from it.
#[cfg(test)]
mod tier_tests {
    use super::*;

    /// Every platform with a window level of its own is GOLD, in every capture mode. That is
    /// macOS, Windows, and any Wayland session that has `wlr-layer-shell`, all of which reach
    /// here with `portal_fallback == false`.
    #[test]
    fn a_session_with_its_own_surface_layer_is_gold() {
        assert_eq!(tier(false, false, false), InkTier::Gold);
        assert_eq!(tier(false, true, false), InkTier::Gold, "including a window recording");
    }

    /// A no-layer-shell session splits by MODE, and only there: region and monitor casts see
    /// the screen (Silver), a window cast sees one window's own pixels (Bronze).
    #[test]
    fn the_fallback_splits_by_capture_mode() {
        assert_eq!(tier(true, false, false), InkTier::Silver);
        assert_eq!(tier(true, true, false), InkTier::Bronze);
    }

    /// DRAGON-738: a WINDOW cast we cannot place on screen is Bronze even with layer shell.
    /// KDE and wlroots are always here (no `zcosmic_toplevel_info_v1`, so no geometry ever),
    /// and COSMIC lands here when two same-size windows defeat DRAGON-737's pixel tie-break.
    /// These rows used to read Gold and delivered a video with no ink in it.
    #[test]
    fn an_unplaceable_window_cast_is_bronze_even_with_layer_shell() {
        assert_eq!(tier(false, true, true), InkTier::Bronze);
        assert_eq!(tier(true, true, true), InkTier::Bronze, "and still Bronze without it");
    }

    /// The rows that must NOT move, spelled out as one table so a reader can check a real
    /// session against it. Everything not named by the window term is exactly where
    /// DRAGON-693 left it.
    #[test]
    fn the_unaffected_rows_are_unchanged() {
        // macOS and Windows: no portal, `platform::window_rect` answers all session long, so
        // the third fact is false by construction and every mode is Gold.
        assert_eq!(tier(false, false, false), InkTier::Gold, "mac/win region or monitor");
        assert_eq!(tier(false, true, false), InkTier::Gold, "mac/win window");
        // A native Linux window recording crops a screen rectangle, so its ink is captured
        // live and no origin is wanted: Gold, and the caller reports the term false.
        assert_eq!(tier(false, true, false), InkTier::Gold, "native Linux window");
        // A REGION or MONITOR cast has the ink on the screen it is casting. The window term
        // cannot reach it, whatever value it carries.
        for unlocated in [false, true] {
            assert_eq!(tier(false, false, unlocated), InkTier::Gold, "layer-shell screen cast");
            assert_eq!(tier(true, false, unlocated), InkTier::Silver, "fallback screen cast");
        }
    }

    /// Only GOLD leaves marks up after a disarm, and the two questions are exact complements
    /// so a caller can ask from either side without a second rule to keep in step.
    #[test]
    fn only_gold_persists_and_the_two_questions_are_complements() {
        assert!(InkTier::Gold.persists_after_disarm());
        assert!(!InkTier::Silver.persists_after_disarm());
        assert!(!InkTier::Bronze.persists_after_disarm());
        for t in [InkTier::Gold, InkTier::Silver, InkTier::Bronze] {
            assert_ne!(t.persists_after_disarm(), t.sweeps_on_disarm(), "{t:?}");
        }
    }

    /// The two tiers that cannot use an always-present surface are exactly the two that mint
    /// their own window, and exactly the two that sweep on disarm. That is not a coincidence:
    /// the window has to go for the user to reach the desktop again, which is WHY they sweep.
    #[test]
    fn arming_its_own_window_and_sweeping_on_disarm_are_the_same_two_tiers() {
        for t in [InkTier::Gold, InkTier::Silver, InkTier::Bronze] {
            assert_eq!(t.arms_own_window(), t.sweeps_on_disarm(), "{t:?}");
        }
    }

    /// Only BRONZE draws on a frozen snapshot: it is the one tier whose recording is not of
    /// the screen the user is looking at.
    #[test]
    fn only_bronze_shows_a_snapshot() {
        assert!(!InkTier::Gold.shows_snapshot());
        assert!(!InkTier::Silver.shows_snapshot());
        assert!(InkTier::Bronze.shows_snapshot());
    }

    /// The default is Gold, which is what a session that has not started recording gets, and
    /// what every platform but one Linux configuration gets when it does.
    #[test]
    fn the_default_is_gold() {
        assert_eq!(InkTier::default(), InkTier::Gold);
        assert_eq!(InkTier::Gold.word(), "gold");
        assert_eq!(InkTier::Silver.word(), "silver");
        assert_eq!(InkTier::Bronze.word(), "bronze");
    }

    /// DRAGON-738: the armed window SAYS which mode it is, because a downgrade the user
    /// cannot see is a downgrade they will read as a bug. The two Bronze causes give two
    /// different reasons, because they are two different facts about the user's desktop.
    #[test]
    fn a_bronze_armed_window_names_its_mode_and_its_cause() {
        let cannot_place = armed_note(InkTier::Bronze, true).expect("bronze says something");
        let no_layer = armed_note(InkTier::Bronze, false).expect("bronze says something");
        assert_ne!(cannot_place, no_layer, "two causes, two reasons");
        assert!(cannot_place.contains("will not say where"), "{cannot_place}");
        assert!(no_layer.contains("keeps recording live"), "{no_layer}");
    }

    /// And SILVER is told NOTHING, whatever the window term says. Its armed window is in the
    /// cast, so a caption there would be burned into the user's recording; the faint scrim is
    /// the most that tier can afford to say, which is why it has one. Gold has no armed
    /// window to print on at all.
    #[test]
    fn only_bronze_may_print_on_its_armed_window() {
        for unlocated in [false, true] {
            assert_eq!(armed_note(InkTier::Gold, unlocated), None, "gold has no armed window");
            assert_eq!(
                armed_note(InkTier::Silver, unlocated),
                None,
                "a silver caption would be recorded"
            );
            assert!(armed_note(InkTier::Bronze, unlocated).is_some());
        }
    }

    /// Project rule: no em-dashes anywhere, user copy included.
    #[test]
    fn the_notes_carry_no_em_dashes() {
        for tier in [InkTier::Gold, InkTier::Silver, InkTier::Bronze] {
            for unlocated in [false, true] {
                let note = armed_note(tier, unlocated).unwrap_or("");
                assert!(!note.contains('\u{2014}'), "{tier:?}/{unlocated}: {note}");
                assert!(!note.contains('\u{2013}'), "{tier:?}/{unlocated}: {note}");
            }
        }
    }
}

/// DRAGON-736: WHO puts the ink in the file, over the whole matrix of capture mode, backend
/// and tier.
///
/// Pinned here because the version this replaced hid the backend term inside a `cfg!`, so the
/// row that was wrong (a Linux portal window cast) could not be written down as a test at all.
#[cfg(test)]
mod needs_composite_tests {
    use super::*;

    /// A REGION or MONITOR recording is of the screen, so the compositor captures the ink with
    /// everything else, on every platform and every backend. The window fact cannot move it.
    #[test]
    fn a_screen_recording_never_needs_us_to_composite() {
        for excludes in [false, true] {
            assert!(!needs_composite(false, excludes, InkTier::Gold), "excludes={excludes}");
            assert!(!needs_composite(false, excludes, InkTier::Silver), "excludes={excludes}");
        }
    }

    /// A WINDOW recording splits on the backend, and that split is the whole ticket. Both rows
    /// are reachable on Linux in the same build on the same desktop: the native screencopy
    /// worker crops the screen, the portal worker casts the toplevel's own buffer.
    #[test]
    fn a_window_recording_splits_on_what_the_capture_sees() {
        assert!(!needs_composite(true, false, InkTier::Gold), "a screen-rectangle crop");
        assert!(needs_composite(true, true, InkTier::Gold), "a window-buffer capture");
    }

    /// BRONZE reaches the same compositing from the other end and needs no window term at all:
    /// the ink is drawn on a frozen still of the stream, so it was never on the screen being
    /// cast. It is only ever a WINDOW session in practice, but the rule does not depend on it.
    #[test]
    fn bronze_always_needs_compositing() {
        for window_mode in [false, true] {
            for excludes in [false, true] {
                assert!(
                    needs_composite(window_mode, excludes, InkTier::Bronze),
                    "window_mode={window_mode} excludes={excludes}"
                );
            }
        }
    }

    /// The shipped combinations, spelled out end to end so a future reader can check a real
    /// session against this table rather than re-deriving it.
    #[test]
    fn the_shipped_sessions_land_where_they_should() {
        // Linux, layer shell present, native screencopy: the ink is captured live.
        assert!(!needs_composite(true, false, tier(false, true, false)));
        // Linux, layer shell present, portal window cast WE COULD PLACE: Gold, we composite.
        assert!(needs_composite(true, true, tier(false, true, false)));
        // Linux, layer shell present, portal window cast we could NOT place (KDE, wlroots, a
        // COSMIC tie): BRONZE since DRAGON-738, and we composite it.
        assert!(needs_composite(true, true, tier(false, true, true)));
        // Linux, no layer shell, portal window cast: BRONZE, and we composite it.
        assert!(needs_composite(true, true, tier(true, true, false)));
        // Linux, no layer shell, portal monitor cast: SILVER, and the cast carries the ink.
        assert!(!needs_composite(false, false, tier(true, false, false)));
        // macOS and Windows window recordings: always the window's own buffer.
        assert!(needs_composite(true, true, tier(false, true, false)));
    }
}

/// BRONZE's two spaces (DRAGON-693): the window the user points at, and the stream the ink is
/// recorded in.
#[cfg(test)]
mod stream_space_tests {
    use super::*;

    /// A picture fits INSIDE its viewport, centred, with its aspect kept. Contain rather than
    /// cover, so the frame's own corners are reachable by a stroke.
    #[test]
    fn a_picture_fits_inside_its_viewport_and_is_centred() {
        // A 2:1 picture in a square viewport: full width, bars top and bottom.
        let (x, y, w, h) = fit_rect((800, 400), (1000.0, 1000.0));
        assert_eq!((w, h), (1000.0, 500.0));
        assert_eq!(x, 0.0);
        assert_eq!(y, 250.0);
        // And a tall one in a wide viewport: full height, bars left and right.
        let (x, y, w, h) = fit_rect((400, 800), (1000.0, 400.0));
        assert_eq!((w, h), (200.0, 400.0));
        assert_eq!(y, 0.0);
        assert_eq!(x, 400.0);
    }

    /// It DOES enlarge a small picture, unlike the capture overlay's letterbox bridge. A 400pt
    /// window's stream on a 27-inch display would otherwise be annotated at postage-stamp size.
    #[test]
    fn a_small_picture_is_enlarged_to_fit() {
        let (_, _, w, h) = fit_rect((400, 300), (1600.0, 1200.0));
        assert_eq!((w, h), (1600.0, 1200.0));
    }

    /// The round trip: the picture's corners and centre map to the frame's corners and centre.
    /// This is what "the ink lands under the pointer" means as an equation.
    #[test]
    fn a_viewport_point_maps_back_onto_the_frame() {
        let content = (800u32, 400u32);
        let fit = fit_rect(content, (1000.0, 1000.0));
        assert_eq!(to_stream((0.0, 250.0), fit, content, 0.0), (0.0, 0.0));
        assert_eq!(to_stream((1000.0, 750.0), fit, content, 0.0), (800.0, 400.0));
        assert_eq!(to_stream((500.0, 500.0), fit, content, 0.0), (400.0, 200.0));
    }

    /// A point in the LETTERBOX BARS clamps onto the frame's edge rather than landing outside
    /// it. The recording does not contain those pixels, so the ink must not be recorded there.
    #[test]
    fn a_point_in_the_bars_clamps_onto_the_frame() {
        let content = (800u32, 400u32);
        let fit = fit_rect(content, (1000.0, 1000.0));
        let p = to_stream((500.0, 10.0), fit, content, 0.0);
        assert_eq!(p, (400.0, 0.0), "the top bar clamps to the frame's top edge");
        let p = to_stream((500.0, 990.0), fit, content, 0.0);
        assert_eq!(p, (400.0, 400.0), "and the bottom bar to its bottom");
    }

    /// The margin keeps a stroke's full WEIGHT inside the frame, the same rule the region
    /// clamp follows: the ink's half-width, in the frame's own pixels.
    #[test]
    fn the_margin_keeps_the_strokes_weight_inside() {
        let content = (800u32, 400u32);
        let fit = fit_rect(content, (800.0, 400.0));
        assert_eq!(to_stream((0.0, 0.0), fit, content, 10.0), (10.0, 10.0));
        assert_eq!(to_stream((800.0, 400.0), fit, content, 10.0), (790.0, 390.0));
    }

    /// Degenerate input answers something finite rather than dividing by zero: a zero-sized
    /// stream (before the first frame fixes it) and a zero-sized window (before the first
    /// configure) are both ordinary states for a frame or two.
    #[test]
    fn degenerate_sizes_stay_finite() {
        let f = fit_rect((0, 0), (100.0, 100.0));
        assert_eq!(f, (0.0, 0.0, 100.0, 100.0));
        assert_eq!(to_stream((10.0, 10.0), f, (0, 0), 0.0), (0.0, 0.0));
        let f = fit_rect((100, 100), (0.0, 0.0));
        assert!(f.2 >= 0.0 && f.3 >= 0.0, "{f:?}");
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::*;

    const CROP: Rect = (100.0, 50.0, 400.0, 300.0);

    /// A point inside is untouched, so ordinary drawing is byte-identical to no clamp.
    #[test]
    fn a_point_well_inside_is_left_alone() {
        assert_eq!(clamp_into((250.0, 200.0), CROP, 3.0), (250.0, 200.0));
    }

    /// The margin is the ink's half-width, so a stroke drawn hard against the crop edge has
    /// its whole weight inside instead of half of it hanging over the recorded boundary.
    #[test]
    fn the_margin_keeps_the_stroke_weight_inside() {
        assert_eq!(clamp_into((0.0, 0.0), CROP, 3.0), (103.0, 53.0));
        assert_eq!(clamp_into((9_999.0, 9_999.0), CROP, 3.0), (497.0, 347.0));
    }

    /// A rect too small to hold the margin collapses to its centre rather than inverting.
    /// An inverted clamp would put the point OUTSIDE the crop, which is the exact opposite
    /// of what this exists for.
    #[test]
    fn a_rect_smaller_than_the_margin_collapses_to_its_middle() {
        let tiny: Rect = (10.0, 10.0, 2.0, 2.0);
        assert_eq!(clamp_into((0.0, 0.0), tiny, 5.0), (11.0, 11.0));
        assert_eq!(clamp_into((99.0, 99.0), tiny, 5.0), (11.0, 11.0));
    }

    /// A zero margin is still a clamp: the point lands ON the edge, never past it.
    #[test]
    fn a_zero_margin_still_bounds_the_point() {
        assert_eq!(clamp_into((-40.0, 900.0), CROP, 0.0), (100.0, 350.0));
    }

    /// DRAGON-734, the shape of the defect, kept so nobody has to re-derive it.
    ///
    /// A crop the size of the portal's window PLACEHOLDER, one point at the origin, collapses
    /// EVERY sample of every stroke onto the same coordinate at the default ink width. The
    /// clamp is behaving exactly as the test above says it should; what was wrong was giving
    /// it that rectangle at all, which is the job of `selection::region_crop`. The lesson is
    /// that a degenerate crop here is silent: no panic, no warning, just a marker that draws
    /// one invisible dot for a whole recording.
    #[test]
    fn the_portal_placeholder_crop_collapses_every_sample_to_one_point() {
        let placeholder: Rect = (0.0, 0.0, 1.0, 1.0);
        let margin = WIDTH_PT * 0.5;
        let dot = clamp_into((0.0, 0.0), placeholder, margin);
        for p in [(400.0, 300.0), (2560.0, 720.0), (5119.0, 1439.0), (-8.0, -8.0)] {
            assert_eq!(clamp_into(p, placeholder, margin), dot, "sample {p:?} escaped");
        }
        assert_eq!(dot, (0.5, 0.5));
    }
}

#[cfg(test)]
mod rect_subtract_tests {
    use super::*;

    const OUT: Rect = (0.0, 0.0, 1000.0, 600.0);

    /// The area of a rect list, for checking a subtraction kept what it should.
    fn area(rs: &[Rect]) -> f32 {
        rs.iter().map(|r| r.2 * r.3).sum()
    }

    /// Whether any point of `rs` is inside `hole`.
    fn touches(rs: &[Rect], hole: Rect) -> bool {
        rs.iter().any(|r| overlap_area(*r, hole) > 0.0)
    }

    /// The shipped case: the toolbar floating in the middle of the output. Four bands come
    /// back, none of them over the toolbar, and together they are the output minus it.
    #[test]
    fn a_toolbar_in_the_middle_leaves_four_bands() {
        let bar: Rect = (400.0, 500.0, 200.0, 48.0);
        let bands = rect_subtract(OUT, bar);
        assert_eq!(bands.len(), 4);
        assert!(!touches(&bands, bar), "a band overlaps the toolbar");
        assert!((area(&bands) - (1000.0 * 600.0 - 200.0 * 48.0)).abs() < 0.01);
    }

    /// The bands never overlap EACH OTHER either. An input region is a union so overlap
    /// would not be wrong, but it makes the result impossible to reason about and hands the
    /// compositor rectangles it does not need.
    #[test]
    fn the_bands_do_not_overlap_each_other() {
        let bands = rect_subtract(OUT, (400.0, 500.0, 200.0, 48.0));
        for (i, a) in bands.iter().enumerate() {
            for b in &bands[i + 1..] {
                assert_eq!(overlap_area(*a, *b), 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    /// The toolbar hugging an edge (its default bottom-centred home, and every drag that
    /// pushes it into a corner) drops the bands that would be empty instead of emitting
    /// zero-area rectangles.
    #[test]
    fn a_toolbar_against_an_edge_emits_no_empty_bands() {
        // Bottom-centred, flush with the bottom edge: no band below it.
        let bands = rect_subtract(OUT, (400.0, 552.0, 200.0, 48.0));
        assert_eq!(bands.len(), 3);
        assert!(bands.iter().all(|r| r.2 > 0.0 && r.3 > 0.0));
        // A corner: two bands.
        let bands = rect_subtract(OUT, (0.0, 0.0, 200.0, 48.0));
        assert_eq!(bands.len(), 2);
        assert!(bands.iter().all(|r| r.2 > 0.0 && r.3 > 0.0));
    }

    /// A toolbar that has been dragged onto another monitor does not intersect this output
    /// at all, so the whole surface stays drawable.
    #[test]
    fn a_hole_that_misses_leaves_the_rect_whole() {
        assert_eq!(rect_subtract(OUT, (2000.0, 100.0, 200.0, 48.0)), vec![OUT]);
        assert_eq!(rect_subtract(OUT, (0.0, 0.0, 0.0, 0.0)), vec![OUT]);
    }

    /// A hole covering the output leaves nothing, which is the honest answer rather than a
    /// degenerate rectangle: there is nowhere left to draw.
    #[test]
    fn a_hole_that_swallows_everything_leaves_nothing() {
        assert!(rect_subtract(OUT, (-10.0, -10.0, 5000.0, 5000.0)).is_empty());
    }

    /// A degenerate surface answers nothing rather than emitting negative extents.
    #[test]
    fn a_degenerate_surface_yields_nothing() {
        assert!(rect_subtract((0.0, 0.0, 0.0, 400.0), (1.0, 1.0, 2.0, 2.0)).is_empty());
    }
}

#[cfg(test)]
mod best_output_tests {
    use super::*;

    /// A 1080p pair, side by side.
    const SCREENS: [Rect; 2] = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 1920.0, 1080.0)];

    /// A region wholly on one display picks that display, which is every ordinary case.
    #[test]
    fn a_region_on_one_display_picks_it() {
        assert_eq!(best_output(&SCREENS, (100.0, 100.0, 400.0, 300.0)), Some(0));
        assert_eq!(best_output(&SCREENS, (2100.0, 100.0, 400.0, 300.0)), Some(1));
    }

    /// A region drawn across the seam belongs to the display holding most of it: that is
    /// where the user is looking and where the toolbar anchored itself.
    #[test]
    fn a_region_across_the_seam_picks_the_larger_share() {
        assert_eq!(best_output(&SCREENS, (1820.0, 100.0, 400.0, 300.0)), Some(1));
        assert_eq!(best_output(&SCREENS, (1620.0, 100.0, 400.0, 300.0)), Some(0));
    }

    /// A target that touches nothing has no host, and the caller must be told so rather
    /// than handed display zero.
    #[test]
    fn a_target_that_touches_nothing_has_no_output() {
        assert_eq!(best_output(&SCREENS, (5000.0, 5000.0, 10.0, 10.0)), None);
        assert_eq!(best_output(&[], (0.0, 0.0, 10.0, 10.0)), None);
        // A zero-area target overlaps nothing, by area.
        assert_eq!(best_output(&SCREENS, (10.0, 10.0, 0.0, 0.0)), None);
    }

    /// An exact tie takes the earlier output, so the answer does not depend on the order
    /// the system happened to enumerate displays in.
    #[test]
    fn an_exact_tie_takes_the_first() {
        assert_eq!(best_output(&SCREENS, (1720.0, 0.0, 400.0, 1080.0)), Some(0));
    }
}

