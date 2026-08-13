//! `ColorPickerMsg` handlers (DRAGON-582) — the picker overlay's pointer lane and the
//! result window's editing lane.
//!
//! The decisions these bodies apply are all pure and live in `app::color_picker::geom`
//! (which pixel, where the label goes, what may write the recents). What is here is the
//! wiring: sampling through the `PixelSource` seam, rasterising the magnifier only when
//! the sampled pixel actually changed, and the one-shot handover from overlay to window.
//!
//! # Privacy
//!
//! A picked colour is the user's content. Log lines here name the notation
//! (`ColorFormat::id`) or the event, never a colour value.

use super::super::*;
use crate::app::color_picker::{build_magnifier_raster, geom, Hover};
use crate::color::Srgb;

/// The corner radius the window's gradient rasters take, so the square and the tracks
/// follow the appearance page's "Edge rounding" exactly as the swatches do
/// (DRAGON-630, the owner's review): the same `s` token `view::swatch_radius` reads,
/// off the live theme the way `picker_ring` reads the accent.
fn picker_corner_radius() -> f64 {
    crate::app::theme::rounding(&cosmic::theme::active()).s[0] as f64
}

/// The round swatch's rim tone as raster bytes: the theme's SUBDUED tone
/// (`theme::subdued`), the owner's "subdued, not white/black" for swatch borders.
/// `theme::subtle` stood here first and the owner flagged it as brighter than asked.
fn picker_rim() -> [u8; 3] {
    let c = crate::app::theme::subdued(&cosmic::theme::active());
    [
        (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// May a resample leave the magnifier's RASTER stale to keep up with a fast pointer
/// (DRAGON-TBD)?
///
/// One question, asked once, because the answer is a property of the CALLER and not of the
/// picker's state. Getting it from the state instead is the mistake this type exists to make
/// impossible: the pacing measures pointer speed, and a keyboard nudge arriving while the hand
/// is still coasting would inherit that speed and have its raster deferred, so the arrow keys
/// would feel broken in exactly the moment the user reached for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RasterPolicy {
    /// Rebuild the raster whenever the picture changed. Every route but the pointer's own
    /// per-frame one: the keyboard nudge (DRAGON-599), the frozen-snapshot delivery
    /// (DRAGON-601) and a session's first sample. All byte-identical to before the pacing.
    Always,
    /// The pointer's per-frame path, and ONLY it: may decline a raster while the pointer is
    /// sweeping (`geom::raster_due`), re-arming itself so the settle is never missed.
    Paced,
}

/// **Pure**, unit-tested: may this resample leave the magnifier's raster stale (DRAGON-TBD)?
///
/// One line, and it has its own name and its own tests because of what the FIRST term
/// guarantees: [`RasterPolicy::Always`] refuses to skip whatever the pointer is doing, so a
/// keyboard nudge pressed while the hand is still coasting from a sweep cannot inherit that
/// speed and have its raster deferred. That is the failure this seam exists to prevent, it is
/// invisible in a headless build, and a person reading the call site cannot tell it holds; a
/// test can.
fn skips_raster(
    policy: RasterPolicy,
    since_last_raster: Option<std::time::Duration>,
    speed: f32,
) -> bool {
    policy == RasterPolicy::Paced && !geom::raster_due(since_last_raster, speed)
}

impl App {
    pub(in crate::app) fn update_color_picker(
        &mut self,
        message: ColorPickerMsg,
    ) -> Task<cosmic::Action<Msg>> {
        match message {
            ColorPickerMsg::Moved(output, point) => {
                // DRAGON-599: the pointer really moved, so the sample belongs to the pointer
                // again and the keyboard's displacement is dropped. Recording the position and
                // resetting the nudge are the whole of this arm.
                //
                // DRAGON-TBD: and, once the lens exists, it is now nearly the whole arm. The
                // resample used to run right here, once per raw `CursorMoved`, and that is more
                // often than the screen is redrawn: every intermediate raster between two
                // presented frames is built and thrown away unseen, and macOS delivers pointer
                // motion uncoupled from the display's cadence, so it paid for most of them.
                // The lens is re-sampled once per RENDERED FRAME instead, by the redraw
                // published from `widgets::color_pick` while `resample_due` is set, so the
                // cadence is whatever the display actually runs at (120Hz on a ProMotion panel,
                // where a fixed 16ms timer visibly under-sampled and read as stepped motion).
                //
                // The bookkeeping stays synchronous because it is two field writes and because
                // `needs_pointer` (DRAGON-610) reads `pointer` on the very next redraw.
                //
                // DRAGON-650: this message itself now arrives at most once per PRESENTED
                // frame, coalesced by `widgets::color_pick` (its module doc, "Pointer reports
                // ride the redraw", carries the Windows present-starvation measurement that
                // forced it). Nothing here had to change for that: the arm was already shaped
                // for "record the latest position", however often it arrives.
                self.color_picker.nudge =
                    geom::nudge_after(self.color_picker.nudge, geom::SampleMove::Pointer);
                self.color_picker.pointer = Some((output.clone(), point));
                self.color_picker.resample_due = true;
                // …with ONE exception, and it is a correctness one rather than a latency one.
                // While there is NO hover yet, "the pointer is known, the snapshot has landed
                // and there is still no sample" is what `keyboard::picker_sample_state` reads
                // as UNREADABLE, and an accept key pressed in that gap would tell the user this
                // display cannot be read (DRAGON-612). Before the deferral that gap could not
                // exist, because the move that set the pointer also set the hover. So the FIRST
                // sample after any gap still runs inline, which is also what keeps the loupe
                // appearing the instant the pointer is located (DRAGON-609/610).
                //
                // Nothing is lost by it: with no hover there is nothing to coalesce, and every
                // reason a hover STAYS absent (the snapshot still in flight, a display with no
                // pixel source, a disc entirely off-surface) makes the resample return early
                // without rastering anything at all. The burst this defers is a moving pointer
                // over a hover that already exists, which is the whole of the reported problem.
                if self.color_picker.hover.is_none() {
                    self.color_picker.resample_due = false;
                    return self.color_picker_resample();
                }
                Task::none()
            }
            // DRAGON-TBD: one per RENDERED FRAME, published by `widgets::color_pick` from the
            // redraw it is already handed while `resample_due` is set (or riding the frame's
            // own coalesced `Moved`, DRAGON-650). It reads the pointer position that report
            // just recorded, so a burst of motion costs ONE look per frame rather than one
            // per event, and it arrives at the display's own cadence rather than a number we
            // picked.
            //
            // Clearing the flag FIRST is what stops the loop: iced re-dispatches its redraw
            // event after a message published from one, so a flag left set would publish again
            // in the same pass. The paced branch inside the resample is the one thing allowed
            // to set it back, which is precisely the "look again next frame" it needs.
            ColorPickerMsg::ResamplePoll => {
                self.color_picker.resample_due = false;
                self.color_picker_resample_with(RasterPolicy::Paced)
            }
            ColorPickerMsg::Pick(output, point) => self.color_picker_pick(&output, point),
            ColorPickerMsg::Zoom(steps) => self.color_picker_zoom(steps),
            // macOS: `PreviewMsg::PinchPoll`'s exact shape (`preview/mod.rs`). Ensure the
            // recognizer is attached (idempotent, cheap once installed), then drain the
            // accumulated magnification and reduce it to whole notches through
            // `geom::pinch_notches`, the same accumulate-a-remainder shape
            // `widgets::color_pick`'s own wheel handler uses, kept here instead of there
            // because this poll fires from the app/subscription layer, not from that widget's
            // `Event` handler. No pinch pending is a no-op, same as the preview route.
            #[cfg(target_os = "macos")]
            ColorPickerMsg::PinchPoll => {
                crate::platform::mac::pinch::install_pinch();
                let delta = crate::platform::mac::pinch::take_pinch();
                if delta == 0.0 {
                    return Task::none();
                }
                let (notches, remainder) =
                    geom::pinch_notches(self.color_picker.pinch_accum, delta);
                self.color_picker.pinch_accum = remainder;
                if notches == 0 {
                    return Task::none();
                }
                self.color_picker_zoom(notches)
            }
            // DRAGON-599: one source pixel, from an arrow key or its vim letter. It moves the
            // SAMPLE, never the pointer, which no Wayland client can move.
            ColorPickerMsg::Nudge(dir) => {
                let (dx, dy) = dir.delta();
                self.color_picker.nudge =
                    geom::nudge_after(self.color_picker.nudge, geom::SampleMove::Keys(dx, dy));
                self.color_picker_resample()
            }
            // DRAGON-630: the gradient square. The square hands back field units: x is
            // saturation, y runs DOWN while value runs up. The TRACKED HSV is the
            // master here, written before the colour, so a drag through the achromatic
            // edge cannot snap the hue (`color::hsv_tracking`), and `apply_picker_color`
            // (not `set_picker_color`) so the byte-quantised colour cannot re-track the
            // exact position the hand just chose.
            ColorPickerMsg::SvChanged(nx, ny) => {
                let hsv = [
                    self.color_picker.hsv[0],
                    nx.clamp(0.0, 1.0) as f64,
                    (1.0 - ny.clamp(0.0, 1.0)) as f64,
                ];
                self.color_picker.hsv = hsv;
                self.color_picker.draft = None;
                self.apply_picker_color(crate::color::srgb_from_hsv(hsv), geom::ColorSource::Edit);
                Task::none()
            }
            // DRAGON-630: the hue strip. Same master-HSV rule as the square; this is
            // also the one interaction that re-rasters the square (its whole picture is
            // the hue).
            ColorPickerMsg::HueChanged(nx) => {
                let mut hsv = self.color_picker.hsv;
                hsv[0] = (nx.clamp(0.0, 1.0) as f64) * 360.0;
                self.color_picker.hsv = hsv;
                self.color_picker.draft = None;
                self.refresh_sv_raster();
                self.apply_picker_color(crate::color::srgb_from_hsv(hsv), geom::ColorSource::Edit);
                Task::none()
            }
            // DRAGON-630: the alpha strip. Only the alpha moves: the colour, the recents
            // and the square stay put, and only the swatch disc re-rasters (the strip
            // itself paints the alpha RANGE, which does not depend on the alpha).
            ColorPickerMsg::AlphaChanged(na) => {
                self.color_picker.alpha = (na.clamp(0.0, 1.0) * 255.0).round() as u8;
                self.color_picker.draft = None;
                self.refresh_color_rasters();
                Task::none()
            }
            // DRAGON-630: the mode dropdown. Persist (the remembered mode is the
            // owner's ask, and this is its one writer, so the save rides here) and copy
            // the value in the NEW mode's spelling, the control's stated contract.
            // Re-selecting the current mode is a no-op: nothing changed, so nothing is
            // saved and nothing lands on the clipboard.
            ColorPickerMsg::ModeSelected(idx) => {
                // A selection always CLOSES the menu, even the no-op re-selection of
                // the current mode: the click landed, so the menu's job is done.
                self.color_picker.mode_menu_open = false;
                let Some(mode) = crate::color::ColorFormat::ALL.get(idx).copied() else {
                    return Task::none();
                };
                if mode == self.color_picker.mode {
                    return Task::none();
                }
                self.color_picker.mode = mode;
                self.color_picker.draft = None;
                self.save_state();
                self.copy_picker_value()
            }
            // DRAGON-630 rev 4: the mode chip's menu. Transient view state, no save.
            ColorPickerMsg::ModeMenuToggled => {
                self.color_picker.mode_menu_open = !self.color_picker.mode_menu_open;
                Task::none()
            }
            ColorPickerMsg::BoxEdited(idx, text) => {
                // The colour follows the box the moment the text PARSES; a value that
                // does not parse yet (half typed) leaves the colour where it was, so the
                // swatch never flashes through nonsense on the way to a valid value.
                // The WHOLE-VALUE box (the layout toggle's collapsed state) parses the
                // full spelling; a channel box parses its one component.
                let parsed = if idx == crate::app::color_picker::WHOLE_VALUE_BOX {
                    self.color_picker.mode.parse_with_alpha(&text)
                } else {
                    self.color_picker.mode.with_component(
                        self.color_picker.color,
                        self.color_picker.alpha,
                        idx,
                        &text,
                    )
                };
                if let Some((c, a)) = parsed {
                    self.color_picker.alpha = a;
                    self.set_picker_color(c, geom::ColorSource::Edit);
                }
                self.color_picker.draft = Some((self.color_picker.mode, idx, text));
                Task::none()
            }
            ColorPickerMsg::BoxCommitted => {
                // Drop the draft so every box, including this one, re-renders in its
                // canonical spelling.
                self.color_picker.draft = None;
                // And FILE the colour, exactly as the Add button would (the owner's ask).
                // Enter in a value box is the one gesture in this window that says "this
                // is the colour I meant", so it is the natural second way to reach the
                // same rule; there is no third, because everything else here is a drag or
                // a keystroke mid-edit and would fill the history with the colours you
                // passed through on the way.
                self.add_shown_color_to_history();
                Task::none()
            }
            // DRAGON-630 rev 3: the split-vs-whole layout toggle. Persisted like the
            // mode (one-shot process, remembered state, this is the one writer). The
            // draft drops because it belonged to a box that no longer exists.
            ColorPickerMsg::InputLayoutToggled => {
                self.color_picker.split_inputs = !self.color_picker.split_inputs;
                self.color_picker.draft = None;
                self.save_state();
                Task::none()
            }
            ColorPickerMsg::CopyValue => self.copy_picker_value(),
            ColorPickerMsg::PickAgain => {
                // The SAME route every other launcher takes (the tray entry, the global
                // shortcut, the editor's toolbar pipette): a detached `--color-picker` child.
                //
                // A fresh LAUNCH is the point of this, not a shortcut around the work
                // (DRAGON-594, and this is the owner's own reason). A launch mints overlays
                // for the outputs it finds and freezes them then, so the next pick can come
                // from a DIFFERENT MONITOR. Re-entering the overlay in THIS process would
                // reuse what this session already holds and quietly pin the user to one
                // display. DO NOT "simplify" this into an in-process re-entry: the fresh
                // launch is carrying a decision the owner made deliberately, twice.
                //
                // Re-entry is also more expensive than it looks: it would mean re-grabbing the
                // frozen scene and, on the portal fallback, re-requesting a ScreenCast
                // mid-session. A launch already does all of that correctly.
                //
                // This window STAYS OPEN, and DRAGON-613 is what makes that the whole story:
                // the child hands its colour BACK to this window (we are listening on our
                // per-pid picker socket, advertised by our colour-picker marker, which also
                // keeps the sibling sweep off us), so the new colour lands here and becomes
                // the newest recent, and no second window appears. Fresh launch, one window:
                // the multi-monitor reason above and the single-window rule are satisfied by
                // changing only where the value is DELIVERED.
                //
                // The spawn stays best-effort with no channel back, so closing on the strength
                // of it would risk throwing away the colour the user has in hand for a child
                // that never appeared. A child that cannot reach us opens its own window,
                // which is the pre-DRAGON-613 behaviour and never a lost colour.
                log::debug!("color picker: the pipette started another pick");
                // `CCK_COLOR_TO_PID=0` says "no editor" EXPLICITLY, and it has to be said
                // rather than left unset (DRAGON-613). A child inherits our environment, and
                // this process may itself have been launched by an editor whose delivery
                // failed — that is exactly how a window comes to exist with the variable
                // still set. Without this the child would target that dead editor, fail, and
                // (before the ladder in `deliver_pick`) open a second window. `0` is already
                // rejected by `color_target_pid`, so this rides the existing rule instead of
                // adding one, and the spawn seam can only ADD variables, never clear them.
                crate::recording_ui::spawn_capture_child_args(
                    &["--color-picker"],
                    &[(crate::app::color_picker::COLOR_TO_PID_ENV, "0")],
                );
                Task::none()
            }
            ColorPickerMsg::LoadRecent(index) => {
                // LOADS only. `set_picker_color`'s `RecentClick` source is what stops
                // this reordering the list (`geom::writes_recents`).
                if let Some(c) = self.color_picker.recents.get(index).copied() {
                    self.set_picker_color(c, geom::ColorSource::RecentClick);
                }
                Task::none()
            }
            ColorPickerMsg::AddToHistory => {
                self.add_shown_color_to_history();
                Task::none()
            }
            ColorPickerMsg::ClearCopied => {
                self.color_picker.copied = None;
                Task::none()
            }
            ColorPickerMsg::PickCopyDeadline => {
                // The latch is cleared by whichever of the focus and this deadline arrives
                // first, so reaching here with it still set means the window never took the
                // keyboard inside the budget. Say so: a copy nobody can find is worse when
                // nothing in the log admits it did not happen.
                if !self.color_picker.copy_waiting {
                    return Task::none();
                }
                self.color_picker.copy_waiting = false;
                log::error!(
                    "color picker: the picked color could not be copied — this session serves \
                     the clipboard from a focused window and the result window never took the \
                     keyboard within {}s. The window's Copy button still works.",
                    crate::share::WINDOW_COPY_FOCUS_BUDGET.as_secs()
                );
                Task::none()
            }
        }
    }

    /// THE surface point this picker reads on `output`, for a pointer at `pointer`
    /// (DRAGON-599). One function, asked by every consumer, so the lens, the hex label and the
    /// pick itself can never disagree about which pixel is being taken.
    ///
    /// Two layers, in order. The POINTER's own point first (DRAGON-587's one-point offset used to
    /// sit here and was removed by DRAGON-597:
    /// the pointer itself where the sprite can be hidden, one point up and left of it where
    /// the arrow is on screen), then the KEYBOARD's displacement on top of that
    /// (`geom::nudged_sample`). With no keys pressed the second layer is the identity, so
    /// every existing path is byte-identical.
    ///
    /// A display whose snapshot has not arrived has no pixel size to step by, and nothing to
    /// sample either, so the base is returned unchanged rather than guessing a step.
    fn picker_sample(&self, o: &OutputState, pointer: (f32, f32)) -> (f32, f32) {
        let viewport = o.units().size_to_point(o.logical_size);
        // DRAGON-597 removed the one-point offset that used to sit here: the pointer can be
        // hidden on every surface now, so there is no arrow tip to escape and the pointer's own
        // point IS the sample. The keyboard nudge (DRAGON-599) still applies on top of it.
        let Some(frozen) = self.frozen.get(&o.name) else {
            return pointer;
        };
        let image = (frozen.img.width(), frozen.img.height());
        geom::nudged_sample(pointer, self.color_picker.nudge, viewport, image)
    }

    /// Re-read the pixel the picker is pointing at, and re-rasterise the magnifier only if the
    /// SOURCE PIXEL changed.
    ///
    /// The guard matters: a move inside one pixel is the common case at any sensible
    /// pointer speed, and rebuilding the disc would raster ~24k pixels and re-upload them (on
    /// the software-forced image arm, mint a fresh atlas entry too) for a picture that has
    /// not changed.
    /// It is also what makes DRAGON-TBD's 16ms poll free on a tick where nothing moved.
    ///
    /// The point READ is the pointer's own point, plus any keyboard nudge. It used to be
    /// displaced one point up and left as well, to escape an arrow sprite that covered its own
    /// hotspot pixel (DRAGON-587); DRAGON-597 removed that once the pointer could be hidden on
    /// every surface, and the tombstone is in `color_picker::geom`. Everything downstream, the
    /// disc's centre, the hex label and the pick itself, uses that one point, so the lens shows
    /// exactly what a click would take.
    ///
    /// DRAGON-599: this is driven by the KEYBOARD as well as the pointer, which is why it takes
    /// no position. Both routes write `ColorPickerState::pointer` / `nudge` first and then run
    /// this, so there is one sampling path rather than a pointer one and a nearly-identical
    /// keyboard one that could drift.
    ///
    /// DRAGON-TBD: this entry point is the ACCURATE one and always was. It rebuilds the raster
    /// whenever the picture changed, full stop, and it is what the keyboard nudge, the
    /// frozen-snapshot delivery and the first sample of a session all call. Only the pointer's
    /// own per-frame path goes through [`Self::color_picker_resample_with`] with
    /// [`RasterPolicy::Paced`].
    pub(in crate::app) fn color_picker_resample(&mut self) -> Task<cosmic::Action<Msg>> {
        self.color_picker_resample_with(RasterPolicy::Always)
    }

    /// The body of [`Self::color_picker_resample`], with the one question the two callers
    /// disagree about handed in (DRAGON-TBD). See [`RasterPolicy`].
    fn color_picker_resample_with(&mut self, policy: RasterPolicy) -> Task<cosmic::Action<Msg>> {
        let Some((output, pointer)) = self.color_picker.pointer.clone() else {
            return Task::none();
        };
        let output = output.as_str();
        let Some(o) = self.outputs.iter().find(|o| o.name == output) else {
            return Task::none();
        };
        let viewport = o.units().size_to_point(o.logical_size);
        let sample = self.picker_sample(o, pointer);
        let Some((px, py, color)) = self.sample_pixel(o, sample) else {
            // STILL LOADING is not the same as UNREADABLE. The flats grab is deferred off
            // the launch critical path (so the overlay maps immediately), so the first
            // pointer moves of every picker launch arrive before the snapshot does.
            // Saying "this display cannot be read" there would be a false alarm on a
            // perfectly good display, once per launch.
            if self.frozen_pending {
                self.color_picker.hover = None;
                return Task::none();
            }
            // No readable source for this display. Say so once, and keep saying it
            // until the pointer reaches a display we CAN read.
            if !self.color_picker.unavailable {
                log::warn!(
                    "color picker: no pixel source for display '{output}', so no color can \
                     be reported there"
                );
            }
            self.color_picker.unavailable = true;
            self.color_picker.hover = None;
            return Task::none();
        };
        self.color_picker.unavailable = false;
        let zoom = self.color_picker.zoom;
        // How much of the lens is on screen at this position. `None` means none of it is,
        // which is not a state the pointer can reach over its own overlay, but a surface
        // reporting a stale point could: draw nothing rather than an empty disc.
        let Some(disc) = geom::disc_view(sample, viewport) else {
            self.color_picker.hover = None;
            return Task::none();
        };
        // How fast the pointer is travelling, from the PREVIOUS sample point (which is exactly
        // `Hover::sample`) and the instant it was taken. Computed before the early returns
        // below so the clock advances on every look, not only on the ones that raster.
        let now = std::time::Instant::now();
        let speed = match (self.color_picker.hover.as_ref(), self.color_picker.sampled_at) {
            (Some(h), Some(at)) => geom::sample_speed(h.sample, sample, now.duration_since(at)),
            // No previous sample to measure against is not a fast pointer, it is no
            // information, and no information rasters (`geom::sample_speed`'s own rule).
            _ => 0.0,
        };
        self.color_picker.sampled_at = Some(now);
        let same_picture = self.color_picker.hover.as_ref().is_some_and(|h| {
            h.output == output && h.pixel == (px, py) && h.zoom == zoom && h.disc == disc
        });
        if same_picture {
            // Only the position moved. Keep the rasterised disc and slide it.
            if let Some(h) = self.color_picker.hover.as_mut() {
                h.sample = sample;
            }
            return Task::none();
        }
        // DRAGON-TBD: the picture DID change, but on the pointer's own per-frame path a fast
        // sweep changes it every single frame, and none of those pictures is one the user is
        // reading: they are travelling. `geom::raster_min_interval` carries the measurement
        // (57.5µs a raster, so this is about not doing invisible work rather than about a
        // frame budget) and the ramp.
        //
        // What is skipped is ONLY the raster. The lens still follows the pointer and the hex
        // chip still reports the pixel under it, because `sample` and `color` are not what the
        // raster describes. `pixel`, `zoom` and `disc` deliberately are NOT updated: they are
        // the IDENTITY of the pixels currently in the buffer, so writing them here would make
        // the `same_picture` guard above agree with a lens showing something else, and a
        // pointer that then stopped would keep that stale picture for good.
        //
        // DRAGON-650: "the lens still follows the pointer" is now actually true, and it was
        // not when this pacing landed. The view used to PLACE the disc from `h.disc.origin`,
        // the raster's own identity, so on every skipped frame the lens stood still and then
        // jumped up to `RASTER_MAX_INTERVAL` of travel in one step — the reported "skips
        // around erratically", obvious on a 60Hz panel where ordinary motion clears
        // `DELIBERATE_SPEED`. The view now derives the drawn position from `h.sample` on
        // every frame (`geom::drawn_disc_origin`), so this branch's `h.sample` write IS the
        // lens's movement, and the raster identity stays honestly stale.
        //
        // The skip also requires the hover to be on THIS output (DRAGON-650): `h.sample` is a
        // point in `h.output`'s own surface space, so writing a NEW output's coordinates into
        // a hover still tagged with the old one would place the lens and the chip at a
        // meaningless position on the wrong display for up to a pacing interval. A monitor
        // crossing happens once per crossing, not once per frame, so rastering it immediately
        // costs nothing the pacing exists to save.
        //
        // Leaving `resample_due` set is what guarantees the settle: it buys one more look on
        // the next frame, and the frame after a hand stops measures zero speed, so it rasters.
        let hover_on_this_output =
            self.color_picker.hover.as_ref().is_some_and(|h| h.output == output);
        if hover_on_this_output
            && skips_raster(
                policy,
                self.color_picker.rastered_at.map(|t| now.duration_since(t)),
                speed,
            )
        {
            if let Some(h) = self.color_picker.hover.as_mut() {
                h.sample = sample;
                h.color = color;
            }
            self.color_picker.resample_due = true;
            return Task::none();
        }
        let Some(frozen) = self.frozen.get(output) else {
            return Task::none();
        };
        let (w, h, rgba) = geom::magnifier_rgba(&frozen.img, (px, py), zoom, self.picker_ring(), disc);
        self.color_picker.hover = Some(Hover {
            output: output.to_string(),
            sample,
            pixel: (px, py),
            color,
            // DRAGON-650: keyed on what THIS process was forced to render with, not on the
            // platform fact — the Windows 10 picker keeps wgpu now, so asking the platform
            // would park its lens on the atlas-churning image arm for no reason. See
            // `process_forced_software_backend`.
            magnifier: build_magnifier_raster(
                w,
                h,
                rgba,
                crate::app::process_forced_software_backend(),
            ),
            zoom,
            disc,
        });
        self.color_picker.rastered_at = Some(now);
        Task::none()
    }

    /// The magnifier's accent RING, as the raster wants it: thickness in points and a
    /// straight RGBA ink (DRAGON-587 baked it into the buffer so it clips with the disc).
    ///
    /// (The two free functions below feed the WINDOW rasters the same way: theme facts
    /// read at build time, because this all runs in `update` with no theme in hand.)
    ///
    /// The thickness is the user's "Selection box thickness", the same setting the region
    /// selector's box reads, which is what DRAGON-582 asked for: one width for both. The
    /// colour is the live accent, read the way every other non-view site in the app reads it
    /// (`cosmic::theme::active()`), because this runs in `update`, not in a view with a theme
    /// in hand.
    fn picker_ring(&self) -> (f32, [u8; 4]) {
        let thickness = self.selection_box_thickness.clamp(1, 8) as f32;
        let accent = crate::app::theme::accent(&cosmic::theme::active());
        (
            thickness,
            [
                (accent.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (accent.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (accent.b * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            ],
        )
    }

    /// DRAGON-587: zoom the magnifier by `steps` notches, positive = in.
    ///
    /// The ONE handler behind all three routes (trackpad, wheel, numpad `+`/`-`): each of them
    /// only decides how many notches it is worth, and the arithmetic and the clamp are
    /// [`geom::zoom_after_step`]'s. A step that lands on the value we already have does
    /// nothing at all, so holding `+` at the ceiling costs no re-raster.
    ///
    /// The disc is rebuilt HERE rather than in `view`, for the reason `Hover` documents:
    /// rasterising per frame would re-upload the disc on every redraw.
    ///
    /// It stays SYNCHRONOUS, unlike the pointer's resample (DRAGON-TBD). A zoom route is a
    /// wheel notch, a pinch notch or a key press, all of which are already coarse, and the
    /// clamp above makes a step that changes nothing cost nothing; there is no burst here to
    /// coalesce.
    fn color_picker_zoom(&mut self, steps: i32) -> Task<cosmic::Action<Msg>> {
        let zoom = geom::zoom_after_step(self.color_picker.zoom, steps);
        if zoom == self.color_picker.zoom {
            return Task::none();
        }
        self.color_picker.zoom = zoom;
        // Re-rasterise what the pointer is already over, at the new magnification. Nothing
        // else about the hover changes: the same pixel is still being sampled.
        let Some(h) = self.color_picker.hover.as_ref() else {
            return Task::none();
        };
        let Some(frozen) = self.frozen.get(&h.output) else {
            return Task::none();
        };
        let (pixel, disc) = (h.pixel, h.disc);
        let (w, ht, rgba) = geom::magnifier_rgba(&frozen.img, pixel, zoom, self.picker_ring(), disc);
        if let Some(h) = self.color_picker.hover.as_mut() {
            // DRAGON-650: the same forced-backend predicate as the resample's raster, and
            // for the same reason.
            h.magnifier = build_magnifier_raster(
                w,
                ht,
                rgba,
                crate::app::process_forced_software_backend(),
            );
            h.zoom = zoom;
        }
        Task::none()
    }

    /// A left click on a picker overlay: take the colour, copy its hex, tear the
    /// overlays down and open the result window.
    ///
    /// A click on a display with NO readable source is INERT. Refusing is the only
    /// honest answer there: the alternative is delivering a colour we did not read.
    ///
    /// It reads the SAME point the move handler does, the pointer's own, and that is not
    /// optional: the two reading different points would deliver a colour the magnifier never
    /// showed, which is the one thing a colour picker may not do. This used to say "through the
    /// same sampling seam", which existed only while the arrow fallback displaced the
    /// sample (DRAGON-587, removed in DRAGON-597).
    fn color_picker_pick(
        &mut self,
        output: &str,
        point: (f32, f32),
    ) -> Task<cosmic::Action<Msg>> {
        let Some(o) = self.outputs.iter().find(|o| o.name == output) else {
            return Task::none();
        };
        // DRAGON-599: the SAME resolved sample the lens is showing, keyboard nudge included.
        // A click carries the pointer's raw position and nothing else, so reading the pointer
        // directly here would take the pixel under the cursor while the loupe was showing the
        // nudged one — a picker reporting a colour it never displayed, which is the one thing
        // it may never do.
        let sample = self.picker_sample(o, point);
        let Some((_, _, color)) = self.sample_pixel(o, sample) else {
            // Same split as the move handler: a click while the snapshot is still in
            // flight is simply early, not a failure, and must not be reported as one.
            if self.frozen_pending {
                return Task::none();
            }
            self.color_picker.unavailable = true;
            log::warn!("color picker: the click landed on a display with no pixel source");
            return Task::none();
        };
        // A PICK is the one thing that writes the recents. That rule is the same whoever
        // launched the picker, including the editor below (DRAGON-587).
        self.set_picker_color(color, geom::ColorSource::Pick);
        self.color_picker.pick_output = Some(output.to_string());
        self.save_state();
        // Hand the colour to whoever it is FOR (DRAGON-587 for an editor, DRAGON-613 for the
        // one picker window). `Some` means a live consumer positively ACKNOWLEDGED it and
        // this process is done; `None` means nobody took it, and we open the result window
        // below exactly as we always did.
        //
        // `deliver_pick` is a LADDER and this is its bottom rung, which is what makes
        // "a pick is never lost" structural rather than a list of handled cases: no
        // consumer, a consumer mid-close, a wedged one, a version-mismatched one and a
        // platform with no transport all arrive here the same way, and all get a window the
        // user can see. Nothing above may consume the pick without a positive ack.
        if let Some(task) = self.deliver_pick(color) {
            return task;
        }
        log::debug!("color picker: picked a color and opened the result window");
        let mut cmds = self.destroy_surfaces();
        // The picked value goes on the clipboard as part of the pick, IN THE REMEMBERED
        // MODE's spelling (DRAGON-630, the owner's ask: what the window copies on
        // opening is the mode you left it in, not always hex). A pick is opaque and
        // `set_picker_color` above has already reset the alpha, so this is the mode's
        // plain spelling. HOW it gets there is the one question every copy in the app
        // asks (`share::copy_step`).
        //
        // DRAGON-587, the owner's report that this copy simply did not happen. It used to be
        // an unconditional `copy_text_task` pushed into THIS batch, which is right on the
        // standalone route and impossible on the window one: the same batch destroys every
        // overlay and only QUEUES the result window's open, so at the moment the write runs
        // this process has no focused surface to carry the selection serial, and the write is
        // dropped. That is the exact shape DRAGON-550 fixed for the preview editor's
        // open-time copy, so it takes the same ladder rather than a second one.
        //
        // The window cannot possibly hold focus yet (it is minted three lines below), so the
        // focus term is a literal `false` rather than a lookup that could only ever say so.
        let value = self.color_picker.value_text();
        match crate::share::copy_step(crate::share::copy_route(), false, false) {
            // A detached worker owns the selection and outlives us: spawn it now, exactly as
            // before. `copy_text_task` returns an empty task on this route.
            crate::share::CopyStep::Detached => {
                cmds.push(crate::share::copy_text_task(&value));
                // The window is about to open with the pick already on the clipboard, so
                // it opens wearing the tick and the word (the owner's ask).
                cmds.push(self.flash_copied());
            }
            // The window route: arm the latch and bound the wait. `flush_deferred_pick_copy`
            // writes it the moment the result window takes the keyboard, which is also the
            // input event whose serial the selection needs.
            crate::share::CopyStep::WaitForFocus => {
                self.color_picker.copy_waiting = true;
                cmds.push(Task::perform(
                    async {
                        tokio::time::sleep(crate::share::WINDOW_COPY_FOCUS_BUDGET).await;
                    },
                    |()| {
                        cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::PickCopyDeadline))
                    },
                ));
            }
            // Neither is reachable from here (the focus term is false and the budget is
            // fresh), and both are answered by the deferral above rather than by a write this
            // surface cannot serve. Written out so a future change to the ladder cannot leave
            // this arm silently doing nothing.
            crate::share::CopyStep::ThroughWindow | crate::share::CopyStep::ReportFailed => {
                self.color_picker.copy_waiting = true;
            }
        }
        let (id, open) = crate::app::color_picker::open_color_picker_window();
        self.color_picker.window = Some(id);
        // DRAGON-613: BIND THE LISTENER BEFORE THE MARKER BELOW, the same ordering
        // `preview_surface_for` uses and for the same reason: the marker IS the discovery
        // record, so binding second would open a window in which a later pick finds a window
        // that is not listening and needlessly opens a second one. (That direction only
        // costs the optimisation, never the colour.) A bind failure is non-fatal: we simply
        // do not receive, and every later pick behaves exactly as it did before this existed.
        #[cfg(any(unix, windows))]
        if self.handoff_host.is_none() {
            let addr = crate::instance::color_picker_host_address(std::process::id());
            match crate::preview_ipc::start_host_at(addr) {
                Ok(host) => {
                    log::info!("color picker: listening for picks on {}", host.address());
                    self.handoff_host = Some(host);
                }
                Err(e) => log::warn!("color picker: not listening for picks ({e})"),
            }
        }
        // DRAGON-582: advertise the open window so a LATER capture's sibling sweep spares
        // this process. Picking a colour and then screenshotting something to use it in is
        // the obvious next step, and without this the screenshot would kill the window
        // holding the value. Cleared at `finish_session`, like the preview marker.
        // DRAGON-613: it is ALSO the discovery record a later pick finds, so that pick can
        // update this window instead of opening a second one.
        crate::instance::set_color_picker_marker(true);
        cmds.push(open);
        Task::batch(cmds)
    }

    /// Hand this pick to whoever it is FOR, and end the session if they took it.
    ///
    /// `Some(task)` ONLY when a live consumer has positively ACKNOWLEDGED the colour, in
    /// which case this process is done: the colour is where the user wanted it. `None` for
    /// every other outcome, so the caller opens the result window exactly as it always did.
    ///
    /// **ONE LADDER, tried in order, each rung only if it applies:** the editor that asked
    /// (if [`crate::app::color_picker::PickDestination`] names one), then THE colour picker
    /// window (if one is open anywhere), then this process's own new window. Written as a
    /// ladder rather than as two independent destinations because a rung that fails must
    /// fall to the NEXT one, not to the bottom.
    ///
    /// That distinction is load-bearing for the single-window rule. An editor pick whose
    /// editor has since closed used to go straight to opening a window, which would open a
    /// SECOND one whenever a picker window was already up. Degrading it into the picker rung
    /// keeps "at most one window" true in every case, including the failure cases, and the
    /// colour is still never lost because the bottom rung is unconditional.
    fn deliver_pick(&mut self, color: Srgb) -> Option<Task<cosmic::Action<Msg>>> {
        use crate::app::color_picker::PickDestination;
        let rgb = [color.r, color.g, color.b];
        let dest = crate::app::color_picker::pick_destination(
            std::env::var(crate::app::color_picker::COLOR_TO_PID_ENV).ok().as_deref(),
        );
        // Rung 1, DRAGON-587: a pick launched from a preview editor's PIPETTE belongs to that
        // editor. Deliver it and end with no result window: the user is mid-annotation and
        // asked for a drawing colour, not for a window to read and close, and the colour is
        // immediately visible in the editor's own swatch.
        if let PickDestination::Editor(pid) = dest {
            match crate::preview_ipc::send_color_to_pid(pid, rgb) {
                Ok(()) => {
                    log::debug!("color picker: the editor at pid {pid} took the picked color");
                    // The value COPY is deliberately skipped here when the session copies
                    // through a window, and the reason is stated rather than hidden: there is
                    // no window here to serve a selection from, and the colour has already
                    // gone where it was asked to go. On a data-control session the detached
                    // worker still runs, so the value (in the remembered mode's spelling,
                    // DRAGON-630) is on the clipboard as a bonus.
                    if crate::share::copy_route() == crate::share::CopyRoute::Standalone {
                        crate::share::copy_text(&self.color_picker.mode.format(color));
                    }
                    let mut cmds = self.destroy_surfaces();
                    cmds.push(self.finish_session());
                    return Some(Task::batch(cmds));
                }
                Err(e) => log::info!(
                    "color picker: the editor at pid {pid} did not take the color ({e}); \
                     trying the picker window"
                ),
            }
        }
        // Rung 2, DRAGON-613: if a picker window is open ANYWHERE it takes the colour, which
        // becomes the colour it shows and its newest recent, and no second window appears.
        match crate::preview_ipc::send_color_to_picker(rgb) {
            Ok(pid) => {
                log::debug!(
                    "color picker: the open picker window at pid {pid} took the picked color"
                );
                // The clipboard is the RECEIVER's job on this rung, and deliberately so. It
                // has a real window that can hold the keyboard, which is the only way a
                // selection goes out on the `ThisWindow` route; writing it here would put
                // nothing on the clipboard for exactly the sessions DRAGON-587 fixed. See
                // `App::apply_handoff_pick`.
                let mut cmds = self.destroy_surfaces();
                cmds.push(self.finish_session());
                Some(Task::batch(cmds))
            }
            // Rung 3 is the caller's: no consumer took it, so this process opens its own
            // window, exactly as it did before any of this existed. This is the ONE place
            // every failure lands (no window, one mid-close, a wedged one, a
            // version-mismatched one, a platform with no transport), which is what makes
            // "a pick is never lost" structural rather than a list of handled cases.
            Err(e) => {
                log::info!(
                    "color picker: no open picker window took the color ({e}); opening one here"
                );
                None
            }
        }
    }

    /// DRAGON-613: a pick made by ANOTHER process has arrived for this window. Apply it
    /// exactly as a local pick would.
    ///
    /// `None` when this process has no picker window, which the drain turns into a refusal so
    /// the sender opens its own window rather than losing the colour. `Some(task)` once the
    /// colour is actually in our state, which is the only point at which the drain may ack.
    ///
    /// It routes through the SAME `set_picker_color(.., ColorSource::Pick)` a local pick
    /// uses, which is the one place `geom::writes_recents` is applied. So a handed-over pick
    /// IS a pick: it becomes the shown colour and is promoted to the front of the recents,
    /// under the one rule, with no second copy of that rule here.
    ///
    /// It also takes the CLIPBOARD job the sender could not do. Before this, a second pick
    /// opened a second window and that window's focus is what carried the hex out on a
    /// [`crate::share::CopyRoute::ThisWindow`] session (Flatpak, GNOME, sandboxed niri and
    /// Hyprland). Handing off and exiting with no window would have quietly stopped that, so
    /// the copy moves to the process that still HAS a window, through the app's one copy
    /// ladder (`share::copy_step`) rather than a second reading of it.
    // Its only caller is the drain (`drain_preview_handoffs`), which is a `Task::none()`
    // stub only where no transport exists (neither unix sockets nor Windows named pipes).
    // So this is honestly dead only there, and it stays portable: the body is plain app
    // state plus the shared copy ladder. DRAGON-651 proved the old claim here — that a
    // Windows named-pipe transport would call it unchanged — by doing exactly that.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    pub(in crate::app) fn apply_handoff_pick(
        &mut self,
        rgb: [u8; 3],
    ) -> Option<Task<cosmic::Action<Msg>>> {
        let id = self.color_picker.window?;
        let color = Srgb::new(rgb[0], rgb[1], rgb[2]);
        self.set_picker_color(color, geom::ColorSource::Pick);
        self.save_state();
        // The colour itself is user content and is never logged.
        log::debug!("color picker: took a color picked by another process, and promoted it");
        // Raise the window: the user just picked a colour and this window is the only thing
        // reporting it, so leaving it behind whatever they were looking at would read as the
        // pick having done nothing.
        let mut cmds = vec![window::gain_focus(id)];
        // The remembered mode's spelling (DRAGON-630), like every other pick copy; the
        // pick just reset the alpha, so this is the mode's plain form.
        let value = self.color_picker.value_text();
        // Unlike the pick handler, the window here already EXISTS, so the focus term is a
        // real lookup rather than a literal `false`, and an already-focused window writes
        // immediately instead of waiting for a focus event that would never come.
        let focused = self.focused_window == Some(id);
        match crate::share::copy_step(crate::share::copy_route(), focused, false) {
            crate::share::CopyStep::Detached | crate::share::CopyStep::ThroughWindow => {
                cmds.push(crate::share::copy_text_task(&value));
                cmds.push(self.flash_copied());
            }
            // Our window exists but is not focused. The `gain_focus` above is the request
            // that fixes that, and `flush_deferred_pick_copy` writes the hex when it lands.
            crate::share::CopyStep::WaitForFocus => {
                self.color_picker.copy_waiting = true;
                cmds.push(Task::perform(
                    async {
                        tokio::time::sleep(crate::share::WINDOW_COPY_FOCUS_BUDGET).await;
                    },
                    |()| cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::PickCopyDeadline)),
                ));
            }
            // Unreachable with a fresh budget, and written out rather than folded into the
            // arm above so a future change to the ladder cannot leave it silently doing
            // nothing. The deadline handler reports the miss.
            crate::share::CopyStep::ReportFailed => self.color_picker.copy_waiting = true,
        }
        Some(Task::batch(cmds))
    }

    /// DRAGON-587: the result window took keyboard focus — if the pick's hex copy is waiting
    /// on exactly that, write it now.
    ///
    /// The twin of `App::flush_deferred_auto_copy`, and for the same reason: on the
    /// [`crate::share::CopyRoute::ThisWindow`] route the selection goes out over
    /// `wl_data_device`, which needs a serial from an input event delivered to this client,
    /// and a keyboard focus IS that event.
    ///
    /// A no-op for every other window, every other route, and a pick whose deadline already
    /// fired: `copy_waiting` is the one-shot latch. The cost of this route is the documented
    /// one, so it is stated rather than hidden: the selection lives exactly as long as this
    /// process, so closing the picker window takes the copy with it unless a clipboard manager
    /// has claimed it in the meantime.
    pub(in crate::app) fn flush_deferred_pick_copy(
        &mut self,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        if !self.color_picker.copy_waiting || self.color_picker.window != Some(id) {
            return Task::none();
        }
        self.color_picker.copy_waiting = false;
        log::debug!("color picker: the result window took focus, writing the pick's copy");
        // Re-derived from the live state rather than stored twice (and in the remembered
        // mode's spelling, DRAGON-630). Nothing can have changed it between the pick and
        // this focus: the window is only now becoming interactive.
        let write = crate::share::copy_text_task(&self.color_picker.value_text());
        // THIS is the moment the pick reaches the clipboard on the window route, so this
        // is where the flash belongs: the window has just appeared and taken the
        // keyboard, which is exactly the "opened the panel" instant the owner means.
        Task::batch([write, self.flash_copied()])
    }

    /// Set the colour the window shows, and write the recents only when the SOURCE says
    /// so ([`geom::writes_recents`]). The one place that rule is applied.
    ///
    /// The tracked HSV follows the colour here (`color::hsv_tracking`), which is what
    /// keeps the square and the hue strip honest after a pick, a recent-click, a typed
    /// edit or a handed-over pick. The square and hue handlers deliberately do NOT come
    /// through here: they own the HSV and call [`Self::apply_picker_color`] directly,
    /// so byte quantisation cannot re-track the exact position the hand just chose.
    fn set_picker_color(&mut self, color: Srgb, source: geom::ColorSource) {
        self.color_picker.hsv = crate::color::hsv_tracking(self.color_picker.hsv, color);
        self.refresh_sv_raster();
        self.apply_picker_color(color, source);
    }

    /// The body of [`Self::set_picker_color`] minus the HSV tracking (DRAGON-630).
    fn apply_picker_color(&mut self, color: Srgb, source: geom::ColorSource) {
        self.color_picker.color = color;
        // A new colour makes any in-flight draft stale: it belonged to the old value.
        if source != geom::ColorSource::Edit {
            self.color_picker.draft = None;
            // A pick or a loaded recent IS an opaque colour: alpha is an authoring
            // control for the value being copied out, not a property of anything
            // sampled, so it resets rather than tinting a colour the user just took.
            self.color_picker.alpha = u8::MAX;
        }
        if geom::writes_recents(source) {
            self.color_picker.recents =
                geom::push_recent(&self.color_picker.recents, color, geom::RECENTS_CAP);
        }
        self.refresh_color_rasters();
    }

    /// Rebuild the SATURATION/VALUE square's raster (and the hue strip's, once): the
    /// square's whole picture is the hue, so only hue changes come here (DRAGON-630).
    /// Rasters are built in update, never in `view`, the magnifier's own rule: a handle
    /// minted per frame churns iced's texture atlas.
    fn refresh_sv_raster(&mut self) {
        use crate::app::color_picker::geom as g;
        let radius = picker_corner_radius();
        let cp = &mut self.color_picker;
        if cp.hue_raster.is_none() {
            let (w, h) = (g::STRIPS_W as u32, g::STRIP_H as u32);
            cp.hue_raster = Some(widget::image::Handle::from_rgba(
                w,
                h,
                crate::color::hue_strip_rgba(w, h, radius),
            ));
        }
        let (w, h) = (g::CONTENT_W as u32, g::SV_H as u32);
        cp.sv_raster = Some(widget::image::Handle::from_rgba(
            w,
            h,
            crate::color::sv_square_rgba(cp.hsv[0], w, h, radius),
        ));
    }

    /// Rebuild the rasters that follow the COLOUR and the ALPHA: the alpha strip (the
    /// colour ramps across it) and the round swatch (DRAGON-630).
    fn refresh_color_rasters(&mut self) {
        use crate::app::color_picker::geom as g;
        let radius = picker_corner_radius();
        let rim = picker_rim();
        let cp = &mut self.color_picker;
        let (w, h) = (g::STRIPS_W as u32, g::STRIP_H as u32);
        cp.alpha_raster = Some(widget::image::Handle::from_rgba(
            w,
            h,
            crate::color::alpha_strip_rgba(cp.color, w, h, radius),
        ));
        let d = g::SWATCH_CIRCLE as u32;
        cp.swatch_raster = Some(widget::image::Handle::from_rgba(
            d,
            d,
            crate::color::swatch_circle_rgba(cp.color, cp.alpha, d, rim),
        ));
    }

    /// File the colour the window is SHOWING into the history: the shared tail of the
    /// "Add color" button on the divider and of Enter in a value box.
    ///
    /// The colour is already the one on screen, so nothing about the window changes. This
    /// is deliberately NOT `apply_picker_color`, whose non-Edit sources reset the alpha
    /// and drop the draft: right for a colour that just ARRIVED, wrong for one the user
    /// built and is now keeping. What it borrows from a pick is the part that matters,
    /// `geom::push_recent`'s rule: the colour goes to the front, any earlier copy of it is
    /// removed rather than duplicated, and the list is capped. Filing a colour that
    /// already leads the history is therefore a no-op, which is what has to happen for a
    /// gesture that cannot know whether you made it twice.
    ///
    /// The save is the one a pick performs, for the same reason: an entry that vanished
    /// when this one-shot window closed would not be history.
    fn add_shown_color_to_history(&mut self) {
        self.color_picker.recents = geom::push_recent(
            &self.color_picker.recents,
            self.color_picker.color,
            geom::RECENTS_CAP,
        );
        self.save_state();
        log::debug!("color picker: added the shown color to the history");
    }

    /// Copy the current mode's whole value and flash the copy affordance: the shared
    /// tail of the copy button and the mode stepper (DRAGON-630).
    fn copy_picker_value(&mut self) -> Task<cosmic::Action<Msg>> {
        let value = self.color_picker.value_text();
        log::debug!("color picker: copied the {} value", self.color_picker.mode.id());
        Task::batch([crate::share::copy_text_task(&value), self.flash_copied()])
    }

    /// Raise the "Copied!" flash: the copy button's tick plus the word beside it, for
    /// [`crate::widgets::copy_button::COPIED_FLASH`].
    ///
    /// Called from every path that ACTUALLY writes the clipboard, which is why it is its
    /// own function rather than two lines inside the Copy button's handler. The window's
    /// own opening copy is one of those paths (the owner's ask: the flash should be up
    /// the moment the window appears, because by then the pick is already on the
    /// clipboard), and on the deferred route that moment is when the window takes focus
    /// and the write goes out, NOT when the pick happened. A copy that misses its
    /// deadline raises nothing: the flash says "this worked", so it may only appear where
    /// something did.
    ///
    /// The returned task is one delayed clear rather than a subscription ticking for two
    /// seconds: the flash needs exactly one redraw, at its end.
    fn flash_copied(&mut self) -> Task<cosmic::Action<Msg>> {
        self.color_picker.copied = Some((self.color_picker.mode, std::time::Instant::now()));
        Task::perform(
            async {
                tokio::time::sleep(crate::widgets::copy_button::COPIED_FLASH).await;
            },
            |()| cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::ClearCopied)),
        )
    }

    /// Finish the result window natively once its async-set title has landed: the mac
    /// traffic-light centring (which also reveals the window vibrancy), and the Windows
    /// centre-then-show plus caption buttons and Mica.
    ///
    /// Title-matched, so it polls on the same 30 x 40ms budget every other native
    /// finalize in the app uses, then gives up loudly rather than retrying forever.
    pub(in crate::app) fn finalize_color_picker_window(
        &mut self,
        id: window::Id,
        attempt: u8,
    ) -> Task<cosmic::Action<Msg>> {
        if self.color_picker.window != Some(id) {
            return Task::none();
        }
        let title_task =
            self.set_window_title(crate::app::color_picker::WINDOW_TITLE.to_string(), id);
        #[cfg(target_os = "macos")]
        {
            // This window is a REAL window, so it gets the REGULAR activation policy: a
            // Dock icon, a Cmd+Tab entry, and focus from another app, exactly like the
            // settings window. Without it the picker window was reachable only by clicking
            // it, and a Cmd+Tab away from it left no way back.
            //
            // WHY here and not in `app::boots_regular_policy`, which is where every other
            // window launch is answered: a colour-picker launch is overlay-FIRST. It mints
            // the same per-output capture-shaped overlays a screenshot does, and only a
            // pick turns it into a window launch. Booting Regular would therefore promote
            // the OVERLAY phase, which breaks two invariants at once: the DRAGON-154
            // AeroSpace opt-out only ignores a window whose owner is `.accessory` at the
            // window's first AX exposure, so the picker's overlays would start being tiled
            // off their displays, and DRAGON-151's menu-bar stamp would land in the frozen
            // snapshot the picker reads its colours FROM. It would also put a Dock icon up
            // for the two picker launches that open no window at all (a pick delivered to
            // an editor, DRAGON-587, or handed to an already-live picker window,
            // DRAGON-613). So the promotion waits for the window, the same post-boot flip
            // `finalize_preview_window` makes for the windowed preview and for the same
            // reason: the overlays are gone by now (`color_picker_pick` destroys them in
            // the batch that queues this open), and a flip with a window already up is
            // keyboard-healthy where the DRAGON-150 pre-window flip was not.
            crate::platform::mac::window::ensure_regular_policy();
            // The same activation pair the permission window uses: `gain_focus` reaches
            // winit's `activateIgnoringOtherApps`, and `activate_our_app` adds the macOS
            // 14+ cooperative form winit never calls. This window is opened by a process
            // that is not frontmost (a tray spawn, or its own overlay just closed), which
            // is exactly when the deprecated form can be declined. It runs AFTER the policy
            // above, since activating an Accessory app mints no Dock icon to activate into.
            crate::platform::mac::window::activate_our_app();
            let _ = attempt;
            Task::batch([
                window::gain_focus(id),
                title_task,
                Task::done(cosmic::Action::App(Msg::WindowChrome(
                    WindowChromeMsg::MacCenterTitlebar(crate::app::color_picker::WINDOW_TITLE, 0),
                ))),
                // DRAGON-587: and take the zoom button + full screen away, which is the half
                // of "never resizable" that `min_size == max_size` cannot reach. Its own
                // message rather than a fold into the centring poll above, because that one
                // also serves the settings window, which IS resizable.
                Task::done(cosmic::Action::App(Msg::WindowChrome(
                    WindowChromeMsg::MacPinWindow(crate::app::color_picker::WINDOW_TITLE, 0),
                ))),
            ])
        }
        #[cfg(windows)]
        {
            const MAX_ATTEMPTS: u8 = 30;
            const RETRY_MS: u64 = 40;
            let title = crate::app::color_picker::WINDOW_TITLE;
            // Centre it on the display the pick happened on, which is where the user is
            // looking. `preview_overlay_rect(None)` falls back to the pointer's display.
            let (pos, size) = crate::platform::windows::window::preview_overlay_rect(
                self.color_picker.pick_output.as_deref(),
            );
            let monitor = (pos.0, pos.1, size.0 as i32, size.1 as i32);
            if crate::platform::windows::window::show_centered(title, monitor) {
                // Chrome, not glass: the caption buttons are installed unconditionally,
                // exactly as the preview window does.
                crate::platform::windows::caption::install_native_caption_buttons(title);
                // ...and that install is what makes the window narrower than its own layout
                // (DRAGON-668). The subclass carves a non-client frame OUT OF THE CLIENT
                // (`caption::calc_frame`), 16x8 physical px measured here, while leaving the
                // OUTER rect alone. The picker is pinned `min_size == max_size ==
                // color_window_size()`, so unlike every other window it cannot grow to absorb
                // that: its 366pt of sections were being laid out into a 350pt client, and the
                // shortfall lands entirely on the last element of each row — the copy button,
                // and the fourth of the four component boxes.
                //
                // `enforce_client_floor` is the settings window's existing answer and it fits
                // exactly: it takes a LOGICAL client size, measures the live non-client band
                // rather than predicting it, and returns without touching anything once the
                // client is already at or above the floor, so re-running it on a DPI change
                // cannot make the window creep. The floor here IS the window's fixed size,
                // which is the one size the layout is built for.
                let (cw, ch) = crate::app::color_picker::geom::color_window_size();
                crate::platform::windows::window::enforce_client_floor(
                    title,
                    (cw.round().max(1.0) as u32, ch.round().max(1.0) as u32),
                );
                if self.glass.is_some_and(|g| g.frosted_windows) {
                    crate::platform::windows::window::apply_window_glass(title);
                }
                return title_task;
            }
            if attempt >= MAX_ATTEMPTS {
                log::warn!(
                    "color picker window never matched its title after {MAX_ATTEMPTS} attempts \
                     — it may stay hidden"
                );
                return title_task;
            }
            Task::batch([
                title_task,
                Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                    },
                    move |()| {
                        cosmic::Action::App(Msg::WindowChrome(
                            WindowChromeMsg::ColorPickerWindowOpened(id, attempt + 1),
                        ))
                    },
                ),
            ])
        }
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        {
            let _ = attempt;
            title_task
        }
    }
}

/// DRAGON-TBD: which resample routes may leave the magnifier's raster stale. The pacing is a
/// performance choice and the keyboard nudge is a correctness one, so the property pinned here
/// is that the first can never reach the second.
#[cfg(test)]
mod raster_policy_tests {
    use super::*;
    use std::time::Duration;

    /// Every speed worth naming, from stopped to an absurd one, so "any speed" below is not
    /// three convenient samples.
    const SPEEDS: [f32; 7] =
        [0.0, 100.0, geom::DELIBERATE_SPEED, 2_000.0, geom::FLICK_SPEED, 50_000.0, f32::NAN];

    /// THE guarantee the owner asked for. An arrow key (or its vim letter) must move the
    /// sample and rebuild the lens immediately, EVERY time, and it routes through
    /// `color_picker_resample`, which is `RasterPolicy::Always`. No pointer speed and no
    /// recent raster may make that skip.
    ///
    /// The realistic hazard is not hypothetical: a nudge is most likely pressed just after the
    /// hand has stopped moving, which is exactly when the measured speed is still high and a
    /// raster is freshly spent. Both conditions are pinned here together.
    #[test]
    fn a_keyboard_nudge_is_never_paced_whatever_the_pointer_was_doing() {
        for speed in SPEEDS {
            for since in [None, Some(Duration::ZERO), Some(Duration::from_micros(1)),
                          Some(Duration::from_millis(39)), Some(Duration::from_secs(9))]
            {
                assert!(
                    !skips_raster(RasterPolicy::Always, since, speed),
                    "an unpaced route skipped its raster at {speed} pt/s, {since:?} since the last"
                );
            }
        }
    }

    /// The same for the two other `Always` routes, which are correctness paths too: the
    /// frozen-snapshot delivery that makes the loupe appear at all (DRAGON-601) and a
    /// session's first sample. Both share the arm above, so this is really one more statement
    /// of intent than a second mechanism, and it is worth stating because the pacing would be
    /// invisible on those routes until someone reported a picker that opened blank.
    #[test]
    fn the_first_lens_of_a_session_is_never_paced() {
        for speed in SPEEDS {
            assert!(!skips_raster(RasterPolicy::Always, None, speed));
            // …and even on the paced route, "nothing rastered yet" always rasters.
            assert!(!skips_raster(RasterPolicy::Paced, None, speed));
        }
    }

    /// The pacing still DOES something, or this seam would be decoration: on the pointer's own
    /// route, mid-flick and one frame after a raster, it declines.
    #[test]
    fn the_pointer_route_still_paces_a_flick() {
        let one_frame = Some(Duration::from_micros(8_333));
        assert!(skips_raster(RasterPolicy::Paced, one_frame, geom::FLICK_SPEED));
        // And releases as soon as the hand slows, on the very next look.
        assert!(!skips_raster(RasterPolicy::Paced, one_frame, 0.0));
        assert!(!skips_raster(RasterPolicy::Paced, one_frame, geom::DELIBERATE_SPEED));
    }
}

/// DRAGON-587: what the PICK's clipboard copy does, on each session shape. It pins the
/// picker to the app's ONE copy ladder (`share::copy_step`) rather than to a second copy of
/// the reasoning, which is the whole point of the fix.
#[cfg(test)]
mod pick_copy_tests {
    use crate::share::{CopyRoute, CopyStep, copy_step};

    /// The pick's own inputs: the result window is minted BY this handler, so it can never
    /// hold focus yet, and the budget is fresh. Those two terms are fixed; only the session's
    /// route varies, and it decides everything.
    const AT_PICK: (bool, bool) = (false, false);

    /// A data-control Linux session, macOS and Windows are BYTE-IDENTICAL to before the fix:
    /// the detached worker takes the selection in the pick's own batch and outlives us.
    #[test]
    fn a_standalone_session_still_copies_in_the_pick_itself() {
        assert_eq!(
            copy_step(CopyRoute::Standalone, AT_PICK.0, AT_PICK.1),
            CopyStep::Detached
        );
    }

    /// THE reported bug. A Flatpak on COSMIC (and GNOME, and sandboxed niri/Hyprland) has no
    /// data-control, so the hex can only ride one of our own focused surfaces — and at the
    /// instant of the pick there is none, because the same batch destroys the overlay and only
    /// QUEUES the result window. The answer must be "wait", never "write now".
    #[test]
    fn a_window_route_pick_waits_for_the_result_windows_focus() {
        assert_eq!(
            copy_step(CopyRoute::ThisWindow, AT_PICK.0, AT_PICK.1),
            CopyStep::WaitForFocus
        );
        assert_ne!(
            copy_step(CopyRoute::ThisWindow, AT_PICK.0, AT_PICK.1),
            CopyStep::ThroughWindow,
            "writing at pick time is exactly what put nothing on the clipboard"
        );
    }

    /// Once the window HAS the keyboard the deferred write goes out, and a wait that outlives
    /// the budget is reported rather than forgotten. These are the two ends
    /// `flush_deferred_pick_copy` and `PickCopyDeadline` implement.
    #[test]
    fn the_focus_writes_and_the_budget_reports() {
        assert_eq!(copy_step(CopyRoute::ThisWindow, true, false), CopyStep::ThroughWindow);
        assert_eq!(copy_step(CopyRoute::ThisWindow, false, true), CopyStep::ReportFailed);
    }
}

/// The value the pick puts on the clipboard: the REMEMBERED mode's spelling
/// (DRAGON-630, the owner's ask). Hex is the default mode, so DRAGON-582's original
/// wording ("the hex INCLUDING the leading `#`") still holds on an untouched config,
/// and both facts are pinned because the copy is a string the user pastes into a
/// stylesheet: its exact spelling is the feature.
#[cfg(test)]
mod pick_copy_text_tests {
    use crate::app::color_picker::ColorPickerState;
    use crate::color::{ColorFormat, Srgb};

    #[test]
    fn the_default_mode_still_copies_the_leading_hash_hex() {
        let st = ColorPickerState { color: Srgb::new(255, 136, 0), ..Default::default() };
        assert_eq!(st.value_text(), "#FF8800");
        assert!(
            st.value_text().starts_with('#'),
            "a hex without the # is not a colour anyone can paste"
        );
    }

    /// A remembered RGB mode makes the pick copy `rgb(…)`: the mode the user left the
    /// window in is the spelling the next pick delivers.
    #[test]
    fn a_remembered_mode_spells_the_pick_copy() {
        let st = ColorPickerState {
            color: Srgb::new(255, 136, 0),
            mode: ColorFormat::Rgb,
            ..Default::default()
        };
        assert_eq!(st.value_text(), "rgb(255, 136, 0)");
    }
}
