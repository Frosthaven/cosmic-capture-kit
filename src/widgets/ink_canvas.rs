//! The live recording ink surface's canvas (DRAGON-691).
//!
//! A leaf widget that fills its surface, draws the ink, and reports the three pointer events
//! a stroke is made of: press, move-while-down, release. Since DRAGON-739 it optionally also
//! reports a HOVER, a move with no button down, for the one caller that needs to know where
//! the pointer is when nothing is being drawn: an armed tool takes pointer focus off the
//! recorded window, so a window recording loses its cursor and the capture worker draws one.
//! That is the whole of it.
//!
//! # Why not the preview editor's annotation canvas
//!
//! `widgets::annotation_canvas` is four thousand lines of an EDITOR: hit-testing, selection
//! sets, resize handles, rubber bands, text carets, an eraser, undo-aware gestures and a
//! coordinate map through a zoom/pan viewport. None of that exists here. Live ink has no
//! selection, no handles, no zoom and no editing at all: you draw, and the drawing either
//! stays until Erase or dissolves on its own. Dragging that widget onto the capture overlay
//! would mean carrying an editor's entire input model into a surface that is on screen
//! DURING a recording, where every stray affordance is in the video.
//!
//! # The geometry arrives finished
//!
//! The widget is handed FILLED POLYGONS with their colours already resolved
//! ([`InkCanvas::fills`]), not strokes. The scene, the dissolve and the run splitting are
//! `app::ink`'s job and `crate::ink`'s maths; this draws what it is given, in the order it is
//! given. That keeps the fade testable without a renderer, and it is what lets one fill carry
//! one alpha (a canvas fill has a single colour, so a gradient along a stroke has to arrive
//! as several fills).
//!
//! Geometry rather than a shader or an image, deliberately: Windows 10 rasterizes our
//! overlays with iced's SOFTWARE backend (`platform::software_overlays`), which draws no
//! shader primitive at all. Canvas geometry is on the paths tiny-skia supports, so the ink
//! works on the one platform that would otherwise be blank, the same reasoning
//! `widgets::color_pick` records for the colour picker.

use cosmic::iced::core::widget::{Tree, tree};
use cosmic::iced::core::{
    Clipboard, Color, Event, Layout, Length, Rectangle, Shell, Size, Vector, mouse,
};
use cosmic::widget::Widget;
use cosmic::widget::canvas::{self, Path};

/// One fill: a colour (alpha included) and the polygons drawn in it.
///
/// Every polygon of a group is wound the same way and the group is filled as ONE path with
/// the NON-ZERO rule, which is what makes a scribble that crosses itself union instead of
/// cancelling into holes, and what makes a partially transparent group composite exactly once
/// instead of darkening at its overlaps. That contract comes from
/// [`crate::pen_stroke::stroke_fill_polygons`], which builds them.
pub type InkFill = (Color, Vec<Vec<(f32, f32)>>);

/// The hover reporter: the pointer's surface-LOCAL position, or `None` once it has left
/// (DRAGON-739). An alias because the boxed closure's own type is past clippy's complexity bar.
type HoverFn<Msg> = Box<dyn Fn(Option<(f32, f32)>) -> Msg>;

/// Per-instance state: whether a stroke is in flight.
///
/// The press has to be remembered because a pointer MOVE arrives whether or not a button is
/// down, and a move with no press is the user simply passing over the surface. Without this
/// the first hover after arming would begin a stroke.
#[derive(Default)]
struct State {
    drawing: bool,
}

/// The ink surface's canvas. Fills its surface; draws [`InkCanvas::fills`]; reports a stroke.
pub struct InkCanvas<Msg> {
    /// The finished ink, drawn in order (so a caller can layer a dissolving stroke's runs
    /// from faintest to strongest).
    fills: Vec<InkFill>,
    /// Built with the surface-LOCAL press point when a stroke begins.
    on_press: Box<dyn Fn((f32, f32)) -> Msg>,
    /// Built with the surface-LOCAL point on every move while the button is held.
    on_move: Box<dyn Fn((f32, f32)) -> Msg>,
    /// Built when the button is released, or when the pointer leaves mid-stroke.
    on_release: Box<dyn Fn() -> Msg>,
    /// Built with the surface-LOCAL point on every move with NO button down, and with `None`
    /// when the pointer leaves the surface (DRAGON-739).
    ///
    /// Optional, because reporting a hover is a cost and only one caller needs it: an armed
    /// tool takes pointer focus off the recorded window, so a WINDOW recording loses its
    /// cursor and the capture worker has to draw one. A caller that hands no closure pays
    /// nothing, and this widget's own behaviour is byte-identical to before.
    on_hover: Option<HoverFn<Msg>>,
    /// Whether a DRAWING tool is selected. `false` makes the surface display-only: it draws
    /// the ink that is still on screen and reports nothing, which is the state the Pointer tool
    /// leaves leftover marker ink in.
    armed: bool,
}

impl<Msg> InkCanvas<Msg> {
    pub fn new(
        fills: Vec<InkFill>,
        on_press: impl Fn((f32, f32)) -> Msg + 'static,
        on_move: impl Fn((f32, f32)) -> Msg + 'static,
        on_release: impl Fn() -> Msg + 'static,
    ) -> Self {
        Self {
            fills,
            on_press: Box::new(on_press),
            on_move: Box::new(on_move),
            on_release: Box::new(on_release),
            on_hover: None,
            armed: false,
        }
    }

    /// Arm the surface, so a press begins a stroke. See [`InkCanvas::armed`].
    pub fn armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }

    /// Report the pointer's position while no button is down. See [`InkCanvas::on_hover`].
    pub fn on_hover(mut self, f: impl Fn(Option<(f32, f32)>) -> Msg + 'static) -> Self {
        self.on_hover = Some(Box::new(f));
        self
    }
}

impl<Msg: 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for InkCanvas<Msg> {
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

    /// The DEFAULT cursor, armed or not: see [`requested_interaction`].
    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        requested_interaction(self.armed)
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
        if !self.armed {
            // Display-only: marker ink that outlived its tool still draws, and nothing this
            // surface sees may reach the app. The window is click-through in this state on
            // macOS and Windows anyway; this is the same answer stated where the events are.
            return;
        }
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                state.drawing = true;
                shell.publish((self.on_press)((p.x - bounds.x, p.y - bounds.y)));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.drawing => {
                // The position is taken from the CURSOR rather than from the event, so a move
                // that leaves the surface still reports its last position over it. `position`
                // (not `position_over`) because a stroke dragged past the edge must keep
                // sampling; the app clamps it into the recorded crop.
                let Some(p) = cursor.position() else {
                    return;
                };
                shell.publish((self.on_move)((p.x - bounds.x, p.y - bounds.y)));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.drawing => {
                state.drawing = false;
                shell.publish((self.on_release)());
                shell.capture_event();
            }
            // The pointer leaving with the button still down ENDS the stroke. A stroke left
            // open would keep appending on the next pass of the pointer, which draws a line
            // the user never made, and on a multi-display desktop the pointer leaves this
            // surface every time it crosses to another monitor.
            Event::Mouse(mouse::Event::CursorLeft) if state.drawing => {
                state.drawing = false;
                shell.publish((self.on_release)());
                if let Some(hover) = self.on_hover.as_ref() {
                    shell.publish(hover(None));
                }
            }
            // DRAGON-739: a move with NO button down. The event is NOT captured, unlike the
            // stroke arms above: nothing is being drawn, so nothing else on this surface may
            // be starved of it. `position_over` rather than `position`, because a pointer that
            // has wandered off the surface (over the toolbar's hole in the input region, or
            // onto another output) is a pointer we must stop drawing rather than one to place
            // by a stale reading.
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(hover) = self.on_hover.as_ref() {
                    let at = cursor
                        .position_over(bounds)
                        .map(|p| (p.x - bounds.x, p.y - bounds.y));
                    shell.publish(hover(at));
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if let Some(hover) = self.on_hover.as_ref() {
                    shell.publish(hover(None));
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        _theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use cosmic::iced::advanced::graphics::geometry::Renderer as _;
        use cosmic::iced::core::Renderer as _;
        if self.fills.is_empty() {
            return;
        }
        let bounds = layout.bounds();
        // The canvas builds its geometry in widget-LOCAL coordinates and the translation puts
        // it where the surface is; the surrounding layer scissors it to these bounds. Exactly
        // the recipe `annotation_canvas` draws its vector scene with, for the same reason: a
        // vector redraw churns no texture atlas, so ink being drawn does not flicker.
        renderer.with_layer(bounds, |renderer| {
            let mut frame = canvas::Frame::new(renderer, Size::new(bounds.width, bounds.height));
            for (color, polys) in &self.fills {
                if polys.is_empty() {
                    continue;
                }
                let path = Path::new(|b| {
                    for poly in polys {
                        let Some(first) = poly.first() else { continue };
                        b.move_to(cosmic::iced::Point::new(first.0, first.1));
                        for q in &poly[1..] {
                            b.line_to(cosmic::iced::Point::new(q.0, q.1));
                        }
                        b.close();
                    }
                });
                frame.fill(&path, *color);
            }
            let geometry = frame.into_geometry();
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                renderer.draw_geometry(geometry);
            });
        });
    }
}

impl<'a, Msg: 'static> From<InkCanvas<Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: InkCanvas<Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

/// **Pure**, unit-tested: the mouse cursor this surface asks for. Always the DEFAULT, armed or
/// not (DRAGON-744).
///
/// The parameter is kept because it names the axis that USED to decide the answer, the same
/// shape `platform::overlay_hides_pointer` uses for the same reason, and because the tests
/// below are what pin that the axis is gone.
///
/// # Tombstone: it used to ask for a CROSSHAIR while armed, and the request never arrived
///
/// The idea was that the surface covers the whole output, so the pointer is over it the entire
/// time and the cursor IS the affordance saying the screen has become a drawing. DRAGON-739
/// then built on it: it paints a cursor into a window recording's frames (the compositor drops
/// the embedded one while our surface holds pointer focus), and it painted a CROSSHAIR because
/// "the shape is ours by construction, since we are the ones who set it".
///
/// The owner reported the opposite: "i dont see a crosshair when im drawing mid recording", and
/// asked for the crosshair only if it could be GUARANTEED. It cannot. Three reasons, in the
/// order they matter, and the first is structural rather than a race:
///
/// 1. **The recording toolbar is a DIFFERENT surface, with its own stored interaction.** iced
///    issues a cursor only when one WINDOW's resolved interaction CHANGES
///    (`iced_winit::window::Window::update_mouse`), while the Wayland cursor is per SEAT
///    (`SctkSeat::set_cursor`). The toolbar is iced UI inside the per-output CAPTURE OVERLAY, a
///    separate layer surface (see `app::ink`'s module doc), so an inked recording always has
///    two of our surfaces on screen. Cross to the toolbar and the overlay's window sets the
///    arrow; cross back and THIS window's stored value is still the crosshair, so nothing is
///    issued at all and the arrow stands for the rest of the session. Pausing, picking a colour
///    or reaching the Pointer button is exactly that round trip. No retry closes it, because
///    there is nothing to retry: the request is never made.
/// 2. **The re-assert dance is best effort, and in a recording its cost is measured in
///    FRAMES.** [`crate::widgets::cursor_reassert`] (DRAGON-331) is the remedy for a compositor
///    that drops a `set_cursor` issued too soon after `wl_pointer.enter`, and it is a 200ms
///    settle measured against cosmic-comp plus a visible 24ms dip to the default cursor, so the
///    re-assert is a CHANGE iced will re-issue. `app::color_picker` already runs it and already
///    carries the resulting defect, logged and declined (`widgets::color_pick`, the DRAGON-611
///    enter log): a live grab landing in the dip photographs the arrow the picker exists to
///    hide. A recording photographs everything. At 30fps that dance disagrees with the file for
///    about one frame of dip and six of settle on EVERY entry to the surface, which is the
///    owner's disconnect back again, smaller and now intermittent.
/// 3. **`wl_pointer.set_cursor` has no reply, no error and no event.** The compositor validates
///    it against the latest enter serial and says nothing either way, and `SctkSeat` records
///    `active_icon` on the way out regardless, so a dropped request also suppresses the next
///    re-assert. Nothing in this stack can ever learn what the user is actually looking at, on
///    their machine or on ours. **A guarantee that is unobservable by construction is not one**,
///    and that sentence is the whole reason this is not worth reproposing.
///
/// A fourth, if the first three ever fall: the settle was measured against cosmic-comp, while
/// `ink_live::live_cursor_needed` draws for every focus-gated compositor (niri and KDE too).
/// Whatever held here would still not be portable.
///
/// **The answer is the same on every TIER, and that is a choice rather than an oversight.**
/// Silver and Bronze host this canvas in an ordinary toplevel, where the request would very
/// likely have been honoured, and nothing there could disagree with the file anyway: those
/// tiers record a screen rectangle or a frozen still, so whatever the compositor draws is what
/// the recording gets. Keeping the crosshair for them would make the affordance depend on a
/// tier the user cannot see, which is the intermittency this ticket exists to remove. One
/// answer everywhere.
///
/// So the shape is not ours and cannot be made ours. The DEFAULT is asked for instead, and
/// `record::ink_live`'s module doc carries why the arrow is then the right thing to paint.
pub(crate) fn requested_interaction(_armed: bool) -> mouse::Interaction {
    mouse::Interaction::default()
}

/// DRAGON-744. Small, but this is the whole of the fix: the surface asking for a shape the
/// compositor will not show is what put a cursor in the file that the user never saw.
#[cfg(test)]
mod requested_interaction_tests {
    use super::*;

    /// Both rows, and they are the same row. Restoring `Crosshair` for the armed one is the
    /// regression, so it has to delete this test and read the tombstone to do it.
    #[test]
    fn the_ink_surface_never_forces_a_shape() {
        assert_eq!(requested_interaction(true), mouse::Interaction::default());
        assert_eq!(requested_interaction(false), mouse::Interaction::default());
    }

    /// The DEFAULT is the ARROW, not a hidden pointer. `Interaction::None` converts to
    /// `CursorIcon::Default` in `iced_winit::conversion::mouse_interaction`, while
    /// `Interaction::Hidden` is the one that reaches `set_cursor_visible(false)`. Asking for
    /// the wrong one here would blank the pointer on screen while the file kept drawing one.
    #[test]
    fn the_default_is_a_visible_cursor() {
        assert_ne!(mouse::Interaction::default(), mouse::Interaction::Hidden);
        assert_ne!(mouse::Interaction::default(), mouse::Interaction::Crosshair);
    }
}
