//! The colour picker's INPUT surface (DRAGON-582).
//!
//! A leaf widget that fills its output overlay, reports every pointer position, and
//! reports a left click. It draws NOTHING: the dim, the frozen backdrop, the magnifier
//! disc and the hex label are ordinary stacked iced elements underneath it
//! (`app::color_picker::view`).
//!
//! That split is deliberate rather than incidental. Windows 10 renders our overlays with
//! iced's SOFTWARE rasterizer (`platform::software_overlays`), which cannot draw a
//! `shader` widget at all and has no image-drawing entry point on the plain `Renderer`
//! trait a leaf widget gets. Composing from `widget::image` and `widget::container`
//! keeps the whole picker on the paths tiny-skia does support, so the one platform that
//! most needs a working colour picker is not the one platform it is blank on.
//!
//! The widget is transparent and sits ON TOP of that stack, so it receives the events the
//! elements below would otherwise not care about.

use cosmic::iced::core::widget::{Tree, tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, mouse,
};
use cosmic::widget::Widget;

/// Screen scroll DISTANCE that counts as one zoom notch on a pixel-precise device
/// (DRAGON-587).
///
/// A mouse wheel reports LINES, one per detent, so a notch is a notch. A trackpad reports
/// PIXELS, and a two-finger swipe easily carries hundreds of them, so it needs a divisor or
/// one flick would cross the whole zoom range. This is `zoom_pan.rs`'s `WHEEL_PAN_STEP`, the
/// same number the preview editor already converts pixel deltas with, so the two gestures feel
/// the same in both places rather than each inventing a sensitivity.
const WHEEL_ZOOM_STEP: f32 = 48.0;

/// How close to its own edge an ENTER must land to read as the pointer ARRIVING rather than
/// the pointer being REVEALED, in logical points. See [`pointer_report_moves_sample`].
const ENTER_EDGE_BAND: f32 = 8.0;

/// Per-instance state.
///
/// `zoom_accum` is the FRACTION of a notch a pixel-precise scroll has carried so far
/// (DRAGON-587). Without it a slow trackpad swipe, which arrives as a stream of small pixel
/// deltas, would round to zero on every event and never zoom at all.
///
/// `after_enter` marks that a `CursorEntered` arrived on the PREVIOUS event, so the
/// `CursorMoved` our iced fork pushes straight after it can be told apart from a real motion
/// (DRAGON-611). See [`pointer_report_moves_sample`].
///
/// `entered_at` is the DRAGON-331 cursor re-assert stamp (`crate::widgets::cursor_reassert`).
/// DRAGON-597 removed it from this widget and DRAGON-601 put it back. Two rounds of wrong
/// reasoning are recorded here, because both are easy to arrive at again.
///
/// **DRAGON-597's reason for removing it**: the re-assert exists because cosmic-comp drops a
/// `set_cursor` issued too soon after `wl_pointer.enter`, and the only cursors this surface
/// asks for are HIDDEN, "with no compositor drop in play", and the DEFAULT ARROW, which is
/// what a dropped `set_cursor` leaves behind anyway. Wrong on the first half: hiding the
/// pointer IS a `set_cursor`. Our iced fork implements `set_cursor_visible(false)` as
/// `wl_pointer.set_cursor(serial, NULL, 0, 0)`, the same request through the same door. A
/// dropped hide is also strictly WORSE than the dropped crosshair the mechanism was built for,
/// because a visible arrow over the sample is the very thing the picker removes it to avoid.
///
/// **DRAGON-601's first reason for restoring it**: that the hide was issued, dropped by the
/// compositor, and never re-sent because the resolved interaction never CHANGES under a
/// resting pointer. Also wrong, and measured wrong. Under `WAYLAND_DEBUG=1` the hide never
/// reached the wire AT ALL: no `wl_pointer.set_cursor`, no `wp_cursor_shape_device_v1.set_shape`,
/// nothing, across the whole session. It was not dropped by the compositor, it was never sent.
///
/// **What is actually happening.** The overlay maps under the pointer and asks for its cursor
/// on its first frame, about 55 ms BEFORE the compositor sends `wl_pointer.enter`. A cursor
/// request needs an enter serial, so sctk cannot make it, and it is discarded with no error
/// anyone sees. The seat then records the icon as applied regardless, so the re-assert the
/// enter itself would have done compares equal and skips. The picker is simply early, once,
/// and nothing ever asks again. That is why the stamp now starts from the surface's own
/// APPEARANCE (`cursor_reassert::arm_on_appear`) rather than from a `CursorEntered` that never
/// arrives on a layer surface: the re-assert lands 200 ms after the surface appeared, by which
/// time the serial exists.
#[derive(Default)]
struct State {
    zoom_accum: f32,
    after_enter: bool,
    entered_at: Option<cosmic::iced::core::time::Instant>,
}

/// **Pure**, unit-tested: the cursor this surface asks the toolkit for.
///
/// **Neither answer asks whether the pointer is OVER the widget** (DRAGON-601, second pass),
/// and dropping that question is the fix. The measurement behind it:
///
/// A picker session mints one of these surfaces on EVERY output, and each one FILLS its
/// output, so while the session is up the pointer is over one of them by construction and the
/// right answer on all of them is the same: no sprite. Asking "is the pointer over me" first
/// made the answer depend on the toolkit knowing WHERE the pointer is, and on a Wayland LAYER
/// SURFACE it does not know until the pointer MOVES. Traced live under `WAYLAND_DEBUG=1` on
/// cosmic-comp: the compositor delivers `wl_pointer.enter(serial, surface, 2944.6, 1173.7)` to
/// the overlay, and for the next 2.4 seconds, across ~250 of our own frames, the widget is
/// handed `mouse::Cursor::Unavailable` and receives no `CursorEntered` at all. The first
/// `wl_pointer.motion` fixes it within 61 microseconds. So the answer was "default arrow" for
/// exactly as long as the user did not touch the mouse, and the sprite sat on top of the pixel
/// the magnifier was reading. That is the owner's "i have to move the mouse before the mouse
/// vanishes".
///
/// The DIP has to lose that question too, and this is the part that is easy to get half right.
/// It reports the default cursor for a few milliseconds so that the hide which follows is a
/// CHANGE, and the toolkit only re-issues a cursor on a change. A dip that is skipped because
/// the toolkit cannot place the pointer produces no change, so the hide is computed forever and
/// sent never. Measured exactly that way on a build that dropped the question from the hide but
/// left it on the dip: the widget returned `Hidden` from the first frame to the last, and not
/// one cursor request reached the wire.
///
/// The remaining answer, the fallthrough default, is unreachable while the iced [patch] is in
/// place; see the tombstone in the body.
fn picker_interaction(hide_pointer: bool, in_dip: bool) -> mouse::Interaction {
    // The dip outranks the hide: its entire job is to make the hide that follows a change.
    if in_dip {
        return mouse::Interaction::default();
    }
    if hide_pointer {
        return mouse::Interaction::Hidden;
    }
    // UNREACHABLE today, and kept as the honest degradation if the iced [patch] is ever
    // dropped: the compositor's DEFAULT arrow.
    //
    // DRAGON-597 tombstone. This used to be a designed-around ARROW rather than a
    // fallthrough, and the reasoning is worth keeping because it explains a choice that
    // otherwise looks arbitrary. On a layer surface the sprite could not be hidden at all,
    // so the picker asked for the arrow ON PURPOSE, not for a crosshair: an arrow's hotspot
    // is its TIP, so its visual mass falls down and right and everything up and left of the
    // tip stays visible, while a crosshair is drawn AROUND its own hotspot and covers the
    // read pixel from every side. The SAMPLE then moved one point up and left, to the
    // nearest pixel the tip did not cover. Both halves went together in DRAGON-597, and if
    // they ever come back they come back together: an arrow with an unshifted sample reports
    // a pixel the user cannot see, which is the bug the owner reported three times.
    mouse::Interaction::default()
}

/// **Pure**, unit-tested: may a REDRAW publish the pointer's position, standing in for a
/// `wl_pointer.enter` the widget never received? (DRAGON-610)
///
/// # The measurement this exists for
///
/// A tray-launched picker holds its frozen-flats grab until the overlay takes keyboard focus,
/// which is what dismisses the tray dropdown (DRAGON-600). Measured on a build with the settle
/// deliberately lengthened from 150ms to 2000ms: while that hold is up, this widget's `update`
/// is **never called at all**, not even with `RedrawRequested`. The compositor's
/// `wl_pointer.enter` lands at +670ms, squarely inside that dead window, and reaches the
/// app's `event::listen_with` subscription but no widget. The instant the hold released at
/// +2638ms a redraw could place the cursor, and it stayed placeable for the next 4.1 seconds
/// while the picker drew nothing, because [`ColorPickSurface`] samples only on a
/// `CursorMoved` and no further pointer event was coming under a resting hand. The loupe
/// appeared only when the user finally moved, at +6786ms.
///
/// So the position was in the toolkit the whole time and nothing asked for it. This asks.
///
/// # Why the gate is the APP's state and not this widget's
///
/// **This is the trap, and the wrong version is the obvious one.** A `bool` in [`State`]
/// saying "I have published once" looks equivalent and is not: iced recreates a widget's
/// `State` when the view's structure changes, and the picker's own view changes structure at
/// least twice per session, once when the frozen backdrop appears and again when the hover
/// layers are inserted. The same measurement above shows `State` being remade three times. A
/// State-scoped flag therefore re-arms mid-session, and a later redraw would publish a
/// `Moved`, which resets the keyboard nudge (`geom::nudge_after`, `SampleMove::Pointer`,
/// DRAGON-599) and silently snaps an arrow-key sample back under the pointer.
///
/// `needs_pointer` comes from `ColorPickerState::pointer.is_none()` instead, so the gate
/// closes for good the moment any sample lands, from this route or a real move.
///
/// # Two properties worth stating, because both look like bugs otherwise
///
/// * **The hotkey path is unchanged by construction**, not by intention. There the enter
///   reaches the widget normally and sets the pointer before any of this matters, so
///   `needs_pointer` is already false and this publishes nothing.
/// * **The loupe still does not appear during the hold, and must not.** The flats have not
///   been grabbed yet, so there is nothing to sample and `color_picker_resample` correctly
///   declines while `frozen_pending`. This makes the loupe appear at the hold release plus
///   the drain, which is the same moment the hotkey path shows it. Do not "fix" that.
///
/// Nudge-before-first-sample is the one ordering this deliberately does not protect: arrow
/// keys pressed before any pointer exists accumulate a nudge that this first sample resets.
/// That is the right answer anyway, since the first sample belongs at the pointer.
fn picker_wants_redraw_sample(needs_pointer: bool, cursor_placeable: bool) -> bool {
    needs_pointer && cursor_placeable
}

/// **Pure**, unit-tested: may this pointer report MOVE the picker's sample? (DRAGON-611)
///
/// # The two facts that arrive in one event
///
/// A `wl_pointer.enter` says "the pointer is at this position on your surface". That sentence
/// is true for two completely different reasons, and the picker must treat them differently:
///
/// * **The user MOVED the pointer here**, crossing in from another output. The loupe should
///   follow, and refusing to follow is the bug.
/// * **The pointer was REVEALED here.** Something that was covering this surface went away and
///   the compositor handed the pointer back, unmoved. Nothing happened that the user did, and
///   moving the loupe is the bug.
///
/// The second case is not hypothetical and it is not rare. A region capture over a live picker
/// hits it EVERY time: `begin_capture` tears its own overlay down and deliberately waits 200ms
/// before grabbing, so the compositor hands the pointer back to the picker underneath, our iced
/// fork turns that enter into a `CursorMoved` indistinguishable from a real move, the loupe
/// jumps, and the grab 200ms later photographs the jump. Measured on the owner's own artifact:
/// a circle fit put the magnifier disc 24.3px BELOW the bottom edge of the captured region,
/// nowhere near where they had parked it. The same disturbance comes from the `pointer_output`
/// probe, the preview editor and the settings window, so this is not a region-path special case.
///
/// # The distinction
///
/// **A pointer that ARRIVES must cross the surface's perimeter. A pointer that is REVEALED
/// appears wherever it already was, which is the interior.**
///
/// This is sound rather than a heuristic, and the reason is [`ColorPickSurface`]'s own geometry:
/// a picker session mints one of these per output, anchored on all four sides with no exclusive
/// zone, so the surface boundary IS the output boundary. The only way a pointer can legitimately
/// reach one is by crossing an edge from a neighbouring output. An interior appearance has no
/// physical path behind it.
///
/// # Why the band must not be widened
///
/// [`ENTER_EDGE_BAND`] is deliberately narrow, and the two ways it can be wrong are NOT
/// symmetric:
///
/// * **Too narrow** costs a fast crossing that lands deep inside before the first enter is
///   delivered. That self-heals on the very next motion event, which for a pointer still in
///   flight is one frame away, so nothing is visible. It would take a flick that stops dead in
///   the same instant to leave a stale loupe, and even then the next touch of the mouse fixes it.
/// * **Too wide** re-admits the actual bug, permanently, for any reveal that happens near a
///   screen edge. There is no self-healing on that side, because the capture has already been
///   taken by then.
///
/// So widening this to "be safe" makes it less safe. If a genuine crossing is ever seen being
/// missed, the fix is a trailing-motion confirmation, not a bigger band.
///
/// # The two escape hatches, both deliberate
///
/// * **A real motion always wins.** `from_enter` false returns true unconditionally: this
///   predicate only ever second-guesses an enter, never the user's hand.
/// * **The first sample always wins.** With no sample yet there is no loupe to protect and
///   nothing to keep still, so an enter anywhere places it. That is what keeps the startup
///   loupe (DRAGON-609) and the redraw stand-in (DRAGON-610) working untouched.
fn pointer_report_moves_sample(
    from_enter: bool,
    had_sample: bool,
    point: (f32, f32),
    surface: (f32, f32),
) -> bool {
    if !from_enter || !had_sample {
        return true;
    }
    let (x, y) = point;
    let (w, h) = surface;
    // A degenerate surface has no interior to speak of, so everything reads as an edge and the
    // behaviour falls back to what it was before this existed.
    x <= ENTER_EDGE_BAND
        || y <= ENTER_EDGE_BAND
        || (w - x) <= ENTER_EDGE_BAND
        || (h - y) <= ENTER_EDGE_BAND
}

/// Fills the overlay, publishes pointer moves and the left click.
pub struct ColorPickSurface<Msg> {
    /// Built with the pointer's SURFACE-LOCAL point on every `CursorMoved`.
    on_move: Box<dyn Fn((f32, f32)) -> Msg>,
    /// Built with the same point on a left press.
    on_pick: Box<dyn Fn((f32, f32)) -> Msg>,
    /// Built with a signed NOTCH COUNT (positive = zoom in) when the wheel or a trackpad
    /// scroll asks the magnifier to zoom (DRAGON-587).
    on_zoom: Box<dyn Fn(i32) -> Msg>,
    /// Hide the pointer SPRITE over this surface (DRAGON-587). Set only where the surface can
    /// actually do it (`platform::overlay_pointer_hideable`), which since DRAGON-597 is
    /// everywhere; everywhere else the DEFAULT ARROW stays, because `Interaction::Hidden` on a
    /// surface that cannot hide is a no-op.
    hide_pointer: bool,
    /// The picker has no sample position yet, so a redraw may supply one
    /// ([`picker_wants_redraw_sample`], DRAGON-610). Fed from
    /// `ColorPickerState::pointer.is_none()`, never from this widget's own `State`; the
    /// predicate's doc says why that distinction is load-bearing.
    needs_pointer: bool,
}

impl<Msg> ColorPickSurface<Msg> {
    pub fn new(
        on_move: impl Fn((f32, f32)) -> Msg + 'static,
        on_pick: impl Fn((f32, f32)) -> Msg + 'static,
        on_zoom: impl Fn(i32) -> Msg + 'static,
    ) -> Self {
        Self {
            on_move: Box::new(on_move),
            on_pick: Box::new(on_pick),
            on_zoom: Box::new(on_zoom),
            hide_pointer: false,
            needs_pointer: false,
        }
    }

    /// Hide the pointer sprite while the picker is up. See [`Self::hide_pointer`].
    pub fn hide_pointer(mut self, hide: bool) -> Self {
        self.hide_pointer = hide;
        self
    }

    /// The picker still has no sample position, so a redraw may publish one
    /// (DRAGON-610). See [`picker_wants_redraw_sample`].
    pub fn needs_pointer(mut self, needs: bool) -> Self {
        self.needs_pointer = needs;
        self
    }
}

impl<Msg: 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for ColorPickSurface<Msg> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &cosmic::iced::core::layout::Limits,
    ) -> cosmic::iced::core::layout::Node {
        cosmic::iced::core::layout::Node::new(
            limits
                .width(Length::Fill)
                .height(Length::Fill)
                .resolve(Length::Fill, Length::Fill, Size::ZERO),
        )
    }

    /// `layout` and `cursor` are deliberately unused: see [`picker_interaction`]. This surface
    /// fills its overlay and the picker mints one on every output, so "is the pointer over me"
    /// has no answer this widget needs, and on a layer surface the toolkit cannot answer it
    /// until the pointer moves anyway.
    fn mouse_interaction(
        &self,
        tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        picker_interaction(
            self.hide_pointer,
            crate::widgets::cursor_reassert::in_dip(
                tree.state.downcast_ref::<State>().entered_at,
            ),
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        // The re-assert dance starts when this SURFACE appears, not when a pointer enters it
        // (DRAGON-601, second pass). A redraw is the one event a mapped overlay is always
        // handed, and on a layer surface it is the only one it gets under a resting pointer:
        // the compositor's `wl_pointer.enter` never reaches a widget there. See
        // `cursor_reassert::arm_on_appear` for the measurement.
        if matches!(event, Event::Window(cosmic::iced::core::window::Event::RedrawRequested(_))) {
            crate::widgets::cursor_reassert::arm_on_appear(
                &mut tree.state.downcast_mut::<State>().entered_at,
                shell,
            );
            // DRAGON-610: stand in for the `wl_pointer.enter` this widget never received,
            // using the position the toolkit is already holding. See
            // `picker_wants_redraw_sample` for the measurement, for why the gate is the
            // app's `pointer.is_none()` rather than anything in `State`, and for why the
            // loupe still does not appear until the flats land.
            let placeable = cursor.position_over(bounds);
            if picker_wants_redraw_sample(self.needs_pointer, placeable.is_some())
                && let Some(p) = placeable
            {
                shell.publish((self.on_move)((p.x - bounds.x, p.y - bounds.y)));
            }
        }
        // BEFORE the position guard, deliberately: the two events this has to see are
        // `CursorEntered` (which starts the dance) and `CursorLeft` (which cancels it), and a
        // leave has no position over these bounds, so guarding first would leave the stamp
        // armed for a pointer that is gone. Redraws drive the schedule forward from here too,
        // which is what completes the dance under a pointer that never moves.
        crate::widgets::cursor_reassert::arm(
            &mut tree.state.downcast_mut::<State>().entered_at,
            event,
            shell,
        );
        // DRAGON-611: was the move we are about to see SYNTHESISED FROM AN ENTER? Our iced fork
        // answers a `wl_pointer.enter` by pushing `CursorEntered` and then `CursorMoved` into
        // the same event batch, back to back, for the same surface (`sctk_event.rs`, the
        // `PointerEventKind::Enter` arm). A real `Motion` pushes the move alone. So the tag is
        // simply "did an enter arrive on the previous event".
        //
        // Set here and cleared by the very next event WHATEVER it is, so the tag survives
        // exactly one hop. That matters: if iced ever stops delivering the pair back to back,
        // the flag is absent rather than stale, every move reads as a real motion, and the
        // behaviour is what it was before this existed. A flag that could go stale would
        // instead start ignoring real moves, which is far worse than the bug it fixes.
        let from_enter = {
            let st = tree.state.downcast_mut::<State>();
            if matches!(event, Event::Mouse(mouse::Event::CursorEntered)) {
                st.after_enter = true;
                // The picker's cursor re-assert re-arms on this enter too, and its dip shows
                // the DEFAULT ARROW from SETTLE-DIP to SETTLE (DRAGON-601). A live grab landing
                // in that window can photograph an arrow the picker exists to hide. Logged, not
                // fixed: suppressing the re-assert risks DRAGON-601's measured failure, where a
                // lost cursor request leaves a visible arrow for the whole session. This line is
                // what makes the question answerable from a capture's timestamp.
                log::debug!(
                    "DRAGON-611 picker: pointer enter; the cursor re-assert dip opens at \
                     +{}ms and closes at +{}ms, so a grab landing in that window can \
                     photograph the default arrow",
                    crate::widgets::cursor_reassert::SETTLE
                        .saturating_sub(crate::widgets::cursor_reassert::DIP)
                        .as_millis(),
                    crate::widgets::cursor_reassert::SETTLE.as_millis(),
                );
                false
            } else {
                std::mem::take(&mut st.after_enter)
            }
        };
        let Some(p) = cursor.position_over(bounds) else {
            return;
        };
        // Surface-LOCAL, so the app never has to know where iced laid this widget out.
        // It fills the overlay, so the origin is the surface's, but subtracting it keeps
        // that an implementation detail rather than an assumption.
        let local = (p.x - bounds.x, p.y - bounds.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                shell.publish((self.on_pick)(local));
                shell.capture_event();
            }
            // Every move, not only transitions: the magnifier follows the pointer
            // continuously, and the handler itself is cheap when the SOURCE PIXEL has not
            // changed (it re-rasterises only when it has).
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                // DRAGON-611: an enter that merely REVEALED a pointer nobody moved must not
                // move the loupe. Declining publishes nothing at all, so the app's `Moved` arm
                // never runs, which also leaves the keyboard nudge alone: before this, a region
                // capture over a live picker silently discarded the user's arrow-key
                // displacement as well as jumping the loupe (`geom::nudge_after`, DRAGON-599).
                if pointer_report_moves_sample(
                    from_enter,
                    !self.needs_pointer,
                    local,
                    (bounds.width, bounds.height),
                ) {
                    shell.publish((self.on_move)(local));
                } else {
                    log::debug!(
                        "DRAGON-611 picker: held the sample; an enter revealed the pointer at \
                         ({:.0}, {:.0}) in the interior of a {:.0}x{:.0} surface, which is not \
                         a crossing, so the loupe stays where the user left it",
                        local.0,
                        local.1,
                        bounds.width,
                        bounds.height,
                    );
                }
            }
            // DRAGON-587: the wheel and the trackpad both zoom the magnifier. No modifier is
            // required, unlike the preview editor's Ctrl+wheel, because there is nothing else
            // for a scroll to mean over this surface: the picker has no pan and no scroll.
            //
            // The two devices arrive as the same iced event with different deltas, so they are
            // converted to the same unit (notches) and then take the SAME path. Whole notches
            // are published and the remainder is kept, so a slow trackpad swipe accumulates
            // into one step instead of rounding away, and a wheel detent is exactly one step.
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let notches = match delta {
                    // A wheel detent, and the axis a tilt-wheel or a horizontal swipe gives.
                    mouse::ScrollDelta::Lines { x, y } => {
                        if *y != 0.0 { *y } else { *x }
                    }
                    mouse::ScrollDelta::Pixels { x, y } => {
                        let d = if *y != 0.0 { *y } else { *x };
                        d / WHEEL_ZOOM_STEP
                    }
                };
                let st = tree.state.downcast_mut::<State>();
                st.zoom_accum += notches;
                let whole = st.zoom_accum.trunc();
                if whole != 0.0 {
                    st.zoom_accum -= whole;
                    shell.publish((self.on_zoom)(whole as i32));
                }
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut cosmic::Renderer,
        _theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        // Nothing: see the module doc. Every pixel of the picker is drawn by the
        // ordinary elements stacked below this surface.
    }
}

impl<'a, Msg: 'static> From<ColorPickSurface<Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: ColorPickSurface<Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

/// DRAGON-601: which cursor the picker's input surface asks for. The pinned property is the
/// one that was wrong in the shipped build, and then wrong again in the first attempt at
/// fixing it: neither answer may wait on the toolkit knowing where the pointer is, because on
/// a layer surface it does not know until the pointer moves.
#[cfg(test)]
mod picker_interaction_tests {
    use super::*;

    /// The owner's report, as a test. The overlay has just mapped under a pointer nobody is
    /// touching, so the toolkit reports no cursor at all. The picker must still ask for the
    /// hide, because the sprite is sitting on the pixel it is about to read.
    #[test]
    fn a_pointer_the_toolkit_cannot_locate_is_still_hidden() {
        assert_eq!(picker_interaction(true, false), mouse::Interaction::Hidden);
    }

    /// The dip outranks the hide while it is open. Its whole job is to make the hide that
    /// follows a CHANGE the toolkit will re-issue, so a dip that did not win would leave the
    /// hide computed forever and sent never, which is a build that was measured doing exactly
    /// that.
    #[test]
    fn the_dip_wins_while_it_is_open() {
        assert_eq!(picker_interaction(true, true), mouse::Interaction::default());
    }

    /// The pair that matters is a pair: the dip must be followed by a DIFFERENT answer, or
    /// there is no change to re-issue and the whole dance is decoration.
    #[test]
    fn the_dip_and_the_answer_after_it_actually_differ() {
        assert_ne!(picker_interaction(true, true), picker_interaction(true, false));
    }

    /// The honest degradation if the iced [patch] ever goes: the compositor's default arrow,
    /// dip or no dip. Never `Hidden`, because asking a surface that cannot hide to hide is how
    /// the picker ends up reporting a pixel the user cannot see.
    #[test]
    fn a_surface_that_cannot_hide_asks_for_the_default() {
        for dip in [true, false] {
            assert_eq!(picker_interaction(false, dip), mouse::Interaction::default(), "dip={dip}");
        }
    }
}

/// DRAGON-610: when a redraw may stand in for a `wl_pointer.enter` the widget never got.
/// The pinned property is that the gate closes once the picker has a position, because the
/// version that re-opens is the one that silently eats a keyboard nudge.
#[cfg(test)]
mod picker_redraw_sample_tests {
    use super::*;

    /// The bug, as a test. A tray-launched picker sat through its dropdown hold, the enter
    /// landed in a window where this widget was handed no events at all, and afterwards the
    /// cursor was placeable on every redraw with nothing asking for it.
    #[test]
    fn a_placeable_cursor_with_no_sample_yet_is_taken() {
        assert!(picker_wants_redraw_sample(true, true));
    }

    /// The hotkey path, and every moment after the first sample on any path. Its enter
    /// reached the widget and set the pointer, so this route is closed and publishes
    /// nothing. This is what makes the fix additive rather than a behaviour change.
    #[test]
    fn a_picker_that_already_has_a_sample_is_left_alone() {
        for placeable in [true, false] {
            assert!(
                !picker_wants_redraw_sample(false, placeable),
                "placeable={placeable}"
            );
        }
    }

    /// A redraw on the output the pointer is NOT over. A picker session mints one of these
    /// per display, so on a multi-monitor desktop most of them can never place the cursor
    /// and must publish nothing rather than a position they do not have.
    #[test]
    fn a_redraw_that_cannot_place_the_cursor_publishes_nothing() {
        assert!(!picker_wants_redraw_sample(true, false));
    }

    /// Both terms are load-bearing, so neither may be dropped to "simplify" this.
    #[test]
    fn neither_term_alone_is_enough() {
        assert!(!picker_wants_redraw_sample(true, false));
        assert!(!picker_wants_redraw_sample(false, true));
        assert!(!picker_wants_redraw_sample(false, false));
    }
}

/// DRAGON-611: telling "the user moved the pointer" apart from "the pointer was revealed to
/// me", which arrive as the same event. The pinned property has TWO sides on purpose, because
/// the tempting simplifications break one side each: ignoring every enter breaks the user
/// crossing in from another monitor, and trusting every enter is the bug itself.
#[cfg(test)]
mod pointer_arrival_tests {
    use super::*;

    /// The owner's big display, and their small one. Both are real, and the small one matters
    /// because a band that is negligible on 5120x1440 must still be negligible on 800x480.
    const BIG: (f32, f32) = (5120.0, 1440.0);
    const SMALL: (f32, f32) = (800.0, 480.0);

    /// The bug, as a test. A region capture tore its overlay down over a live picker, the
    /// compositor handed the pointer back, and the loupe jumped to where the user had been
    /// drawing instead of staying where they parked it. The grab 200ms later photographed
    /// the jump.
    #[test]
    fn an_enter_that_merely_reveals_the_pointer_does_not_move_the_sample() {
        assert!(!pointer_report_moves_sample(true, true, (2400.0, 900.0), BIG));
        assert!(!pointer_report_moves_sample(true, true, (400.0, 240.0), SMALL));
    }

    /// The legitimate case a blanket "ignore enters" would break: the pointer crosses in from
    /// a neighbouring output, so it arrives ACROSS an edge, and the loupe must follow. All
    /// four edges, because which one a crossing uses depends on how the displays are arranged
    /// and this must not silently work on only the owner's layout.
    #[test]
    fn a_pointer_crossing_in_from_another_output_is_followed() {
        let (w, h) = BIG;
        for p in [(0.0, 700.0), (w, 700.0), (2500.0, 0.0), (2500.0, h)] {
            assert!(
                pointer_report_moves_sample(true, true, p, BIG),
                "crossing in at {p:?} should move the sample"
            );
        }
    }

    /// A real `wl_pointer.motion` is never second-guessed, wherever it lands. This predicate
    /// exists to doubt an ENTER, never the user's hand.
    #[test]
    fn a_real_motion_always_wins() {
        for had_sample in [true, false] {
            assert!(
                pointer_report_moves_sample(false, had_sample, (2400.0, 900.0), BIG),
                "had_sample={had_sample}"
            );
        }
    }

    /// The first sample of a session, from the enter the picker gets when its overlay maps
    /// under a resting pointer (DRAGON-609) or from the redraw that stands in for one
    /// (DRAGON-610). There is no loupe to protect yet, so it lands wherever the pointer is.
    /// Without this the picker would come up showing nothing at all.
    #[test]
    fn the_first_sample_lands_anywhere() {
        assert!(pointer_report_moves_sample(true, false, (2400.0, 900.0), BIG));
        assert!(pointer_report_moves_sample(true, false, (400.0, 240.0), SMALL));
    }

    /// **The band must not be widened**, and this pins it from both sides so that doing so
    /// fails loudly. The predicate's doc explains why the two errors are not symmetric: too
    /// narrow self-heals on the next motion event, too wide re-admits the bug permanently.
    #[test]
    fn the_edge_band_holds_its_width_from_both_sides() {
        let inside = ENTER_EDGE_BAND - 0.5;
        let outside = ENTER_EDGE_BAND + 0.5;
        assert!(
            pointer_report_moves_sample(true, true, (inside, 700.0), BIG),
            "within the band still reads as a crossing"
        );
        assert!(
            !pointer_report_moves_sample(true, true, (outside, 700.0), BIG),
            "past the band reads as a reveal"
        );
        // And the same at the far edge, so the two ends cannot drift apart.
        let (w, _) = BIG;
        assert!(pointer_report_moves_sample(true, true, (w - inside, 700.0), BIG));
        assert!(!pointer_report_moves_sample(true, true, (w - outside, 700.0), BIG));
    }

    /// The band stays a rim rather than swallowing a small display. On the owner's 800x480 the
    /// interior must still be overwhelmingly interior, or the fix would not hold there.
    #[test]
    fn the_band_is_still_a_rim_on_a_small_output() {
        let (w, h) = SMALL;
        let rim = 2.0 * ENTER_EDGE_BAND;
        assert!(rim < w / 4.0 && rim < h / 4.0, "the band has grown into the interior");
        assert!(!pointer_report_moves_sample(true, true, (rim + 1.0, rim + 1.0), SMALL));
    }

    /// A surface with no area has no interior, so it degrades to the old behaviour rather
    /// than freezing a loupe nothing can ever unfreeze.
    #[test]
    fn a_degenerate_surface_degrades_to_following() {
        assert!(pointer_report_moves_sample(true, true, (0.0, 0.0), (0.0, 0.0)));
    }
}
