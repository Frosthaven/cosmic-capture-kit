//! DRAGON-366 — **TEMPORARY** per-frame diagnostics for the preview editor's
//! "every control lags on a very large capture" report. Delete this module and its
//! call sites once the ticket is settled (they are all tagged `DRAGON-366`).
//!
//! # Why this exists
//! The lag only reproduces on the owner's machine with a 5120×2880 capture, and the candidate
//! causes need opposite fixes, so guessing is expensive. Two survive:
//!
//! * **Hypothesis 1 — the base image**, sampled down ~2.2× every frame from a fragmented,
//!   mip-less atlas entry: a cost proportional to the SOURCE resolution, paid on every frame,
//!   completely independent of which tool is active — so it would be visible even with an
//!   EMPTY scene. (Measured offscreen on the owner's GPU class at 0.115 ms/frame, which makes
//!   it unlikely; but that was a replica, not the live app.)
//! * **Hypothesis 2 — the effects pipeline** ([`super::annotation_fx`]), whose ping-pong
//!   accumulators live at SOURCE resolution and which runs a full-res seed plus one full-res
//!   pass per effect item every frame. It only exists once the scene holds a highlight /
//!   pixelate / blur or a non-zero dim. (Measured offscreen: the seed is cheap, but the
//!   box-mean inner loop costs ~2.3 ms per megapixel of BLUR area and up to ~1.7 ms per
//!   megapixel of PIXELATE area at the maximum block — so a large blur on a 5K capture is
//!   tens of ms per frame.)
//!
//! The decisive evidence is therefore **which INTERACTION is on screen when a frame is slow**,
//! and whether any effect exists at that moment. Both are on every line below.
//!
//! # What is logged
//! Three independent channels, each separately sampled and capped:
//!
//! * `frame` — one line per sampled view build of the still-image editor: the **gap** since
//!   the previous build (the editor's real redraw cadence — this is what "laggy" means), the
//!   **build** time (CPU spent in our own view code), the **still** time (the `still_media`
//!   sub-build, where the per-item fx scan and the content-aware pixelate analysis live), the
//!   **interaction** (`idle`, `create/blur`, `drag/text`, `resize/box`, `type/text`, …), and
//!   whether any effect is live (`fx=none` versus the per-kind counts).
//! * `fx` — one line per sampled effects-shader prepare: CPU time, the accumulator resolution,
//!   the item and pass counts, and whether the base texture actually re-uploaded.
//! * `layers` — one line per sampled `LayerStack` prepare: CPU time and, per slot, the raster
//!   dimensions and whether this frame re-uploaded them (the text layer lives here).
//! * `gesture` — one line per sampled pointer-motion event handled by `App::annot_gesture_to`:
//!   the UPDATE-path cost of that single event, the gap since the previous event (i.e. the
//!   pointer's real event rate), what the text layer did about it (`proxy` = the DRAGON-367/368
//!   live-transform fast path re-placed the existing raster, with the accumulated scale it is
//!   being shown at; `raster` = it was re-rendered, with the render's own time and the raster's
//!   pixel dimensions), and how many motion events have arrived since the last view build.
//!
//! # The blind spot the `gesture` channel closes (DRAGON-367)
//! The first three channels are all emitted from the VIEW path. The text drag's cost is not
//! there: `refresh_text_display` runs in `update`, once per motion EVENT, and a high-polling
//! mouse delivers events far faster than frames — so a few ms of resvg per event becomes a
//! hundreds-of-ms event-queue backlog that the view path only ever sees as a large `gap` with a
//! small `build`. That is exactly the shape the owner reported ("it would lock in place and then
//! teleport"), and no view-side channel could ever have named it.
//!
//! The two numbers that settle it are on every `gesture` line: `handled` (what one event costs
//! the update path) and `evts-since-frame` (how far behind the queue is). A fixed drag shows
//! `handled` in the tens of microseconds, `text=proxy`, and `evts-since-frame` in the low
//! single digits however large the caption's type is. DRAGON-368 extends that to RESIZE: with
//! the size ladder gone every resize event changes `size_px`, so a resize that shows `text=proxy
//! x1.00x…` rather than `text=RASTERED` is the live-transform proxy doing its job.
//!
//! # Sampling, and why `idle` is never suppressed
//! The owner cannot deliberately sit idle — they have to draw something to test anything. The
//! frames BETWEEN their actions (after the preview opens, after creating a box before the drag
//! starts, after a drag ends) are therefore the only baseline hypothesis 1 ever gets, so:
//!
//! * the **first frame of every new interaction is ALWAYS logged**, whatever the sampling
//!   interval — which also means the first idle frame after each gesture ends is always
//!   captured, and a two-frame drag can never produce zero lines;
//! * `idle` is sampled more sparsely afterwards (it can repeat indefinitely) but never off.
//!
//! Caps are **per interaction verb**, not global, so a long "drag it in circles" cannot starve
//! the later blur test of its lines. (Each capture is its own process under the one-shot model,
//! so the small-capture and large-capture runs get fresh caps regardless.)
//!
//! # Logging discipline (the DRAGON-361 precedent)
//! At `warn` on purpose: `main`'s env_logger defaults to the `warn` filter, so these land with
//! no `RUST_LOG` set. Every channel is HARD-CAPPED; the last permitted line of each says so, so
//! a truncated log can never be mistaken for silence. Each line also carries the running MAX of
//! the timings, so a sampled line can never under-report the worst frame.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// How many `frame` lines each interaction VERB may emit before that verb goes quiet.
const VERB_CAP: u32 = 60;
/// Emit one `frame` line per this many consecutive frames of a GESTURE (a brief drag must
/// still produce lines; the first frame of any interaction is logged unconditionally anyway).
const GESTURE_EVERY: u32 = 3;
/// The same, for `idle` — which can repeat indefinitely, so it is sampled more sparsely. Its
/// FIRST frame after every interaction is still always logged.
const IDLE_EVERY: u32 = 20;
/// How many `fx` lines one process may emit before going quiet.
const FX_CAP: u32 = 100;
/// Emit one `fx` line per this many effects-shader prepares.
const FX_EVERY: u32 = 4;
/// How many `layers` lines one process may emit before going quiet.
const LAYERS_CAP: u32 = 80;
/// Emit one `layers` line per this many `LayerStack` prepares.
const LAYERS_EVERY: u32 = 6;
/// How many `gesture` lines each interaction VERB may emit before that verb goes quiet. Motion
/// events arrive several times per frame, so this is the tightest cap of the four.
const MOTION_CAP: u32 = 40;
/// Emit one `gesture` line per this many motion events of the SAME verb. The first event of any
/// new verb is logged unconditionally, so a flick can never sample down to nothing.
const MOTION_EVERY: u32 = 5;

/// The tail added to the final permitted line of a channel, so a log that simply STOPS is
/// never read as "the code path went quiet".
fn cap_note(seen: u32, cap: u32, channel: &str) -> String {
    if seen + 1 == cap {
        format!(" — DRAGON-366 {channel} diagnostic cap reached, no further {channel} lines")
    } else {
        String::new()
    }
}

/// How often a verb's frames are sampled after its first one.
fn sample_interval(verb: &str) -> u32 {
    if verb == "idle" { IDLE_EVERY } else { GESTURE_EVERY }
}

// ── The `frame` channel ───────────────────────────────────────────────────────────────────

/// Everything one still-image view build says about the scene it just produced, and about what
/// the user was DOING while it was produced. Built by `App::image_loaded_view`.
pub struct FrameFacts {
    /// What the user is doing: `"idle"`, `"create"` (rubber-band drawing a new item),
    /// `"drag"` (moving an existing one), `"resize"`, `"move-many"`, `"erase"`, `"type"`.
    /// This is the cap + sampling key, so a long drag cannot starve a later interaction.
    pub verb: &'static str,
    /// WHICH item kind the verb applies to (`"box"`, `"blur"`, `"text"`, …), or `"-"` when the
    /// verb has no single subject. Text is called out because it carries an extra raster layer.
    pub item: &'static str,
    /// Wall time spent building the whole view tree (CPU, our code), in ms.
    pub build_ms: f64,
    /// Wall time spent inside `still_media` specifically (the fx-item scan, the content-aware
    /// pixelate analysis, and the layer assembly), in ms.
    pub still_ms: f64,
    /// The capture's SOURCE pixel dims — the quantity both hypotheses scale with.
    pub source: (u32, u32),
    /// The fitted on-screen picture size (logical points) the base is drawn at.
    pub shown: (f32, f32),
    /// The viewport the fit was computed against.
    pub avail: (f32, f32),
    /// The view zoom.
    pub zoom: f32,
    /// `true` for the layer-shell OVERLAY preview (always full-output), `false` for the CSD
    /// window (whose surface size tracks the capture, so it multiplies every per-frame cost).
    pub overlay: bool,
    /// How many annotation items are in the scene at all.
    pub annots: usize,
    /// Region-effect counts — the ONLY thing that turns the effects pipeline on. `(0, 0, 0)`
    /// with `dim == 0` means `fx=none`: hypothesis 2 is not even instantiated on this frame.
    pub fx_blur: usize,
    pub fx_pixelate: usize,
    pub fx_highlight: usize,
    /// The global dim amount. Non-zero ALSO instantiates the effects shader.
    pub dim: f32,
    /// A covermark raster layer is mounted this frame.
    pub covermark: bool,
    /// A text raster layer is mounted this frame.
    pub text_layer: bool,
}

/// Rolling state for the `frame` channel.
struct FrameState {
    last: Option<Instant>,
    /// The verb of the previous frame — a change means a NEW interaction, always logged.
    last_verb: Option<&'static str>,
    /// Frames seen so far within the current interaction.
    in_verb: u32,
    /// Emitted-line count per verb (the per-interaction cap).
    per_verb: Vec<(&'static str, u32)>,
    max_gap: f64,
    max_build: f64,
    max_still: f64,
}

static FRAME: Mutex<FrameState> = Mutex::new(FrameState {
    last: None,
    last_verb: None,
    in_verb: 0,
    per_verb: Vec::new(),
    max_gap: 0.0,
    max_build: 0.0,
    max_still: 0.0,
});

/// Record one still-image view build and, when it is the first frame of a new interaction or a
/// sampled one, log it.
///
/// The GAP is the headline number: the interval between consecutive view builds, which during a
/// continuous drag IS the editor's frame period. Comparing it against `build` answers the one
/// structural question — if the gap is large while `build` stays small, the time is being spent
/// DOWNSTREAM of our view code (renderer, GPU or compositor) and no app-side work will fix it;
/// if `build` tracks the gap, the cost is ours and `still` says how much of it is the effects
/// and layer assembly.
pub fn view_built(facts: FrameFacts) {
    let now = Instant::now();
    // DRAGON-367: how many pointer-motion events the update path absorbed to produce this
    // frame. Swapped (not just read) so each frame reports its OWN events, and reset even on a
    // sampled-out frame so a line can never over-count.
    let evts = EVENTS_SINCE_FRAME.swap(0, Ordering::Relaxed);
    let Ok(mut st) = FRAME.lock() else { return };
    let gap_ms = st.last.map(|t| (now - t).as_secs_f64() * 1000.0).unwrap_or(0.0);
    st.last = Some(now);
    // A gap over a second means the editor was simply IDLE between redraws, not slow; folding
    // that into the running max would make every later line unreadable.
    if gap_ms < 1000.0 {
        st.max_gap = st.max_gap.max(gap_ms);
    }
    st.max_build = st.max_build.max(facts.build_ms);
    st.max_still = st.max_still.max(facts.still_ms);

    // A change of verb starts a new interaction: always logged, whatever the sampling says.
    // This is also what guarantees the first IDLE frame after every gesture is captured — the
    // only baseline hypothesis 1 ever gets, since the owner cannot deliberately test idle.
    let first = st.last_verb != Some(facts.verb);
    if first {
        st.last_verb = Some(facts.verb);
        st.in_verb = 0;
    }
    let n = st.in_verb;
    st.in_verb += 1;
    if !first && n % sample_interval(facts.verb) != 0 {
        return;
    }
    let slot = match st.per_verb.iter().position(|(v, _)| *v == facts.verb) {
        Some(i) => i,
        None => {
            st.per_verb.push((facts.verb, 0));
            st.per_verb.len() - 1
        }
    };
    let seen = st.per_verb[slot].1;
    if seen >= VERB_CAP {
        return;
    }
    st.per_verb[slot].1 += 1;
    let (max_gap, max_build, max_still) = (st.max_gap, st.max_build, st.max_still);
    drop(st);

    // `fx=none` is the load-bearing token: it says hypothesis 2 is not instantiated on this
    // frame, so anything slow here belongs to hypothesis 1 or to something not yet identified.
    let fx = if facts.fx_blur + facts.fx_pixelate + facts.fx_highlight == 0 && facts.dim == 0.0 {
        "none".to_string()
    } else {
        format!(
            "blur{} pix{} hl{} dim{:.2}",
            facts.fx_blur, facts.fx_pixelate, facts.fx_highlight, facts.dim
        )
    };
    log::warn!(
        "DRAGON-366 frame [{}/{}] #{seen}{}: gap {gap_ms:.1}ms (max {max_gap:.1}) build \
         {:.2}ms (max {max_build:.2}) still {:.2}ms (max {max_still:.2}) motion-evts {evts} | \
         src {}x{} shown {:.0}x{:.0} avail {:.0}x{:.0} zoom {:.2} surface={} | annots {} \
         fx={fx} cm={} textlayer={}{}",
        facts.verb,
        facts.item,
        if first { " FIRST" } else { "" },
        facts.build_ms,
        facts.still_ms,
        facts.source.0,
        facts.source.1,
        facts.shown.0,
        facts.shown.1,
        facts.avail.0,
        facts.avail.1,
        facts.zoom,
        if facts.overlay { "overlay" } else { "window" },
        facts.annots,
        if facts.covermark { "yes" } else { "no" },
        if facts.text_layer { "yes" } else { "no" },
        cap_note(seen, VERB_CAP, "frame"),
    );
}

// ── The `gesture` channel (DRAGON-367) ────────────────────────────────────────────────────

/// Pointer-motion events handled since the last view build. `gesture_handled` bumps it and
/// `view_built` swaps it back to zero, so each `frame` line reports how many events the update
/// path absorbed to produce it — the direct measure of whether the pointer's queue is keeping
/// up. One or two is healthy; a number that climbs is a backlog forming.
static EVENTS_SINCE_FRAME: AtomicU32 = AtomicU32::new(0);

/// Rolling state for the `gesture` channel, shaped like [`FrameState`] so a long drag of one
/// kind cannot starve a later interaction of its lines.
struct GestureState {
    last: Option<Instant>,
    last_verb: Option<&'static str>,
    in_verb: u32,
    per_verb: Vec<(&'static str, u32)>,
    max_ms: f64,
    max_raster_ms: f64,
}

static GESTURE: Mutex<GestureState> = Mutex::new(GestureState {
    last: None,
    last_verb: None,
    in_verb: 0,
    per_verb: Vec::new(),
    max_ms: 0.0,
    max_raster_ms: 0.0,
});

/// A scoped timer over ONE motion event's update-path handling. Constructed at the top of
/// `App::annot_gesture_to` and logged on drop, so every early return is timed too.
///
/// It stays silent until [`GestureTimer::note`] names the gesture: the handler's first two
/// early returns (no such preview, no gesture in flight) are not motion events being handled at
/// all, and logging them would dilute the channel with noise.
pub struct GestureTimer {
    t0: Instant,
    verb: Option<&'static str>,
    item: &'static str,
    /// What the text layer did: `"-"` (no text involved), `"proxy"` (DRAGON-367/368 re-placed
    /// the existing raster) or `"raster"` (a full re-render, timed in `raster_ms`).
    outcome: &'static str,
    /// The proxy's accumulated scale (raster's drawing → what is on screen). `1.0` is a pure
    /// move; anything else is a resize riding the live-transform proxy, and how far it is from
    /// 1.0 is how soft the caption currently looks.
    proxy_scale: f32,
    raster_ms: f64,
    /// The text raster's pixel dimensions — the quantity a re-render's cost scales with, and
    /// therefore the one that must be shown to correlate cost against the type size.
    raster_px: (u32, u32),
}

impl GestureTimer {
    /// Start timing. Cheap enough to sit unconditionally at the top of the handler.
    pub fn start() -> Self {
        Self {
            t0: Instant::now(),
            verb: None,
            item: "-",
            outcome: "-",
            proxy_scale: 1.0,
            raster_ms: 0.0,
            raster_px: (0, 0),
        }
    }

    /// Name the gesture this event belongs to; also what ARMS the line (see the struct doc).
    pub fn note(&mut self, verb: &'static str, item: &'static str) {
        self.verb = Some(verb);
        self.item = item;
    }

    /// The DRAGON-367/368 fast path took it: the raster was re-placed (moved, and possibly
    /// scaled by `scale`), not re-rendered.
    pub fn proxied(&mut self, scale: f32) {
        self.outcome = "proxy";
        self.proxy_scale = scale;
    }

    /// The text layer was fully re-rendered, costing `ms` for a `px` raster.
    pub fn rastered(&mut self, ms: f64, px: (u32, u32)) {
        self.outcome = "raster";
        self.raster_ms = ms;
        self.raster_px = px;
    }
}

impl Drop for GestureTimer {
    fn drop(&mut self) {
        let Some(verb) = self.verb else { return };
        gesture_handled(
            verb,
            self.item,
            self.t0.elapsed().as_secs_f64() * 1000.0,
            self.outcome,
            self.proxy_scale,
            self.raster_ms,
            self.raster_px,
        );
    }
}

/// Record one handled motion event and, when it opens a new interaction or is a sampled one,
/// log it. `handled_ms` is the WHOLE update-path cost of that single event.
fn gesture_handled(
    verb: &'static str,
    item: &'static str,
    handled_ms: f64,
    outcome: &'static str,
    proxy_scale: f32,
    raster_ms: f64,
    raster_px: (u32, u32),
) {
    let now = Instant::now();
    let backlog = EVENTS_SINCE_FRAME.fetch_add(1, Ordering::Relaxed) + 1;
    let Ok(mut st) = GESTURE.lock() else { return };
    let gap_ms = st.last.map(|t| (now - t).as_secs_f64() * 1000.0).unwrap_or(0.0);
    st.last = Some(now);
    st.max_ms = st.max_ms.max(handled_ms);
    st.max_raster_ms = st.max_raster_ms.max(raster_ms);
    let first = st.last_verb != Some(verb);
    if first {
        st.last_verb = Some(verb);
        st.in_verb = 0;
    }
    let n = st.in_verb;
    st.in_verb += 1;
    if !first && !n.is_multiple_of(MOTION_EVERY) {
        return;
    }
    let slot = match st.per_verb.iter().position(|(v, _)| *v == verb) {
        Some(i) => i,
        None => {
            st.per_verb.push((verb, 0));
            st.per_verb.len() - 1
        }
    };
    let seen = st.per_verb[slot].1;
    if seen >= MOTION_CAP {
        return;
    }
    st.per_verb[slot].1 += 1;
    let (max_ms, max_raster_ms) = (st.max_ms, st.max_raster_ms);
    drop(st);

    // The raster clause is only meaningful once a text layer is involved, so it stays off the
    // line entirely for the shape/effect gestures that never touch one.
    let raster = match outcome {
        "proxy" => format!(" | text=proxy x{proxy_scale:.3} (no re-render)"),
        "raster" => format!(
            " | text=RASTERED {raster_ms:.2}ms (max {max_raster_ms:.2}) {}x{}",
            raster_px.0, raster_px.1
        ),
        _ => String::new(),
    };
    log::warn!(
        "DRAGON-366 gesture [{verb}/{item}] #{seen}{}: handled {handled_ms:.3}ms (max \
         {max_ms:.3}) | event gap {gap_ms:.1}ms evts-since-frame {backlog}{raster}{}",
        if first { " FIRST" } else { "" },
        cap_note(seen, MOTION_CAP, "gesture"),
    );
}

// ── The `fx` channel ──────────────────────────────────────────────────────────────────────

/// Rolling state for the `fx` channel.
struct FxState {
    seen: u32,
    calls: u32,
    max_ms: f64,
}

static FX: Mutex<FxState> = Mutex::new(FxState { seen: 0, calls: 0, max_ms: 0.0 });

/// Record one effects-shader prepare (`annotation_fx`) and, on a sampled call, log it.
///
/// `accum` is the ping-pong accumulator resolution — SOURCE-sized by design, so on a 5K capture
/// each pass moves ~59 MB. `passes` is how many offscreen passes the plan will replay (seed +
/// one per item, blur counting `blur_passes` each). `uploaded` says whether the base texture
/// actually re-uploaded this frame: it should be `false` on every frame after the first, and a
/// `true` on every frame would be a bug in its own right.
///
/// NOTE this times the CPU-side prepare only. The passes themselves are replayed later, on the
/// GPU, and their cost does NOT appear here — it shows up as a larger `gap` on the `frame`
/// channel. That pairing is deliberate: a small `fx` prepare next to a large `frame` gap with
/// `fx=blur…` is exactly the signature of hypothesis 2.
pub fn fx_prepared(ms: f64, accum: (u32, u32), items: usize, passes: u32, uploaded: bool) {
    let Ok(mut st) = FX.lock() else { return };
    st.max_ms = st.max_ms.max(ms);
    let n = st.calls;
    st.calls += 1;
    if n % FX_EVERY != 0 {
        return;
    }
    let seen = st.seen;
    if seen >= FX_CAP {
        return;
    }
    st.seen += 1;
    let max_ms = st.max_ms;
    drop(st);
    let mb = (accum.0 as f64 * accum.1 as f64 * 4.0) / (1024.0 * 1024.0);
    log::warn!(
        "DRAGON-366 fx #{seen}: prepare {ms:.2}ms (max {max_ms:.2}) | accum {}x{} ({mb:.1}MB \
         per pass) items {items} passes {passes} base-uploaded={uploaded}{}",
        accum.0,
        accum.1,
        cap_note(seen, FX_CAP, "fx"),
    );
}

// ── The `layers` channel ──────────────────────────────────────────────────────────────────

/// Rolling state for the `layers` channel.
struct LayersState {
    seen: u32,
    calls: u32,
    max_ms: f64,
}

static LAYERS: Mutex<LayersState> = Mutex::new(LayersState { seen: 0, calls: 0, max_ms: 0.0 });

/// Record one `LayerStack` prepare and, on a sampled call, log it. `uploads` is one entry per
/// layer: `(slot, width, height, re_uploaded)` — slot 0 = video/base, 1 = covermark, 2 = text.
///
/// A layer that re-uploads on EVERY frame is paying its full raster cost per frame, which is
/// exactly the shape DRAGON-362 fixed for text (by rastering only the caption REGION at display
/// scale). This is the guard that it stayed fixed: during the owner's "drag the text box in
/// circles" test, `slot2` should show a SMALL raster and only occasional `UPLOADED` marks. A
/// source-sized slot2, or `UPLOADED` on every line, is a regression.
pub fn layers_prepared(ms: f64, uploads: &[(u32, u32, u32, bool)]) {
    let Ok(mut st) = LAYERS.lock() else { return };
    st.max_ms = st.max_ms.max(ms);
    let n = st.calls;
    st.calls += 1;
    if n % LAYERS_EVERY != 0 {
        return;
    }
    let seen = st.seen;
    if seen >= LAYERS_CAP {
        return;
    }
    st.seen += 1;
    let max_ms = st.max_ms;
    drop(st);
    let detail = uploads
        .iter()
        .map(|(slot, w, h, up)| {
            format!("slot{slot}={w}x{h}{}", if *up { " UPLOADED" } else { "" })
        })
        .collect::<Vec<_>>()
        .join(" ");
    log::warn!(
        "DRAGON-366 layers #{seen}: prepare {ms:.2}ms (max {max_ms:.2}) | {} layer(s) [{detail}]{}",
        uploads.len(),
        cap_note(seen, LAYERS_CAP, "layers"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap note must appear on exactly the LAST permitted line of a channel — that is the
    /// whole point of it (a log that simply stops must never read as "the path went quiet").
    #[test]
    fn cap_note_lands_only_on_the_final_permitted_line() {
        assert!(cap_note(0, 3, "frame").is_empty());
        assert!(cap_note(1, 3, "frame").is_empty());
        assert!(cap_note(2, 3, "frame").contains("cap reached"));
        // Past the cap the caller has already returned, but the arithmetic must not wrap or
        // re-announce.
        assert!(cap_note(3, 3, "frame").is_empty());
    }

    /// The channel name is interpolated so each cap announcement says WHICH stream ended.
    #[test]
    fn cap_note_names_its_channel() {
        assert!(cap_note(99, 100, "fx").contains("DRAGON-366 fx diagnostic cap reached"));
        assert!(cap_note(79, 80, "layers").contains("DRAGON-366 layers diagnostic cap reached"));
    }

    /// `idle` repeats indefinitely between the owner's actions, so it is sampled sparsely — but
    /// a GESTURE may only last a handful of frames and must not sample down to nothing.
    #[test]
    fn gestures_are_sampled_far_more_densely_than_idle() {
        assert_eq!(sample_interval("idle"), IDLE_EVERY);
        for verb in ["create", "drag", "resize", "move-many", "erase", "type"] {
            assert_eq!(sample_interval(verb), GESTURE_EVERY);
            assert!(
                sample_interval(verb) < sample_interval("idle"),
                "a short gesture must not be sampled as sparsely as idle"
            );
        }
    }

    /// The sampling rule the log's readability depends on: with the FIRST frame of every
    /// interaction always emitted, even a two-frame gesture yields a line — so no interaction
    /// the owner performs can vanish from the log entirely.
    #[test]
    fn the_first_frame_of_an_interaction_always_logs_however_brief() {
        // `view_built` logs when `first || n % interval == 0`; model that predicate here.
        let logs = |first: bool, n: u32, verb: &str| first || n.is_multiple_of(sample_interval(verb));
        assert!(logs(true, 0, "create"), "the first frame of a gesture always logs");
        assert!(logs(true, 0, "idle"), "the first idle frame after a gesture always logs");
        // A two-frame drag: frame 0 is FIRST, so at least one line exists.
        assert!(logs(true, 0, "drag"));
        assert!(!logs(false, 1, "drag"), "frame 1 of a drag is sampled out (interval 3)");
        assert!(logs(false, 3, "drag"), "…and frame 3 comes back");
    }
}
