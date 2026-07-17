//! A viewport wrapper that ZOOMS and PANS its content (the preview image) without
//! touching the content itself: the whole child is drawn scaled about the viewport
//! centre and translated by a pan offset, clipped to the viewport bounds. Because it
//! transforms the single already-composited image element, any covermarks baked into
//! that image ride along with it — zoom/pan never re-place them.
//!
//! Input (only while the cursor is over the viewport):
//! - Ctrl + wheel  → zoom  (`on_zoom(delta)`, +up = in)
//! - Alt + wheel   → pan vertically   (`on_pan(0, dy)`)
//! - Alt+Shift+wheel → pan horizontally (`on_pan(dx, 0)`)
//! - Alt + drag    → pan               (`on_pan(dx, dy)` in screen px)
//!
//! `zoom`/`pan` are owned by the app (PreviewState) and passed in each build, so this
//! widget is stateless about the transform — it only tracks live modifiers + the drag.

use cosmic::iced::core::widget::{tree, Operation, Tree};
use cosmic::iced::core::{
    keyboard, layout, mouse, overlay, renderer, Clipboard, Event, Layout, Length, Point,
    Rectangle, Shell, Size, Transformation, Vector,
};
// `Renderer` (the advanced trait) brings `with_layer` / `with_transformation` into scope.
use cosmic::iced::advanced::Renderer as _;
use cosmic::widget::Widget;

/// Screen px panned per wheel notch (line delta) for alt+scroll.
const WHEEL_PAN_STEP: f32 = 48.0;

#[derive(Default)]
struct State {
    mods: keyboard::Modifiers,
    /// Last cursor position while an alt-drag pan is active (None = not dragging).
    drag: Option<Point>,
    /// Active scrollbar drag: `(is_vertical, last_cursor)`.
    sb_drag: Option<(bool, Point)>,
}

/// Scrollbar geometry (kept in sync between draw + hit-testing).
const SB: f32 = 8.0; // scrollbar thickness (track + thumb)
const SB_MIN: f32 = 24.0; // minimum thumb length
const SB_HIT: f32 = 6.0; // extra px each side for an easier grab
/// Inset from the right / bottom edge so the scrollbar clears the window's resize border
/// (the compositor grabs the outer strip for resize before the app sees the click).
const SB_EDGE: f32 = 8.0;
/// The full strip a visible scrollbar reserves from the content (thickness + edge inset) —
/// the single source of truth shared with the preview overlay's canvas-viewport sizing, so
/// its zoom/pan math reserves exactly what this widget draws.
pub(crate) const SCROLLBAR_TOTAL: f32 = SB + SB_EDGE;

/// `(horizontal, vertical)`: whether a `content_px`×`zoom` picture overflows `bounds` on
/// each axis (so that axis needs a scrollbar). Each present bar shrinks the perpendicular
/// viewport, so a bar can tip the other axis into overflow — accounted for here. A free fn
/// (pure inputs → outputs, no widget state) so the geometry is unit-testable directly.
fn overflow_of(content_px: (f32, f32), zoom: f32, bounds: Rectangle) -> (bool, bool) {
    let cw = content_px.0 * zoom;
    let ch = content_px.1 * zoom;
    let strip = SB + SB_EDGE;
    let h0 = cw > bounds.width + 0.5;
    let v0 = ch > bounds.height + 0.5;
    let h = cw > bounds.width - if v0 { strip } else { 0.0 } + 0.5;
    let v = ch > bounds.height - if h0 { strip } else { 0.0 } + 0.5;
    (h, v)
}

/// The content rectangle for a `content_px`×`zoom` picture in `bounds` — the full bounds
/// minus the scrollbar strips (right for the vertical bar, bottom for the horizontal one).
fn content_bounds_of(content_px: (f32, f32), zoom: f32, bounds: Rectangle) -> Rectangle {
    let (h, v) = overflow_of(content_px, zoom, bounds);
    let mut cb = bounds;
    if v {
        cb.width = (cb.width - SB - SB_EDGE).max(0.0);
    }
    if h {
        cb.height = (cb.height - SB - SB_EDGE).max(0.0);
    }
    cb
}

/// The (horizontal, vertical) scrollbar `(track, thumb)` rects for a `content_px`×`zoom`
/// picture panned by `pan` within `bounds` — `None` per axis when it doesn't overflow.
/// Tracks are flush to the right / bottom edge; when both show, each is shortened by the
/// other's thickness (no corner overlap).
#[allow(clippy::type_complexity)]
fn scrollbars_of(
    content_px: (f32, f32),
    zoom: f32,
    pan: (f32, f32),
    bounds: Rectangle,
) -> (Option<(Rectangle, Rectangle)>, Option<(Rectangle, Rectangle)>) {
    let (h_present, v_present) = overflow_of(content_px, zoom, bounds);
    // Shorten each track by the OTHER bar's full strip (thickness + edge inset) so the
    // bottom bar stops at the vertical bar's left edge — no overlap, no clipping.
    let corner = if h_present && v_present { SB + SB_EDGE } else { 0.0 };
    let cw = content_px.0 * zoom;
    let ch = content_px.1 * zoom;
    // The thumb spans visible/content of the track, positioned so it reaches an edge
    // exactly when the pan reaches that edge — `top` is the content offset scrolled past
    // the top/left (`(content - bounds)/2 - pan`), mapped into the track.
    let v = v_present.then(|| {
        let x = bounds.x + bounds.width - SB - SB_EDGE;
        let ty = bounds.y;
        let th = (bounds.height - corner).max(1.0);
        let frac = (th / ch).clamp(0.0, 1.0);
        let center = ((ch - bounds.height) * 0.5 - pan.1 + th * 0.5) / ch;
        let track = Rectangle { x, y: ty, width: SB, height: th };
        let h = (frac * th).clamp(SB_MIN.min(th), th);
        let y = (ty + center * th - h / 2.0).clamp(ty, ty + th - h);
        (track, Rectangle { x, y, width: SB, height: h })
    });
    let h = h_present.then(|| {
        let y = bounds.y + bounds.height - SB - SB_EDGE;
        let tx = bounds.x;
        let tw = (bounds.width - corner).max(1.0);
        let frac = (tw / cw).clamp(0.0, 1.0);
        let center = ((cw - bounds.width) * 0.5 - pan.0 + tw * 0.5) / cw;
        let track = Rectangle { x: tx, y, width: tw, height: SB };
        let w = (frac * tw).clamp(SB_MIN.min(tw), tw);
        let x = (tx + center * tw - w / 2.0).clamp(tx, tx + tw - w);
        (track, Rectangle { x, y, width: w, height: SB })
    });
    (h, v)
}

/// Clamp a desired ABSOLUTE pan (for a `content_px`×`zoom` picture in `bounds`) so the
/// picture's edges stay reachable. The picture is centred in the full bounds; the
/// right/bottom get an extra scrollbar strip so those edges pan out from under the bars,
/// while the left/top just reach the edge.
fn clamp_pan_of(content_px: (f32, f32), zoom: f32, bounds: Rectangle, want: (f32, f32)) -> (f32, f32) {
    let cw = content_px.0 * zoom;
    let ch = content_px.1 * zoom;
    let (h_present, v_present) = overflow_of(content_px, zoom, bounds);
    let rev_x = if v_present { SB + SB_EDGE } else { 0.0 };
    let rev_y = if h_present { SB + SB_EDGE } else { 0.0 };
    let ox = (cw - bounds.width) * 0.5;
    let oy = (ch - bounds.height) * 0.5;
    (
        want.0.clamp(-(ox + rev_x).max(0.0), ox.max(0.0)),
        want.1.clamp(-(oy + rev_y).max(0.0), oy.max(0.0)),
    )
}

pub struct ZoomPan<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    zoom: f32,
    pan: (f32, f32),
    /// Pan tool active: a plain left-drag pans (grabby hand), no Alt needed.
    pan_mode: bool,
    /// The fitted picture's pixel size at zoom 1.0 (dw, dh). With the widget's ACTUAL bounds
    /// this drives BOTH the pan clamp and the scrollbar geometry, so the thumb tracks the real
    /// reachable range (an app-side viewport estimate can't know the true windowed canvas).
    content_px: (f32, f32),
    on_zoom: Box<dyn Fn(f32, f32, f32) -> Msg + 'a>,
    on_pan: Box<dyn Fn(f32, f32) -> Msg + 'a>,
}

impl<'a, Msg> ZoomPan<'a, Msg> {
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        zoom: f32,
        pan: (f32, f32),
        pan_mode: bool,
        content_px: (f32, f32),
        on_zoom: impl Fn(f32, f32, f32) -> Msg + 'a,
        on_pan: impl Fn(f32, f32) -> Msg + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            zoom,
            pan,
            pan_mode,
            content_px,
            on_zoom: Box::new(on_zoom),
            on_pan: Box::new(on_pan),
        }
    }

    /// The (horizontal, vertical) scrollbar `(track, thumb)` rectangles for the current
    /// pan/zoom — `None` per axis when it doesn't overflow. When BOTH axes show, each track
    /// is shortened by the other's thickness so they don't meet in the bottom-right corner.
    /// The content rectangle — the full bounds minus the scrollbar strips (right for the
    /// vertical bar, bottom for the horizontal one). The image/covermark are clipped to
    /// this, so nothing draws into the scrollbar area.
    fn content_bounds(&self, bounds: Rectangle) -> Rectangle {
        content_bounds_of(self.content_px, self.zoom, bounds)
    }

    /// `(horizontal, vertical)`: whether the picture overflows the bounds on each axis (so
    /// that axis needs a scrollbar). Each present bar shrinks the perpendicular viewport, so
    /// a bar can tip the other axis into overflow — accounted for here.
    fn overflow(&self, bounds: Rectangle) -> (bool, bool) {
        overflow_of(self.content_px, self.zoom, bounds)
    }

    /// The (horizontal, vertical) scrollbar `(track, thumb)` rects for the current pan/zoom
    /// — `None` per axis when it doesn't overflow. Tracks are flush to the right / bottom
    /// edge; when both show, each is shortened by the other's thickness (no corner overlap).
    #[allow(clippy::type_complexity)]
    fn scrollbars(
        &self,
        bounds: Rectangle,
    ) -> (Option<(Rectangle, Rectangle)>, Option<(Rectangle, Rectangle)>) {
        scrollbars_of(self.content_px, self.zoom, self.pan, bounds)
    }

    /// Fill a bar rect (track or thumb) with the given corner radius.
    fn fill_bar(
        &self,
        renderer: &mut cosmic::Renderer,
        rect: Rectangle,
        color: cosmic::iced::Color,
        radius: f32,
    ) {
        use cosmic::iced::{Background, Border};
        renderer.fill_quad(
            renderer::Quad {
                bounds: rect,
                border: Border {
                    radius: radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            Background::Color(color),
        );
    }

    /// Draw each visible scrollbar: a square track in the canvas background colour, flush to
    /// the edge, and a rounded thumb that's dim at rest and brightens on hover.
    fn draw_scrollbars(
        &self,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        cursor: mouse::Cursor,
        bounds: Rectangle,
    ) {
        use cosmic::iced::Color;
        let track: Color = theme.cosmic().background.base.into();
        let thumb_dim = Color::from_rgba(0.5, 0.5, 0.5, 0.5);
        let thumb_hot = Color::from_rgba(0.78, 0.78, 0.78, 0.9);
        let over = cursor.position();
        let (h, v) = self.scrollbars(bounds);
        // The thumb follows the user's COSMIC rounding rule, capped at the pill
        // (the default token exceeds SB/2, so "round" keeps today's shape).
        let thumb_r = crate::app::theme::rounding(theme).s1().min(SB / 2.0);
        for (t, th) in [h, v].into_iter().flatten() {
            self.fill_bar(renderer, t, track, 0.0); // square track, flush to the edge
            let hot = over.is_some_and(|p| th.expand(SB_HIT).contains(p));
            self.fill_bar(renderer, th, if hot { thumb_hot } else { thumb_dim }, thumb_r);
        }
    }

    /// Clamp a desired ABSOLUTE pan so the picture's edges stay reachable, measured against
    /// the widget's real `bounds` (not an app-side estimate). The picture is centred in the
    /// full bounds; the right/bottom get an extra scrollbar strip so those edges pan out from
    /// under the bars, while the left/top just reach the edge.
    fn clamp_pan(&self, bounds: Rectangle, want: (f32, f32)) -> (f32, f32) {
        clamp_pan_of(self.content_px, self.zoom, bounds, want)
    }

    /// A pan DELTA that lands the (built-in) pan at a clamped-in-range target — the app adds
    /// this delta, so it can never scroll past the real edges.
    fn pan_delta(&self, bounds: Rectangle, dx: f32, dy: f32) -> (f32, f32) {
        let c = self.clamp_pan(bounds, (self.pan.0 + dx, self.pan.1 + dy));
        (c.0 - self.pan.0, c.1 - self.pan.1)
    }

    /// The transform mapping the child's screen coords to the zoomed/panned view:
    /// scale about the viewport centre `c`, then translate by the pan offset.
    /// `q' = zoom*q + (c*(1-zoom) + pan)`.
    fn transform(&self, bounds: Rectangle) -> Transformation {
        let cx = bounds.x + bounds.width / 2.0;
        let cy = bounds.y + bounds.height / 2.0;
        let tx = cx * (1.0 - self.zoom) + self.pan.0;
        let ty = cy * (1.0 - self.zoom) + self.pan.1;
        Transformation::translate(tx, ty) * Transformation::scale(self.zoom)
    }
}

impl<'a, Msg: Clone + 'a> Widget<Msg, cosmic::Theme, cosmic::Renderer> for ZoomPan<'a, Msg> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        match event {
            // Track modifiers so wheel/drag can branch on Ctrl/Alt/Shift.
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                st.mods = *m;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if cursor.is_over(bounds) {
                    let (dx, dy) = match delta {
                        mouse::ScrollDelta::Lines { x, y } => (*x, *y),
                        mouse::ScrollDelta::Pixels { x, y } => {
                            (*x / WHEEL_PAN_STEP, *y / WHEEL_PAN_STEP)
                        }
                    };
                    if st.mods.control() {
                        // Ctrl+wheel → zoom on the dominant axis, toward the cursor.
                        let step = if dy != 0.0 { dy } else { dx };
                        let (ux, uy) = cursor
                            .position()
                            .map(|p| {
                                (
                                    p.x - (bounds.x + bounds.width / 2.0),
                                    p.y - (bounds.y + bounds.height / 2.0),
                                )
                            })
                            .unwrap_or((0.0, 0.0));
                        shell.publish((self.on_zoom)(step, ux, uy));
                    } else if st.mods.shift() {
                        // Shift+wheel → horizontal pan (whichever axis the wheel gave).
                        let d = if dx != 0.0 { dx } else { dy };
                        let (px, py) = self.pan_delta(bounds, d * WHEEL_PAN_STEP, 0.0);
                        shell.publish((self.on_pan)(px, py));
                    } else {
                        // Plain wheel → pan (no Alt needed); trackpads may give both axes.
                        let (px, py) =
                            self.pan_delta(bounds, dx * WHEEL_PAN_STEP, dy * WHEEL_PAN_STEP);
                        shell.publish((self.on_pan)(px, py));
                    }
                    shell.capture_event();
                    return;
                }
            }
            // Grab a scrollbar thumb (takes priority over the pan drag).
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position() {
                    let (h, v) = self.scrollbars(bounds);
                    let hit = |t: &Option<(Rectangle, Rectangle)>| {
                        t.map(|(_, thumb)| thumb.expand(SB_HIT).contains(p)).unwrap_or(false)
                    };
                    if hit(&v) {
                        st.sb_drag = Some((true, p));
                        shell.capture_event();
                        return;
                    }
                    if hit(&h) {
                        st.sb_drag = Some((false, p));
                        shell.capture_event();
                        return;
                    }
                    // Otherwise start a pan drag when Alt held or the pan tool is active.
                    if (st.mods.alt() || self.pan_mode) && cursor.is_over(bounds) {
                        st.drag = Some(p);
                        shell.capture_event();
                        return;
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if st.sb_drag.is_some() => {
                if let Some(p) = cursor.position() {
                    let (vertical, last) = st.sb_drag.unwrap();
                    st.sb_drag = Some((vertical, p));
                    // Map thumb travel → pan: dragging the thumb by Δ px moves the picture by
                    // −Δ / thumb_fraction (content = track / fraction).
                    let (h_present, v_present) = self.overflow(bounds);
                    let corner = if h_present && v_present { SB + SB_EDGE } else { 0.0 };
                    let cw = self.content_px.0 * self.zoom;
                    let ch = self.content_px.1 * self.zoom;
                    if vertical {
                        let frac = ((bounds.height - corner) / ch).clamp(0.01, 1.0);
                        let (px, py) = self.pan_delta(bounds, 0.0, -(p.y - last.y) / frac);
                        shell.publish((self.on_pan)(px, py));
                    } else {
                        let frac = ((bounds.width - corner) / cw).clamp(0.01, 1.0);
                        let (px, py) = self.pan_delta(bounds, -(p.x - last.x) / frac, 0.0);
                        shell.publish((self.on_pan)(px, py));
                    }
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.sb_drag.is_some() =>
            {
                st.sb_drag = None;
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if st.drag.is_some() => {
                if let Some(p) = cursor.position() {
                    let last = st.drag.unwrap();
                    st.drag = Some(p);
                    let (px, py) = self.pan_delta(bounds, p.x - last.x, p.y - last.y);
                    shell.publish((self.on_pan)(px, py));
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.drag.is_some() =>
            {
                st.drag = None;
                shell.capture_event();
                return;
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        if st.drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if cursor.is_over(layout.bounds()) && (st.mods.alt() || self.pan_mode) {
            return mouse::Interaction::Grab;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        // Un-zoomed: draw straight through (no clip layer, no transform) so the default
        // path is untouched.
        if (self.zoom - 1.0).abs() < f32::EPSILON && self.pan == (0.0, 0.0) {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
            return;
        }
        let transform = self.transform(bounds);
        // Clip the content (image + any covermark/shader layers) to everything EXCEPT the
        // scrollbar strips, so nothing draws into the scrollbar area.
        renderer.with_layer(self.content_bounds(bounds), |r| {
            r.with_transformation(transform, |r| {
                self.content
                    .as_widget()
                    .draw(&tree.children[0], r, theme, style, layout, cursor, viewport);
            });
        });
        // A fresh layer on TOP for the scrollbars themselves.
        renderer.with_layer(bounds, |r| self.draw_scrollbars(r, theme, cursor, bounds));
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Msg: Clone + 'a> From<ZoomPan<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: ZoomPan<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle { x, y, width: w, height: h }
    }

    #[test]
    fn no_overflow_at_fit_zoom_has_no_bars() {
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        assert_eq!(overflow_of((100.0, 100.0), 1.0, bounds), (false, false));
        let cb = content_bounds_of((100.0, 100.0), 1.0, bounds);
        assert_eq!((cb.x, cb.y, cb.width, cb.height), (0.0, 0.0, 300.0, 300.0));
        let (h, v) = scrollbars_of((100.0, 100.0), 1.0, (0.0, 0.0), bounds);
        assert!(h.is_none() && v.is_none(), "no bars when the picture fits");
    }

    #[test]
    fn horizontal_only_overflow_reserves_the_bottom_strip() {
        // Wide content, height fits on its own: only the horizontal (bottom) bar shows,
        // which reserves height (a strip along the bottom), not width.
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        assert_eq!(overflow_of((600.0, 100.0), 1.0, bounds), (true, false));
        let cb = content_bounds_of((600.0, 100.0), 1.0, bounds);
        assert_eq!((cb.width, cb.height), (300.0, 300.0 - SCROLLBAR_TOTAL));
    }

    #[test]
    fn vertical_only_overflow_reserves_the_right_strip() {
        // Tall content, width fits on its own: only the vertical (right) bar shows,
        // which reserves width, not height.
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        assert_eq!(overflow_of((100.0, 600.0), 1.0, bounds), (false, true));
        let cb = content_bounds_of((100.0, 600.0), 1.0, bounds);
        assert_eq!((cb.width, cb.height), (300.0 - SCROLLBAR_TOTAL, 300.0));
    }

    #[test]
    fn clamp_pan_reaches_both_extremes_and_pins_the_non_overflowing_axis() {
        // Horizontal-only overflow (300x300 bounds, 600x100 content @ zoom 1): the
        // non-overflowing axis must clamp to exactly 0, and x must reach both real edges.
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        let content = (600.0, 100.0);
        assert_eq!(clamp_pan_of(content, 1.0, bounds, (1000.0, 1000.0)), (150.0, 0.0));
        assert_eq!(clamp_pan_of(content, 1.0, bounds, (-1000.0, -1000.0)), (-150.0, 0.0));
        assert_eq!(clamp_pan_of(content, 1.0, bounds, (0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn scrollbar_thumb_length_is_proportional_to_the_visible_fraction() {
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        // Content twice the viewport: thumb is half the track — checked on both axes,
        // since the h/v branches are hand-mirrored (a transposition bug would show on
        // only one of them).
        let (_, v) = scrollbars_of((100.0, 600.0), 1.0, (0.0, 0.0), bounds);
        assert_eq!(v.expect("vertical overflow present").1.height, 150.0);
        let (h, _) = scrollbars_of((600.0, 100.0), 1.0, (0.0, 0.0), bounds);
        assert_eq!(h.expect("horizontal overflow present").1.width, 150.0);
        // Content four times the viewport: thumb is a quarter of the track.
        let (_, v) = scrollbars_of((100.0, 1200.0), 1.0, (0.0, 0.0), bounds);
        assert_eq!(v.expect("vertical overflow present").1.height, 75.0);
    }

    #[test]
    fn scrollbar_thumb_length_floors_at_sb_min() {
        let bounds = r(0.0, 0.0, 300.0, 300.0);
        // Content 1000x the viewport: the raw proportional thumb (<1px) must floor at SB_MIN.
        let (_, v) = scrollbars_of((100.0, 300_000.0), 1.0, (0.0, 0.0), bounds);
        let (_, thumb) = v.expect("vertical overflow present");
        assert_eq!(thumb.height, 24.0);
    }
}
