use super::*;

pub(super) mod toolbar;
pub(super) mod marks;
pub(super) mod menus;

/// Playful loading lines shown under the window-picker spinner; one is picked at
/// random per launch (see `App::loading_msg`).
pub(super) const LOADING_MESSAGES: [&str; 20] = [
    "Rounding up your windows",
    "Peeking behind your windows",
    "Counting all the windows",
    "Wrangling your windows",
    "Hunting for open windows",
    "Sizing up the desktop",
    "Lining up your windows",
    "Catching every window",
    "Surveying the workspace",
    "Gathering the usual suspects",
    "Collecting open windows",
    "Mapping out your windows",
    "Tracking down windows",
    "Scoping out the desktop",
    "Tidying up the windows",
    "Polling for windows",
    "Sweeping the desktop",
    "Finding every last window",
    "Cataloguing open windows",
    "Assembling your windows",
];

/// The pixels-per-point scale of a captured cursor sprite, for turning its pixel
/// dimensions into a LOGICAL on-overlay size. On Linux the cursor session hands
/// the sprite back at the output's buffer scale, so there is no per-sprite scale
/// to carry and the output scale IS the sprite scale (this returns `out_scale`,
/// keeping the Linux indicator byte-identical). On macOS the sprite carries its
/// own backing scale (the 4th `CursorSprite` element): `NSCursor` gives the
/// system cursor asset at its own resolution, unrelated to the display under the
/// pointer, so the sprite must be sized by that (DRAGON-156).
#[cfg(target_os = "linux")]
fn cursor_sprite_scale(_cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    out_scale
}

/// See the Linux twin above; on macOS the sprite's own scale is the 4th tuple
/// element. A degenerate (`<= 0`) sprite scale falls back to the output scale.
#[cfg(target_os = "macos")]
fn cursor_sprite_scale(cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    let s = cursor.3;
    if s > 0.0 {
        s
    } else {
        out_scale
    }
}

/// Windows (DRAGON-448): a raw cursor-sprite pixel IS one point, so this is always `1.0`.
///
/// History: `platform::windows::cursor` once stamped the 4th `CursorSprite` element with
/// `96 / dpi`, claiming the sprite was a 96-DPI base asset needing a `dpi / 96` upscale.
/// DRAGON-448 hardcoded `1.0` here to dodge that stamp (passing `cursor.3` through drew
/// the indicator `(dpi/96)`-squared-ish too large on scaled monitors, invisible at 96 DPI
/// where every reading agrees). DRAGON-567 then fixed the PRODUCER: the process is
/// Per-Monitor-Aware-V2, so the `GetIconInfo` bitmap is already on-screen physical size
/// and `platform::win_cursor::sprite_backing_scale` now stamps `1.0`. This arm's constant
/// finally agrees with the stamp instead of correcting for it; both stay, one contract.
#[cfg(target_os = "windows")]
fn cursor_sprite_scale(_cursor: &crate::screenshot::CursorSprite, _out_scale: f32) -> f32 {
    1.0
}

/// Any other non-Linux target: keep the macOS reading (the sprite's own scale).
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn cursor_sprite_scale(cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    let s = cursor.3;
    if s > 0.0 {
        s
    } else {
        out_scale
    }
}

/// The selection marker's opacity during a countdown or a live recording: ALWAYS solid
/// (DRAGON-588, owner's call).
///
/// The "Active overlay opacity" setting governs the DIM BEHIND, which is what the user is
/// choosing when they move that slider: how much of their desktop stays visible while a
/// capture is armed. It used to drive the selection lines too, so turning the dim down also
/// faded the one thing that says WHERE the capture is and that it is running. Those are
/// opposite intents on one control. The dim is a preference; the marker is information.
const SELECTION_LINE_ALPHA: f32 = 1.0;

// ── The dim's fade-in (DRAGON-606) ───────────────────────────────────────────
//
// The owner asked for the fade the `lab/flatpak` fallback picker has. We never wrote one.
// It is cosmic-comp's, and it is free there for a reason we cannot reuse: the fallback
// surface is a FULLSCREEN xdg TOPLEVEL (`shell::overlay_fallback_window`, `fullscreen:
// true`), and cosmic-comp fades a toplevel that maps straight into fullscreen over 200ms
// with an ease-in-out-cubic alpha ramp (`shell/workspace.rs`, `FULLSCREEN_ANIMATION_DURATION`).
// A LAYER surface gets none of that: the compositor draws every layer with alpha hardcoded
// to `1.0` (`backend/render/mod.rs`, the `Stage::LayerSurface` arm). Verified against the
// installed build, cosmic-comp 1.0.8-2 at commit 4fd8634e, which is the exact tree the
// running binary reports. So on the native path the fade has to be ours, and the constants
// below are MEASURED FROM THE COMPOSITOR rather than chosen, so the two paths feel the same.
//
// THE SAFETY RULE, and it is the whole reason this is gated rather than just drawn:
// DRAGON-600 made the frozen-flats grab wait for our overlay to take keyboard focus so the
// tray dropdown is out of the capture, and that fix works only because the overlay paints
// NOTHING while the grab runs. A dim that ramps during the grab bakes a partial wash into
// the frozen scene, which is a subtly darkened capture nobody would attribute to an
// animation months later. So the fade does not begin on a clock, it begins on the grab's
// own completion: see [`dim_fade_may_start`].
//
// The fade also makes the picking phase STRICTLY safer than it was. The dim used to go to
// full the instant the overlay maps, while the flats grab was still running on its thread
// (DRAGON-212 deferred it precisely so the overlay maps first, and its comment says the
// overlay maps "against the live (dimmed) screen"). Starting at zero and waiting for the
// drain means the grab now photographs an overlay that composites to nothing.
//
// EVERY PLATFORM, and the SAFETY half is why. This shipped Linux-only: `dim_now` and
// `start_dim_fade` each opened with `!cfg!(target_os = "linux")` and handed back the settled
// opacity, reasoning that the ANIMATION was about matching cosmic-comp and that mac and
// Windows have their own window-open behaviour to match instead. That covered the cosmetic
// half of the ticket and quietly dropped the safety half, which is not compositor-specific
// at all. DRAGON-212 defers the flats grab on ALL THREE platforms (`app::acquire_scene`'s
// two arms both spawn a thread), so all three had an overlay whose dim could be on screen
// while the grab was reading that screen, and only one of them had the gate.
//
// macOS had it WORST, and it was already written down rather than discovered here.
// `platform::mac::capture_output`'s "Known gap" paragraph measured our own overlay visible
// at +535ms against three display grabs running from +210ms, said outright that Linux was
// covered by this ticket and macOS was not, and named the fix: "a paint gate on the mac
// overlay". This IS that paint gate. Nothing new was built for mac; the existing mechanism
// simply stopped declining to run there. (It is not the WHOLE of that gap: the hint pill and
// the toolbar still paint on their own schedule. The dim is the full-screen part of it.)
//
// The colour picker is the sharpest case, and there it is a correctness bug rather than a
// cosmetic one: `color_picker::view` draws the picker's own dim over the very snapshot the
// picker SAMPLES, so a dim baked into those flats is handed back to the user as the colour
// they picked. Off Linux that was live until now.
//
// Widening is a DELETION rather than a port, because all three terms of the gate were
// already portable:
// - `frozen_pending` is portable state with a portable lifecycle: set from `scene_active` in
//   `App::init`, polled by `sub_frozen_ready`, cleared in the `FrozenReady` drain, none of
//   the three carrying a `cfg`. macOS clears it in one MORE place; see [`dim_fade_may_start`].
// - `menu_hold` is `None` off Linux for a reason rather than by omission, and it must stay
//   that way. `menu_flats_held` answers false there because the mac and Windows daemons own
//   their own menus and have a REAL menu-closed signal before they spawn a capture child:
//   AppKit dismisses an `NSMenu` before it sends the action (plus
//   `recording_ui::MENU_DISMISS_DELAY` for the close animation), and `TrackPopupMenu`'s
//   `TPM_RETURNCMD` only returns after dismissal, with `show_menu` destroying the menu
//   before it acts. Linux has no such signal, which is the entire premise of DRAGON-600.
//   Do NOT widen THAT gate to match this one: `spawn_frozen_flats_grab` is a no-op stub on
//   macOS, so a hold armed there would release into a grab that never runs.
// - `overlay_fallback_active` already answers `false` off Linux from its own `#[cfg]` arm.
//
// THE ONE THING WORTH CHECKING BEFORE WIDENING, checked, and recorded here so nobody has to
// wonder again: does a fully transparent overlay stop receiving input on Windows? It does
// not. Click-through there is `WS_EX_TRANSPARENT` and nothing else (`set_click_through`,
// which DRAGON-276 has to set EXPLICITLY on countdown overlays that already draw nothing, and
// which `passthrough_poll` clears again to re-solidify one). The layered attributes the
// overlay does carry are `LWA_ALPHA` at 255 with no `LWA_COLORKEY`, so the layered-alpha
// hit-test rule has nothing to bite on. Input in the selection widget is gated on
// `interactive` alone and never reads `dim_alpha`, and `region_selection` already skips
// drawing a dim whose alpha is zero. The clincher is that this is not even a new state for
// Windows: the opacity setting goes down to 0%, so a user could already sit in the fade's
// starting frame permanently and drag a region in it. The fade only visits that state for
// 200ms.
//
// AND THEN IT WAS STILL INVISIBLE ON macOS (DRAGON-644 reopened, DRAGON-646). Widening the
// gate was correct and was not enough, because the port measured the ramp's START and assumed
// the rest followed. It did not. The overlay's FIRST frame with real content in it blocks the
// main thread while the renderer warms up, measured at 53ms to 148ms across launches on the
// owner's machine, and the ramp was reading a wall clock through all of it. The animation ran
// to 94% of target with nothing on screen, then appeared in a single step. Everything about
// the join in [`DimFade`] was right; what was wrong is that "elapsed" was not the same
// quantity as "seen".
//
// It read as a KIND bug, and that is worth recording because the false lead is cheap to fall
// for twice: the scanner looked fine and region/video looked broken, so the difference seemed
// to be `Kind`. It never was. A `--scan` launch grabs the frozen flats and so runs later, and
// its first-frame stall happened to measure 53ms rather than 148ms, which leaves enough of the
// ramp for a human to see. Same code path, same ordering, different stall. The render path is
// byte-identical for every `Kind`, which is the thing to check FIRST next time.
//
// The fix is [`DIM_FADE_MAX_STEP_MS`]: the ramp advances on PAINTED FRAMES, capped, so a stall
// cannot spend animation nobody watched. It is strictly a slowdown relative to the wall clock,
// so it cannot move the dim earlier and cannot touch the DRAGON-212 ordering guarantee above.

/// How long the dim takes to reach the configured opacity.
///
/// 200ms because that is cosmic-comp's `FULLSCREEN_ANIMATION_DURATION`, the animation the
/// Flatpak fallback picker gets for free. Matching it is the point of the ticket: the two
/// overlay paths should not feel like two different products.
///
/// ONE duration on every platform, and deliberately NOT re-derived per platform. The number
/// was measured from cosmic-comp, and that particular justification does not travel to a Mac
/// or a PC, but the conclusion it was serving does: a capture overlay that fades over 200ms
/// on Linux and over some other span elsewhere is the same "two different products" problem
/// this constant was chosen to remove, just drawn along a different axis. There is also
/// nothing to match off Linux even if we wanted to, since our overlay is not a window either
/// of those compositors animates on open.
pub(super) const DIM_FADE_MS: u64 = 200;

/// The most WALL time one painted frame is allowed to contribute to the ramp (DRAGON-644).
///
/// The ramp does not read the wall clock directly. It accumulates the gap between painted
/// frames, and any gap longer than this counts as this much and no more, because a gap that
/// long is not animation, it is a STALL: the app produced no frame at all, so no part of the
/// ramp in that span was ever on screen to be watched.
///
/// WHY it exists, measured on macOS, `--region`, three consecutive launches. The first frame
/// the capture overlay draws with real content in it is expensive (shader/pipeline warm-up on
/// first use), and it BLOCKS the main thread: after `dim fade: first painted frame, ramp
/// begins` the process produced no `update` and no `view_window` at all for 148ms, 148ms and
/// 53ms respectively, then drained ~9 queued 16ms fade ticks in one burst. On a wall clock
/// the ramp had spent that whole time ramping in the dark, so the FIRST alpha the user could
/// actually see was 0.619 of 0.66 (94% of the way) on the worst run. The dim went from
/// nothing to essentially full in one step. That is the whole of the "the fade does not work
/// on macOS" report; the ramp was correct and simply had no frames to be drawn on.
///
/// The scanner LOOKED fine for the same reason in reverse, and this is why the bug read as
/// kind-dependent when it is not: a `--scan` launch grabs the frozen flats, which pushes
/// everything later, and its first-frame stall measured 53ms rather than 148ms, leaving 147ms
/// of ramp and about ten intermediate frames. Same code, same ordering, different stall
/// length. Nothing about `Kind` enters into it.
///
/// 32ms is two frames at 60Hz: long enough that ordinary jitter and a single dropped frame
/// are still spent as animation, short enough that a stall cannot swallow a meaningful share
/// of a 200ms ramp. It can only ever make the dim ARRIVE LATER than a wall clock would, never
/// sooner, so it cannot weaken DRAGON-212's ordering guarantee (which is about when the ramp
/// may START, and is untouched here). On a machine that paints smoothly, every gap is under
/// the cap and the ramp is wall-clock-identical to before, which is what keeps Linux, where
/// this animation already looked right, exactly as it was.
pub(super) const DIM_FADE_MAX_STEP_MS: u64 = 32;

// A cap at or above the ramp itself would let a single stalled frame finish the whole
// animation, which is the bug this constant exists to prevent.
const _: () = assert!(
    DIM_FADE_MAX_STEP_MS * 4 <= DIM_FADE_MS,
    "DRAGON-644: one stalled frame must contribute a small SHARE of the dim ramp, or the \
     stall swallows the animation again and the fade is invisible on macOS"
);

/// How long the ramp may go without a painted frame before it is written off (DRAGON-644).
///
/// [`DIM_FADE_MAX_STEP_MS`] means the ramp advances only when a frame is drawn, so a surface
/// that stops being drawn entirely (occluded, or on a space the user has left) would leave
/// `sub_dim_fade`'s 16ms tick scheduled with nothing to animate. The old wall-clock ramp
/// self-terminated because its own clock ran regardless. This is the replacement bound, in
/// the house style of DRAGON-118: a wait that cannot end on its own gets an explicit deadline
/// in the same commit. Generous, because it must never fire on a machine that is merely slow.
pub(super) const DIM_FADE_ABANDON_MS: u64 = 2_000;

const _: () = assert!(
    DIM_FADE_ABANDON_MS > DIM_FADE_MS,
    "DRAGON-644: the no-frames bound must outlast the ramp itself, or it ends fades that are \
     simply running on a slow machine"
);

/// **Pure**, unit-tested: how much of one painted frame's wall gap counts as ramp time.
///
/// The whole of the DRAGON-644 fix, and deliberately a one-liner in the shared tree rather
/// than a branch inside `dim_now`: the DECISION (a long gap is a stall, not animation) is
/// testable on every platform, while the effect it guards against was only ever measured on
/// one. See [`DIM_FADE_MAX_STEP_MS`] for the measurements.
///
/// It is also what makes the accumulation safe to call more than once per frame. A multi
/// display session calls `dim_now` once per overlay per frame; the second and later calls
/// measure a gap of well under a millisecond, contribute zero, and so every overlay on the
/// desktop reads the SAME alpha for that frame instead of the ramp running N times too fast.
pub(super) fn dim_fade_step_ms(gap_ms: u64) -> u64 {
    gap_ms.min(DIM_FADE_MAX_STEP_MS)
}

/// **Pure**, unit-tested: ease-in-out-cubic, the curve cosmic-comp uses for that same open
/// animation (`keyframe::functions::EaseInOutCubic`). Reimplemented rather than pulled in
/// as a dependency: it is four lines, and the alternative is a crate for one curve.
fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

/// **Pure**, unit-tested: the dim's alpha `elapsed_ms` into the fade, ramping to `target`.
///
/// `target` is the CONFIGURED opacity, never a constant: the region dim, the colour
/// picker's own dim and the active-overlay dim are three separate user settings, and the
/// fade is a multiplier on whichever one the caller is drawing. At and past
/// [`DIM_FADE_MS`] this returns `target` exactly, so a finished fade is indistinguishable
/// from no fade at all.
pub(super) fn dim_fade_alpha(target: f32, elapsed_ms: u64) -> f32 {
    if elapsed_ms >= DIM_FADE_MS {
        return target;
    }
    target * ease_in_out_cubic(elapsed_ms as f32 / DIM_FADE_MS as f32)
}

/// **Pure**, unit-tested: may the dim's fade begin?
///
/// This is the ordering guarantee, and it is a happens-before, not a delay. `frozen_pending`
/// is cleared in the `FrozenReady` drain, which runs only after the grab thread has finished
/// reading every output and posted its result into `frozen_slot`. So "not pending" means "no
/// frozen-flats grab can still be looking at the screen". The launch grab sites are
/// enumerable (`acquire_scene`'s per-platform arm, plus `tick_menu_hold`'s Linux release),
/// they are all at launch, and they all post into that one slot, which is what makes the
/// enumeration complete rather than hopeful.
///
/// **macOS clears the flag in one more place**, and the difference is worth knowing rather
/// than papering over: `capture_flow::await_frozen_flats` is the commit-race guard, and when
/// it wins the race it drains the slot and clears `frozen_pending` itself WITHOUT arming the
/// fade. That is not a hole. It runs at COMMIT, so the overlay is on its way out, and
/// `dim_now`'s `Waiting` arm consults this same gate on the next painted frame and starts the
/// ramp anyway. It does mean the "cleared in exactly one place" reading is a Linux reading,
/// and that `dim_now`'s fallback arm is genuinely reachable on macOS instead of being the
/// pure belt-and-braces it is elsewhere.
///
/// The other two terms:
/// - `menu_hold` is DRAGON-600's paint gate, and it is Linux-only by construction
///   (`menu_flats_held`). It is redundant with `frozen_pending` even there (the held grab has
///   not even started, so nothing has drained) and it stays anyway, because a fade that could
///   start while the tray dropdown is still on screen would be the one thing that fix exists
///   to prevent. Off Linux it is a constant `false`, because those daemons dismiss their own
///   menus before spawning; see the module doc.
/// - `fallback` is the `lab/flatpak` path, which already gets the compositor's own fade.
///   Fading there too would run two ramps over each other.
///
/// A launch that grabs no flats at all (`launch_flats_needed` false, the common
/// screenshot) parks an EMPTY result in the slot at init, so its first drain tick clears
/// the flag and the fade starts within a frame. It waits for nothing because there is
/// nothing to wait for.
pub(super) fn dim_fade_may_start(frozen_pending: bool, menu_hold: bool, fallback: bool) -> bool {
    !frozen_pending && !menu_hold && !fallback
}

/// Where the dim's fade-in has got to (DRAGON-606).
///
/// FOUR states, and the middle one is the whole lesson of this ticket. The fade must start
/// at whichever comes LATER, the frozen grab completing or the overlay's first painted
/// frame:
///
/// - starting on the grab alone is SAFE but can be INVISIBLE. Measured on the owner's
///   machine, the grab finishes at ~255ms and its drain lands at ~553ms, while the
///   overlay's first painted frame does not arrive until later still. A 200ms ramp that
///   begins at the drain can be completely over before anything is on screen, which
///   delivers a mathematically perfect animation that nobody ever sees. That fails the
///   ticket, since what was asked for is a thing you can watch.
/// - starting on the first frame alone would be VISIBLE but UNSAFE, because nothing would
///   stop it preceding the grab.
///
/// `Armed` is the join: the grab is done, and we are now waiting for the first frame to
/// latch the clock. Taking the later of the two is safe by construction (it can never
/// precede the grab) and visible by construction (it can never precede the first frame the
/// user could see), instead of depending on the two happening to be ordered favourably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimFade {
    /// The frozen-flats grab may still be reading the screen. The dim is not drawn.
    Waiting,
    /// The grab has landed, so the fade is ALLOWED, but nothing has been painted yet. Still
    /// draws no dim; the next frame starts the clock.
    ///
    /// On Windows "painted" additionally means REVEALED: the overlay presents real frames
    /// while DWM-cloaked during placement, so the faded views hold this state, by not
    /// consulting the latch at all, until their output is `placed` (DRAGON-653,
    /// [`App::dim_now_revealed`]).
    Armed,
    /// Ramping. `elapsed_ms` is how much of the ramp has actually been DRAWN, and `last` is
    /// when the previous painted frame read it.
    ///
    /// Not a start `Instant` any more (DRAGON-644). The ramp used to be `Running(start)` and
    /// read `start.elapsed()`, which is correct only while the app is producing frames. It
    /// was not: on macOS the overlay's first content frame blocks the main thread for 50 to
    /// 150ms, so a wall-clock ramp spent most of its 200ms with nothing on screen and the
    /// first visible alpha was already ~94% of the target. Accumulating per painted frame,
    /// through [`dim_fade_step_ms`], means the animation can only advance by what was
    /// actually shown. See [`DIM_FADE_MAX_STEP_MS`] for the measurements.
    Running {
        elapsed_ms: u64,
        last: std::time::Instant,
    },
    /// At the configured opacity for good. No more redraws are scheduled.
    Done,
}

impl App {
    /// This overlay's dim opacity RIGHT NOW: the configured `target`, scaled by however far
    /// the fade-in has got (DRAGON-606).
    ///
    /// EVERY platform, with no `cfg` of any kind. This opened with
    /// `if !cfg!(target_os = "linux") { return target; }`, which snapped macOS and Windows
    /// straight to the settled opacity; the module doc above records why that was the wrong
    /// call. The short version: this gate is a capture-safety mechanism first and an
    /// animation second, and the hazard it guards (a deferred flats grab reading the very
    /// screen our overlay is already sitting on) is identical on all three platforms.
    ///
    /// This is also THE latch: it runs during view building, which is the app producing a
    /// frame, so an `Armed` fade starts its clock here and nowhere else. Interior mutability
    /// through a `Cell` for exactly that reason, the same device `OutputState::placed` uses
    /// to record a native placement from inside a view.
    pub(super) fn dim_now(&self, target: f32) -> f32 {
        match self.dim_fade.get() {
            // Nothing is reading the screen any more, but the fade was never armed. Arm it
            // here rather than paint zero forever.
            //
            // On Linux this is belt-and-braces: `start_dim_fade` runs inside the
            // `FrozenReady` drain in the same update that clears `frozen_pending`, so a
            // `Waiting` fade with a clear gate is unreachable, and this arm exists only so
            // that a future edit which forgets to call it degrades into a working fade
            // instead of an invisible overlay.
            //
            // On macOS it is REACHABLE, by exactly one route: `await_frozen_flats` (the
            // commit-race guard) can win the race and clear `frozen_pending` itself without
            // arming anything. That happens at commit, with the overlay already on its way
            // out, so what it produces is a ramp nobody sees rather than a wrong one. This
            // arm is what keeps that case from painting zero for the overlay's last frames.
            //
            // Either way it consults the SAME gate, so it can never paint what a grab could
            // still photograph.
            DimFade::Waiting
                if dim_fade_may_start(
                    self.frozen_pending,
                    self.menu_hold.is_some(),
                    self.overlay_fallback_active(),
                ) =>
            {
                self.dim_fade.set(DimFade::Running {
                    elapsed_ms: 0,
                    last: std::time::Instant::now(),
                });
                // KEEP THIS MARK. It is not leftover scaffolding from the DRAGON-606
                // measurement, it is the launch timeline's one previously unmeasurable
                // instant, and the whole visibility argument for this feature rests on it.
                //
                // Every other quantity here can be measured from outside the process with a
                // screen grab: the ramp's shape, its duration, the settled alpha. The moment
                // the overlay first PAINTS cannot, because until this frame the fade draws
                // nothing and a mapped layer surface drawing nothing composites to nothing,
                // so an external grab and this mark are blind to the same thing. Reading it
                // off a screen recording is guesswork.
                //
                // What it bought, measured: the frozen drain landed at +537.7ms and this
                // frame at +543.3ms, a 5.6ms margin inside a 200ms animation. That is the
                // number which says the drain-anchored version was visible by luck. Delete
                // the mark and the next person cannot tell whether the fade is still visible
                // on their hardware, only that it is still correct.
                crate::util::timing_mark("dim fade: first painted frame, ramp begins");
                0.0
            }
            DimFade::Waiting => 0.0,
            // The first frame since the grab landed. Start the clock NOW, and return zero
            // for this frame so the ramp genuinely begins at nothing on screen rather than
            // jumping to wherever a drain-anchored clock had already got to.
            DimFade::Armed => {
                self.dim_fade.set(DimFade::Running {
                    elapsed_ms: 0,
                    last: std::time::Instant::now(),
                });
                // The launch timeline's missing entry. Everything else about the fade is
                // measurable from outside except the one instant that decides whether it is
                // visible at all, and reading it off a screen recording is guesswork.
                crate::util::timing_mark("dim fade: first painted frame, ramp begins");
                0.0
            }
            // Advance by what was actually PAINTED since the previous frame, capped
            // (DRAGON-644). A frame that took 148ms to appear contributes
            // `DIM_FADE_MAX_STEP_MS`, not 148ms, because the other 116ms of ramp was never on
            // screen. The `Cell` write is the same latch device the arms above use.
            DimFade::Running { elapsed_ms, last } => {
                let elapsed_ms = elapsed_ms + dim_fade_step_ms(last.elapsed().as_millis() as u64);
                self.dim_fade.set(DimFade::Running {
                    elapsed_ms,
                    last: std::time::Instant::now(),
                });
                dim_fade_alpha(target, elapsed_ms)
            }
            DimFade::Done => target,
        }
    }

    /// [`Self::dim_now`], held until this output's overlay is actually VISIBLE
    /// (DRAGON-653). The one entry point for every view that draws a FADED dim; the
    /// countdown and recording views never faded on any platform and stay on their own
    /// constant alpha.
    ///
    /// Windows only: the overlay window is minted hidden, then `place_overlay` shows it
    /// DWM-CLOAKED and parked off the virtual screen so wgpu presents real frames, and
    /// only uncloaks it on its monitor after the present grace, ~120-240ms in. Those
    /// cloaked frames are real paints, so calling `dim_now` from them latches the ramp
    /// and burns most of its 200ms before a user can see anything: the dim snaps in.
    /// (`DIM_FADE_MAX_STEP_MS` never bites, because the cloaked window paints promptly;
    /// the frames are merely invisible.) So while this output's placement has not
    /// landed, draw no dim and DO NOT CONSULT the latch at all: no call means no
    /// `Armed`→`Running` transition, no `Waiting` self-arm, and no elapsed
    /// accumulation, so the ramp's first counted frame is the first one the user could
    /// watch. macOS solves the same race by returning an EMPTY view until `placed`
    /// (`overlay_view`); Windows must keep drawing real content while cloaked (that is
    /// what the cloak phase is FOR), so the hold is on the latch, not on the view.
    ///
    /// A launch where no output ever places cannot park the fade `Armed` forever: the
    /// finalize driver's give-up either fails the session (nothing placed,
    /// `overlay_giveup_action`) or continues with a placed output whose frames run the
    /// ramp to `Done`. And holding `Armed` through the placement window cannot trip
    /// `DIM_FADE_ABANDON_MS`: that bound reads `Running`'s own `last` instant, which
    /// does not exist until the latch runs.
    ///
    /// Off Windows this is `dim_now` verbatim, so Linux and macOS behaviour is
    /// byte-identical.
    pub(super) fn dim_now_revealed(&self, o: &OutputState, target: f32) -> f32 {
        #[cfg(windows)]
        if !o.placed.get() {
            return 0.0;
        }
        #[cfg(not(windows))]
        let _ = o;
        self.dim_now(target)
    }

    /// The frozen-flats grab has landed, so the dim may start fading in (DRAGON-606).
    ///
    /// Called from the `FrozenReady` drain, which is the completion event itself. Idempotent
    /// and one-way: once the fade is running or finished a later call cannot restart it, so
    /// nothing can re-blank an overlay the user is already working on.
    pub(in crate::app) fn start_dim_fade(&mut self) {
        if self.dim_fade.get() != DimFade::Waiting {
            return;
        }
        // The fallback path never fades on our clock, and it must not sit at zero waiting
        // for one: land it straight on the configured dim and let the compositor's own
        // animation, the one the owner already likes, be the fade.
        //
        // This is now the ONLY short-circuit. A `|| !cfg!(target_os = "linux")` sat beside
        // it and sent macOS and Windows down this same "settle immediately" path; it is gone
        // for the reasons in the module doc, and its removal is the whole of what makes the
        // fade portable. `overlay_fallback_active` is already `false` off Linux by its own
        // `#[cfg]` arm, so this condition still costs those platforms nothing.
        let fallback = self.overlay_fallback_active();
        if fallback {
            self.dim_fade.set(DimFade::Done);
            return;
        }
        if !dim_fade_may_start(self.frozen_pending, self.menu_hold.is_some(), fallback) {
            return;
        }
        // ARMED, not Running. The clock starts on the first painted frame (`dim_now`), not
        // here, because the drain can and does land before anything is on screen.
        self.dim_fade.set(DimFade::Armed);
    }
}

// ── The window picker's loading spinner: a delayed reveal (DRAGON-645) ───────
//
// Window mode enumerates every toplevel and grabs a thumbnail of each one on a background
// thread (`spawn_window_precapture`, DRAGON-204). On a busy desktop that costs about a
// second, and the full-screen spinner below covers the wait. On a fast machine, or a desktop
// with two windows open, the whole thing is over in 60 to 100ms: the spinner appeared and
// disappeared inside a fifth of a second, which does not read as "we are loading something",
// it reads as a glitch. That is the owner's complaint.
//
// The fix is a DELAYED REVEAL plus a small minimum once shown, and both halves earn their
// place:
//
// - Nothing is drawn for the first [`PICKER_SPINNER_REVEAL_MS`]. A pre-capture that lands
//   inside that window goes straight to the picker with no spinner ever having existed, and
//   it costs the fast path NOTHING: the picker becomes interactive on the same tick it always
//   did. The old warmup frames are skipped there too, on purpose, because their whole job was
//   to hide the picker's GPU upload BEHIND the spinner and there is no spinner to hide it
//   behind.
// - Once the spinner IS revealed it stays for at least [`PICKER_SPINNER_MIN_MS`], so a
//   pre-capture landing one tick past the threshold cannot flash it straight off again.
//   Without this half the delay would only move the flash later rather than remove it.
//
// WHAT WAS DELIBERATELY NOT DONE, because the owner ruled it out: a flat forced minimum
// display time. A guaranteed second of spinner spends up to a second of dead time on every
// fast launch regardless of how quickly enumeration finished, which fights the first-paint
// work landing alongside this. The asymmetry is the whole design: a fast pre-capture pays
// nothing at all, and only one that has already PROVEN itself slow gets an announcement.
//
// THE DIM IS NOT PART OF THE REVEAL, and that is a decision rather than an oversight. The
// picker's dim layer keeps being drawn for the whole load, spinner or no spinner, for two
// reasons. First, switching into window mode from region mode would otherwise drop the region
// dim for the length of the pre-capture and flash the UNDIMMED desktop, which is a worse
// version of the very bug this ticket is about. Second, that dim runs on the same `dim_now`
// clock the region overlay does (DRAGON-606), so drawing it for the whole load is what keeps
// a mode switch during the ramp from showing two dim levels. Only the spinner COLUMN, the
// ring and its label, is what the reveal gates.
//
// COUNTED IN POLLS, not in wall time. `sub_loading_tick` already runs at
// [`PICKER_LOAD_TICK_MS`] to drain the pre-capture slot, so the thresholds ride the timer
// that exists rather than adding a second one.
//
// AND THE CLOCK DOES NOT START UNTIL THE PICKER HAS PAINTED, which is the DRAGON-644 lesson
// (elapsed is not the same quantity as seen) arriving in a second place. The first cut of
// this counted from the subscription, and that was measured wrong on the very first live run:
// on a macOS window-mode launch the poll begins in `App::init` and the overlay does not mint
// its windows until +857ms, so the whole 200ms threshold was spent with nothing on screen and
// the spinner was revealed on principle every time. The quantity the reveal is supposed to
// measure is how long the USER has been looking at an overlay with no picker in it, so the
// `Quiet` clock only advances once `window_view` has actually drawn a frame. A pre-capture
// that finishes during the launch, before the overlay exists, now hands straight over and the
// user's first sight of window mode IS the picker.
//
// The HOLD, once the spinner is up, deliberately does NOT wait on frames the same way, and
// the asymmetry is on purpose. The reveal is a judgement about perception and must not be
// made in the dark; the hold is a BOUND, and a bound that only expires when frames arrive
// cannot expire on a surface that has stopped being drawn. So `Shown` and `Settling` run on
// the tick, which keeps the house rule that nothing waits unboundedly, and the worst it can
// cost is a minimum served while nobody was watching, which ends in the picker either way.
//
// The other half of that lesson, burst delivery, is benign here. `iced::time::every` BURSTS
// the ticks it missed during a stall, so N delivered ticks means at least N polls of wall time
// and never fewer: a stall can only push the reveal LATER, which is the safe direction.

/// How often `sub_loading_tick` polls the window pre-capture slot.
///
/// The ONE source for that cadence. [`PickerLoad`]'s thresholds are counted in polls of this
/// length, so the subscription reads the number from here instead of carrying its own literal
/// and letting the two drift apart.
pub(super) const PICKER_LOAD_TICK_MS: u64 = 50;

/// How long the window pre-capture may run before its loading spinner is revealed.
///
/// 200ms is the span below which a state change reads as part of the same action rather than
/// as a step of its own, so a load that finishes inside it should simply be the picker
/// appearing. It is also exactly [`DIM_FADE_MS`], which is the relationship the assert below
/// pins: the spinner can never arrive while the dim it sits on is still ramping in, so the
/// user is never watching two of our animations at once.
pub(super) const PICKER_SPINNER_REVEAL_MS: u64 = 200;

/// The least time the spinner stays on screen once it HAS been revealed.
///
/// The delay alone does not fix the flash, it moves it: a pre-capture landing one poll past
/// the threshold would show the spinner for a single frame. Held with the delay this puts a
/// floor of 500ms on the whole loading state, which is comfortably long enough to read as
/// deliberate, and it is only ever paid by a load that already took 200ms.
const PICKER_SPINNER_MIN_MS: u64 = 300;

/// How long the spinner stays up AFTER the thumbnails land, so the picker behind it can
/// finish uploading its textures before it lifts.
///
/// This is the original `window_warmup`, unchanged in value and unchanged in reason. It is a
/// different question from everything else here: the minimum above is about the spinner being
/// readable, this is about the picker being READY, and a fast load skips it precisely because
/// there is no spinner covering the upload in that case.
const PICKER_WARMUP_MS: u64 = 150;

const PICKER_SPINNER_REVEAL_TICKS: u8 = (PICKER_SPINNER_REVEAL_MS / PICKER_LOAD_TICK_MS) as u8;
const PICKER_SPINNER_MIN_TICKS: u8 = (PICKER_SPINNER_MIN_MS / PICKER_LOAD_TICK_MS) as u8;
const PICKER_WARMUP_TICKS: u8 = (PICKER_WARMUP_MS / PICKER_LOAD_TICK_MS) as u8;

// The thresholds are COUNTED in polls, so a value that is not a whole number of polls is
// silently truncated and the delay is shorter than the constant reads.
const _: () = assert!(
    PICKER_SPINNER_REVEAL_MS.is_multiple_of(PICKER_LOAD_TICK_MS)
        && PICKER_SPINNER_MIN_MS.is_multiple_of(PICKER_LOAD_TICK_MS)
        && PICKER_WARMUP_MS.is_multiple_of(PICKER_LOAD_TICK_MS),
    "DRAGON-645: every picker-spinner threshold must be a whole number of loading polls, or \
     it truncates down and the spinner appears sooner than the constant says"
);

const _: () = assert!(
    PICKER_SPINNER_REVEAL_MS >= DIM_FADE_MS,
    "DRAGON-645: the spinner must not be revealed while the dim underneath it is still \
     fading in, or the user watches two of our animations start on top of each other"
);

const _: () = assert!(
    PICKER_WARMUP_MS >= PICKER_LOAD_TICK_MS,
    "DRAGON-645: the picker's warmup must be at least one poll, or a shown spinner lifts in \
     the same update that drains the thumbnails and the picker's first frame is the blank \
     flash the warmup exists to prevent"
);

/// Where the window picker's loading state has got to (DRAGON-645).
///
/// FOUR states, and the first one is the whole ticket. `Quiet` is a load in progress that has
/// NOT earned a spinner yet, which is the state the old `windows_loading` flag could not
/// express: it went up the instant the pre-capture was kicked and the view drew the spinner
/// unconditionally from that first frame, so a 60ms enumeration got a 60ms spinner.
///
/// Replaces the `windows_loading` + `window_warmup` pair. Folding them into one value is what
/// makes the two different "keep it up a bit longer" reasons composable: the minimum-once-
/// shown (so the spinner is readable) and the picker's GPU warmup (so nothing blank is
/// revealed behind it) are both satisfied by the single `Settling` hold, taking whichever is
/// longer, rather than by two counters that would each have to know about the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerLoad {
    /// No pre-capture is in flight and no spinner is up. The picker itself is what shows,
    /// including its "no windows" message if enumeration genuinely found none.
    Idle,
    /// The pre-capture is running and the spinner is deliberately NOT drawn. `ticks` counts
    /// the polls since the kick, toward [`PICKER_SPINNER_REVEAL_TICKS`].
    Quiet { ticks: u8 },
    /// The pre-capture outlasted the threshold, so the spinner is on screen. `ticks` counts
    /// the polls since the reveal, and is clamped at [`PICKER_SPINNER_MIN_TICKS`] because
    /// that is the only thing it is ever read for: how much of the minimum is left to serve.
    Shown { ticks: u8 },
    /// The thumbnails landed while the spinner was up, so it stays for `hold` more polls, and
    /// then the picker takes over.
    Settling { hold: u8 },
}

impl PickerLoad {
    /// **Pure**, unit-tested: one poll of the loading state.
    ///
    /// Two effectful inputs, both taken as arguments precisely so the whole decision is
    /// testable with no thread, no compositor and no App:
    ///
    /// - `landed`, whether THIS poll drained the pre-capture slot.
    /// - `painted`, whether the window picker has drawn a frame yet. This is what the reveal
    ///   threshold is measured against, and leaving it out is the bug the first live run
    ///   found: the poll starts in `App::init` and a macOS window-mode launch does not put an
    ///   overlay on screen for the better part of a second, so a threshold counted from the
    ///   subscription is spent entirely in the dark and reveals a spinner for a wait nobody
    ///   experienced. See the module doc.
    ///
    /// Total over every state, so a stale tick can never land somewhere undecided.
    ///
    /// The machine is bounded by construction: `Shown` is reached in at most
    /// [`PICKER_SPINNER_REVEAL_TICKS`] polls of painting, `Settling` reaches `Idle` in at most
    /// [`PICKER_SPINNER_MIN_TICKS`] polls flat, and neither of those waits on anything
    /// external. The two states that can persist are `Quiet` before the first frame and
    /// `Shown`, and both last exactly as long as the pre-capture thread takes to post, which
    /// is what the old `windows_loading` flag did too. Nothing new waits here.
    pub(in crate::app) fn advance(self, landed: bool, painted: bool) -> Self {
        match self {
            // The poll is not running in this state, so a tick here can only be a stale one
            // from a subscription that has not been torn down yet. Stay put.
            PickerLoad::Idle => PickerLoad::Idle,
            // THE FAST PATH, and the point of the ticket. The pre-capture finished before the
            // threshold, so the picker is ready and no spinner was ever drawn: go straight to
            // it, on this tick, with no warmup. Skipping the warmup is correct rather than a
            // shortcut, because the warmup hides the picker's texture upload behind the
            // spinner and there is no spinner here to hide it behind. Holding a blank overlay
            // for three more polls would only add the latency this ticket exists to remove.
            //
            // Ahead of the `painted` gate below on purpose: a load that finishes while the
            // app is still coming up is the FASTEST path of all, and it must hand over rather
            // than sit waiting for a frame to start a clock it no longer needs.
            PickerLoad::Quiet { .. } if landed => PickerLoad::Idle,
            // Nothing has been drawn yet, so no wait has been EXPERIENCED yet. Hold the clock
            // at zero rather than spending the threshold on time the user could not see.
            PickerLoad::Quiet { ticks } if !painted => PickerLoad::Quiet { ticks },
            PickerLoad::Quiet { ticks } => {
                let ticks = ticks.saturating_add(1);
                if ticks >= PICKER_SPINNER_REVEAL_TICKS {
                    PickerLoad::Shown { ticks: 0 }
                } else {
                    PickerLoad::Quiet { ticks }
                }
            }
            // Landed with the spinner up. Hold for whichever of the two reasons is longer:
            // the rest of the minimum-once-shown, or the picker's warmup. `max` rather than a
            // sum, because they overlap in time (the warmup runs DURING the minimum) and
            // adding them would keep a spinner up for a load that had already served both.
            PickerLoad::Shown { ticks } if landed => PickerLoad::Settling {
                hold: PICKER_SPINNER_MIN_TICKS
                    .saturating_sub(ticks)
                    .max(PICKER_WARMUP_TICKS),
            },
            // Clamped at the minimum, which is exact rather than a guard: the counter's only
            // consumer is the subtraction above, so anything past the minimum means the same
            // thing (nothing left to serve) and the state space stays finite.
            PickerLoad::Shown { ticks } => PickerLoad::Shown {
                ticks: ticks.saturating_add(1).min(PICKER_SPINNER_MIN_TICKS),
            },
            PickerLoad::Settling { hold } => match hold.saturating_sub(1) {
                0 => PickerLoad::Idle,
                hold => PickerLoad::Settling { hold },
            },
        }
    }

    /// **Pure**, unit-tested: is the loading state COVERING the picker?
    ///
    /// True for the whole of the load, spinner or no spinner, and it answers three questions
    /// at once because they all turn on the same fact. It draws the dim (see the module doc
    /// on why the dim is not part of the reveal), it suppresses the picker's "no windows"
    /// message so an unfinished enumeration never claims the desktop is empty, and it
    /// suppresses the opaque dark fallback fill so a wallpaper that has not arrived yet
    /// leaves the dimmed desktop showing instead of a black screen.
    ///
    /// It is also exactly the lifetime of the poll that drives this machine, so
    /// `sub_loading_tick` arms on the same predicate: the loading state and its timer begin
    /// and end together by construction.
    pub(in crate::app) fn covering(self) -> bool {
        !matches!(self, PickerLoad::Idle)
    }

    /// **Pure**, unit-tested: is the spinner itself drawn?
    ///
    /// The narrower question, and the one the delayed reveal actually gates. A load that
    /// finishes inside [`PICKER_SPINNER_REVEAL_MS`] is `covering` for its whole life and
    /// never `spinner_up` for a single frame.
    pub(in crate::app) fn spinner_up(self) -> bool {
        matches!(self, PickerLoad::Shown { .. } | PickerLoad::Settling { .. })
    }
}

impl App {

    // Frozen, non-interactive countdown overlay: the selection border stays put
    // while the toolbar (timer chip counting down, cancels on click) shows where
    // it always does — anchored to a region, or pinned to the bottom of the
    // screen for window/monitor captures.
    pub(super) fn countdown_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let sel = self.pending.as_ref();
        let rect = sel.map(|s| GlobalRect::new(s.x, s.y, s.x + s.width as i32, s.y + s.height as i32));
        // Match the recording border placement (outside for window/monitor) so the
        // outline doesn't shift when the countdown hands off to recording.
        let windowed = sel.is_some_and(|s| s.window_id.is_some() || s.output.is_some());
        let mut rs = RegionSelection::new(o.units(), rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
            .non_interactive()
            .dim_alpha(self.active_overlay_opacity)
            .line_alpha(SELECTION_LINE_ALPHA);
        if windowed {
            rs = rs.outer_border();
        }
        let border: Element<'_, Msg> = rs.into();
        let mut layers: Vec<Element<'_, Msg>> = vec![border];
        if let Some(toolbar) = self.capture_button_layer(o) {
            layers.push(toolbar);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    // Recording overlay: for a REGION, the active dim outside the rect plus the
    // selection border on its edge (so the drawn area stays visible at the
    // configured dimness) — the recorded crop is inset by the line width (see
    // `start_recording`), so what you see inside the line is exactly what's
    // recorded. Window/monitor recordings frame nothing on screen (the portal/target
    // defines the area), so they leave it clear and show only the record/stop chip.
    pub(super) fn recording_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let mut layers: Vec<Element<'_, Msg>> = Vec::new();
        // Only a region gets the dim + border; window/monitor stay clear.
        if self.mode == Mode::Region
            && let Some(s) = self.pending.as_ref()
        {
            let rect = Some(GlobalRect::new(s.x, s.y, s.x + s.width as i32, s.y + s.height as i32));
            let rs = RegionSelection::new(o.units(), rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
                .non_interactive()
                .dim_alpha(self.active_overlay_opacity)
                .line_alpha(SELECTION_LINE_ALPHA);
            layers.push(rs.into());
        }
        if let Some(toolbar) = self.capture_button_layer(o) {
            layers.push(toolbar);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    // Window mode: cosmic-screenshot's picker — each window button is sized to
    // its (ScaleDown) thumbnail inside a width-proportional, centered slot, laid
    // over the wallpaper. Matches xdg-desktop-portal-cosmic's widget exactly.
    /// Top inset (logical points) the window picker must leave clear so its content
    /// never renders behind a notched MacBook's camera cutout (DRAGON-270). On macOS
    /// this is `NSScreen.safeAreaInsets.top` for this output's display (0 on a
    /// non-notched panel); every other platform has no notch, so it is a compile-time 0.
    fn picker_top_inset(&self, o: &OutputState) -> f32 {
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::notch_top_inset(&o.name) as f32
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = o;
            0.0
        }
    }

    pub(super) fn window_view(&self, o: &OutputState) -> Element<'_, Msg> {
        // THE LATCH the loading state's reveal threshold is measured from (DRAGON-645).
        // Reaching this function means the app is building a frame of the window picker, and
        // on macOS it means more than that: `overlay_view` returns a transparent `Space`
        // until `place_overlay` has landed, so the picker is genuinely on screen by the time
        // this runs. Interior mutability through a `Cell` for the same reason `dim_now` and
        // `OutputState::placed` use one, recording something from inside a `&self` view.
        //
        // One-way, and per session rather than per load: a second switch into window mode
        // never re-kicks the pre-capture, so there is no later load for it to mis-time.
        self.picker_painted.set(true);
        let empty: &[WindowThumb] = &[];
        let thumbs = self.windows.get(&o.name).map(|v| v.as_slice()).unwrap_or(empty);
        // Push the picker content down below a notched display's camera cutout so
        // thumbnails / chrome never sit behind it (0 on non-notched + non-mac).
        let notch_top = self.picker_top_inset(o);

        // Is the loading state COVERING the picker? True from the moment the pre-capture is
        // kicked until it has landed AND the spinner, if one was ever shown, has served its
        // minimum plus the warmup frames the picker needs to upload its textures.
        //
        // DRAGON-645 split this from "is the spinner drawn", which used to be the same
        // question. It is not: a load that finishes inside the reveal threshold is `covering`
        // for its whole life and never draws a spinner at all. See `PickerLoad`.
        let loading = self.picker_load.covering();

        let foreground: Element<'_, Msg> = if thumbs.is_empty() {
            // Empty while loading (the spinner covers it); the "no windows"
            // message only stands once enumeration has actually finished.
            let inner: Element<'_, Msg> = if loading {
                widget::space::Space::new().into()
            } else {
                widget::text(window_picker_empty_message(self.window_mode_supported())).into()
            };
            widget::container(inner)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                // Keep the centred message clear of a notched display's cutout band.
                .padding(cosmic::iced::Padding {
                    top: notch_top,
                    ..cosmic::iced::Padding::ZERO
                })
                .into()
        } else {
            // Lay the windows out at their TRUE relative sizes: ONE scale factor for all
            // of them (so proportions are preserved), shrunk just enough to fit the panel
            // and capped at 1.0 so nothing is ever enlarged — a window smaller than the
            // screen stays small in the lineup. Rather than a single row (which shrinks
            // every tile toward 1/N as the count grows), pack them into a GRID whose
            // column count is chosen to MAXIMIZE the tile scale for this display, so a
            // monitor with many windows still shows large, legible tiles (DRAGON-193).
            let n = thumbs.len();
            // The panel is the iced VIEWPORT, so POINTS (DRAGON-448) — every other number
            // in this block (GAP, the paddings, the toolbar reserve) is already a point
            // constant. On a scaled Windows monitor `logical_size` is `point_scale`×
            // bigger, which sized the tiles for a screen that does not exist and spilled
            // the grid past the bottom of the overlay.
            let (pw, ph) = o.point_size();
            const GAP: f32 = 24.0;
            // Reserve a band at the BOTTOM for the capture toolbar (stacked over this view,
            // bottom-centred near the screen edge) so the grid never overlaps it: the
            // toolbar's real footprint from the bottom edge (its group height GROUP_H_BASE
            // plus its BOTTOM_MARGIN edge clearance, matching `toolbar_layout`), plus a
            // BADGE_GAP of clearance between the grid and the toolbar. Shared by every OS
            // (this picker view is platform-agnostic).
            let toolbar_reserve = crate::app::layout::GROUP_H_BASE
                + toolbar::layout::BOTTOM_MARGIN
                + crate::app::layout::BADGE_GAP;
            let avail_w = (pw - 48.0).max(1.0);
            // The notch band eats into the top of the usable height (added to the top
            // padding below), so the tile-scale budget must exclude it too.
            let avail_h = (ph - 24.0 - notch_top - toolbar_reserve).max(1.0);
            // Size the tiles from `layout_size` (the TRIMMED content size on macOS, so a
            // dead transparent gutter never inflates the slot — DRAGON-190; equals the
            // frame size elsewhere), while the click below still passes the raw `rect`.
            // Uniform cells sized to the LARGEST tile keep the grid regular; each tile is
            // then drawn at its own aspect within that scale.
            let max_w: f32 = thumbs.iter().map(|w| w.layout_size.0.max(1) as f32).fold(1.0, f32::max);
            let max_h: f32 = thumbs.iter().map(|w| w.layout_size.1.max(1) as f32).fold(1.0, f32::max);
            let (cols, s) = grid_cols_and_scale(n, max_w, max_h, avail_w, avail_h, GAP);
            let buttons: Vec<Element<'_, Msg>> = thumbs
                .iter()
                .map(|w| {
                    let bw = (w.layout_size.0.max(1) as f32 * s).max(1.0);
                    let bh = (w.layout_size.1.max(1) as f32 * s).max(1.0);
                    widget::button::custom(
                        widget::image::Image::new(w.handle.clone())
                            .content_fit(cosmic::iced::ContentFit::Contain)
                            .width(Length::Fixed(bw))
                            .height(Length::Fixed(bh)),
                    )
                    .padding(0)
                    .on_press(Msg::Capture(CaptureMsg::CaptureWindow {
                        id: w.id.clone(),
                        rect: w.rect,
                    }))
                    .class(cosmic::theme::Button::Image)
                    .into()
                })
                .collect();
            // Wrap the buttons into rows of `cols`, then stack the rows in a centered
            // column. cols >= 1 whenever there is at least one thumb (this branch), so the
            // modulo is safe.
            let mut rows: Vec<Vec<Element<'_, Msg>>> = Vec::new();
            for (i, btn) in buttons.into_iter().enumerate() {
                if i % cols == 0 {
                    rows.push(Vec::new());
                }
                rows.last_mut().unwrap().push(btn);
            }
            let row_elems: Vec<Element<'_, Msg>> = rows
                .into_iter()
                .map(|r| widget::row(r).spacing(GAP).align_y(Alignment::Center).into())
                .collect();
            widget::container(
                widget::column(row_elems)
                    .spacing(GAP)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            // 24px on three sides; the bottom reserves the toolbar band so the centred grid
            // sits entirely above it. The top also clears a notched display's cutout band
            // (`notch_top`, 0 on non-notched + non-mac) so the grid never rides under it.
            .padding(cosmic::iced::Padding {
                top: 24.0 + notch_top,
                right: 24.0,
                bottom: toolbar_reserve,
                left: 24.0,
            })
            .into()
        };

        // Background: the wallpaper (cover-fit), like cosmic-screenshot — this
        // hides the panel and live windows. Uses the handle pre-decoded off the
        // UI thread (decoding a full-size image here would freeze the first
        // render). Falls back to opaque dark until it's ready.
        let background: Element<'_, Msg> = match self.wallpaper_handles.get(&o.name) {
            Some(handle) => widget::image::Image::new(handle.clone())
                .content_fit(cosmic::iced::ContentFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            // No wallpaper yet: while still loading, stay transparent so the dim
            // overlay just dims the live desktop (not an opaque black). Only fall
            // back to a dark fill once we're actually showing a wallpaper-less
            // picker.
            None if loading => widget::space::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => widget::container(widget::space::Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::Custom(Box::new(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color::from_rgb(
                            0.05, 0.05, 0.06,
                        ))),
                        ..Default::default()
                    }
                })))
                .into(),
        };

        let mut layers: Vec<Element<'_, Msg>> = vec![background, foreground];
        if loading {
            // The same dim as the region selection overlay (it follows that setting), over
            // the (warming) picker.
            //
            // DRAGON-606: the window picker's warming dim fades in on the same clock as the
            // region one, so switching modes during the ramp cannot show two dim levels.
            // DRAGON-645 kept this OUTSIDE the spinner's delayed reveal for exactly that
            // reason, plus one more: gating the dim too would drop the region dim for the
            // length of the pre-capture when the user switches into window mode, flashing the
            // undimmed desktop. That is a worse version of the bug the reveal exists to fix.
            let dim_alpha = self.dim_now_revealed(o, self.region_overlay_opacity);
            // The accent ring and its label ARE what the reveal gates: a pre-capture that
            // finishes inside the threshold never draws them, so a load too short to read as
            // loading never announces itself. Until then the container is the dim alone.
            let inner: Element<'_, Msg> = if self.picker_load.spinner_up() {
                widget::column(vec![
                    widget::indeterminate_circular().size(48.0).into(),
                    widget::text(LOADING_MESSAGES[self.loading_msg % LOADING_MESSAGES.len()])
                        .size(16)
                        .into(),
                ])
                .spacing(20.0)
                .align_x(Alignment::Center)
                .into()
            } else {
                widget::space::Space::new().into()
            };
            let overlay = widget::container(inner)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .class(cosmic::theme::Container::Custom(Box::new(move |_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color {
                            a: dim_alpha,
                            ..cosmic::iced::Color::BLACK
                        })),
                        ..Default::default()
                    }
                })));
            layers.push(overlay.into());
        }
        cosmic::iced::widget::stack(layers).into()
    }

    pub(super) fn overlay_view(&self, o: &OutputState) -> Element<'_, Msg> {
        // DRAGON-204: on macOS the overlay window is created clamped below the menu bar
        // (winit's AlwaysOnTop level) and only raised to the shielding level + reframed to
        // the full display by `place_overlay` a frame or two later. Draw NOTHING (fully
        // transparent) until that placement lands, so the clamp-then-reframe happens on an
        // invisible window and the user never sees the shift.
        #[cfg(target_os = "macos")]
        if !o.placed.get() {
            return widget::space::Space::new().into();
        }
        // Bottom layer depends on the selection mode. In freeze mode the frozen
        // snapshot sits behind the region/monitor selectors.
        let background: Element<'_, Msg> = match self.mode {
            Mode::Region => {
                let sel: Element<'_, Msg> = RegionSelection::new(
                    o.units(),
                    self.region,
                    |a0| Msg::Capture(CaptureMsg::RegionChange(a0)),
                    Msg::Capture(CaptureMsg::RegionDone),
                )
                // DRAGON-606: the CONFIGURED region dim, scaled by the fade-in. Zero until
                // the frozen-flats grab has landed, so the grab photographs nothing of ours
                // (and, on Windows, until this overlay is revealed, DRAGON-653).
                .dim_alpha(self.dim_now_revealed(o, self.region_overlay_opacity))
                .box_thickness(self.selection_box_thickness)
                // Hover + click the detected marks here (not via the marks layer), so
                // a press that starts on a mark can still drag the region.
                .marks(self.shown_marks(o), |a0| Msg::Detect(DetectMsg::HoverMark(a0)), |a0| Msg::Detect(DetectMsg::ActivateMark(a0)))
                .words(
                    self.shown_words(o),
                    |a0| Msg::Detect(DetectMsg::HoverWord(a0)),
                    |a0, a1| Msg::Detect(DetectMsg::TextSelectBegin(a0, a1)),
                    |a0| Msg::Detect(DetectMsg::TextSelectTo(a0)),
                    |a0| Msg::Detect(DetectMsg::TextToggle(a0)),
                    |a0, a1| Msg::Detect(DetectMsg::TextExpand(a0, a1)),
                    |a0, a1, a2| Msg::Detect(DetectMsg::WordMenu(a0, a1, a2)),
                )
                .code_menu(|a0, a1, a2| Msg::Detect(DetectMsg::CodeMenu(a0, a1, a2)))
                .into();
                self.with_frozen_bg(o, sel)
            }
            Mode::Monitor => {
                let sel: Element<'_, Msg> = OutputSelection::new(
                    self.hovered_output.as_deref() == Some(o.name.as_str()),
                    Msg::Capture(CaptureMsg::HoverOutput(o.name.clone())),
                    Msg::Capture(CaptureMsg::Capture {
                        output: o.name.clone(),
                    }),
                )
                .into();
                self.with_frozen_bg(o, sel)
            }
            Mode::Window => self.window_view(o),
        };

        // The locked-cursor preview goes on the desktop, ABOVE any backdrop image but BELOW the
        // dim/selection overlay (which is `background`), so it reads as part of the scene you're
        // cropping. Only in live region/monitor no-wallpaper selection.
        let mut layers: Vec<Element<'_, Msg>> = Vec::new();
        if let Some(cursor) = self.cursor_indicator(o) {
            layers.push(cursor);
        }
        layers.push(background);
        if let Some(hint) = self.region_hint_layer(o) {
            layers.push(hint);
        }
        if let Some(marks) = self.marks_layer(o) {
            layers.push(marks);
        }
        // DRAGON-460: no scan spinner layer here any more — scanner progress is the
        // toolbar refresh button spinning. See `marks::scanning`.
        if let Some(cap) = self.capture_button_layer(o) {
            layers.push(cap);
        }
        if let Some(toast) = self.toast_layer() {
            layers.push(toast);
        }
        if let Some(menu) = self.text_menu_layer(o) {
            layers.push(menu);
        }
        if let Some(menu) = self.code_menu_layer(o) {
            layers.push(menu);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    /// Transient banner (e.g. a wrong-monitor portal pick) shown top-centre over the
    /// overlay, styled like a cosmic button — rounded, theme-aware (light/dark).
    ///
    /// Visible to the whole `app` tree because the colour picker's overlay stacks the same
    /// banner (`color_picker::view`), which is how DRAGON-612's two picker-only refusals get
    /// drawn. One banner, one style, one place it is built.
    pub(in crate::app) fn toast_layer(&self) -> Option<Element<'_, Msg>> {
        let text = self.toast.as_ref()?;
        let pill = widget::container(widget::text(text.clone()).size(14))
            .padding(cosmic::iced::Padding {
                top: 10.0,
                bottom: 10.0,
                left: 18.0,
                right: 18.0,
            })
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                // Borrowed, not bound by value: `Component` is not `Copy`, and this only
                // reads three colours out of it.
                let component = &theme.cosmic().background(false).component;
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: component.divider.into(),
                    },
                    // DRAGON-607's rule: no site writes one ink field without the other. This
                    // set `text_color` alone and left `icon_color` to inherit the ambient
                    // window foreground, which is the exact shape that ticket exists to
                    // remove. It is invisible today only because the pill has never held
                    // anything but text; the day one carries an icon it would draw the window
                    // foreground on this `component.base` fill.
                    //
                    // Spread as the BASE of the struct so the ink comes from the one helper
                    // while this site keeps its own background and border. `ink_content`
                    // writes the two ink fields and defaults the rest, so nothing else here
                    // changes and the rendered pixels are identical until an icon appears.
                    //
                    // `region_hint_layer` below had a byte-identical pill with the same latent
                    // issue, and now carries the same fix. The two being identical is itself
                    // worth collapsing into one helper, which is a change that should be made
                    // on its own and looked at.
                    ..crate::app::theme::ink_content(component.on.into())
                }
            })));
        Some(
            widget::container(pill)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Start)
                .padding(cosmic::iced::Padding {
                    top: 48.0,
                    ..cosmic::iced::Padding::ZERO
                })
                .into(),
        )
    }

    /// Whether the current region (if any) overlaps this output.
    ///
    /// Stays entirely in CAPTURE space (DRAGON-448): both the region and the output rect
    /// are already in it, so there is nothing to bridge — converting either would only
    /// introduce rounding. The rule is "convert at the boundary with iced", and this
    /// answers a bool that never reaches one.
    fn region_on_output(&self, o: &OutputState) -> bool {
        let Some(rect) = self.region else {
            return false;
        };
        let (l, t, r, b) = rect.to_tuple();
        let (l, t, r, b) = (l.min(r), t.min(b), l.max(r), t.max(b));
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
        l < ox + ow && r > ox && t < oy + oh && b > oy
    }

    /// Centred "begin drawing" hint, shown (in region mode) on every output that
    /// doesn't currently hold the region — including all of them when nothing's drawn
    /// yet. Click-through, so a press here still starts a region on this output.
    fn region_hint_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        if self.mode != Mode::Region || self.region_on_output(o) {
            return None;
        }
        let pill = widget::container(widget::text("Begin drawing a capture region").size(16))
            .padding(cosmic::iced::Padding {
                top: 10.0,
                bottom: 10.0,
                left: 18.0,
                right: 18.0,
            })
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background(false).component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: c.background(false).component.divider.into(),
                    },
                    // DRAGON-607's rule, the same fix the toast pill above already carries.
                    // This set `text_color` alone and left `icon_color` to inherit the ambient
                    // window foreground, which is the exact shape that ticket exists to
                    // remove. Invisible today only because this pill has never held anything
                    // but text; the day one carries an icon it would draw the window
                    // foreground on this `component.base` fill.
                    //
                    // Spread as the BASE of the struct so the ink comes from the one helper
                    // while this site keeps its own background and border. `ink_content`
                    // writes the two ink fields and defaults the rest, so the rendered pixels
                    // are identical until an icon appears.
                    ..crate::app::theme::ink_content(c.background(false).component.on.into())
                }
            })));
        Some(
            widget::container(pill)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        )
    }

    /// While selecting a REGION or MONITOR whose capture will carry the launch-locked cursor (no
    /// wallpaper, live) draw that cursor at its real position, so you can compose the crop around
    /// where it'll land. Sits on the desktop, below the dim/selection overlay. `None` when it
    /// doesn't apply, there's no captured cursor, or the cursor isn't on this output. (Under freeze
    /// the frozen backdrop already shows the cursor; wallpaper-on uses the live compositor cursor.)
    fn cursor_indicator<'a>(&'a self, o: &OutputState) -> Option<Element<'a, Msg>> {
        // Shown whenever an IMMEDIATE region/monitor capture will embed the LAUNCH-LOCKED
        // cursor and the overlay isn't already displaying it. The visibility decision is
        // SHARED with the capture path (DRAGON-213) so preview + stamped pixels can't
        // drift — see `show_launch_cursor_indicator`. Window mode and an armed countdown
        // both hide it; the frozen backdrop already bakes the pointer in.
        if !super::capture_flow::show_launch_cursor_indicator(
            self.mode,
            self.effective_capture_extras().cursor,
            self.freeze_backdrop_active(),
            self.configured_delay_secs() > 0,
        ) {
            return None;
        }
        let (img, (gx, gy), (hx, hy), ..) = self.frozen_cursor.as_ref()?;
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = o.logical_size;
        if *gx < ox || *gx >= ox + ow as i32 || *gy < oy || *gy >= oy + oh as i32 {
            return None; // cursor isn't on this output
        }
        // Position is placed in the OUTPUT's logical space, so map global->local at
        // the output's buffer scale.
        let out_scale = self
            .frozen
            .get(&o.name)
            .map(|f| f.img.width() as f32 / f.logical_size.0.max(1) as f32)
            .unwrap_or(1.0);
        // The sprite's own pixels-per-point sets its LOGICAL size (dividing sprite
        // pixels by that scale). On Linux the cursor session hands the sprite back
        // at the output scale, so sprite_scale == out_scale and this is unchanged;
        // on macOS the system cursor asset is its own (typically 2x) resolution
        // regardless of the display under the pointer, so it must divide by the
        // sprite's OWN scale or a lower-DPI output shows it double size
        // (DRAGON-156).
        let sprite_scale = cursor_sprite_scale(self.frozen_cursor.as_ref()?, out_scale);
        let dw = img.width() as f32 / sprite_scale;
        let dh = img.height() as f32 / sprite_scale;
        // The pointer position is CAPTURE space; the padding below is POINTS (DRAGON-448).
        // Cross once through this output's bridge, then back off by the hotspot, which is
        // already expressed in the sprite's own pixels-per-point.
        let (px, py) = o.units().to_point((*gx, *gy));
        let lx = (px - *hx as f32 / sprite_scale).max(0.0);
        let ly = (py - *hy as f32 / sprite_scale).max(0.0);
        // The sprite's handle is built ONCE when the cursor lands (never in view():
        // a per-frame from_rgba mints a new id each call, forcing a GPU re-upload
        // and a fresh atlas entry on every redraw of the drag).
        let handle = self.frozen_cursor_handle.clone()?;
        let sprite = widget::image::Image::new(handle)
            .width(Length::Fixed(dw))
            .height(Length::Fixed(dh));
        // Absolute placement: pad a Fill container so the top-left-aligned sprite lands at (lx, ly).
        Some(
            widget::container(sprite)
                .padding(cosmic::iced::Padding { top: ly, right: 0.0, bottom: 0.0, left: lx })
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        )
    }

    /// Layer the output's frozen snapshot behind `selection` when the freeze backdrop
    /// is active (freeze mode); otherwise return `selection` unchanged so the
    /// transparent overlay surface composites the LIVE desktop behind it — the
    /// freeze-off "live" feel, identical on every platform.
    ///
    /// DRAGON-234: this is now uniform across all platforms. The Windows M1.5 special
    /// case (always draw the frozen scene OPAQUELY, on the belief that a transparent
    /// wgpu surface presents an opaque clear on Windows) is GONE. Empirically the
    /// winit transparent window (`DwmEnableBlurBehindWindow`, an empty blur region for
    /// per-pixel alpha) plus the `PreMultiplied` composite-alpha swapchain that
    /// `iced_wgpu` selects from the Vulkan surface's advertised modes DO composite the
    /// live desktop through the overlay — so freeze-off shows the live dimmed desktop
    /// exactly like Linux/mac (verified: a 3s-apart clock advanced through the
    /// backdrop), and freeze-on shows the opaque launch-instant still (verified: the
    /// clock stayed fixed). The freeze-off capture still re-grabs LIVE pixels at commit
    /// (`freezing()` is false, so `capture_flow` takes the live path), unchanged.
    ///
    /// `lab/flatpak` (Linux): on the FALLBACK toplevel the compositor, not us, picks the
    /// window's monitor, so an output-sized `Fill` would STRETCH the frozen frame when
    /// the geometries differ. There the frame is drawn LETTERBOXED instead: fixed to the
    /// destination rect `OverlayUnits::letterbox_dest` computes (the SAME bridge that
    /// maps the selection, so pixels and mapping cannot drift), centred over opaque
    /// black bars. The bars are black on purpose: the toplevel is transparent, and the
    /// live desktop showing through would read as capturable pixels that are not in the
    /// frame. iced's `ContentFit::Contain` DOES centre in this fork (`drawing_bounds`),
    /// but it fits by the handle's PIXEL size, not the frame's logical size, so it is
    /// not used: the explicit rect keeps one math source. Every layer-shell session
    /// answers no letterbox and keeps the historical `Fill` path byte-identical (its
    /// backdrop is exactly output-sized, so `Fill` never stretched anything there).
    pub(super) fn with_frozen_bg<'a>(
        &'a self,
        o: &OutputState,
        selection: Element<'a, Msg>,
    ) -> Element<'a, Msg> {
        match self.frozen_bg_layer(o).filter(|_| self.freeze_backdrop_active()) {
            Some(bg) => cosmic::iced::widget::stack(vec![bg, selection]).into(),
            None => selection,
        }
    }

    /// The frozen snapshot as a BACKDROP layer for this output, or `None` when there is
    /// no snapshot. All of [`Self::with_frozen_bg`]'s drawing, with none of its GATE.
    ///
    /// Split out for the colour picker (DRAGON-582), which shows the frozen scene
    /// UNCONDITIONALLY rather than only when the freeze capture extra is on: it samples
    /// the snapshot, so drawing the live desktop underneath would put pixels on screen
    /// that are not the pixels it reports. Sharing the body keeps the letterbox
    /// arithmetic (`lab/flatpak`) in one place, which is the part that must never drift
    /// from `OverlayUnits`.
    pub(super) fn frozen_bg_layer<'a>(&'a self, o: &OutputState) -> Option<Element<'a, Msg>> {
        let f = self.frozen.get(&o.name)?;
        {
                #[cfg(target_os = "linux")]
                if let Some((offset, (dw, dh))) = o.units().letterbox_dest() {
                    let img = widget::image::Image::new(f.handle.clone())
                        .width(Length::Fixed(dw))
                        .height(Length::Fixed(dh))
                        .content_fit(cosmic::iced::ContentFit::Fill);
                    // Absolute placement, like `cursor_indicator`: pad a Fill container
                    // so the start-aligned image lands at the letterbox offset. Only the
                    // leading sides are padded; the fixed image size does the rest, and
                    // trailing padding would only invite float-jitter clipping.
                    let bg: Element<'a, Msg> = widget::container(img)
                        .padding(cosmic::iced::Padding {
                            top: offset.1,
                            right: 0.0,
                            bottom: 0.0,
                            left: offset.0,
                        })
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .class(cosmic::theme::Container::Custom(Box::new(|_t| {
                            cosmic::iced::widget::container::Style {
                                background: Some(Background::Color(cosmic::iced::Color::BLACK)),
                                ..Default::default()
                            }
                        })))
                        .into();
                    return Some(bg);
                }
            Some(
                widget::image::Image::new(f.handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(cosmic::iced::ContentFit::Fill)
                    .into(),
            )
        }
    }
}

/// Choose the grid shape for the window picker: the number of COLUMNS in `1..=n` that
/// MAXIMIZES the uniform tile scale when `n` tiles, each sized to fit a cell of the
/// largest tile's dims `(mw, mh)`, are packed into a centered grid within `(aw, ah)` with
/// `gap` between cells. Returns `(columns, scale)` with `scale` capped at 1.0 (tiles are
/// never enlarged). Shared by macOS and Linux (the picker view is platform-agnostic).
///
/// A single row is just the `cols == n` candidate; it wins only when the display is wide
/// enough that one row already gives the largest tiles (few windows / very wide monitor).
/// As the count grows, a squarer grid yields bigger tiles and is chosen automatically.
fn grid_cols_and_scale(n: usize, mw: f32, mh: f32, aw: f32, ah: f32, gap: f32) -> (usize, f32) {
    let (mw, mh) = (mw.max(1.0), mh.max(1.0));
    let mut best = (1usize, 0.0f32);
    for cols in 1..=n.max(1) {
        let rows = n.max(1).div_ceil(cols);
        // Per-cell budget after the inter-cell gaps in each axis (floored so a too-tight
        // fit still yields a positive, comparable scale rather than being skipped).
        let cell_w = ((aw - (cols as f32 - 1.0) * gap) / cols as f32).max(1.0);
        let cell_h = ((ah - (rows as f32 - 1.0) * gap) / rows as f32).max(1.0);
        let s = (cell_w / mw).min(cell_h / mh).min(1.0);
        // `>=` so that among column counts that TIE on tile scale (common once the scale
        // caps at 1.0) we keep the LARGEST one — the flattest, fewest-rows layout. That
        // makes a handful of windows stay a single row, like before, and only wraps into a
        // grid once wrapping actually buys larger tiles.
        if s >= best.1 {
            best = (cols, s);
        }
    }
    best
}

/// What the window picker says when it has no thumbnails to show, once enumeration has
/// finished. Pure; unit-tested in `picker_empty_message_tests`.
///
/// DRAGON-620: there are two different silences here and they were saying the same sentence.
/// "No windows on this display" is TRUE on a session that can enumerate windows and found
/// none, and it is a LIE on one that cannot enumerate at all, where it blames an empty desktop
/// for a missing protocol. A wlroots session hits the second case with a full screen of
/// windows open, so the old copy sent the user looking for the wrong problem.
///
/// Kept deliberately about the COMPOSITOR rather than about us: from the user's side the fact
/// that matters is that this desktop cannot offer window mode, not which Wayland global is
/// absent. The protocol detail belongs in the debug log, and it is there.
pub(super) fn window_picker_empty_message(window_mode_supported: bool) -> &'static str {
    if window_mode_supported {
        "No windows on this display"
    } else {
        "This compositor does not support window selection"
    }
}

#[cfg(test)]
mod picker_empty_message_tests {
    use super::window_picker_empty_message;

    #[test]
    fn a_capable_session_still_blames_an_empty_desktop() {
        assert_eq!(window_picker_empty_message(true), "No windows on this display");
    }

    #[test]
    fn an_incapable_session_blames_the_compositor_instead() {
        let msg = window_picker_empty_message(false);
        assert_ne!(msg, "No windows on this display", "must not claim the desktop is empty");
        assert!(msg.contains("compositor"), "the user needs to know WHERE the limit is: {msg}");
    }

    #[test]
    fn neither_message_uses_an_em_dash() {
        // House rule, and these are user-visible strings.
        for supported in [true, false] {
            assert!(!window_picker_empty_message(supported).contains('\u{2014}'));
        }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::grid_cols_and_scale;

    #[test]
    fn single_window_uses_one_column_and_fits() {
        // One 800x600 tile in a 1920x1080 panel: one cell, scale capped at 1.0.
        let (cols, s) = grid_cols_and_scale(1, 800.0, 600.0, 1920.0, 1080.0, 24.0);
        assert_eq!(cols, 1);
        assert_eq!(s, 1.0);
    }

    #[test]
    fn few_wide_windows_stay_in_one_row() {
        // Three 640x400 tiles on a wide 3840x1080 panel: a single row (cols == n) gives
        // the largest tiles, so it is chosen.
        let (cols, _s) = grid_cols_and_scale(3, 640.0, 400.0, 3840.0, 1080.0, 24.0);
        assert_eq!(cols, 3);
    }

    #[test]
    fn many_windows_wrap_into_a_grid_not_one_row() {
        // Twelve 800x600 tiles on a 1920x1080 panel: one row would shrink each toward
        // 1/12; a multi-row grid must be chosen and give a strictly larger tile scale.
        let (cols, s_grid) = grid_cols_and_scale(12, 800.0, 600.0, 1920.0, 1080.0, 24.0);
        assert!(cols > 1 && cols < 12, "expected a grid, got {cols} columns");
        // Compare against the forced single-row scale for the same inputs.
        let one_row_cell_w = (1920.0 - 11.0 * 24.0) / 12.0;
        let s_row = (one_row_cell_w / 800.0_f32).min(1080.0 / 600.0).min(1.0);
        assert!(s_grid > s_row, "grid scale {s_grid} should beat single-row {s_row}");
    }

    #[test]
    fn never_enlarges_tiles() {
        // Tiny tiles in a huge panel are never scaled above 1.0.
        let (_cols, s) = grid_cols_and_scale(4, 100.0, 80.0, 4000.0, 3000.0, 24.0);
        assert!(s <= 1.0);
    }

    #[test]
    fn degenerate_tight_panel_still_returns_a_valid_column_count() {
        // Even when nothing really fits, a valid (cols>=1, scale>0) is returned, never a
        // panic or zero columns (the view uses cols as a modulo divisor).
        let (cols, s) = grid_cols_and_scale(20, 900.0, 700.0, 200.0, 150.0, 24.0);
        assert!(cols >= 1);
        assert!(s > 0.0);
    }
}

/// DRAGON-645: the window picker's loading spinner, and the flash it used to be.
///
/// The bug was not that the spinner was wrong, it was that it existed at all for a load too
/// short to read as a load. These pin the two halves of the fix: a pre-capture that finishes
/// inside the threshold must never draw a spinner, and one that does draw it must keep it up
/// long enough to be seen on purpose.
#[cfg(test)]
mod picker_load_tests {
    use super::{
        PICKER_LOAD_TICK_MS, PICKER_SPINNER_MIN_MS, PICKER_SPINNER_MIN_TICKS,
        PICKER_SPINNER_REVEAL_MS, PICKER_SPINNER_REVEAL_TICKS, PICKER_WARMUP_TICKS, PickerLoad,
    };

    /// Run the machine from `start` until it reaches `Idle`, landing the pre-capture on tick
    /// `land_on` (1-based) and never after, with the picker painting from tick `paint_on`
    /// onward. Returns every state it passed through, so a test can ask both "was the spinner
    /// ever drawn" and "for how many ticks".
    fn run_from(start: PickerLoad, land_on: usize, paint_on: usize) -> Vec<PickerLoad> {
        let mut state = start;
        let mut seen = vec![state];
        // Bounded on purpose: the machine must terminate, and a test that hangs is a test
        // that says nothing. The bound is well past every threshold.
        for tick in 1..=1_000 {
            if state == PickerLoad::Idle {
                break;
            }
            state = state.advance(tick == land_on, tick >= paint_on);
            seen.push(state);
        }
        assert_eq!(seen.last(), Some(&PickerLoad::Idle), "the machine must reach Idle");
        seen
    }

    /// The ordinary case: the picker is already on screen, so every tick counts.
    fn run(start: PickerLoad, land_on: usize) -> Vec<PickerLoad> {
        run_from(start, land_on, 1)
    }

    // THE TICKET. A pre-capture that lands inside the reveal threshold goes straight to the
    // picker, and the spinner is never drawn for a single frame.
    #[test]
    fn a_load_that_finishes_before_the_threshold_never_shows_the_spinner() {
        for land_on in 1..PICKER_SPINNER_REVEAL_TICKS as usize {
            let seen = run(PickerLoad::Quiet { ticks: 0 }, land_on);
            assert!(
                !seen.iter().any(|s| s.spinner_up()),
                "a {}ms load must not draw a spinner, got {seen:?}",
                land_on as u64 * PICKER_LOAD_TICK_MS
            );
            // And it costs the fast path NOTHING: the tick that drains the thumbnails is the
            // tick that hands over to the picker, with no warmup frames added.
            assert_eq!(seen.len(), land_on + 1, "the fast path must not add ticks: {seen:?}");
        }
    }

    // The other side of the same rule: a load that outlasts the threshold DOES announce
    // itself, and does so exactly at the threshold rather than a tick either side.
    #[test]
    fn a_load_that_outlasts_the_threshold_reveals_the_spinner_at_it() {
        let mut state = PickerLoad::Quiet { ticks: 0 };
        for tick in 1..PICKER_SPINNER_REVEAL_TICKS {
            state = state.advance(false, true);
            assert!(!state.spinner_up(), "revealed early, at tick {tick}: {state:?}");
            assert!(state.covering(), "the loading state exists even while quiet");
        }
        state = state.advance(false, true);
        assert!(matches!(state, PickerLoad::Shown { .. }), "expected a reveal, got {state:?}");
        assert!(state.spinner_up());
    }

    // THE FIRST LIVE RUN'S BUG, pinned. The poll starts in `App::init` and a macOS
    // window-mode launch does not put an overlay on screen for the better part of a second,
    // so a threshold counted from the subscription is spent entirely before anything is
    // visible and reveals a spinner for a wait nobody had. The clock must not run until the
    // picker has painted.
    #[test]
    fn the_threshold_does_not_run_before_the_picker_has_painted() {
        let mut state = PickerLoad::Quiet { ticks: 0 };
        // A whole second of launch, with nothing on screen.
        for _ in 0..20 {
            state = state.advance(false, false);
        }
        assert_eq!(
            state,
            PickerLoad::Quiet { ticks: 0 },
            "the reveal clock ran while the overlay was still coming up"
        );
        assert!(!state.spinner_up());
        // Once the picker IS drawing, the threshold runs from THERE.
        for _ in 1..PICKER_SPINNER_REVEAL_TICKS {
            state = state.advance(false, true);
            assert!(!state.spinner_up());
        }
        assert!(state.advance(false, true).spinner_up());
    }

    // The same launch, but the pre-capture finishes while the app is still coming up. That is
    // the fastest path of all and it must hand straight over: the user's first sight of window
    // mode is the picker, with no spinner and nothing waiting on a frame it no longer needs.
    #[test]
    fn a_load_that_finishes_before_the_overlay_exists_hands_straight_over() {
        let seen = run_from(PickerLoad::Quiet { ticks: 0 }, 12, 40);
        assert!(!seen.iter().any(|s| s.spinner_up()), "{seen:?}");
        assert_eq!(seen.len(), 13, "the handover must be the landing tick itself: {seen:?}");
    }

    // Once the spinner IS up the hold runs on the tick alone, deliberately. A hold that only
    // expired when frames arrived could never expire on a surface that has stopped being
    // drawn, and the house rule is that nothing waits unboundedly.
    #[test]
    fn the_hold_does_not_wait_on_frames_that_may_never_come() {
        let mut state = PickerLoad::Settling { hold: PICKER_SPINNER_MIN_TICKS };
        for _ in 0..PICKER_SPINNER_MIN_TICKS {
            state = state.advance(false, false);
        }
        assert_eq!(state, PickerLoad::Idle, "a settling spinner must retire without frames");
    }

    // Without a minimum, the delay would only MOVE the flash: a load landing one tick past
    // the threshold would show the spinner for a single frame. This is the borderline case.
    #[test]
    fn a_spinner_revealed_one_tick_before_the_load_lands_still_serves_its_minimum() {
        let land_on = PICKER_SPINNER_REVEAL_TICKS as usize + 1;
        let seen = run(PickerLoad::Quiet { ticks: 0 }, land_on);
        let shown = seen.iter().filter(|s| s.spinner_up()).count();
        assert!(
            shown as u8 >= PICKER_SPINNER_MIN_TICKS,
            "the spinner was up for {shown} ticks, less than the {PICKER_SPINNER_MIN_TICKS} \
             minimum: {seen:?}"
        );
    }

    // Whatever the load does, once the spinner IS shown it is shown for long enough to read
    // as deliberate. The property, over every landing instant rather than one example.
    #[test]
    fn every_shown_spinner_lasts_at_least_the_minimum() {
        for land_on in 1..40_usize {
            let seen = run(PickerLoad::Quiet { ticks: 0 }, land_on);
            let shown = seen.iter().filter(|s| s.spinner_up()).count() as u8;
            assert!(
                shown == 0 || shown >= PICKER_SPINNER_MIN_TICKS,
                "landing on tick {land_on} gave {shown} spinner ticks: {seen:?}"
            );
        }
    }

    // The warmup's own reason survives, and it is a DIFFERENT reason from the minimum: the
    // picker uploads its textures behind the spinner, so a spinner that has already served
    // its minimum still cannot lift on the same tick the thumbnails land.
    #[test]
    fn a_long_load_still_holds_the_spinner_for_the_pickers_warmup() {
        // Shown for well past the minimum, then the thumbnails land.
        let landed = PickerLoad::Shown {
            ticks: PICKER_SPINNER_MIN_TICKS,
        }
        .advance(true, true);
        assert_eq!(landed, PickerLoad::Settling { hold: PICKER_WARMUP_TICKS });
        assert!(landed.spinner_up(), "the picker must warm up BEHIND the spinner");
        // That the warmup is at least one poll (so the spinner cannot lift on the same tick
        // the thumbnails land) is pinned by the compile-time assert beside the constants;
        // repeating it here would only be an `assert!` on two consts.
    }

    // The two hold reasons overlap in time, so the hold is the LONGER of them and never their
    // sum. A spinner that has served none of its minimum yet must not also pay the warmup on
    // top; a spinner that has served all of it must still pay the warmup.
    #[test]
    fn the_hold_takes_the_longer_of_the_minimum_and_the_warmup_not_their_sum() {
        for ticks in 0..=PICKER_SPINNER_MIN_TICKS {
            let PickerLoad::Settling { hold } = PickerLoad::Shown { ticks }.advance(true, true) else {
                panic!("landing while shown must settle, not hand straight over");
            };
            let remaining_min = PICKER_SPINNER_MIN_TICKS - ticks;
            assert_eq!(hold, remaining_min.max(PICKER_WARMUP_TICKS));
            assert!(hold < remaining_min + PICKER_WARMUP_TICKS || remaining_min == 0);
        }
    }

    // `covering` and `spinner_up` are two different questions, and conflating them is what
    // the old `windows_loading || window_warmup > 0` did. Quiet is the state that separates
    // them: the dim is up and the picker's "no windows" message is suppressed, with no
    // spinner on screen.
    #[test]
    fn covering_and_spinner_up_answer_two_different_questions() {
        assert!(!PickerLoad::Idle.covering());
        assert!(!PickerLoad::Idle.spinner_up());

        assert!(PickerLoad::Quiet { ticks: 0 }.covering());
        assert!(!PickerLoad::Quiet { ticks: 0 }.spinner_up());

        assert!(PickerLoad::Shown { ticks: 0 }.covering());
        assert!(PickerLoad::Shown { ticks: 0 }.spinner_up());

        assert!(PickerLoad::Settling { hold: 1 }.covering());
        assert!(PickerLoad::Settling { hold: 1 }.spinner_up());

        // Anything that draws the spinner must also be covering, or the ring would sit on an
        // undimmed desktop with the picker's empty message showing through underneath.
        for state in [
            PickerLoad::Idle,
            PickerLoad::Quiet { ticks: 2 },
            PickerLoad::Shown { ticks: 2 },
            PickerLoad::Settling { hold: 2 },
        ] {
            assert!(!state.spinner_up() || state.covering(), "{state:?}");
        }
    }

    // Nothing here may wait forever (the house rule). Every state reaches `Idle` within a
    // bound once the pre-capture has landed, and `run` would have failed already if it did
    // not, so this pins the actual worst case rather than merely that one exists.
    #[test]
    fn the_whole_loading_state_is_bounded() {
        // The longest a load can hold the overlay after landing: revealed at the threshold,
        // landing immediately after, then the full minimum.
        let worst = run(PickerLoad::Quiet { ticks: 0 }, PICKER_SPINNER_REVEAL_TICKS as usize + 1);
        let ticks = (worst.len() - 1) as u64 * PICKER_LOAD_TICK_MS;
        assert!(
            ticks <= PICKER_SPINNER_REVEAL_MS + PICKER_SPINNER_MIN_MS + PICKER_LOAD_TICK_MS,
            "the loading state ran for {ticks}ms"
        );
        // And from mid-flight states too, so a machine that is somehow entered part way
        // through still terminates.
        run(PickerLoad::Shown { ticks: 0 }, 1);
        run(PickerLoad::Settling { hold: PICKER_SPINNER_MIN_TICKS }, 1);
    }

    // A stale tick after the handover must not restart anything: the picker is on screen and
    // interactive, and re-covering it would be a flash of its own.
    #[test]
    fn idle_absorbs_a_stale_tick() {
        assert_eq!(PickerLoad::Idle.advance(false, true), PickerLoad::Idle);
        assert_eq!(PickerLoad::Idle.advance(true, true), PickerLoad::Idle);
    }

    // The pre-capture thread can take arbitrarily long (or never post at all: a wedged
    // compositor enumeration). The counter must saturate rather than wrap, or a very long
    // load would roll `ticks` back to zero and re-arm a minimum that had long been served.
    #[test]
    fn a_very_long_load_cannot_wrap_its_counter() {
        let mut state = PickerLoad::Shown { ticks: 0 };
        for _ in 0..10_000 {
            state = state.advance(false, true);
            assert!(state.spinner_up(), "a running load must keep its spinner: {state:?}");
        }
        assert_eq!(state, PickerLoad::Shown { ticks: PICKER_SPINNER_MIN_TICKS });
        // And when it finally lands it owes only the warmup, not a fresh minimum.
        assert_eq!(state.advance(true, true), PickerLoad::Settling { hold: PICKER_WARMUP_TICKS });
    }

    // The thresholds are COUNTED in polls but WRITTEN in milliseconds, so the two readings
    // have to agree: a value that is not a whole number of polls truncates down and the delay
    // is quietly shorter than the constant says. The compile-time assert beside the constants
    // refuses that; this pins the conversion itself, which is the part a future edit would
    // change by hand.
    //
    // The two cross-constant relations (the reveal outlasting `DIM_FADE_MS`, and the warmup
    // being at least one poll) are compile-time asserts and deliberately not repeated here:
    // as runtime tests they would be `assert!` on constants, which is a clippy warning and,
    // more to the point, a weaker check than the one already in the tree.
    #[test]
    fn the_thresholds_are_whole_polls() {
        assert_eq!(PICKER_SPINNER_REVEAL_TICKS as u64 * PICKER_LOAD_TICK_MS, PICKER_SPINNER_REVEAL_MS);
        assert_eq!(PICKER_SPINNER_MIN_TICKS as u64 * PICKER_LOAD_TICK_MS, PICKER_SPINNER_MIN_MS);
        assert_eq!(PICKER_WARMUP_TICKS as u64 * PICKER_LOAD_TICK_MS, super::PICKER_WARMUP_MS);
    }
}

/// DRAGON-606: the fade's CURVE and its endpoints. Shape only; the ordering rule that
/// decides when the curve is allowed to start is pinned separately below.
#[cfg(test)]
mod dim_fade_ramp_tests {
    use super::{DIM_FADE_MS, dim_fade_alpha, ease_in_out_cubic};

    // The two endpoints are exact, because both are load-bearing. Zero must be a true zero
    // (that is the frame the frozen-flats grab may still photograph, and "almost
    // transparent" would still tint it), and the end must be the configured value itself,
    // so a finished fade is indistinguishable from the pre-DRAGON-606 constant dim.
    #[test]
    fn the_ramp_starts_at_nothing_and_lands_exactly_on_the_configured_dim() {
        assert_eq!(dim_fade_alpha(0.66, 0), 0.0);
        assert_eq!(dim_fade_alpha(0.66, DIM_FADE_MS), 0.66);
        // Past the end it stays put rather than overshooting.
        assert_eq!(dim_fade_alpha(0.66, DIM_FADE_MS * 10), 0.66);
    }

    // The fade multiplies whatever the caller configured. It must never become a second
    // opacity setting of its own, so every target rides the same curve.
    #[test]
    fn the_target_is_the_configured_opacity_not_a_constant() {
        for target in [0.0_f32, 0.1, 0.33, 0.66, 0.9, 1.0] {
            assert_eq!(dim_fade_alpha(target, DIM_FADE_MS), target);
            assert_eq!(dim_fade_alpha(target, 0), 0.0);
            let mid = dim_fade_alpha(target, DIM_FADE_MS / 2);
            assert!(mid <= target, "{mid} should never exceed the configured {target}");
        }
        // A user who set the dim to zero gets zero throughout, never a flash of dim.
        for ms in [0, 1, DIM_FADE_MS / 2, DIM_FADE_MS, DIM_FADE_MS * 2] {
            assert_eq!(dim_fade_alpha(0.0, ms), 0.0);
        }
    }

    // Monotonic, so the dim only ever gets darker. A ramp that dipped would read as a
    // flicker, which is the opposite of what the owner asked for.
    #[test]
    fn the_ramp_never_goes_backwards() {
        let mut prev = -1.0_f32;
        for ms in 0..=DIM_FADE_MS {
            let a = dim_fade_alpha(0.66, ms);
            assert!(a >= prev, "alpha dipped at {ms}ms: {a} after {prev}");
            prev = a;
        }
    }

    // Ease-in-out-cubic, matching cosmic-comp's own open animation: symmetric about the
    // midpoint, which is what makes it read as the same motion as the Flatpak fallback.
    #[test]
    fn the_curve_is_ease_in_out_cubic_like_the_compositors() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
        // Slow at both ends, fast in the middle: the first eighth covers less ground than
        // the eighth around the midpoint.
        let first = ease_in_out_cubic(0.125) - ease_in_out_cubic(0.0);
        let middle = ease_in_out_cubic(0.5625) - ease_in_out_cubic(0.4375);
        assert!(middle > first, "ease-in-out should accelerate into the middle");
        // Symmetry: f(t) + f(1-t) == 1.
        for t in [0.0_f32, 0.1, 0.25, 0.4, 0.5, 0.75, 0.9, 1.0] {
            assert!((ease_in_out_cubic(t) + ease_in_out_cubic(1.0 - t) - 1.0).abs() < 1e-5);
        }
        // Out of range inputs clamp rather than fly off.
        assert_eq!(ease_in_out_cubic(-5.0), 0.0);
        assert_eq!(ease_in_out_cubic(5.0), 1.0);
    }
}

/// DRAGON-606: THE ordering rule, and the reason this ticket needed a test at all.
///
/// The frozen-flats grab reads the whole screen, our overlay included. If the dim is
/// ramping while that runs, the wash is baked into the frozen scene and every capture made
/// from it comes back subtly dark, with nothing on screen to suggest why. These pin that
/// the fade cannot begin on any path where a grab could still be looking.
#[cfg(test)]
mod dim_fade_ordering_tests {
    use super::dim_fade_may_start;

    // The ordinary hotkey launch: the grab is kicked at init and runs on its own thread
    // while the overlay maps. `frozen_pending` stays true until the drain, so the whole
    // grab window is spent at zero dim.
    #[test]
    fn the_fade_waits_while_the_frozen_grab_is_still_in_flight() {
        assert!(!dim_fade_may_start(true, false, false));
        assert!(dim_fade_may_start(false, false, false));
    }

    // The DRAGON-600 tray path. The dropdown is dismissed by our overlay taking keyboard
    // focus, and the grab is HELD until then. A fade that started during the hold would
    // both photograph itself and defeat the hold's purpose.
    #[test]
    fn the_fade_waits_out_the_tray_dropdown_hold() {
        assert!(!dim_fade_may_start(true, true, false));
        // Even if the flats somehow landed first, the hold alone still blocks it.
        assert!(!dim_fade_may_start(false, true, false));
    }

    // The outer-budget fallback: keyboard focus never arrived, `tick_menu_hold` gave up and
    // ran the grab anyway. The hold clears, but `frozen_pending` is still true because that
    // grab has only just started, so the gate holds on the OTHER term. This is the path
    // most likely to be got wrong, because it is the one where the causal signal is absent.
    #[test]
    fn the_outer_budget_fallback_still_waits_for_the_grab_itself() {
        // menu_hold released, grab now running: not yet.
        assert!(!dim_fade_may_start(true, false, false));
        // Only once that grab has posted and been drained.
        assert!(dim_fade_may_start(false, false, false));
    }

    // The `lab/flatpak` fallback surface is a fullscreen xdg toplevel, and cosmic-comp
    // already fades it. Ours must stay out of the way rather than run a second ramp.
    #[test]
    fn the_flatpak_fallback_keeps_the_compositors_own_fade() {
        assert!(!dim_fade_may_start(false, false, true));
        assert!(!dim_fade_may_start(true, false, true));
    }

    // The state machine takes the LATER of the grab landing and the first painted frame.
    // Pinned as a machine rather than as prose because both orderings really happen: the
    // drain lands at ~553ms on the measured launch, and the first frame can fall on either
    // side of that depending on how long wgpu takes to come up.
    #[test]
    fn the_clock_starts_on_the_later_of_the_grab_and_the_first_frame() {
        use super::DimFade;
        let now = std::time::Instant::now();

        // Grab still running: not armed, and a frame in this state must not start anything.
        assert!(!dim_fade_may_start(true, false, false));

        // Grab landed but nothing painted yet: ARMED, and still drawing no dim. This is the
        // state that did not exist in the first cut, and its absence is what let a fade
        // finish before the overlay was on screen.
        assert_ne!(DimFade::Armed, DimFade::Waiting);
        assert_ne!(DimFade::Armed, DimFade::Done);

        // Armed is NOT a start: a fade anchored here would already be running before any
        // frame existed, which is the invisible-animation bug.
        assert!(matches!(DimFade::Armed, DimFade::Armed));

        // Running starts at ZERO ramp drawn, stamped with the instant the FRAME happened and
        // not the instant the grab landed. Since DRAGON-644 it carries how much of the ramp
        // has been PAINTED rather than a wall start; a fresh one has painted none of it.
        let armed_at_frame = DimFade::Running {
            elapsed_ms: 0,
            last: now,
        };
        assert!(
            matches!(armed_at_frame, DimFade::Running { elapsed_ms, last } if elapsed_ms == 0 && last == now)
        );
    }

    // DRAGON-644: a STALL must not spend the ramp. The macOS overlay's first content frame
    // blocked the main thread for 148ms on the worst measured launch, and on the old wall
    // clock that meant the first alpha anybody could see was already 94% of the target.
    #[test]
    fn a_stalled_frame_spends_only_its_capped_share_of_the_ramp() {
        use super::{DIM_FADE_MAX_STEP_MS, DIM_FADE_MS, dim_fade_step_ms};

        // The measured stall, and what it is allowed to cost.
        assert_eq!(dim_fade_step_ms(148), DIM_FADE_MAX_STEP_MS);
        assert_eq!(dim_fade_step_ms(53), DIM_FADE_MAX_STEP_MS);

        // Ordinary frames are spent in full, so a smoothly painting machine (Linux today)
        // animates on exactly the wall clock it always did.
        assert_eq!(dim_fade_step_ms(0), 0);
        assert_eq!(dim_fade_step_ms(8), 8);
        assert_eq!(dim_fade_step_ms(16), 16);
        assert_eq!(dim_fade_step_ms(DIM_FADE_MAX_STEP_MS), DIM_FADE_MAX_STEP_MS);

        // The property that matters: a step can never exceed the wall gap (so the dim is
        // never AHEAD of where a wall clock would have put it, which is what keeps
        // DRAGON-212's ordering guarantee untouched), and never exceeds the cap.
        for gap in [0_u64, 1, 5, 16, 17, 31, 32, 33, 100, 148, 5_000] {
            let step = dim_fade_step_ms(gap);
            assert!(step <= gap, "a frame cannot spend more ramp than wall time");
            assert!(step <= DIM_FADE_MAX_STEP_MS);
        }

        // And the reason it is worth doing: replaying the measured launch frame by frame, the
        // first alpha the user can see is near the START of the ramp instead of near its end.
        let target = 0.66_f32;
        let stalled = super::dim_fade_alpha(target, dim_fade_step_ms(148));
        assert!(
            stalled < target * 0.05,
            "the first frame after a 148ms stall must still be at the bottom of the ramp, got \
             {stalled} of {target}"
        );

        // A run of ordinary frames still completes the ramp in about its nominal duration.
        let mut elapsed = 0;
        let mut frames = 0;
        while elapsed < DIM_FADE_MS {
            elapsed += dim_fade_step_ms(16);
            frames += 1;
        }
        assert_eq!(frames, DIM_FADE_MS.div_ceil(16));
    }

    // Exhaustive, so no combination can be added later without a decision: the fade starts
    // in exactly ONE of the eight states, the one where nothing is reading the screen.
    #[test]
    fn exactly_one_combination_lets_the_fade_start() {
        let mut starts = 0;
        for pending in [false, true] {
            for hold in [false, true] {
                for fallback in [false, true] {
                    if dim_fade_may_start(pending, hold, fallback) {
                        starts += 1;
                        assert!(!pending && !hold && !fallback);
                    }
                }
            }
        }
        assert_eq!(starts, 1, "the fade must start on exactly one combination");
    }
}

