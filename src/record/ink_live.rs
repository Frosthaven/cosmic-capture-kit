//! Compositing live annotation ink onto the CAPTURED FRAME (DRAGON-724).
//!
//! The sibling of [`crate::record::ink_bake`], and the one that should normally run. That one
//! replays the strokes over the FINISHED video in the finalize pass; this one paints them onto
//! each frame on its way to the encoder, which is what every other capture mode on every
//! platform already gets for free (the compositor puts our ink surface in the frame, because
//! the ink surface is on the screen being captured).
//!
//! # Why the bake was not enough
//!
//! Two costs, both measured on one 3 minute 36 second inked window take (macOS, Apple Silicon,
//! VideoToolbox):
//!
//! * **The marks led the cursor that drew them.** The ink was a separate stream married to the
//!   video after the fact, over two clocks, so the two could drift. Ink painted onto the frame
//!   it belongs to has nothing left to drift against, which is why this ticket removes the
//!   CLASS of defect rather than one instance of it.
//! * **The pass was slow, and tuning it was nearly exhausted.** The overlay forces a full video
//!   re-encode (a filtered stream cannot be stream-copied): 28.2s of spinner for that take,
//!   about 7.7x realtime on the HARDWARE tier after DRAGON-716 had already taken the cheap win.
//!   A ten minute take would have been roughly eighty seconds of spinner.
//!
//! With the composite live, the finalize pass goes back to stream-copying its video, so an
//! inked take stops and delivers in the same time an un-inked one does.
//!
//! # What this costs instead, and what pays for it
//!
//! It puts per-frame image work on the throughput-critical capture path, which DRAGON-712,
//! DRAGON-713, DRAGON-717 and DRAGON-721 spent a whole batch protecting. Three things keep
//! that honest, and none of them may be removed without re-measuring:
//!
//! 1. **A SIZE GATE**, [`live_ink_affordable`]. Above it the painter refuses the session and
//!    the finalize bake still runs, unchanged. The gate is pure and unit-tested, so the whole
//!    truth table is provable on any host.
//! 2. **The raster is cached per PUBLISHED SCENE, not per frame.** The app republishes the
//!    scene whenever it changes; between two publishes every tick composites the same cached
//!    layer, so a still drawing costs a blend and no rasterization at all.
//! 3. **The layer is only as big as the ink.** [`layer_bounds`] takes the bounding box of the
//!    visible strokes, so the blend is proportional to what the user drew and not to the frame.
//!    A tick with nothing on screen returns the captured bytes UNTOUCHED, which is what keeps
//!    the promise that a recording with no ink is byte-identical to before.
//!
//! Nothing here waits, so DRAGON-118 gains no new bound to nest: [`LiveInk::snapshot`] takes the
//! lock, clones one `Arc` and drops it, and every step after that is arithmetic over memory the
//! worker owns. The lock is never held across the rasterize, and never across any I/O at all. It
//! is also poison-tolerant on both sides, so a panicking UI thread costs the recording its ink
//! rather than the recording.
//!
//! **Measured** on an idle M-series Mac, 2026-08-19, per call to [`LiveInkPainter::paint`],
//! against a tick budget of 33.3ms at 30fps, with a scribble spanning 80% of the frame's width
//! and half its height (a deliberately bad case; a real annotation's box is smaller):
//!
//! | frame | plain frame copy | no ink | ink sitting still | drawing, re-rastered every tick |
//! | -- | -- | -- | -- | -- |
//! | 2560x1440 | 0.372ms | 0.0000ms | 1.233ms | 4.463ms |
//! | 3840x2160 | 0.854ms | 0.0000ms | 2.889ms | 7.701ms |
//!
//! So the gate's ceiling costs 23% of a tick in the worst case and the shipped size 13%, while a
//! session with no ink costs nothing that a clock can see. Note the rasterize rate is capped by
//! the TICK rate however fast the hand moves: the app republishes per pointer sample, but the
//! painter rasterizes at most once per `paint`, i.e. once per frame written.
//!
//! End to end on the same box, `debug test mac-rec-bench largest 20 videotoolbox 30 0` with and
//! without the `ink` word, 2048x1330 at 30fps, read off the WORKER's own
//! `rec/sck video summary` line and not off the harness (see below):
//!
//! | run | achieved unique | received | backlog_dropped | refed | our_copy med |
//! | -- | -- | -- | -- | -- | -- |
//! | control | 28.5fps | 570 | 0 | 30 | 0.5ms |
//! | control | 28.6fps | 568 | 0 | 28 | 1.5ms |
//! | inked | 28.5fps | 569 | 0 | 30 | 1.5ms |
//!
//! The inked run sits inside the two controls' spread on every column. Content, verified by
//! pixels rather than by shape: 600 of 600 samples along the synthetic stroke's path are
//! ink-coloured in the inked take against 0 of 600 in the un-inked one.
//!
//! **Do NOT read that A/B off the harness's own `DISTINCT frames ... % of configured` line on
//! macOS. It is saturated there and always says 100%** (DRAGON-729):
//! `cli::diagnostics::distinct_frame_count` counts runs of identical consecutive `framemd5`
//! hashes, and VideoToolbox does not emit a bit-exact output frame for a repeated input, so no
//! two consecutive hashes ever match. The runs above show it directly: the worker RE-FED 30 and
//! 28 frames respectively, and the harness still called every frame distinct (583 of 583, 585 of
//! 585). The metric is honest on Linux, which is where DRAGON-717 used it to measure 9.3 against
//! 30, so the caveat is VideoToolbox's and not the harness's in general.
//!
//! # Which workers build a painter, and which deliberately do not (DRAGON-736)
//!
//! The question is never "which operating system": it is "does THIS session's capture already
//! contain our ink surface". Four raw-frame workers, three answers:
//!
//! * **macOS SCK** and **Windows WGC** build one. A window recording there reads the target
//!   window's own buffer, always, because each platform has exactly one implementation of it.
//! * **The Linux CPU PORTAL worker** (`record::pipewire`) builds one. An xdg ScreenCast WINDOW
//!   stream is the toplevel's own buffer, so its frames carry none of our surfaces either.
//! * **The Linux NATIVE screencopy worker** (`record::screencopy`) builds NONE, and must not.
//!   It crops a rectangle of the SCREEN, so the compositor has already composited our ink into
//!   the frame; painting again would draw every stroke twice.
//! * **The Linux zero-copy workers** (`record::zero_copy`) build none either, and the pipewire
//!   one REFUSES on purpose rather than by omission: its frames are GPU dmabufs that go
//!   straight into VAAPI and are never mapped by the CPU, so a per-tick composite would mean a
//!   readback, which is the entire cost that path exists to avoid. It takes the retained
//!   [`crate::record::ink_bake`] instead, which is exactly what that fallback is for.
//!
//! One consequence worth stating, because it makes the Linux CPU portal worker's drop rule
//! differ from the other two: that worker also serves BRONZE (DRAGON-693), and the app
//! publishes NO live scene for that tier. So there, [`LiveInkPainter::active`] (the size gate
//! admitted the session) is NOT the same statement as "the ink is in the frames", and only
//! [`LiveInkPainter::painted`] is. See the drop site in `record::pipewire`.
//!
//! # The POINTER, and why it rides its own slot (DRAGON-739)
//!
//! The owner reported it as "it looks like i only lose the cursor while drawing", and the
//! mechanism is not ours: cosmic-comp composites the embedded cursor into a TOPLEVEL capture
//! only while pointer FOCUS is on that toplevel, and our ink surface's input region is the
//! output minus the toolbar for exactly as long as a drawing tool is armed. So arming a tool
//! takes the cursor out of the file, and disarming puts it back. niri has the identical rule
//! and the identical symptom, and KDE has been focus-based deliberately since 2024, so the
//! answer every screen recorder reaches in the end is to draw the pointer yourself.
//!
//! Which is cheap here, because a sprite is one more element in a blend this path already
//! does. Four things keep it from undoing the work above:
//!
//! 1. **It is NOT part of the published scene, and that is the whole design.** The raster
//!    cache in [`LiveInkPainter::refresh_layer`] is keyed on the scene `Arc`, so a pointer
//!    carried inside [`LiveInkFrame`] would mint a new scene on every mouse sample and
//!    re-rasterize the entire drawing on every tick, which is precisely the cost the cache
//!    exists to avoid. The pointer rides its own slot on [`LiveInk`], written by the app with
//!    no allocation, and it is composited as a SECOND small blend with its own bounds.
//! 2. **It never accumulates.** The frame is copied to scratch first and both blends land on
//!    the copy, so the CFR contract's re-feed of an unchanged frame draws the same picture
//!    rather than a second cursor. A stroke that got this wrong would go solid; a cursor that
//!    got it wrong would leave a TRAIL down the screen, which is why the test that pins it
//!    re-feeds the same frame and compares the two outputs byte for byte.
//! 3. **Two people never draw it.** [`live_cursor_needed`] is the pure gate, and
//!    `platform::WindowCursorSource` is the platform fact it reads: Windows Graphics Capture
//!    composites the pointer by GEOMETRY and already over-includes it, so a second one there
//!    would be a doubled cursor, and macOS is assumed geometric on the safe side because no
//!    mac was reachable to settle it. Only a focus-gated platform draws.
//! 4. **The user's "Preserve mouse cursor" setting wins.** A recording made with the pointer
//!    deliberately left out (`CursorRequest::Omit`, which asks the portal for
//!    `CursorMode::Hidden`) must not gain one from us. That term is in the same gate.
//!
//! ## What is drawn, and why it is an ARROW (DRAGON-744)
//!
//! An arrow, ours, vector, [`POINTER_HEIGHT_PT`] points tall, white with a dark halo.
//!
//! **It used to be a crosshair, and the reason given for it was wrong.** DRAGON-739 argued the
//! shape was ours by construction, because `widgets::ink_canvas` asked for
//! `mouse::Interaction::Crosshair` while armed. The owner tested it and reported "i dont see a
//! crosshair when im drawing mid recording", so the file was showing a shape the screen never
//! did. `ink_canvas::requested_interaction` carries the whole mechanism and the dead ends; what
//! matters here is the outcome, which is that the canvas now asks for the DEFAULT and this
//! paints the arrow that answers to.
//!
//! **Why an arrow is the right guess, mechanically.** Nothing here is a guess about the THEME,
//! only about the shape, and three facts pin it on the platforms this gate draws for:
//!
//! * The ink canvas asks for `Interaction::None`, which is also the initial value
//!   `iced_winit`'s `Window` stores, and it issues a cursor request only when that value
//!   CHANGES. So this surface makes no request at all: it cannot be the thing that put a shape
//!   on the seat.
//! * The last of our windows to have issued one, by the time the pointer settles on the canvas,
//!   is the capture overlay carrying the recording toolbar. With nothing hovered its resolved
//!   interaction is `Interaction::None` too, which `iced_winit::conversion::mouse_interaction`
//!   maps to `CursorIcon::Default`, the arrow.
//! * libcosmic's own `pointer_frame` then re-asserts that seat icon on every `Enter` (its
//!   `active_icon != icon` arm), so entering this surface re-issues the arrow. If cosmic-comp
//!   drops that request inside its post-enter window (DRAGON-331), the shape already on screen
//!   is the same arrow. Both roads end in the same place, which is exactly what the crosshair
//!   could not manage.
//!
//! What is NOT reproduced is the user's own cursor THEME. This is the ordinary seven-corner
//! left-pointing arrow, so the shape is right and the artwork is ours. Three sources of the
//! real bitmap were considered and all three are worse:
//!
//! * `App::frozen_cursor` holds a real sprite, but it is the LAUNCH-time pointer, whatever
//!   shape the pointer happened to have when the app started. It is an arrow most of the time
//!   and an I-beam or a resize handle the rest, so it trades a known approximation for an
//!   unknown one. (DRAGON-739 recorded this as a dead end because the shape was wrong for a
//!   crosshair. It stays a dead end for a different reason: the shape is not RELIABLE.)
//! * `screenshot::capture_cursor()` could grab the live one, but it opens its own Wayland
//!   capture session, it races the arm-time focus change, and its result would have to be
//!   rescaled into video pixels. A whole capture path, on the capture thread, for artwork.
//! * Loading the XCursor theme ourselves means a theme lookup, a size negotiation and an
//!   animated-cursor format, in the record worker, for the same cosmetic tie.
//!
//! The POSITION is exact either way, and the position is the thing the recording was missing.
//!
//! ## The variant that looks like the real fix, and the seat that kills it
//!
//! Hide the system cursor over the ink surface and draw our own crosshair INSIDE the canvas,
//! then paint the same vector into the file. Screen and video would agree by construction,
//! because we would be drawing both. It is the obvious next idea, it uses machinery this repo
//! already owns, and it does not work:
//!
//! * The hide is SEAT-GLOBAL. Our iced fork's `hidden` is a field on `SctkSeat`, not on a
//!   surface, and `SctkSeat::set_cursor` early-returns while it is set. So hiding the pointer
//!   for the canvas hides it over the recording TOOLBAR as well, which is a separate layer
//!   surface a user has to aim at to pause, change colour or disarm.
//! * Nothing would put it back there. `Window::update_mouse` calls `set_cursor_visible(true)`
//!   only when leaving `Interaction::Hidden`, and the overlay's window never held that value,
//!   so it has no transition to make.
//!
//! That is a worse defect than the one being fixed: an invisible pointer over the controls,
//! rather than a recorded one that does not match.
//!
//! # Time: the two clocks never meet, and that is the point
//!
//! The bake has to answer "what was on screen at media time t" for every frame of the video, so
//! it depends on the app's media clock agreeing with the worker's. **When this ticket was
//! written it did not**, and the diagnosis measured the gap: the worker takes media 0 at
//! `owned::media_zero`, immediately before the pump spawns, while the app latched
//! `recording_started` on `settled_at`, which is media 0 PLUS the whole opening phase. On the
//! owner's session that was 489.8ms, against a measured 500ms of ink leading the cursor in the
//! delivered file, median of 21 samples. The bake's own compositing was index-correct; only the
//! log's stamps were shifted.
//!
//! **DRAGON-725 closed that gap**, in the same v0.34.0 batch: `recording_started` now latches
//! `RecordHandle::media_zero_at`, which the worker fills inside `owned::media_zero` itself, so
//! the app's clock and the file's are one clock and the bake's stamps land where the bake's
//! indices do. The paragraphs below are therefore no longer a workaround for a live defect.
//! They are the reason this path could never acquire one, which is worth keeping for the same
//! reason the measurement is: an anchor that was wrong twice can be wrong again.
//!
//! This path asks a different question: "what is on screen NOW", for the frame being fed to the
//! encoder now. The app publishes the scene together with the instant IT read its own clock at
//! ([`LiveInkFrame::media_ms`]), and the worker replays exactly that instant. Nothing here
//! converts between the two clocks, and nothing extrapolates forward.
//!
//! **That is what made this path immune to the anchor bug rather than merely hiding it, and it
//! is still load-bearing: do not "tidy" it into asking the worker's tick for a media time.**
//! Every
//! quantity the replay uses is a DIFFERENCE of two app-clock values, so a constant offset in
//! that clock cancels exactly: `visible_at` compares a stroke's stamps against the query
//! instant, `gen_idle_ms` subtracts a generation's last activity from it, and
//! `InkBake::rect_at` picks the newest rect sample at or before it. Shift the whole app clock
//! and the picture is bit-identical, which `offset_invariance_tests` below pins. The worker's
//! tick number appears nowhere.
//!
//! The retained fallback bake still does not get that immunity: above the size gate it replays
//! the log against the VIDEO's frame indices, so it is only ever as right as the anchor it
//! reads. It is right today, because DRAGON-725 gave it the file's own anchor, and the two
//! paths therefore draw the same picture at the same instant. The end state is what it always
//! had to be: ONE shared anchor, not a correction bolted onto either compositor.
//!
//! The one approximation left is the smoothing tail, and it is a fidelity IMPROVEMENT rather
//! than a cost. `crate::pen_stroke`'s fit is causal, so the last
//! [`crate::pen_stroke::SETTLE_TAIL_POINTS`] raw samples behind the pointer are still in flux
//! and everything before them is final. A live composite therefore records the un-settled tail
//! the user was actually watching, where the bake redrew every frame with the FINAL curve,
//! including the frames where that curve did not exist yet. The stretch that can differ is
//! bounded by the same budget `pen_stroke`'s own settling test pins: about 45 surface points of
//! arc, roughly a fifteenth of a second of drawing, and under the 0.6 point decimation
//! tolerance for most of it.
//!
//! # Dead ends, so nobody re-runs them
//!
//! * **Sharing the app's `InkLog` behind one mutex** instead of publishing snapshots. The app
//!   reads that log from its view function, so the worker holding it for a rasterize would
//!   stall the UI thread, and the UI holding it would stall the capture loop. A snapshot the
//!   worker takes by `Arc` clone costs one pointer copy under the lock.
//! * **Compositing in place into the captured frame.** The capture loop re-feeds the freshest
//!   frame whenever a tick has nothing newer of its own (the CFR contract), so a frame written
//!   twice would accumulate two ticks of ink. The painter keeps its own scratch copy for that
//!   reason, and the copy is why the size gate exists.
//! * **Baking only the inked SPANS at finalize** (the alternative this ticket rejected). Cuts
//!   must land on keyframes, the segments need identical codec parameters, and Gold-tier ink
//!   STAYS up rather than dissolving, so one mark that survives to the end makes everything
//!   after the first stroke inked and the saving collapses.

use std::sync::{Arc, Mutex};

/// The per-second PIXEL budget one session may spend painting ink onto its frames.
///
/// The composite's cost is dominated by two full-frame-order passes, the scratch copy and the
/// blend, so "frame area times frame rate" is the quantity that decides whether it fits. The
/// number is set from the two cases the ticket measured: the owner's shipped 2560x1440 at 30fps
/// has headroom (a frame copy there measured at a median of 0.5ms), and a 6400x3600 native
/// Studio Display frame is 92MB, where per-tick image work would take the capture loop apart.
///
/// It admits 4K UHD at 30fps and refuses 4K at 60, which is the honest place to draw the line:
/// above it the recording is already spending everything it has on the encoder.
pub(crate) const LIVE_INK_PIXELS_PER_SEC: u64 = 260_000_000;

const _: () = assert!(
    3840 * 2160 * 30 <= LIVE_INK_PIXELS_PER_SEC,
    "the live ink budget must admit 4K UHD at 30fps, or the common high-resolution window \
     recording silently falls back to the slow finalize bake this ticket exists to retire"
);

const _: () = assert!(
    6400 * 3600 * 30 > LIVE_INK_PIXELS_PER_SEC,
    "the live ink budget must refuse a 6400x3600 native frame, whose 92MB per tick would put \
     the capture loop behind the screen (DRAGON-168 measured that footprint capping the loop \
     at a fraction of an fps)"
);

/// **Pure**, unit-tested: may this session paint its ink onto the captured frames, or must it
/// fall back to the finalize bake?
///
/// The whole of the size gate. Kept in the shared tree deliberately, out of both platform
/// plugins, because the decision has to be provable on the Linux gate like every other rule of
/// its kind.
// It was honestly dead on Linux until DRAGON-736: the painter's production callers were the
// macOS SCK and Windows WGC workers only. The Linux CPU PORTAL worker builds one too now, so
// nothing in this file is Linux-dead any more and the twelve `allow(dead_code)` gates that
// said so are gone. A dead-code warning from here on ANY platform is a real finding.
pub(crate) fn live_ink_affordable(w: u32, h: u32, fps: u32) -> bool {
    let per_sec = (w as u64)
        .saturating_mul(h as u64)
        .saturating_mul(fps.max(1) as u64);
    per_sec > 0 && per_sec <= LIVE_INK_PIXELS_PER_SEC
}

/// **Pure**, unit-tested: must WE draw the pointer into this session's frames?
///
/// The double-cursor gate, and every one of the five terms has to hold at once:
///
/// * `window_buffer_capture`: this session records ONE window and the capture reads that
///   window's OWN buffer, so no surface of ours is in it. That is the first disjunct of
///   `ink::needs_composite`, i.e. the DRAGON-736 backend question and not a platform one. A
///   recording of a SCREEN rectangle already has the compositor's own cursor in it, exactly
///   as it has our ink, and a second one would be a doubled cursor.
/// * `draws_on_the_live_screen`: our drawing surface is over the recorded window on the real
///   screen, which is Gold and Silver. **BRONZE fails this term and must**, which is the whole
///   reason it is a separate term rather than folded into the one above: `needs_composite` is
///   true for Bronze as well, by its OTHER disjunct. Bronze draws on a FROZEN STILL of the
///   stream, in a window of ours, so the user's pointer is over our own window and the
///   recorded window genuinely has no pointer in it and never did. There is nothing missing
///   there to restore.
/// * `cursor_wanted`: the effective "Preserve mouse cursor" extra. A recording the user asked
///   to have no pointer in (`CursorRequest::Omit`, which asks the portal for
///   `CursorMode::Hidden`) must not get one from us. Skipping this term would be the same
///   defect as the one being fixed, only in reverse.
/// * `armed`: a DRAWING tool holds the pointer. With no tool armed the ink surface's input
///   region is empty, focus is back on the recorded window, and the compositor is drawing the
///   cursor itself.
/// * `source`: the platform composites the pointer by pointer FOCUS. On a platform that
///   composites it by GEOMETRY the cursor never left, so drawing one would DOUBLE it. See
///   `platform::WindowCursorSource` for what puts each platform on which side, and for the
///   macOS assumption.
///
/// Pure, and out of every plugin, so the whole truth table (the macOS and Windows rows above
/// all) is proven on the Linux gate.
pub(crate) fn live_cursor_needed(
    window_buffer_capture: bool,
    draws_on_the_live_screen: bool,
    cursor_wanted: bool,
    armed: bool,
    source: crate::platform::WindowCursorSource,
) -> bool {
    window_buffer_capture
        && draws_on_the_live_screen
        && cursor_wanted
        && armed
        && matches!(source, crate::platform::WindowCursorSource::PointerFocus)
}

/// **Pure**, unit-tested: does the captured frame carry its red and blue channels the other way
/// round from the ink raster's?
///
/// tiny-skia always hands back RGBA. The frame is whatever the capture backend delivers, which
/// the encode plan already names for ffmpeg's rawvideo input: macOS feeds ScreenCaptureKit's
/// native BGRA straight through (DRAGON-316), Windows swizzles to RGBA on its own copy, and the
/// Linux PipeWire consumer converts to RGBA before it hands a frame over, so every Linux plan
/// says `rgba` too. The
/// NV12 plans ignore `input_pix_fmt` entirely and their buffer is RGBA, which this answers
/// correctly because no plan sets `bgra` and `nv12` together.
pub(crate) fn frame_swaps_rb(input_pix_fmt: &str) -> bool {
    input_pix_fmt == "bgra"
}

/// One published scene: everything the painter needs to draw one instant of ink.
///
/// The `InkBake` inside is the SAME structure the finalize bake reads, on purpose: one mapping
/// (`ink_bake::point_to_video`), one replay (`ink::fill_runs_at`), so the live composite and the
/// fallback bake cannot draw different pictures.
// Built on every platform (the app publishes without knowing whether a painter exists) and READ
// only by `LiveInkPainter`, which since DRAGON-736 the Linux CPU portal worker builds too.
pub(crate) struct LiveInkFrame {
    /// The strokes, the window's rect timeline, and the ink colour.
    pub bake: crate::record::ink_bake::InkBake,
    /// The media instant this scene was taken at. The painter replays exactly this and never
    /// extrapolates past it; see the module doc's section on time.
    pub media_ms: crate::ink::MediaMs,
}

/// The app-to-worker seam: the live annotation scene, republished whenever it changes.
///
/// One slot holding one `Arc`, so the worker's read is a pointer clone under a lock it holds for
/// nanoseconds. The app rebuilds and replaces; the worker never blocks on the app and the app
/// never blocks on the capture loop.
#[derive(Clone)]
pub(crate) struct LiveInk {
    slot: Arc<Mutex<Option<Arc<LiveInkFrame>>>>,
    /// Where the pointer is, in the SAME ink-surface points the strokes are recorded in, or
    /// `None` when we must not draw one (DRAGON-739).
    ///
    /// **Deliberately a SECOND slot rather than a field of [`LiveInkFrame`].** The painter's
    /// raster cache is keyed on the scene `Arc`, and the pointer moves every tick by
    /// definition, so folding it into the scene would re-rasterize the whole drawing on every
    /// frame written. Here it costs a lock, two floats and no allocation on either side, and
    /// the scene's cache never learns the pointer exists.
    ///
    /// The app writes `None` for every session and every instant [`live_cursor_needed`]
    /// refuses, so the whole policy is decided once, on the UI side, and the worker only ever
    /// draws what it is handed.
    cursor: Arc<Mutex<Option<(f32, f32)>>>,
}

impl LiveInk {
    pub(crate) fn new() -> Self {
        Self { slot: Arc::new(Mutex::new(None)), cursor: Arc::new(Mutex::new(None)) }
    }

    /// App side: where the pointer is, or `None` to draw none at all.
    ///
    /// Carries NO timestamp, and that is the strongest possible form of this module's clock
    /// rule: the pointer is where it is NOW, painted onto the frame being written NOW, so
    /// there is no instant to convert between the two clocks in the first place.
    pub(crate) fn publish_cursor(&self, at: Option<(f32, f32)>) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = at;
        }
    }

    /// Worker side: where the pointer is, if the app says one should be drawn.
    pub(crate) fn cursor(&self) -> Option<(f32, f32)> {
        self.cursor.lock().ok().and_then(|g| *g)
    }

    /// App side: publish the scene as it stands at `media_ms`.
    ///
    /// Overwrites whatever was there, which is right: the worker wants the current scene and a
    /// stale one it never read is of no use to it. Poison-tolerant, like every other slot here:
    /// a lock this drops costs the recording one frame of ink, never the recording.
    pub(crate) fn publish(&self, bake: crate::record::ink_bake::InkBake, media_ms: crate::ink::MediaMs) {
        if let Ok(mut g) = self.slot.lock() {
            *g = Some(Arc::new(LiveInkFrame { bake, media_ms }));
        }
    }

    /// Worker side: the current scene, if the app has published one.
    pub(crate) fn snapshot(&self) -> Option<Arc<LiveInkFrame>> {
        self.slot.lock().ok().and_then(|g| g.clone())
    }

    /// Worker side: is there anything to draw AT ALL as of right now, scene or pointer?
    ///
    /// Cheaper than [`Self::snapshot`] (it clones no `Arc`) and asked more often: the capture
    /// loops consult it once per WAIT to decide whether to sample per media tick (DRAGON-751),
    /// where the painter reads the slots once per written tick.
    ///
    /// Both slots, because either one alone changes what a tick writes: the pointer moves every
    /// tick by definition, and a session can carry one before the first stroke exists (the app
    /// starts publishing at ARM time, DRAGON-739).
    pub(crate) fn has_content(&self) -> bool {
        self.slot.lock().is_ok_and(|g| g.is_some())
            || self.cursor.lock().is_ok_and(|g| g.is_some())
    }
}

impl Default for LiveInk {
    fn default() -> Self {
        Self::new()
    }
}

/// How far outside the ink's own bounds the layer reaches, in frame pixels.
///
/// One pixel, for the antialiased edge tiny-skia paints just past the geometry. Without it a
/// stroke would come out with a hairline of its own outline shaved off along the bounding box.
const AA_MARGIN: f32 = 1.0;

/// One rasterized instant of ink: where it sits in the frame, and its pixels.
///
/// PREMULTIPLIED RGBA, unlike [`crate::record::ink_bake::rasterize`]'s output, which demultiplies
/// because ffmpeg's `rgba` input is straight alpha. Here we do the compositing ourselves, and
/// source-over wants the premultiplied form tiny-skia produces natively, so the demultiply (and
/// the rounding it costs a half-faded stroke) is skipped entirely.
pub(crate) struct InkLayer {
    /// The layer's left edge in frame pixels.
    pub x: u32,
    /// The layer's top edge in frame pixels.
    pub y: u32,
    /// The layer's width in pixels.
    pub w: u32,
    /// The layer's height in pixels.
    pub h: u32,
    /// `w * h * 4` bytes of premultiplied RGBA.
    pub px: Vec<u8>,
}

/// **Pure**, unit-tested: the frame rectangle a set of fill runs touches, clamped to the frame
/// and grown by [`AA_MARGIN`].
///
/// This is what keeps the per-frame cost proportional to the DRAWING rather than to the frame:
/// a signature in one corner of a 4K window rasterizes and blends a few hundred kilobytes, not
/// thirty-three megabytes. `None` when the ink misses the frame entirely, which is the same
/// "bake only the intersection" rule [`crate::record::ink_bake::touches_video`] states for the
/// finalize path.
pub(crate) fn layer_bounds(
    runs: &[crate::ink::InkFillRun],
    frame: (u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let (mut lo_x, mut lo_y) = (f32::MAX, f32::MAX);
    let (mut hi_x, mut hi_y) = (f32::MIN, f32::MIN);
    let mut any = false;
    for (_, polys) in runs {
        for poly in polys {
            for p in poly {
                if !p.0.is_finite() || !p.1.is_finite() {
                    continue;
                }
                lo_x = lo_x.min(p.0);
                hi_x = hi_x.max(p.0);
                lo_y = lo_y.min(p.1);
                hi_y = hi_y.max(p.1);
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let x0 = (lo_x - AA_MARGIN).floor().clamp(0.0, fw);
    let y0 = (lo_y - AA_MARGIN).floor().clamp(0.0, fh);
    let x1 = (hi_x + AA_MARGIN).ceil().clamp(0.0, fw);
    let y1 = (hi_y + AA_MARGIN).ceil().clamp(0.0, fh);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// Rasterize the scene at its own media instant into a layer covering only the ink.
///
/// The mapping, the replay and the fill rule are [`crate::record::ink_bake`]'s, so this and the
/// fallback bake draw the same picture from the same data. `None` whenever there is nothing to
/// draw: no window rect, a degenerate rect, no visible ink, or ink entirely off the frame.
pub(crate) fn rasterize_layer(
    bake: &crate::record::ink_bake::InkBake,
    t_ms: crate::ink::MediaMs,
    frame: (u32, u32),
) -> Option<InkLayer> {
    use resvg::tiny_skia as sk;
    if frame.0 == 0 || frame.1 == 0 {
        return None;
    }
    let rect = bake.rect_at(t_ms)?;
    if rect.2 <= 0 || rect.3 <= 0 {
        return None;
    }
    let map = crate::record::ink_bake::point_to_video(bake.origin, bake.point_scale, rect, frame);
    let scale = crate::record::ink_bake::video_scale(rect, frame);
    let runs = crate::ink::fill_runs_at(&bake.log, t_ms, map, scale);
    if runs.is_empty() {
        return None;
    }
    let (x, y, w, h) = layer_bounds(&runs, frame)?;
    let mut pixmap = sk::Pixmap::new(w, h)?;
    // The layer's own origin is the frame rectangle it covers, so the geometry is translated
    // back by it rather than rasterized over the whole frame and cropped.
    let tf = sk::Transform::from_translate(-(x as f32), -(y as f32));
    let mut drew = false;
    for (alpha, polys) in &runs {
        let mut pb = sk::PathBuilder::new();
        let mut any = false;
        for poly in polys {
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
        let a = (bake.color[3] as f32 / 255.0 * alpha).clamp(0.0, 1.0);
        paint.set_color(sk::Color::from_rgba8(
            bake.color[0],
            bake.color[1],
            bake.color[2],
            (a * 255.0).round() as u8,
        ));
        // Non-zero winding, the contract `pen_stroke::stroke_fill_polygons` builds for: a
        // scribble that crosses itself unions instead of cancelling into holes.
        pixmap.fill_path(&path, &paint, sk::FillRule::Winding, tf, None);
        drew = true;
    }
    if !drew {
        return None;
    }
    Some(InkLayer { x, y, w, h, px: pixmap.take() })
}

/// The arrow's TIP-to-TAIL height, in ink-surface POINTS.
///
/// Twenty-two, which is what the crosshair this replaced spanned (DRAGON-744) and close to what
/// a themed cursor measures, so the recorded pointer keeps the same visual weight it had and is
/// still small enough not to hide the mark being drawn under it.
pub(crate) const POINTER_HEIGHT_PT: f32 = 22.0;
/// The dark halo drawn under the white body, PER SIDE, in points. It is what makes the arrow
/// legible over a white document as well as over a dark one, and it is the crosshair's own
/// `POINTER_EDGE_PT` unchanged, so the recorded cursor reads as strongly as it used to.
///
/// The STROKE is twice this, because a stroke straddles the path it is drawn on: the inner half
/// falls under the white fill and only the outer half is ever seen. Getting that wrong is easy
/// and silent, which is why the constant is named for the visible half rather than for the
/// stroke, and why [`the_halo_is_a_full_point_outside_the_body`] measures it in the raster.
const POINTER_HALO_PT: f32 = 1.0;
/// The smallest and largest HEIGHT the sprite is ever rasterized at, in VIDEO pixels.
///
/// A bound rather than a preference. The span the sprite is scaled by comes from a window rect
/// the platform reported, and a window caught mid-close reports sizes that make the scale
/// enormous; without the ceiling that would allocate a sprite by the megabyte on the capture
/// thread. The floor keeps a heavily downscaled recording's cursor from vanishing into two
/// pixels. The pair is the crosshair's own 5..64 HALF-extent restated as a full height, so the
/// on-screen bounds are unchanged.
const POINTER_MIN_EXT_PX: f32 = 10.0;
const POINTER_MAX_EXT_PX: f32 = 128.0;

/// The arrow outline, in units of the sprite's own height, tip first and wound clockwise.
///
/// The ordinary seven-corner left-pointing arrow: a tip at the origin, the left edge straight
/// down, the notch, the tail, and back along the head's lower right. Written as a table rather
/// than as drawing calls so the SHAPE is one readable thing and the rasterizer is a loop.
const POINTER_SHAPE: [(f32, f32); 7] = [
    (0.0000, 0.0000),
    (0.0000, 0.8421),
    (0.2105, 0.6526),
    (0.3684, 1.0000),
    (0.5053, 0.9368),
    (0.3474, 0.6105),
    (0.5789, 0.6105),
];
/// The arrow's width, in the same units: the widest x in [`POINTER_SHAPE`].
///
/// DERIVED rather than written down. It sizes the pixmap, so a hand-copied number that fell
/// behind an edit to the table above would crop the arrow down its right side, quietly and
/// only for whoever changed the shape.
const POINTER_SHAPE_W: f32 = shape_width();

/// The widest x in [`POINTER_SHAPE`], at compile time.
const fn shape_width() -> f32 {
    let mut w = 0.0;
    let mut i = 0;
    while i < POINTER_SHAPE.len() {
        if POINTER_SHAPE[i].0 > w {
            w = POINTER_SHAPE[i].0;
        }
        i += 1;
    }
    w
}

const _: () = assert!(
    POINTER_HALO_PT > 0.0 && POINTER_HEIGHT_PT > 8.0 * POINTER_HALO_PT,
    "the arrow must be several times taller than its own outline is wide, or the sprite \
     rasterizes as a dark blob and the recording gains a smudge where its cursor should be \
     (DRAGON-739, reshaped in DRAGON-744)"
);
const _: () = assert!(
    POINTER_SHAPE.len() >= 3 && POINTER_SHAPE_W > 0.0,
    "an arrow needs at least three corners and some width, or the sprite is a line or nothing \
     (DRAGON-744)"
);

/// One scene's screen-to-video PLACEMENT: `(output origin, capture units per point, the
/// recorded window's rect)`.
///
/// Exactly the three inputs `ink_bake::point_to_video` takes beside the frame size, kept as a
/// value rather than as the closure they build, because the closure borrows nothing and is
/// cheap to rebuild while a `Fn` cannot be stored in a struct without a box.
type ScenePlace = ((i32, i32), f32, (i32, i32, i32, i32));

/// One rasterized pointer sprite: premultiplied RGBA, with the hotspot it is placed by.
///
/// Separate from [`InkLayer`] on purpose. A layer carries an absolute frame rectangle computed
/// from the ink it holds; a sprite carries no position at all, because it is drawn wherever the
/// pointer is this tick and is cached across every one of them.
pub(crate) struct PointerSprite {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// The pixel inside the sprite that sits ON the pointer's position.
    pub hot: (i32, i32),
    /// `w * h * 4` bytes of premultiplied RGBA.
    pub px: Vec<u8>,
}

/// **Pure**, unit-tested: how many VIDEO pixels one ink-surface POINT is worth.
///
/// Two factors, and both are needed. `point_scale` takes a surface point into the platform's
/// CAPTURE space (it is 2.0 on a HiDPI output), and `ink_bake::video_scale` takes capture space
/// into the video's own pixels. Multiplying them is exactly the uniform scale
/// `ink_bake::point_to_video` carries, which is what makes the drawn cursor the same size on
/// screen as it is in the file.
pub(crate) fn point_span_px(
    point_scale: f32,
    rect: (i32, i32, i32, i32),
    video: (u32, u32),
) -> f32 {
    let s = if point_scale.is_finite() && point_scale > 0.0 { point_scale } else { 1.0 };
    let span = s * crate::record::ink_bake::video_scale(rect, video);
    if span.is_finite() && span > 0.0 { span } else { 0.0 }
}

/// **Pure**, unit-tested: the sprite's HEIGHT in VIDEO pixels for a given point span.
///
/// Split out from the rasterize so the CACHE KEY is one integer that can be compared without
/// drawing anything: the sprite is re-rasterized only when this number moves, which for a
/// window that is not being resized is never.
pub(crate) fn pointer_extent_px(span: f32) -> Option<u32> {
    if !span.is_finite() || span <= 0.0 {
        return None;
    }
    let ext = (POINTER_HEIGHT_PT * span).clamp(POINTER_MIN_EXT_PX, POINTER_MAX_EXT_PX);
    Some(ext.round() as u32)
}

/// Rasterize the arrow at `ext` pixels tall, scaled for a point worth `span` pixels.
///
/// A white body over a dark halo: ONE closed path drawn twice, stroked and then filled, so the
/// dark can only ever be a rim around the body and can never darken it. The fill is non-zero
/// for the same reason the ink's is. Premultiplied RGBA, like [`InkLayer`], because we do the
/// compositing ourselves.
///
/// The hotspot is the arrow's TIP, not its centre. That is the whole difference in placement
/// from the crosshair this replaced, and `pointer_spot` reads it off the sprite, so nothing
/// else had to change.
pub(crate) fn rasterize_pointer(ext: u32, span: f32) -> Option<PointerSprite> {
    use resvg::tiny_skia as sk;
    if ext == 0 || !span.is_finite() || span <= 0.0 {
        return None;
    }
    let h = ext as f32;
    // The stroke straddles the path, so the visible halo is HALF of it: doubling here is what
    // makes `POINTER_HALO_PT` mean the number of points a reader sees.
    let halo = (POINTER_HALO_PT * span).max(1.0);
    let stroke_w = halo * 2.0;
    // How far the ink can reach outside the path: the stroke's outer half, and at the TIP the
    // mitre carries it further, up to `miter_limit` (tiny-skia's default 4) times the stroke
    // width, halved because the limit is on the miter's full diagonal. Padding by that BOUND
    // plus a pixel for the antialiased fringe is a couple of pixels more than the tip actually
    // needs, which is the right trade: a bound cannot be invalidated by an edit to the shape
    // table, and the cost is a few hundred bytes on a sprite rasterized once per session.
    let pad = (stroke_w * 2.0).ceil() as u32 + 1;
    let body_w = (h * POINTER_SHAPE_W).ceil() as u32;
    let (sw, sh) = (body_w.saturating_add(2 * pad), ext.saturating_add(2 * pad));
    let mut pixmap = sk::Pixmap::new(sw, sh)?;
    let o = pad as f32;
    let mut pb = sk::PathBuilder::new();
    for (i, (px, py)) in POINTER_SHAPE.iter().enumerate() {
        let (x, y) = (o + px * h, o + py * h);
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    let path = pb.finish()?;
    let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
    paint.set_color(sk::Color::from_rgba8(0, 0, 0, 217));
    let stroke = sk::Stroke { width: stroke_w, ..Default::default() };
    pixmap.stroke_path(&path, &paint, &stroke, sk::Transform::identity(), None);
    paint.set_color(sk::Color::WHITE);
    pixmap.fill_path(&path, &paint, sk::FillRule::Winding, sk::Transform::identity(), None);
    Some(PointerSprite {
        w: sw,
        h: sh,
        hot: (pad as i32, pad as i32),
        px: pixmap.take(),
    })
}

/// **Pure**, unit-tested: where the sprite's TOP-LEFT goes in the frame, or `None` when it
/// would miss the frame entirely.
///
/// The `None` matters as much as the placement: a pointer outside the recorded window is the
/// normal case, because the ink surface covers the whole output while the video is one window,
/// and a tick with nothing to draw has to hand the CAPTURED bytes straight back rather than
/// copy a frame to blend nothing onto it.
pub(crate) fn pointer_spot(
    at: (f32, f32),
    sprite: (u32, u32),
    hot: (i32, i32),
    frame: (u32, u32),
) -> Option<(i32, i32)> {
    if !at.0.is_finite() || !at.1.is_finite() {
        return None;
    }
    // Clamp before casting: an `f32` past `i32::MAX` is an undefined cast in C and a saturating
    // one in Rust, and relying on the difference is not worth the reader's time.
    let x = at.0.round().clamp(-1.0e9, 1.0e9) as i32 - hot.0;
    let y = at.1.round().clamp(-1.0e9, 1.0e9) as i32 - hot.1;
    let (sw, sh) = (sprite.0 as i32, sprite.1 as i32);
    if sw <= 0 || sh <= 0 {
        return None;
    }
    if x + sw <= 0 || y + sh <= 0 || x >= frame.0 as i32 || y >= frame.1 as i32 {
        return None;
    }
    Some((x, y))
}

/// **Pure**, unit-tested: `round(a * b / 255)` without a division.
///
/// The standard exact form. A real `/ 255` per channel per pixel is measurable at frame sizes,
/// and the naive `>> 8` is not the same number: it darkens every blended edge by up to one
/// level, which on a half-faded magic stroke is visible as a dirty outline.
pub(crate) fn mul255(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}

/// **Pure**, unit-tested: source-over `layer` onto `frame`, in place.
///
/// `frame` is the captured frame's bytes at `fw` x `fh`, four bytes per pixel, in whatever
/// channel order the capture backend delivers; `swap_rb` says which (see [`frame_swaps_rb`]).
/// The layer is premultiplied RGBA, so the blend is the cheap `src + dst * (1 - a)` form with no
/// per-pixel divide, and a fully transparent source pixel is skipped outright, which is most of
/// any layer's bounding box.
///
/// Out-of-range geometry is CLIPPED rather than rejected: a layer whose rectangle was computed
/// against a different frame size can only ever draw less, never past the end of the buffer.
pub(crate) fn composite_over(frame: &mut [u8], fw: u32, fh: u32, layer: &InkLayer, swap_rb: bool) {
    composite_px(
        frame,
        fw,
        fh,
        (layer.x as i32, layer.y as i32),
        (layer.w, layer.h),
        &layer.px,
        swap_rb,
    );
}

/// **Pure**, unit-tested: source-over a premultiplied RGBA rectangle onto `frame`, in place, at
/// a SIGNED position.
///
/// The body [`composite_over`] used to be, with one thing added: the source may start at a
/// negative coordinate and is clipped on all FOUR sides rather than two. The ink layer never
/// needs that, because [`layer_bounds`] clamps its box into the frame before rasterizing. The
/// POINTER sprite does (DRAGON-739): it is placed by its own HOTSPOT, which since DRAGON-744
/// is the arrow's tip, so a pointer within the sprite's padding of the top or left edge lands
/// with a negative origin, and a pointer off the recorded window entirely, which is most of
/// where the ink surface reaches, lands
/// wholly outside. Rejecting those instead of clipping them would cut the cursor off at the
/// window edge, which is exactly where a user drawing an arrow at something puts it.
pub(crate) fn composite_px(
    frame: &mut [u8],
    fw: u32,
    fh: u32,
    at: (i32, i32),
    size: (u32, u32),
    px: &[u8],
    swap_rb: bool,
) {
    let (fw, fh) = (fw as usize, fh as usize);
    let (lw, lh) = (size.0 as usize, size.1 as usize);
    if lw == 0 || lh == 0 || px.len() < lw * lh * 4 || frame.len() < fw * fh * 4 {
        return;
    }
    // The part of the sprite that is inside the frame, in SPRITE coordinates.
    let skip_x = at.0.max(0).saturating_sub(at.0) as usize;
    let skip_y = at.1.max(0).saturating_sub(at.1) as usize;
    if skip_x >= lw || skip_y >= lh {
        return;
    }
    let (lx, ly) = (at.0.max(0) as usize, at.1.max(0) as usize);
    if lx >= fw || ly >= fh {
        return;
    }
    // Where the source's R and B channels land in the destination.
    let (dst_r, dst_b) = if swap_rb { (2usize, 0usize) } else { (0usize, 2usize) };
    let cols = (lw - skip_x).min(fw - lx);
    for row in 0..(lh - skip_y).min(fh - ly) {
        let sbase = ((row + skip_y) * lw + skip_x) * 4;
        let src = &px[sbase..sbase + cols * 4];
        let base = ((ly + row) * fw + lx) * 4;
        let dst = &mut frame[base..base + cols * 4];
        for i in 0..cols {
            let s = &src[i * 4..i * 4 + 4];
            let a = s[3];
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            let d = &mut dst[i * 4..i * 4 + 4];
            d[dst_r] = s[0].saturating_add(mul255(d[dst_r], inv));
            d[1] = s[1].saturating_add(mul255(d[1], inv));
            d[dst_b] = s[2].saturating_add(mul255(d[dst_b], inv));
            d[3] = a.saturating_add(mul255(d[3], inv));
        }
    }
}

/// The capture loop's ink painter: one per session, held beside the frame writer.
///
/// The whole seam a worker sees. `paint` is called with the bytes about to be written and
/// answers with the bytes to write: the captured ones untouched when there is no ink, and a
/// composited scratch copy when there is. A session the size gate refused holds no scene at all
/// and `paint` is then a single `Option` test per tick.
pub(crate) struct LiveInkPainter {
    /// The published scene, or `None` when the size gate refused this session.
    ink: Option<LiveInk>,
    /// Whether the captured frame's red and blue channels are the other way round.
    swap_rb: bool,
    /// The scene the cached layer was rasterized from, for the `Arc::ptr_eq` cache test.
    cached_from: Option<Arc<LiveInkFrame>>,
    /// The frame size the cached layer was rasterized against.
    cached_size: (u32, u32),
    /// The cached layer. `None` means "this scene draws nothing", which is a cached answer too.
    layer: Option<InkLayer>,
    /// The cached scene's screen-to-video PLACEMENT: `(origin, point_scale, window rect)`.
    ///
    /// Kept beside the layer so the POINTER can be mapped through exactly the mapping the ink
    /// was, without re-reading the scene or holding a closure across ticks (DRAGON-739). It is
    /// `Some` whenever the scene names a usable window rect, INCLUDING when the drawing itself
    /// is empty: a tool armed with nothing drawn yet still has a cursor to place.
    place: Option<ScenePlace>,
    /// The cached pointer sprite and the extent it was rasterized at (DRAGON-739; the extent is
    /// the arrow's HEIGHT since DRAGON-744, and was the crosshair's arm before that). The
    /// extent is the cache key, so a pointer moving across a still window re-rasterizes
    /// nothing at all.
    pointer: Option<(u32, PointerSprite)>,
    /// The composited frame handed to the writer. Grown once, reused for the session.
    scratch: Vec<u8>,
    /// How many ticks actually carried INK, for the one summary line at stop.
    ///
    /// **Ink only, never the pointer, and the split is load-bearing rather than tidy.** The
    /// Linux CPU portal worker DROPS its fallback bake when this is non-zero (see the module
    /// doc), so a cursor-only tick counted here would read as "the drawing is already in the
    /// frames" on a session where it is not: a Bronze user who armed a tool, drew a whole
    /// annotation and never had a live scene published for them would lose the entire drawing,
    /// silently, with the video arriving intact.
    ///
    /// **Do not recombine these two counters.** One counter quietly answering two questions is
    /// the class of defect this codebase keeps paying for: DRAGON-725 (`settled_at` answering
    /// both "may STOP be offered" and "where does the file begin"), DRAGON-732, and this. The
    /// rule that falls out of all three is the same one: when a second question arrives, give
    /// it its own answer rather than widening an existing one.
    painted: u64,
    /// How many ticks carried the POINTER sprite (DRAGON-739), for the same summary line.
    ///
    /// Diagnostic only. Nothing decides anything on it, and nothing may start: see the
    /// paragraph above for what happens when these two are folded together.
    pointed: u64,
}

impl LiveInkPainter {
    /// Build the painter for a session whose frames are `w` x `h` at `fps`, fed to ffmpeg as
    /// `input_pix_fmt`.
    ///
    /// This is where the size gate is applied, ONCE, and it says so out loud: a refusal is an
    /// `info` because it changes what the user waits for at stop, and it names both numbers so
    /// the log can be read without the source.
    pub(crate) fn new(ink: LiveInk, w: u32, h: u32, fps: u32, input_pix_fmt: &str) -> Self {
        let affordable = live_ink_affordable(w, h, fps);
        if affordable {
            log::debug!(
                "ink: live annotation will be composited onto the captured frames \
                 ({w}x{h} at {fps}fps, DRAGON-724)"
            );
        } else {
            log::info!(
                "ink: {w}x{h} at {fps}fps is past the live composite's budget, so any live \
                 annotation is composited in the finalize pass instead (DRAGON-724)"
            );
        }
        Self {
            ink: affordable.then_some(ink),
            swap_rb: frame_swaps_rb(input_pix_fmt),
            cached_from: None,
            cached_size: (0, 0),
            layer: None,
            place: None,
            pointer: None,
            scratch: Vec::new(),
            painted: 0,
            pointed: 0,
        }
    }

    /// Did the size gate admit this session?
    ///
    /// On macOS and Windows the finalize bake is skipped exactly when this is true, because
    /// every session that reaches their painter is also one the app publishes a live scene for,
    /// so "admitted" and "the ink is in the frames" say the same thing there.
    ///
    /// **They do not say the same thing on the Linux CPU portal worker**, which also serves
    /// BRONZE (DRAGON-693, a tier the app publishes no live scene for), so that worker asks
    /// [`Self::painted`] instead. See the module doc's section on which workers build a
    /// painter.
    ///
    /// It is also NOT the question the capture loops' wait gate asks: see
    /// [`Self::has_content`], which records what asking this one there would have cost.
    // Honestly dead on LINUX: its one production caller is the drop decision in the macOS and
    // Windows workers, and the Linux worker asks `painted()` for the reason above. Kept as the
    // gate's own name rather than folded away, because "was this session admitted" and "did it
    // draw anything" are different questions and both are asked.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(crate) fn active(&self) -> bool {
        self.ink.is_some()
    }

    /// Is there anything for [`Self::paint`] to draw as of RIGHT NOW?
    ///
    /// The gate the change-driven capture loops bound their wait on (DRAGON-751, over
    /// `pump::VideoTicker::capture_wait`): a tick only shows something the previous tick did not
    /// when there is live content, so this is exactly when sampling per media tick is worth
    /// anything.
    ///
    /// **[`Self::active`] is the wrong question here, and it was the first answer tried.** That
    /// one says the size GATE admitted the session, which is true of every ordinary monitor or
    /// region recording, since every worker builds a painter whether the user annotates or not.
    /// Gating on it changed the pacing of every recording on the machine to buy nothing, and it
    /// made the "a session with no ink keeps today's loop" claim false while reading as though
    /// it were true. This one is false until something is published, so an un-annotated take
    /// keeps the wait it has always had, and it turns true the moment a tick could differ from
    /// the one before it.
    ///
    /// Cheap on purpose: two uncontended lock acquisitions and no allocation, asked once per
    /// loop iteration, against a `paint` that already reads both slots per written tick.
    pub(crate) fn has_content(&self) -> bool {
        self.ink.as_ref().is_some_and(LiveInk::has_content)
    }

    /// How many ticks carried ink.
    pub(crate) fn painted(&self) -> u64 {
        self.painted
    }

    /// How many ticks carried the pointer sprite (DRAGON-739). Diagnostic only; the fallback
    /// bake's drop rule reads [`Self::painted`] and must never read this.
    // Honestly dead off LINUX, and by construction rather than by omission: the app only ever
    // publishes a pointer on a focus-gated platform (`platform::WindowCursorSource`), so the
    // counter on macOS and Windows can only read zero and no worker there has a reason to
    // report it. Kept portable anyway, because the platform fact is a one-line change.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn pointed(&self) -> u64 {
        self.pointed
    }

    /// The bytes to write for this tick: `px` untouched, or a composited copy of it.
    ///
    /// Never mutates `px`. The capture loop re-feeds the freshest frame whenever a tick has
    /// nothing newer of its own, so a frame painted in place would accumulate one tick of ink
    /// per repeat.
    pub(crate) fn paint<'a>(&'a mut self, w: u32, h: u32, px: &'a [u8]) -> &'a [u8] {
        let Some(ink) = self.ink.as_ref() else { return px };
        // A short frame is the frame writer's problem to report (it drops and warns); painting
        // one would only index past the pixels it does have.
        if px.len() < (w as usize) * (h as usize) * 4 {
            return px;
        }
        let snap = ink.snapshot();
        let cursor = ink.cursor();
        self.refresh_layer(snap, w, h);
        // DRAGON-739: resolved BEFORE the copy decision, because a pointer outside the recorded
        // window is the ordinary case and a tick with nothing to draw must hand the captured
        // bytes back untouched.
        let spot = cursor.and_then(|at| self.refresh_pointer(at, w, h));
        if self.layer.is_none() && spot.is_none() {
            return px;
        }
        if self.scratch.len() != px.len() {
            self.scratch = vec![0u8; px.len()];
        }
        // ONE copy, and both blends land on it. The capture loop re-feeds the freshest frame
        // whenever a tick has nothing newer of its own, so a blend done in place would stack
        // one tick per repeat: ink would go solid, and the pointer, which moves every tick,
        // would leave a TRAIL of cursors down the screen.
        self.scratch.copy_from_slice(px);
        if let Some(layer) = self.layer.as_ref() {
            composite_over(&mut self.scratch, w, h, layer, self.swap_rb);
            self.painted += 1;
        }
        // The pointer goes on LAST, over the ink: it is the thing the user is looking at, and
        // a mark drawn under the cursor should not swallow it.
        if let (Some(at), Some((_, sprite))) = (spot, self.pointer.as_ref()) {
            composite_px(
                &mut self.scratch,
                w,
                h,
                at,
                (sprite.w, sprite.h),
                &sprite.px,
                self.swap_rb,
            );
            self.pointed += 1;
        }
        &self.scratch
    }

    /// Make sure the pointer sprite matches this scene's scale, and answer where it goes.
    ///
    /// `None` whenever nothing should be drawn: no scene has been published (so there is no
    /// honest screen-to-video mapping, the same rule the ink itself follows), the rect is
    /// degenerate, or the sprite lands wholly outside the frame.
    ///
    /// The re-rasterize is keyed on the sprite's own EXTENT, not on the pointer position, which
    /// is what keeps a moving cursor off the per-tick budget: across a still window every tick
    /// after the first compares one integer and blends a few hundred bytes.
    fn refresh_pointer(&mut self, at: (f32, f32), w: u32, h: u32) -> Option<(i32, i32)> {
        let (origin, point_scale, rect) = self.place?;
        let map = crate::record::ink_bake::point_to_video(origin, point_scale, rect, (w, h));
        let span = point_span_px(point_scale, rect, (w, h));
        let ext = pointer_extent_px(span)?;
        if self.pointer.as_ref().is_none_or(|(cached, _)| *cached != ext) {
            self.pointer = rasterize_pointer(ext, span).map(|s| (ext, s));
        }
        let (_, sprite) = self.pointer.as_ref()?;
        pointer_spot(map(at), (sprite.w, sprite.h), sprite.hot, (w, h))
    }

    /// Re-rasterize the layer, but only when the scene or the frame size has actually changed.
    ///
    /// The cache is what keeps this off the per-frame budget: the app republishes when the
    /// drawing changes, so ink that is sitting still costs one `Arc::ptr_eq` per tick and no
    /// rasterization at all.
    fn refresh_layer(&mut self, snap: Option<Arc<LiveInkFrame>>, w: u32, h: u32) {
        let same = match (&self.cached_from, &snap) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same && self.cached_size == (w, h) {
            return;
        }
        self.cached_size = (w, h);
        self.layer = snap
            .as_ref()
            .and_then(|s| rasterize_layer(&s.bake, s.media_ms, (w, h)));
        // DRAGON-739: the same scene's PLACEMENT, cached beside the layer so the pointer maps
        // through the mapping the ink did. It survives a scene whose drawing is empty, which is
        // the armed-but-nothing-drawn-yet case, and that is exactly when the cursor matters
        // most: the user has just armed the tool and is deciding where to draw.
        self.place = snap.as_ref().and_then(|s| {
            let rect = s.bake.rect_at(s.media_ms)?;
            (rect.2 > 0 && rect.3 > 0).then_some((s.bake.origin, s.bake.point_scale, rect))
        });
        self.cached_from = snap;
    }
}

#[cfg(test)]
mod size_gate_tests {
    use super::*;

    /// The two cases the ticket measured, which are what the budget was set from. The owner's
    /// shipped size is admitted and the native Studio Display footprint is refused; getting
    /// either of these backwards either loses the feature or takes the capture loop apart.
    #[test]
    fn the_measured_cases_land_on_the_right_side() {
        assert!(live_ink_affordable(2560, 1440, 30), "the owner's shipped window size");
        assert!(!live_ink_affordable(6400, 3600, 30), "a 92MB native Studio Display frame");
    }

    /// 4K UHD at 30 is the ceiling, and doubling the rate crosses it. The gate is about work
    /// per SECOND, not about pixels alone, because a 60fps session does the whole thing twice.
    #[test]
    fn the_ceiling_is_4k_at_thirty() {
        assert!(live_ink_affordable(3840, 2160, 30));
        assert!(!live_ink_affordable(3840, 2160, 60));
        assert!(live_ink_affordable(2560, 1440, 60), "and half the area at twice the rate fits");
    }

    /// A Windows ultrawide (the DRAGON-711 measurement box) stays on the live path.
    #[test]
    fn an_ultrawide_still_composites_live() {
        assert!(live_ink_affordable(5120, 1440, 30));
    }

    /// Degenerate input refuses rather than dividing by zero or overflowing into an accidental
    /// yes. A zero-sized frame has nothing to paint and a nonsense rate must not wrap.
    #[test]
    fn degenerate_sizes_refuse() {
        assert!(!live_ink_affordable(0, 1080, 30));
        assert!(!live_ink_affordable(1920, 0, 30));
        assert!(!live_ink_affordable(u32::MAX, u32::MAX, u32::MAX));
        // A zero rate is read as one, so a 1080p session is still affordable rather than free.
        assert!(live_ink_affordable(1920, 1080, 0));
    }
}

/// DRAGON-739: the double-cursor gate. Every row here is a way to get a recording wrong, and
/// two of them are ways to get a recording wrong on a platform nobody on the project can test.
#[cfg(test)]
mod cursor_gate_tests {
    use super::*;
    use crate::platform::WindowCursorSource::{Geometry, PointerFocus};

    /// The one row that draws: a window recording whose capture is the window's own buffer,
    /// drawn on over the live screen, a user who wants a pointer, a tool armed, on a platform
    /// that composites the cursor by pointer focus.
    #[test]
    fn all_five_terms_have_to_hold() {
        assert!(live_cursor_needed(true, true, true, true, PointerFocus));
    }

    /// **The rows that must NOT change.** A geometric platform is already drawing the pointer
    /// itself, so a second one is a DOUBLED cursor, which is a worse defect than the missing
    /// one this ticket fixes. Windows Graphics Capture is that platform on evidence; macOS is
    /// assumed to be on the safe side, and both are pinned here so a future "tidy" cannot flip
    /// either without this failing.
    #[test]
    fn a_geometric_platform_never_draws_a_second_cursor() {
        assert!(!live_cursor_needed(true, true, true, true, Geometry));
        // And no combination of the other four can talk it into one.
        for buffer in [false, true] {
            for live in [false, true] {
                for wanted in [false, true] {
                    for armed in [false, true] {
                        assert!(
                            !live_cursor_needed(buffer, live, wanted, armed, Geometry),
                            "geometry drew one for {buffer}/{live}/{wanted}/{armed}"
                        );
                    }
                }
            }
        }
    }

    /// The user's "Preserve mouse cursor" answer wins. A recording they asked to have no
    /// pointer in (`CursorRequest::Omit`, which asks the portal for `CursorMode::Hidden`) must
    /// not gain one from us: that is the same defect in reverse.
    #[test]
    fn a_recording_with_the_cursor_turned_off_gets_none() {
        assert!(!live_cursor_needed(true, true, false, true, PointerFocus));
    }

    /// With no tool armed the ink surface's input region is empty, pointer focus is back on
    /// the recorded window, and the compositor is drawing the cursor itself. This is the row
    /// that keeps an ordinary recording exactly as it was.
    #[test]
    fn an_unarmed_session_leaves_it_to_the_compositor() {
        assert!(!live_cursor_needed(true, true, true, false, PointerFocus));
    }

    /// A recording of a SCREEN rectangle (the Linux native screencopy path, and every region
    /// or monitor recording) already carries the compositor's cursor, exactly as it carries
    /// our ink surface. Drawing again would double both.
    #[test]
    fn a_screen_rectangle_recording_already_has_it() {
        assert!(!live_cursor_needed(false, true, true, true, PointerFocus));
    }

    /// **BRONZE**, and the reason the second term exists at all. `ink::needs_composite` is
    /// true there too, by its OTHER disjunct, so a gate written on that one fact alone would
    /// have said yes. Bronze draws on a FROZEN STILL of the stream inside a window of ours, so
    /// the user's pointer is over our window and the recorded window has no pointer in it and
    /// never did. There is nothing missing there to put back.
    #[test]
    fn bronze_draws_on_a_still_and_gets_no_cursor() {
        assert!(!live_cursor_needed(true, false, true, true, PointerFocus));
    }
}

/// DRAGON-744: the file paints the shape the SCREEN was asked for, and the two live in
/// different modules, so nothing but a test can hold them together.
///
/// This is the whole ticket in one assertion, and **a test in the painter that reads the
/// canvas's request is the only thing that could have caught the original defect, where a
/// justification in one file quietly stopped being true because of a change in another.**
/// DRAGON-739 painted a crosshair on the strength of a sentence in a doc comment, that sentence
/// stopped being true, and nothing failed: the owner found it by looking at their own screen. A
/// comment cannot notice.
#[cfg(test)]
mod recorded_shape_agreement_tests {
    use cosmic::iced::core::mouse;

    /// The ink surface asks for the DEFAULT cursor, so the sprite in this file is the arrow
    /// that answers to. Restoring any other request has to change the sprite in the same
    /// commit, and this is what says so.
    ///
    /// It cannot check that [`super::POINTER_SHAPE`] IS an arrow (a table of points has no
    /// opinion about cursor themes); `pointer_sprite_tests` does that from the raster. What it
    /// checks is the half a reader cannot see from either file alone: that the two modules
    /// still agree about which shape the session has.
    #[test]
    fn the_painted_shape_is_the_one_the_surface_asks_for() {
        for armed in [false, true] {
            assert_eq!(
                crate::widgets::ink_canvas::requested_interaction(armed),
                mouse::Interaction::default(),
                "the ink surface asked for a shape other than the default while armed={armed}, \
                 so `record::ink_live`'s arrow sprite is now painting something the screen is \
                 not showing. Change both or neither (DRAGON-744)"
            );
        }
    }
}

/// DRAGON-739: the sprite's arithmetic. All of it is pure, so the shape and the placement are
/// provable with no compositor, no window and no recording.
#[cfg(test)]
mod pointer_sprite_tests {
    use super::*;

    /// The shipped sprite's own size and hotspot, at one video pixel per point, so the
    /// placement rows below read as the real thing rather than as arbitrary numbers.
    /// `the_shipped_sprite_is_the_one_the_placement_rows_use` keeps them honest.
    const SPRITE: (u32, u32) = (23, 32);
    const HOT: (i32, i32) = (5, 5);

    /// The two constants above are the sprite the rasterizer actually produces. Without this
    /// the placement rows could keep passing against a sprite that no longer exists, which is
    /// the only way a hotspot test can be green and wrong at the same time.
    #[test]
    fn the_shipped_sprite_is_the_one_the_placement_rows_use() {
        let s = rasterize_pointer(pointer_extent_px(1.0).expect("a span of 1.0 has an extent"), 1.0)
            .expect("rasterizes");
        assert_eq!((s.w, s.h), SPRITE);
        assert_eq!(s.hot, HOT);
    }

    /// A point is worth `point_scale` capture units and each capture unit is worth
    /// `video / rect` video pixels, so the span is the product. Getting the `point_scale`
    /// factor wrong would draw a HiDPI session's cursor at half size.
    #[test]
    fn a_point_is_worth_both_scales() {
        assert_eq!(point_span_px(1.0, (0, 0, 100, 100), (100, 100)), 1.0);
        assert_eq!(point_span_px(2.0, (0, 0, 100, 100), (100, 100)), 2.0);
        // A window twice the video's size halves it; the two factors then cancel.
        assert_eq!(point_span_px(2.0, (0, 0, 200, 200), (100, 100)), 1.0);
    }

    /// Nonsense in, nothing out. A window caught mid-close reports a degenerate rect, and a
    /// span of zero or NaN must refuse rather than rasterize something enormous.
    #[test]
    fn a_degenerate_span_has_no_sprite() {
        assert_eq!(point_span_px(f32::NAN, (0, 0, 100, 100), (100, 100)), 1.0, "an unusable \
            scale falls back to 1.0, exactly as point_to_video does");
        assert_eq!(point_span_px(1.0, (0, 0, 100, 100), (0, 0)), 0.0);
        assert_eq!(pointer_extent_px(0.0), None);
        assert_eq!(pointer_extent_px(f32::NAN), None);
        assert!(rasterize_pointer(0, 1.0).is_none());
    }

    /// The extent is CLAMPED at both ends, which is what stops a wild window rect allocating a
    /// sprite by the megabyte on the capture thread, and what stops a heavily downscaled
    /// recording's cursor shrinking to nothing.
    #[test]
    fn the_extent_is_bounded_at_both_ends() {
        assert_eq!(pointer_extent_px(1.0), Some(22), "one point per pixel is the arrow's height");
        assert_eq!(pointer_extent_px(0.001), Some(POINTER_MIN_EXT_PX as u32));
        assert_eq!(pointer_extent_px(1_000.0), Some(POINTER_MAX_EXT_PX as u32));
    }

    /// The sprite is an ARROW and not a blob (DRAGON-744): the tip is on the hotspot, the body
    /// hangs down and right of it, and every corner is empty. A blob would pass a "something
    /// was drawn" test and look nothing like a cursor.
    ///
    /// The dimensions are asserted from the constants rather than as literals, because the
    /// padding is a BOUND on how far the stroke's mitre can reach and a reader should not have
    /// to re-derive it to change the halo.
    #[test]
    fn the_sprite_is_an_arrow_and_not_a_blob() {
        let s = rasterize_pointer(22, 1.0).expect("22px of arrow rasterizes");
        let pad = (POINTER_HALO_PT * 2.0 * 2.0).ceil() as u32 + 1;
        let body = (22.0 * POINTER_SHAPE_W).ceil() as u32;
        assert_eq!((s.w, s.h), (body + 2 * pad, 22 + 2 * pad), "the body plus its halo's room");
        assert_eq!(s.hot, (pad as i32, pad as i32), "the hotspot is the TIP, inside the pad");
        let (hx, hy) = (s.hot.0 as u32, s.hot.1 as u32);
        let a = |x: u32, y: u32| s.px[((y * s.w + x) * 4 + 3) as usize];
        assert!(a(hx, hy) > 0, "the tip is on the hotspot");
        assert_eq!(a(hx + 3, hy + 8), 255, "and the head is solid");
        // Every corner is empty: an arrow occupies a wedge, not the box around it. The
        // top-left one is past the TIP, which is what says the hotspot is not the corner.
        assert_eq!(a(0, 0), 0, "the corner past the tip is empty");
        assert_eq!(a(s.w - 1, 0), 0);
        assert_eq!(a(0, s.h - 1), 0);
        assert_eq!(a(s.w - 1, s.h - 1), 0);
        assert!(s.h > s.w, "an arrow is taller than it is wide; the old crosshair was square");
    }

    /// The dark halo really is [`POINTER_HALO_PT`] wide OUTSIDE the white body, which is the
    /// whole of its job: an arrow with no halo disappears into a white document, and that is
    /// what the crosshair's own dark edge was for.
    ///
    /// It is asserted in the raster because the code that produces it is easy to get silently
    /// wrong: a stroke straddles its path, so half of every stroke falls UNDER the fill and
    /// only the outer half is ever seen. The first cut of this sprite asked for a stroke of
    /// `POINTER_HALO_PT` and shipped half a point of halo.
    #[test]
    fn the_halo_is_a_full_point_outside_the_body() {
        let s = rasterize_pointer(22, 1.0).expect("rasterizes");
        let px = |x: u32, y: u32| {
            let i = ((y * s.w + x) * 4) as usize;
            (s.px[i], s.px[i + 3])
        };
        // A row well down the arrow's straight LEFT edge, walked outwards from the body. The
        // left edge runs vertically from the tip, so the halo there is a clean horizontal run.
        let y = s.hot.1 as u32 + 12;
        let x0 = s.hot.0 as u32;
        assert_eq!(px(x0 + 1, y), (255, 255), "just inside the edge is solid white body");
        // One point of halo, at span 1.0, is one pixel: dark (low red) and not transparent.
        let (r, alpha) = px(x0 - 1, y);
        assert!(alpha > 128, "the halo is missing outside the left edge: alpha {alpha}");
        assert!(r < 96, "the halo is not dark outside the left edge: red {r}");
        assert_eq!(px(x0 - 4, y), (0, 0), "and four pixels out is clear again");
    }

    /// The arrow really POINTS: the inked run is narrow at the tip and widens with depth, and
    /// every row of the head starts at the same column, because the left edge is vertical.
    ///
    /// Stronger than the corner check above, and worth having separately: a filled square, a
    /// disc and a diagonal bar all pass "the hotspot is inked and the corners are not", and
    /// none of them is a cursor.
    #[test]
    fn the_arrow_widens_away_from_its_tip() {
        let s = rasterize_pointer(22, 1.0).expect("rasterizes");
        let a = |x: u32, y: u32| s.px[((y * s.w + x) * 4 + 3) as usize];
        let run = |y: u32| (0..s.w).filter(|&x| a(x, y) > 0).count();
        let (hx, hy) = (s.hot.0 as u32, s.hot.1 as u32);
        let near = run(hy + 2);
        let far = run(hy + 11);
        assert!(near >= 2, "the row two below the tip is inked at all");
        assert!(far > near * 2, "the head must widen with depth: {near} then {far}");
        for y in (hy + 2)..(hy + 12) {
            let first = (0..s.w).find(|&x| a(x, y) > 0).expect("every row of the head is inked");
            assert!(first <= hx, "row {y} starts at {first}, right of the vertical left edge");
        }
    }

    /// The sprite is PREMULTIPLIED, like the ink layer, because we do the compositing
    /// ourselves. A straight-alpha sprite would blend far too bright at its antialiased edge.
    #[test]
    fn the_sprite_is_premultiplied() {
        let s = rasterize_pointer(22, 1.0).expect("rasterizes");
        for p in s.px.chunks_exact(4) {
            for c in &p[..3] {
                assert!(*c <= p[3], "channel {c} exceeds alpha {}", p[3]);
            }
        }
    }

    /// The sprite is placed by its HOTSPOT, so the pointer's own position is the arrow's TIP
    /// and not the sprite's top-left corner. Getting this wrong shifts every cursor down and
    /// right by the padding.
    #[test]
    fn the_hotspot_lands_on_the_pointer() {
        assert_eq!(pointer_spot((50.0, 40.0), SPRITE, HOT, (100, 100)), Some((45, 35)));
    }

    /// A pointer near the top-left places the sprite at a NEGATIVE origin, which is the whole
    /// reason `composite_px` clips on four sides. Rejecting it instead would cut the cursor off
    /// along the window's edge.
    #[test]
    fn a_pointer_near_the_edge_places_negative() {
        assert_eq!(pointer_spot((2.0, 1.0), SPRITE, HOT, (100, 100)), Some((-3, -4)));
    }

    /// A pointer outside the recorded window has no spot at all, and that answer is what keeps
    /// the promise that a tick with nothing to draw hands the captured bytes straight back. It
    /// is the ORDINARY case: the ink surface covers the whole output while the video is one
    /// window.
    #[test]
    fn a_pointer_off_the_frame_has_no_spot() {
        assert_eq!(pointer_spot((500.0, 50.0), SPRITE, HOT, (100, 100)), None);
        assert_eq!(pointer_spot((-500.0, 50.0), SPRITE, HOT, (100, 100)), None);
        assert_eq!(pointer_spot((50.0, -500.0), SPRITE, HOT, (100, 100)), None);
        // Touching by one pixel still counts, so the cursor does not blink out at the border.
        // The tip sits at -17, so the sprite's right edge is the frame's column 0 and exactly
        // one column of arrow is inside. One pixel further left is the first `None`.
        assert!(pointer_spot((-17.0, 50.0), SPRITE, HOT, (100, 100)).is_some());
        assert_eq!(pointer_spot((-18.0, 50.0), SPRITE, HOT, (100, 100)), None);
    }

    /// Broken geometry answers `None` rather than an invented position, the same rule
    /// `layer_bounds` follows.
    #[test]
    fn nonsense_has_no_spot() {
        assert_eq!(pointer_spot((f32::NAN, 1.0), SPRITE, HOT, (100, 100)), None);
        assert_eq!(pointer_spot((1.0, f32::INFINITY), SPRITE, HOT, (100, 100)), None);
        assert_eq!(pointer_spot((1.0, 1.0), (0, 0), (0, 0), (100, 100)), None);
    }
}

#[cfg(test)]
mod byte_order_tests {
    use super::*;

    /// macOS feeds ScreenCaptureKit's native BGRA straight through, so its frames need the
    /// swap; everything else is RGBA and does not.
    #[test]
    fn only_a_bgra_feed_swaps() {
        assert!(frame_swaps_rb("bgra"));
        assert!(!frame_swaps_rb("rgba"));
        // The NV12 plans ignore `input_pix_fmt` and their buffer is RGBA. No plan sets both.
        assert!(!frame_swaps_rb("nv12"));
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    fn run(pts: &[(f32, f32)]) -> Vec<crate::ink::InkFillRun> {
        vec![(1.0, vec![pts.to_vec()])]
    }

    /// The layer is the ink's own box plus one pixel of antialiasing slack, which is what keeps
    /// the blend proportional to the drawing instead of to the frame.
    #[test]
    fn the_layer_is_the_ink_plus_a_margin() {
        let b = layer_bounds(&run(&[(100.0, 200.0), (140.0, 260.0)]), (1920, 1080));
        assert_eq!(b, Some((99, 199, 42, 62)));
    }

    /// Ink that runs off the edge is clipped to the frame, never past it: the layer's rectangle
    /// is what the blend indexes with.
    #[test]
    fn the_layer_is_clamped_to_the_frame() {
        let b = layer_bounds(&run(&[(-50.0, -50.0), (30.0, 30.0)]), (100, 100))
            .expect("crosses the frame");
        assert_eq!(b, (0, 0, 31, 31));
        let b = layer_bounds(&run(&[(80.0, 80.0), (400.0, 400.0)]), (100, 100))
            .expect("crosses the frame");
        assert_eq!((b.0 + b.2, b.1 + b.3), (100, 100));
    }

    /// Ink drawn entirely beside the window has no layer at all, so the tick writes the captured
    /// bytes untouched. Same rule the finalize bake's clip states.
    #[test]
    fn ink_off_the_frame_has_no_layer() {
        assert_eq!(layer_bounds(&run(&[(500.0, 500.0), (600.0, 600.0)]), (100, 100)), None);
        assert_eq!(layer_bounds(&run(&[(-99.0, -99.0), (-50.0, -50.0)]), (100, 100)), None);
    }

    /// Nothing to draw, and broken geometry, both answer `None` rather than an invented box.
    #[test]
    fn nothing_and_nonsense_have_no_layer() {
        assert_eq!(layer_bounds(&[], (100, 100)), None);
        assert_eq!(layer_bounds(&run(&[]), (100, 100)), None);
        assert_eq!(layer_bounds(&run(&[(f32::NAN, f32::NAN)]), (100, 100)), None);
    }
}

#[cfg(test)]
mod composite_tests {
    use super::*;

    fn layer(x: u32, y: u32, w: u32, h: u32, px: Vec<u8>) -> InkLayer {
        InkLayer { x, y, w, h, px }
    }

    /// The exact rounding, against the reference division. A shift-only approximation darkens
    /// every blended edge, which reads as a dirty outline on a half-faded stroke.
    #[test]
    fn the_blend_multiply_is_exact() {
        for a in 0..=255u32 {
            for b in [0u32, 1, 63, 127, 128, 200, 254, 255] {
                let want = ((a * b) as f32 / 255.0).round() as u8;
                assert_eq!(mul255(a as u8, b as u8), want, "{a} x {b}");
            }
        }
    }

    /// An opaque source replaces the destination, at the layer's own offset and nowhere else.
    #[test]
    fn an_opaque_layer_replaces_the_pixels_under_it() {
        // A 4x4 black RGBA frame with one opaque red pixel dropped at (1, 2).
        let mut frame = vec![0u8; 4 * 4 * 4];
        for p in frame.chunks_exact_mut(4) {
            p[3] = 255;
        }
        composite_over(&mut frame, 4, 4, &layer(1, 2, 1, 1, vec![255, 0, 0, 255]), false);
        let at = |x: usize, y: usize| &frame[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(1, 2), &[255, 0, 0, 255], "the inked pixel");
        assert_eq!(at(0, 0), &[0, 0, 0, 255], "and nothing else moved");
        assert_eq!(at(2, 2), &[0, 0, 0, 255]);
    }

    /// A half-transparent source blends, and the source is PREMULTIPLIED, so a 50% red is
    /// `[128, 0, 0, 128]` rather than `[255, 0, 0, 128]`.
    #[test]
    fn a_translucent_layer_blends() {
        let mut frame = vec![0u8, 0, 255, 255]; // one opaque blue pixel, RGBA
        composite_over(&mut frame, 1, 1, &layer(0, 0, 1, 1, vec![128, 0, 0, 128]), false);
        assert_eq!(frame[0], 128, "half the red arrived");
        assert!((frame[2] as i32 - 128).abs() <= 1, "half the blue survived: {}", frame[2]);
        assert_eq!(frame[3], 255, "an opaque frame stays opaque");
    }

    /// A fully transparent layer leaves the frame BYTE-IDENTICAL, which is the promise a
    /// recording with nothing on screen relies on.
    #[test]
    fn a_transparent_layer_changes_nothing() {
        let before: Vec<u8> = (0..64u8).collect();
        let mut frame = before.clone();
        composite_over(&mut frame, 4, 4, &layer(0, 0, 4, 4, vec![0u8; 4 * 4 * 4]), false);
        assert_eq!(frame, before);
    }

    /// `swap_rb` swaps only the two colour channels, so a BGRA frame gets its red where red
    /// belongs. Getting this backwards turns every mark the wrong colour.
    #[test]
    fn a_bgra_frame_takes_its_red_in_the_third_byte() {
        let mut frame = vec![0u8, 0, 0, 255];
        composite_over(&mut frame, 1, 1, &layer(0, 0, 1, 1, vec![255, 0, 0, 255]), true);
        assert_eq!(frame, vec![0, 0, 255, 255], "red landed in the BGRA red slot");
    }

    /// A layer hanging off the frame's edge draws the part that fits and never indexes past the
    /// buffer. The clip is what makes a stale rectangle harmless rather than a panic.
    #[test]
    fn a_layer_past_the_edge_is_clipped() {
        let mut frame = vec![0u8; 2 * 2 * 4];
        composite_over(&mut frame, 2, 2, &layer(1, 1, 4, 4, vec![255u8; 4 * 4 * 4]), false);
        let at = |x: usize, y: usize| frame[(y * 2 + x) * 4 + 3];
        assert_eq!(at(1, 1), 255, "the one pixel that fits was drawn");
        assert_eq!(at(0, 0), 0, "and nothing before it");
        // Entirely off the frame: nothing at all, and no panic.
        let mut frame = vec![0u8; 2 * 2 * 4];
        composite_over(&mut frame, 2, 2, &layer(9, 9, 1, 1, vec![255, 255, 255, 255]), false);
        assert!(frame.iter().all(|b| *b == 0));
    }

    /// A layer whose pixel buffer is short of its own rectangle is refused whole rather than
    /// drawn partially, because a short buffer means the rectangle is a lie.
    #[test]
    fn a_short_layer_buffer_draws_nothing() {
        let mut frame = vec![0u8; 4 * 4 * 4];
        composite_over(&mut frame, 4, 4, &layer(0, 0, 4, 4, vec![255u8; 8]), false);
        assert!(frame.iter().all(|b| *b == 0));
    }

    /// DRAGON-739: a sprite starting at a NEGATIVE origin draws the part that fits, taken from
    /// the right part of the source. The ink layer never needs this (its box is clamped before
    /// it rasterizes), and the pointer always might, because it is placed by its hotspot.
    #[test]
    fn a_sprite_before_the_origin_is_clipped_on_the_left_and_top() {
        // A 2x2 source whose four pixels are distinguishable by their red channel.
        let src = vec![
            10, 0, 0, 255, 20, 0, 0, 255, //
            30, 0, 0, 255, 40, 0, 0, 255,
        ];
        let mut frame = vec![0u8; 2 * 2 * 4];
        composite_px(&mut frame, 2, 2, (-1, -1), (2, 2), &src, false);
        let at = |x: usize, y: usize| frame[(y * 2 + x) * 4];
        assert_eq!(at(0, 0), 40, "the source's BOTTOM-RIGHT pixel is the one that fits");
        assert_eq!(at(1, 0), 0, "and nothing was smeared across the row");
        assert_eq!(at(0, 1), 0);
    }

    /// Wholly before the frame is nothing at all, and no panic. A pointer off the left of a
    /// window recording is the ordinary case.
    #[test]
    fn a_sprite_wholly_before_the_frame_draws_nothing() {
        let mut frame = vec![0u8; 2 * 2 * 4];
        composite_px(&mut frame, 2, 2, (-9, -9), (2, 2), &[255u8; 2 * 2 * 4], false);
        assert!(frame.iter().all(|b| *b == 0));
    }

    /// The wrapper really is the same blend: an `InkLayer` composited through
    /// [`composite_over`] and the same rectangle through [`composite_px`] agree byte for byte.
    /// This is what makes the refactor a code move rather than a behaviour change.
    #[test]
    fn the_layer_wrapper_and_the_raw_blend_agree() {
        let px: Vec<u8> = (0..(3 * 3 * 4u16)).map(|b| (b % 251) as u8).collect();
        let mut a = vec![7u8; 8 * 8 * 4];
        let mut b = a.clone();
        composite_over(&mut a, 8, 8, &layer(2, 3, 3, 3, px.clone()), false);
        composite_px(&mut b, 8, 8, (2, 3), (3, 3), &px, false);
        assert_eq!(a, b);
    }
}

/// DRAGON-751: which question the capture loops' wait gate asks.
///
/// Its own module because it pins a DISTINCTION, not a behaviour: `active()` and
/// `has_content()` agree on most sessions and disagree on exactly the one that matters, the
/// ordinary recording nobody annotates. That is the row a future edit would quietly get wrong.
#[cfg(test)]
mod wait_gate_tests {
    use super::*;
    use crate::ink::{InkClip, InkLog, InkTool};
    use crate::record::ink_bake::InkBake;

    fn a_bake() -> InkBake {
        InkBake {
            log: InkLog {
                clips: vec![InkClip {
                    tool: InkTool::Marker,
                    points: vec![(10.0, 10.0), (30.0, 30.0)],
                    speed: Vec::new(),
                    point_ms: vec![0, 10],
                    width: crate::ink::WIDTH_PT,
                    generation: None,
                }],
                gens: Vec::new(),
                erases: Vec::new(),
                wipes: Vec::new(),
            },
            rects: vec![(0, (0, 0, 64, 64))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        }
    }

    /// THE row this gate exists for. Every worker builds a painter for every session, so an
    /// ordinary monitor or region recording is ADMITTED by the size gate and still has nothing
    /// to draw. Gating the wait on `active()` would tighten the loop on every recording on the
    /// machine to buy nothing, which is what the first cut of DRAGON-751 did.
    #[test]
    fn an_admitted_session_with_nothing_published_is_not_content() {
        let p = LiveInkPainter::new(LiveInk::new(), 640, 480, 30, "rgba");
        assert!(p.active(), "640x480 at 30fps is well inside the size budget");
        assert!(!p.has_content(), "and there is nothing whatsoever to draw");
    }

    /// A published scene is content, and so is a pointer on its own: the app starts publishing
    /// at ARM time, before any stroke exists, and a pointer moves every tick by definition.
    #[test]
    fn either_slot_alone_is_content() {
        let scene = LiveInk::new();
        scene.publish(a_bake(), 20);
        assert!(LiveInkPainter::new(scene, 64, 64, 30, "rgba").has_content());

        let pointer = LiveInk::new();
        pointer.publish_cursor(Some((10.0, 10.0)));
        assert!(LiveInkPainter::new(pointer, 64, 64, 30, "rgba").has_content());
    }

    /// A cursor the app withdrew stops being content, so the loop relaxes again rather than
    /// staying tight for the rest of the session.
    #[test]
    fn a_withdrawn_pointer_stops_being_content() {
        let ink = LiveInk::new();
        ink.publish_cursor(Some((10.0, 10.0)));
        let p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        assert!(p.has_content());
        ink.publish_cursor(None);
        assert!(!p.has_content(), "the same painter, asked again, follows the slot");
    }

    /// A session the size gate REFUSED paints nothing whatever the app publishes, so it must
    /// never tighten its wait either: its ink goes through the retained finalize bake.
    #[test]
    fn a_refused_session_is_never_content() {
        let ink = LiveInk::new();
        ink.publish(a_bake(), 20);
        ink.publish_cursor(Some((10.0, 10.0)));
        let p = LiveInkPainter::new(ink, 6400, 3600, 30, "rgba");
        assert!(!p.active(), "6400x3600 at 30fps is past the live composite's budget");
        assert!(!p.has_content(), "and a refused painter draws nothing to sample for");
    }
}

#[cfg(test)]
mod painter_tests {
    use super::*;
    use crate::ink::{InkClip, InkLog, InkTool};
    use crate::record::ink_bake::InkBake;

    fn bake_with_a_stroke() -> InkBake {
        InkBake {
            log: InkLog {
                clips: vec![InkClip {
                    tool: InkTool::Marker,
                    points: vec![(10.0, 10.0), (30.0, 30.0)],
                    speed: Vec::new(),
                    point_ms: vec![0, 10],
                    width: crate::ink::WIDTH_PT,
                    generation: None,
                }],
                gens: Vec::new(),
                erases: Vec::new(),
                wipes: Vec::new(),
            },
            rects: vec![(0, (0, 0, 64, 64))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [255, 0, 0, 255],
        }
    }

    fn opaque_frame(w: u32, h: u32) -> Vec<u8> {
        let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        px
    }

    /// **The guarantee**: a session with no published ink writes the captured bytes untouched,
    /// so a recording that drew nothing is byte-identical to one made before this existed.
    #[test]
    fn no_ink_returns_the_captured_bytes_untouched() {
        let mut p = LiveInkPainter::new(LiveInk::new(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        assert_eq!(p.paint(64, 64, &px), &px[..]);
        assert_eq!(p.painted(), 0);
    }

    /// A session the size gate refused holds no scene at all, so even a published drawing leaves
    /// the frames alone and the finalize bake is what puts it in the file.
    #[test]
    fn a_refused_session_never_paints() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink, 6400, 3600, 30, "rgba");
        assert!(!p.active());
        let px = opaque_frame(64, 64);
        assert_eq!(p.paint(64, 64, &px), &px[..]);
        assert_eq!(p.painted(), 0);
    }

    /// DRAGON-736: a session the gate ADMITTED, which nobody ever publishes a scene to, paints
    /// nothing at all.
    ///
    /// That is the BRONZE shape on the Linux CPU portal worker: the app deliberately keeps the
    /// live slot empty for that tier, because its strokes are in the stream's own pixels for
    /// the finalize bake's identity mapping. So [`LiveInkPainter::active`] answers `true` while
    /// the ink is still entirely in the bake, and a worker that dropped its fallback bake on
    /// `active()` would silently cost the user the whole drawing. `painted()` is what separates
    /// the two, and this pins that it can.
    #[test]
    fn an_admitted_session_with_no_published_scene_paints_nothing() {
        let ink = LiveInk::new();
        let mut p = LiveInkPainter::new(ink, 640, 480, 30, "rgba");
        assert!(p.active(), "640x480 at 30fps is well inside the budget");
        let px = opaque_frame(640, 480);
        for _ in 0..5 {
            let out = p.paint(640, 480, &px);
            // Pointer identity, not just equal bytes: an un-inked session must hand the
            // CAPTURED buffer straight back, with no copy and no allocation on the tick path.
            // That is what makes an un-inked recording byte-identical rather than merely
            // equal-looking, and it is the promise the Linux portal worker inherits by
            // building a painter for every session it runs.
            assert!(std::ptr::eq(out, px.as_slice()), "the captured buffer is handed back as-is");
        }
        assert_eq!(p.painted(), 0, "the gate admitting a session is not the ink being painted");
    }

    /// Published ink lands in the frame, and only where the stroke is.
    #[test]
    fn published_ink_reaches_the_frame() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        assert!(p.active());
        let px = opaque_frame(64, 64);
        let out = p.paint(64, 64, &px).to_vec();
        assert_eq!(out.len(), px.len(), "the frame keeps its size");
        let at = |x: usize, y: usize| out[(y * 64 + x) * 4];
        assert!(at(20, 20) > 200, "the middle of the stroke is red: {}", at(20, 20));
        assert_eq!(at(60, 5), 0, "and the far corner is untouched");
        assert_eq!(p.painted(), 1);
    }

    /// The SAME published scene is rasterized once and reused, which is what keeps the steady
    /// state off the per-frame budget. Two ticks of a still drawing produce identical frames.
    #[test]
    fn a_still_scene_is_rasterized_once() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let first = p.paint(64, 64, &px).to_vec();
        let cached = p.cached_from.clone();
        let second = p.paint(64, 64, &px).to_vec();
        assert_eq!(first, second, "the same scene draws the same picture");
        assert!(
            matches!((&p.cached_from, &cached), (Some(a), Some(b)) if Arc::ptr_eq(a, b)),
            "the layer was not re-rasterized"
        );
    }

    /// A republished scene REPLACES the cached layer, so an erase actually clears the frames
    /// rather than leaving the last raster on them for ever.
    #[test]
    fn republishing_replaces_the_layer() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let inked = p.paint(64, 64, &px).to_vec();
        assert_ne!(inked, px, "the first tick was inked");
        // Erase everything, at an instant after the stroke.
        let mut cleared = bake_with_a_stroke();
        cleared.log.erases.push(15);
        ink.publish(cleared, 40);
        assert_eq!(p.paint(64, 64, &px), &px[..], "the erase reached the frame");
    }

    /// A scene whose window was never located paints nothing: there is no honest mapping, and
    /// the same rule the finalize bake states (a stale rectangle is worse than none).
    #[test]
    fn a_scene_with_no_window_rect_paints_nothing() {
        let ink = LiveInk::new();
        let mut bake = bake_with_a_stroke();
        bake.rects.clear();
        ink.publish(bake, 20);
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        assert_eq!(p.paint(64, 64, &px), &px[..]);
    }

    /// A malformed short frame is passed straight through: reporting it is the frame writer's
    /// job, and painting one would index past the pixels it actually has.
    #[test]
    fn a_short_frame_is_passed_through() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = vec![0u8; 8];
        assert_eq!(p.paint(64, 64, &px), &px[..]);
        assert_eq!(p.painted(), 0);
    }

    /// A scene with a window rect and NO strokes: the armed-but-nothing-drawn-yet shape
    /// (DRAGON-739). It carries the placement the pointer is positioned through, and draws no
    /// ink at all.
    fn bake_with_no_strokes() -> InkBake {
        let mut b = bake_with_a_stroke();
        b.log.clips.clear();
        b
    }

    /// **Row one of the byte-identity contract** (DRAGON-736, restated by DRAGON-739): NO tool
    /// armed and NO strokes means the captured buffer is handed straight back, with no copy
    /// and no allocation on the tick path. Nothing about the pointer may weaken this.
    ///
    /// The app publishes neither a scene nor a pointer in that state, which is what this
    /// simulates: an empty `LiveInk`.
    #[test]
    fn nothing_published_hands_the_captured_buffer_back() {
        let mut p = LiveInkPainter::new(LiveInk::new(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        for _ in 0..5 {
            let out = p.paint(64, 64, &px);
            assert!(std::ptr::eq(out, px.as_slice()), "the captured buffer, not a copy of it");
        }
        assert_eq!((p.painted(), p.pointed()), (0, 0));
    }

    /// **Row two**: a tool ARMED with nothing drawn yet DOES paint, and painting it is the
    /// whole feature. The compositor stops drawing the cursor the instant the tool is armed,
    /// which is well before the first stroke exists, so waiting for one would leave the user
    /// with no cursor for exactly as long as they were deciding where to draw.
    ///
    /// It costs a frame copy for that span, which the size gate already admitted: the gate is
    /// per SESSION and per frame area, and a cursor-only tick is strictly cheaper than an
    /// inked one.
    #[test]
    fn an_armed_session_with_no_strokes_draws_the_pointer() {
        let ink = LiveInk::new();
        ink.publish(bake_with_no_strokes(), 20);
        ink.publish_cursor(Some((32.0, 32.0)));
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let out = p.paint(64, 64, &px).to_vec();
        assert!(!std::ptr::eq(out.as_slice(), px.as_slice()));
        let at = |x: usize, y: usize| &out[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        // The arrow hangs DOWN and RIGHT of its tip, so the hotspot pixel is the tip's own
        // corner, inked but only partly covered by it, while a pixel well inside the head is
        // solid. Both are offsets from the PUBLISHED position, so neither moves if the
        // sprite's padding does.
        assert_ne!(at(32, 32), &[0, 0, 0, 255], "the hotspot pixel carries the tip");
        assert_eq!(at(35, 40), &[255, 255, 255, 255], "and the head is solid white");
        assert_eq!(at(60, 5), &[0, 0, 0, 255], "and the far corner is untouched");
        assert_eq!(p.pointed(), 1);
        assert_eq!(p.painted(), 0, "a cursor is not ink, and the bake's drop rule reads that");
    }

    /// A pointer with NO published scene draws nothing: there is no window rect, so there is no
    /// honest screen-to-video mapping to place it through. The same rule the ink itself follows,
    /// and the captured buffer comes back by pointer.
    #[test]
    fn a_pointer_with_no_scene_draws_nothing() {
        let ink = LiveInk::new();
        ink.publish_cursor(Some((32.0, 32.0)));
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let out = p.paint(64, 64, &px);
        assert!(std::ptr::eq(out, px.as_slice()));
        assert_eq!(p.pointed(), 0);
    }

    /// A pointer outside the recorded window draws nothing, and hands the captured buffer
    /// back. This is the ordinary case rather than an edge one: the ink surface covers the
    /// whole output while the video is one window.
    #[test]
    fn a_pointer_beside_the_window_draws_nothing() {
        let ink = LiveInk::new();
        ink.publish(bake_with_no_strokes(), 20);
        ink.publish_cursor(Some((900.0, 900.0)));
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        assert!(std::ptr::eq(p.paint(64, 64, &px), px.as_slice()));
        assert_eq!(p.pointed(), 0);
    }

    /// **The trail test.** A moving pointer over a re-fed frame must draw ONE cursor, at the
    /// current position, and not a line of them down the screen.
    ///
    /// The capture loop re-feeds the freshest frame whenever a tick has nothing newer of its
    /// own, so this is not a hypothetical: on a still screen it is what every tick does. A
    /// stroke that accumulated would go solid and might pass unnoticed; a cursor that
    /// accumulates leaves an obvious trail, which is why this compares against a FRESH painter
    /// given only the final position rather than against "it changed".
    #[test]
    fn a_moving_pointer_leaves_no_trail() {
        let ink = LiveInk::new();
        ink.publish(bake_with_no_strokes(), 20);
        let mut p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        for step in 0..8 {
            ink.publish_cursor(Some((16.0 + step as f32 * 4.0, 16.0 + step as f32 * 4.0)));
            let _ = p.paint(64, 64, &px);
        }
        let walked = p.paint(64, 64, &px).to_vec();

        let fresh_ink = LiveInk::new();
        fresh_ink.publish(bake_with_no_strokes(), 20);
        fresh_ink.publish_cursor(Some((44.0, 44.0)));
        let mut fresh = LiveInkPainter::new(fresh_ink, 64, 64, 30, "rgba");
        let once = fresh.paint(64, 64, &px).to_vec();

        assert_eq!(walked, once, "eight ticks of a moving pointer drew more than one cursor");
        assert_eq!(px, opaque_frame(64, 64), "and the captured frame was never touched");
    }

    /// A pointer that moves does NOT re-rasterize the ink. The layer's cache is keyed on the
    /// scene `Arc` and the pointer lives in its own slot, which is the whole reason it was not
    /// made a field of the scene: a cursor moving at the mouse's own rate would otherwise
    /// re-draw the entire drawing on every frame written.
    #[test]
    fn a_moving_pointer_does_not_re_rasterize_the_ink() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        ink.publish_cursor(Some((40.0, 12.0)));
        let mut p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let _ = p.paint(64, 64, &px);
        let cached = p.cached_from.clone();
        for step in 0..5 {
            ink.publish_cursor(Some((40.0 + step as f32, 12.0)));
            let _ = p.paint(64, 64, &px);
        }
        assert!(
            matches!((&p.cached_from, &cached), (Some(a), Some(b)) if Arc::ptr_eq(a, b)),
            "the ink layer was re-rasterized because the pointer moved"
        );
        assert_eq!(p.painted(), 6, "and every one of those ticks still carried the ink");
        assert_eq!(p.pointed(), 6);
    }

    /// The pointer draws OVER the ink, not under it. A mark made under the cursor should not
    /// swallow the thing the user is looking at.
    ///
    /// Asked as "does a pixel exist that the ink owned and the cursor took", rather than by
    /// naming one: since DRAGON-744 the sprite is an arrow whose body hangs down and right of
    /// its hotspot, so a hard-coded coordinate would be a guess about the shape rather than a
    /// statement about the z-order.
    #[test]
    fn the_pointer_draws_over_the_ink() {
        let px = opaque_frame(64, 64);
        let render = |cursor: Option<(f32, f32)>| {
            let ink = LiveInk::new();
            ink.publish(bake_with_a_stroke(), 20);
            ink.publish_cursor(cursor);
            let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
            p.paint(64, 64, &px).to_vec()
        };
        let bare = render(None);
        // The stroke runs (10,10) to (30,30), so a tip placed above and left of its middle
        // lays the arrow's body straight across the ink.
        let over = render(Some((18.0, 14.0)));
        let red = |p: &[u8]| p[0] > 200 && p[1] < 64 && p[2] < 64;
        let covered = (0..64 * 64)
            .filter(|i| {
                red(&bare[i * 4..i * 4 + 4]) && over[i * 4..i * 4 + 4] == [255, 255, 255, 255]
            })
            .count();
        assert!(covered > 0, "the cursor covered no inked pixel at all, so this proved nothing");
    }

    /// A published pointer the app later withdraws (the tool went back to Pointer, or the
    /// cursor left the ink surface) stops being drawn, and with no ink on screen the tick goes
    /// back to handing the captured buffer straight back.
    #[test]
    fn withdrawing_the_pointer_stops_drawing_it() {
        let ink = LiveInk::new();
        ink.publish(bake_with_no_strokes(), 20);
        ink.publish_cursor(Some((32.0, 32.0)));
        let mut p = LiveInkPainter::new(ink.clone(), 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        assert!(!std::ptr::eq(p.paint(64, 64, &px), px.as_slice()), "drawn while published");
        ink.publish_cursor(None);
        assert!(std::ptr::eq(p.paint(64, 64, &px), px.as_slice()), "and gone once withdrawn");
        assert_eq!(p.pointed(), 1);
    }

    /// A session the SIZE GATE refused holds no slot at all, so a published pointer reaches
    /// nothing. The fallback bake puts the ink in the file there and the cursor is simply not
    /// drawn, which is today's behaviour and is the honest answer for a frame too big to touch
    /// per tick.
    #[test]
    fn a_refused_session_draws_no_pointer_either() {
        let ink = LiveInk::new();
        ink.publish(bake_with_no_strokes(), 20);
        ink.publish_cursor(Some((32.0, 32.0)));
        let mut p = LiveInkPainter::new(ink, 6400, 3600, 30, "rgba");
        assert!(!p.active());
        let px = opaque_frame(64, 64);
        assert!(std::ptr::eq(p.paint(64, 64, &px), px.as_slice()));
        assert_eq!(p.pointed(), 0);
    }

    /// The painter NEVER mutates the frame it was handed. The capture loop re-feeds the freshest
    /// frame whenever a tick has nothing newer, so painting in place would stack one tick of ink
    /// per repeat until the stroke went solid.
    #[test]
    fn the_captured_frame_is_never_mutated() {
        let ink = LiveInk::new();
        ink.publish(bake_with_a_stroke(), 20);
        let mut p = LiveInkPainter::new(ink, 64, 64, 30, "rgba");
        let px = opaque_frame(64, 64);
        let before = px.clone();
        let first = p.paint(64, 64, &px).to_vec();
        assert_eq!(px, before, "the source frame is untouched");
        let second = p.paint(64, 64, &px).to_vec();
        assert_eq!(first, second, "re-feeding the same frame draws the same ink, not twice as much");
    }
}

#[cfg(test)]
mod offset_invariance_tests {
    use super::*;
    use crate::ink::{InkClip, InkGen, InkLog, InkTool, MediaMs};
    use crate::record::ink_bake::InkBake;

    /// A scene with everything that reads the clock: a dissolving generation, a marker stroke,
    /// an erase, a wipe and two window-rect samples.
    fn scene(shift: MediaMs) -> InkBake {
        InkBake {
            log: InkLog {
                clips: vec![
                    InkClip {
                        tool: InkTool::MagicMarker,
                        points: vec![(12.0, 12.0), (28.0, 30.0), (44.0, 20.0)],
                        speed: Vec::new(),
                        point_ms: vec![1_000 + shift, 1_040 + shift, 1_080 + shift],
                        width: crate::ink::WIDTH_PT,
                        generation: Some(0),
                    },
                    InkClip {
                        tool: InkTool::Marker,
                        points: vec![(20.0, 44.0), (50.0, 50.0)],
                        speed: Vec::new(),
                        point_ms: vec![1_200 + shift, 1_260 + shift],
                        width: crate::ink::WIDTH_PT,
                        generation: None,
                    },
                ],
                gens: vec![InkGen { id: 0, last_activity_ms: 1_080 + shift }],
                // Before both strokes, so it is inert on them: it is here to prove the FILTER
                // that decides that is a comparison of two clock values and not an absolute.
                erases: vec![900 + shift],
                // Partway through its 400ms lift at the 2s the first test samples, so the sweep
                // FRACTION is exercised rather than just its two ends.
                wipes: vec![1_900 + shift],
            },
            rects: vec![(shift, (0, 0, 64, 64)), (1_500 + shift, (4, 4, 64, 64))],
            origin: (0, 0),
            point_scale: 1.0,
            color: [0, 200, 255, 255],
        }
    }

    /// **THE clock invariant.** Shift the app's whole media clock, stamps and query alike, and
    /// the painted picture is BIT-IDENTICAL.
    ///
    /// This is why the live composite was immune to the anchor defect rather than merely hiding
    /// it: `recording_started` was media 0 plus the worker's opening phase, measured at 489.8ms
    /// on the owner's session and up to ~3s since DRAGON-717 made the opening wait on the FIFO
    /// rendezvous. Every quantity this path reads is a difference of two app-clock values, so
    /// that offset cancels. DRAGON-725 has since given the app the file's own anchor, which
    /// makes this an invariant to KEEP rather than a defence now spent: the anchor has been
    /// wrong twice already, and a future change that resolved the ink instant from the worker's
    /// tick instead would break exactly this test, which is the point of having it.
    #[test]
    fn shifting_the_whole_app_clock_paints_the_same_picture() {
        // 490ms of opening phase, near enough the measured 489.8ms, at a 30fps tick.
        for shift in [0, 490, 2_985, -400] {
            let a = rasterize_layer(&scene(0), 2_000, (64, 64));
            let b = rasterize_layer(&scene(shift), 2_000 + shift, (64, 64));
            match (a, b) {
                (Some(a), Some(b)) => {
                    assert_eq!((a.x, a.y, a.w, a.h), (b.x, b.y, b.w, b.h), "shift {shift}");
                    assert_eq!(a.px, b.px, "shift {shift} changed the pixels");
                }
                (None, None) => panic!("the scene should draw something at 2s"),
                _ => panic!("shift {shift} changed whether anything was drawn at all"),
            }
        }
    }

    /// The dissolve is a DIFFERENCE too, so a shifted clock reaches every stage of a sweep at
    /// the same offset from the stroke. Sampled across the hold and the sweep, since an alpha
    /// that only matched at one instant would prove nothing.
    #[test]
    fn the_dissolve_runs_at_the_same_age_on_a_shifted_clock() {
        let hold = crate::ink::MAGIC_HOLD_MS as MediaMs;
        let gone = crate::ink::MAGIC_GONE_MS as MediaMs;
        for at in [1_100, 1_080 + hold / 2, 1_080 + hold + 200, 1_080 + gone + 50] {
            let a = rasterize_layer(&scene(0), at, (64, 64));
            let b = rasterize_layer(&scene(490), at + 490, (64, 64));
            assert_eq!(
                a.map(|l| l.px),
                b.map(|l| l.px),
                "the dissolve differed at {at}ms into the scene"
            );
        }
    }
}

#[cfg(test)]
mod smoothing_tail_tests {
    /// DRAGON-724's second known cost, quantified: how much of a stroke can the LIVE composite
    /// draw differently from the finalize bake?
    ///
    /// The bake redraws every frame from the FINAL fit; the live composite draws the fit as it
    /// stood when those pixels were captured. `pen_stroke`'s pipeline is causal, so the two can
    /// only differ over the stretch still in flux behind the pointer, and that stretch is
    /// bounded by `SETTLE_TAIL_POINTS` raw samples at the pointer's own thinning step. This
    /// pins the bound rather than the picture, because the live answer is the more faithful of
    /// the two: it is what the user was looking at.
    #[test]
    fn only_the_unsettled_tail_can_differ() {
        // A hand-drawn-ish arc at the pointer's thinning step.
        let raw: Vec<(f32, f32)> = (0..120)
            .map(|i| {
                let t = i as f32 * 0.05;
                (10.0 + t * 12.0, 40.0 + (t * 1.3).sin() * 18.0)
            })
            .collect();
        let live = crate::pen_stroke::smooth_path(&raw[..90], crate::ink::WIDTH_PT);
        let finished = crate::pen_stroke::smooth_path(&raw, crate::ink::WIDTH_PT);
        let same = live
            .iter()
            .zip(&finished)
            .take_while(|(a, b)| a.0.to_bits() == b.0.to_bits() && a.1.to_bits() == b.1.to_bits())
            .count();
        assert!(same > 0, "nothing settled at all, so the causal contract is gone");
        let moved: f32 = live[same.saturating_sub(1)..]
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .sum();
        // The same budget `pen_stroke`'s own settling test uses: samples x the pointer's
        // thinning step (`app::preview::annotate::PEN_MIN_STEP`, 1.5, which is private to the
        // app tree), with slack.
        let budget = crate::pen_stroke::SETTLE_TAIL_POINTS as f32 * 1.5 * 2.0;
        assert!(
            moved <= budget,
            "the live composite could differ over {moved} points of arc, budget {budget}"
        );
    }
}

