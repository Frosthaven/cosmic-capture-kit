//! The image CROP interaction canvas (DRAGON-382): a transparent leaf `Widget` layered OVER
//! the preview's [`crate::widgets::ZoomPan`] while the crop tool is active. It dims the media
//! outside the crop rectangle, draws the rule-of-thirds grid + the eight drag handles in the
//! accent colour, and routes pointer drags of the sides/corners/body back to the app as
//! [`CropEvent`]s (all points already mapped to IMAGE SOURCE pixels).
//!
//! # Why a wrapper, not a sibling
//! Like [`crate::widgets::annotation_canvas::AnnotationCanvas`], this OWNS the ZoomPan as its
//! child and forwards every event it doesn't consume to it, so zoom + pan keep working during a
//! crop session. It applies the SAME transform the ZoomPan draws with (via the shared
//! [`crate::widgets::annotation_canvas::CanvasMap`]), so its chrome and the picture stay in
//! lock-step at any zoom/pan. The crop MODEL (the rect, snap, undo, bake) lives in
//! `crate::app::preview::crop`; this module owns only the interaction + the overlay drawing.

use cosmic::iced::core::widget::{tree, Operation, Tree};
use cosmic::iced::core::{
    keyboard, layout, mouse, overlay, renderer, Clipboard, Event, Layout, Length, Point,
    Rectangle, Shell, Size, Vector,
};
use cosmic::iced::advanced::Renderer as _;
use cosmic::iced::{Background, Border, Color};
use cosmic::widget::Widget;

use crate::widgets::annotation_canvas::CanvasMap;

/// Which part of the crop rectangle a drag grabs. The four edges, the four corners, or the
/// body (a whole-rectangle move). Shared by the widget's events, the app's drag handler and the
/// pure geometry in `crate::app::preview::crop`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropHandle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
    /// The body: a drag moves the whole rectangle.
    Move,
}

impl CropHandle {
    pub fn moves_west(self) -> bool {
        matches!(self, CropHandle::W | CropHandle::NW | CropHandle::SW)
    }
    pub fn moves_east(self) -> bool {
        matches!(self, CropHandle::E | CropHandle::NE | CropHandle::SE)
    }
    pub fn moves_north(self) -> bool {
        matches!(self, CropHandle::N | CropHandle::NE | CropHandle::NW)
    }
    pub fn moves_south(self) -> bool {
        matches!(self, CropHandle::S | CropHandle::SE | CropHandle::SW)
    }
}

/// What the crop overlay reports to the app — points already in IMAGE SOURCE pixels.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::enum_variant_names)] // Drag{Begin,To,End} is the clearest naming for one gesture.
pub enum CropEvent {
    /// A press grabbed `handle` at image point `(x, y)`.
    DragBegin(CropHandle, f32, f32),
    /// The drag moved to image point `(x, y)`; `suppress_snap` = the override modifier
    /// (Cmd on macOS, Ctrl elsewhere) is held.
    DragTo(f32, f32, bool),
    /// The drag released.
    DragEnd,
}

/// The drawn handle square's side (screen px).
const HANDLE_SIZE: f32 = 10.0;
/// The (larger) hit target around each handle centre (screen px), for an easy grab.
const HANDLE_HIT: f32 = 16.0;
/// The dim scrim alpha over the cropped-out area.
const SCRIM_ALPHA: f32 = 0.6;

#[derive(Default)]
struct State {
    mods: keyboard::Modifiers,
    /// The handle being dragged, if any.
    drag: Option<CropHandle>,
}

pub struct CropCanvas<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    zoom: f32,
    pan: (f32, f32),
    /// The fitted picture's pixel size at zoom 1 (dw, dh) — the ZoomPan's `content_px`.
    disp: (f32, f32),
    /// Image source pixel dims (fw, fh).
    source: (f32, f32),
    /// The live crop rectangle in SOURCE px: `(x, y, w, h)`.
    rect: (f32, f32, f32, f32),
    accent: Color,
    on_event: Box<dyn Fn(CropEvent) -> Msg>,
}

impl<'a, Msg> CropCanvas<'a, Msg> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        zoom: f32,
        pan: (f32, f32),
        disp: (f32, f32),
        source: (f32, f32),
        rect: (f32, f32, f32, f32),
        accent: Color,
        on_event: impl Fn(CropEvent) -> Msg + 'static,
    ) -> Self {
        Self {
            content: content.into(),
            zoom,
            pan,
            disp,
            source,
            rect,
            accent,
            on_event: Box::new(on_event),
        }
    }

    fn map(&self, bounds: Rectangle) -> CanvasMap {
        CanvasMap {
            bounds: (bounds.width, bounds.height),
            zoom: self.zoom,
            pan: self.pan,
            disp: self.disp,
            source: self.source,
            // The crop SESSION shows the whole image (the rect is repositionable over the full
            // source), so there is never a view-crop offset here (DRAGON-385).
            offset: (0.0, 0.0),
        }
    }

    /// The content rectangle (global) the ZoomPan draws + clips to — the bounds minus the
    /// scrollbar strips. The crop chrome clips to this.
    fn content_rect(&self, bounds: Rectangle) -> Rectangle {
        crate::widgets::zoom_pan::content_bounds(self.disp, self.zoom, bounds)
    }

    /// The eight handle CENTRES in GLOBAL screen coords, tagged by which handle each is.
    fn handle_points(&self, bounds: Rectangle) -> [(CropHandle, Point); 8] {
        let map = self.map(bounds);
        let (ox, oy) = (bounds.x, bounds.y);
        let (x, y, w, h) = self.rect;
        let pt = |sx: f32, sy: f32| {
            let (cx, cy) = map.to_canvas((sx, sy));
            Point::new(ox + cx, oy + cy)
        };
        [
            (CropHandle::NW, pt(x, y)),
            (CropHandle::N, pt(x + w / 2.0, y)),
            (CropHandle::NE, pt(x + w, y)),
            (CropHandle::E, pt(x + w, y + h / 2.0)),
            (CropHandle::SE, pt(x + w, y + h)),
            (CropHandle::S, pt(x + w / 2.0, y + h)),
            (CropHandle::SW, pt(x, y + h)),
            (CropHandle::W, pt(x, y + h / 2.0)),
        ]
    }

    /// The crop rectangle in GLOBAL screen coords.
    fn rect_on_screen(&self, bounds: Rectangle) -> Rectangle {
        let map = self.map(bounds);
        let (ox, oy) = (bounds.x, bounds.y);
        let (x, y, w, h) = self.rect;
        let a = map.to_canvas((x, y));
        let b = map.to_canvas((x + w, y + h));
        Rectangle {
            x: ox + a.0.min(b.0),
            y: oy + a.1.min(b.1),
            width: (b.0 - a.0).abs(),
            height: (b.1 - a.1).abs(),
        }
    }

    /// Which handle (if any) a global cursor point grabs: a corner/edge handle first, else the
    /// body (Move) when inside the crop rectangle.
    fn hit(&self, bounds: Rectangle, p: Point) -> Option<CropHandle> {
        for (handle, c) in self.handle_points(bounds) {
            let r = Rectangle {
                x: c.x - HANDLE_HIT / 2.0,
                y: c.y - HANDLE_HIT / 2.0,
                width: HANDLE_HIT,
                height: HANDLE_HIT,
            };
            if r.contains(p) {
                return Some(handle);
            }
        }
        self.rect_on_screen(bounds).contains(p).then_some(CropHandle::Move)
    }

    /// The override modifier (disable snapping): Cmd on macOS, Ctrl elsewhere — Photoshop's rule.
    fn suppress_snap(mods: keyboard::Modifiers) -> bool {
        #[cfg(target_os = "macos")]
        {
            mods.logo()
        }
        #[cfg(not(target_os = "macos"))]
        {
            mods.control()
        }
    }

    fn emit(&self, shell: &mut Shell<'_, Msg>, ev: CropEvent) {
        shell.publish((self.on_event)(ev));
    }
}

impl<'a, Msg: Clone + 'a> Widget<Msg, cosmic::Theme, cosmic::Renderer> for CropCanvas<'a, Msg> {
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
        let map = self.map(bounds);
        let st = tree.state.downcast_mut::<State>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                st.mods = *m;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position()
                    && self.content_rect(bounds).contains(p)
                    // Alt (or a held middle button) belongs to the ZoomPan pan — don't grab it.
                    && !st.mods.alt()
                    && let Some(handle) = self.hit(bounds, p)
                {
                    let img = map.to_image((p.x - bounds.x, p.y - bounds.y));
                    st.drag = Some(handle);
                    self.emit(shell, CropEvent::DragBegin(handle, img.0, img.1));
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if st.drag.is_some() => {
                if let Some(p) = cursor.position() {
                    let img = map.to_image((p.x - bounds.x, p.y - bounds.y));
                    self.emit(shell, CropEvent::DragTo(img.0, img.1, Self::suppress_snap(st.mods)));
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if st.drag.is_some() => {
                st.drag = None;
                self.emit(shell, CropEvent::DragEnd);
                shell.capture_event();
                return;
            }
            _ => {}
        }
        // Anything not consumed (wheel zoom, alt-pan, scrollbar drags) goes to the ZoomPan.
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
        let bounds = layout.bounds();
        let st = tree.state.downcast_ref::<State>();
        // The grab cursor while dragging, and a resize/move hint on hover over a handle.
        let hover = st.drag.or_else(|| cursor.position().and_then(|p| self.hit(bounds, p)));
        if !st.mods.alt()
            && let Some(h) = hover
        {
            return match h {
                CropHandle::Move => mouse::Interaction::Grabbing,
                CropHandle::N | CropHandle::S => mouse::Interaction::ResizingVertically,
                CropHandle::E | CropHandle::W => mouse::Interaction::ResizingHorizontally,
                CropHandle::NW | CropHandle::SE => mouse::Interaction::ResizingDiagonallyDown,
                CropHandle::NE | CropHandle::SW => mouse::Interaction::ResizingDiagonallyUp,
            };
        }
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
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
        // 1. The wrapped ZoomPan draws the image (clipped to its content, scrollbars on top).
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        let bounds = layout.bounds();
        let clip = self.content_rect(bounds);
        let cr = self.rect_on_screen(bounds);
        let accent = self.accent;
        // The visible crop rect, clamped to the content area (the scrim strips reference this).
        let cr = cr.intersection(&clip).unwrap_or(Rectangle { x: clip.x, y: clip.y, width: 0.0, height: 0.0 });
        renderer.with_layer(clip, |r| {
            let fill = |r: &mut cosmic::Renderer, rect: Rectangle, color: Color| {
                if rect.width <= 0.0 || rect.height <= 0.0 {
                    return;
                }
                r.fill_quad(
                    renderer::Quad { bounds: rect, ..Default::default() },
                    Background::Color(color),
                );
            };
            // 2. The dim scrim: four black strips around the crop rect (the inside stays clear).
            let scrim = Color { r: 0.0, g: 0.0, b: 0.0, a: SCRIM_ALPHA };
            // Top strip (full width, above the crop rect).
            fill(r, Rectangle { x: clip.x, y: clip.y, width: clip.width, height: (cr.y - clip.y).max(0.0) }, scrim);
            // Bottom strip (full width, below).
            let below_y = cr.y + cr.height;
            fill(r, Rectangle { x: clip.x, y: below_y, width: clip.width, height: (clip.y + clip.height - below_y).max(0.0) }, scrim);
            // Left strip (between the top/bottom strips).
            fill(r, Rectangle { x: clip.x, y: cr.y, width: (cr.x - clip.x).max(0.0), height: cr.height }, scrim);
            // Right strip.
            let right_x = cr.x + cr.width;
            fill(r, Rectangle { x: right_x, y: cr.y, width: (clip.x + clip.width - right_x).max(0.0), height: cr.height }, scrim);

            // 3. The rule-of-thirds grid inside the crop rect (accent, thin, semi-transparent).
            if cr.width > 1.0 && cr.height > 1.0 {
                let line = Color { a: 0.55, ..accent };
                let lw = 1.0;
                for k in 1..=2 {
                    let fx = cr.x + cr.width * (k as f32 / 3.0);
                    fill(r, Rectangle { x: fx - lw / 2.0, y: cr.y, width: lw, height: cr.height }, line);
                    let fy = cr.y + cr.height * (k as f32 / 3.0);
                    fill(r, Rectangle { x: cr.x, y: fy - lw / 2.0, width: cr.width, height: lw }, line);
                }
            }

            // 4. The crop rectangle's border (accent, 1.5px, drawn as a border-only quad).
            r.fill_quad(
                renderer::Quad {
                    bounds: cr,
                    border: Border { width: 1.5, color: accent, radius: 0.0.into() },
                    ..Default::default()
                },
                Background::Color(Color::TRANSPARENT),
            );

            // 5. The eight drag handles (filled accent, centred on each point). Their SHAPE follows
            //    the active COSMIC theme's roundness: a perfect CIRCLE when any corner rounding is
            //    selected (the small token is non-zero), a perfect SQUARE when the theme is square
            //    (radius 0). Read from the same source every other canvas draw reads
            //    (`crate::app::theme::rounding`), so it tracks the desktop preference with no new
            //    setting. The hit target (`handle_points` / `HANDLE_HIT`) is unchanged — this is a
            //    draw-shape choice only. A quad radius of half the side renders as a circle (the
            //    quad renderer clamps to half the shorter axis).
            let s = HANDLE_SIZE;
            let radius = if crate::app::theme::rounding(theme).s1() > 0.0 { s / 2.0 } else { 0.0 };
            for (_, c) in self.handle_points(bounds) {
                r.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle { x: c.x - s / 2.0, y: c.y - s / 2.0, width: s, height: s },
                        border: Border { radius: radius.into(), color: accent, width: 0.0 },
                        ..Default::default()
                    },
                    Background::Color(accent),
                );
            }
        });
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

impl<'a, Msg: Clone + 'a> From<CropCanvas<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: CropCanvas<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}
