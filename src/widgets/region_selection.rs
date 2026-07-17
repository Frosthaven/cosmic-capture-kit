//! Region drag-selection overlay widget.
//!
//! A leaf libcosmic `Widget` that fills its output surface and lets the user:
//!
//! - drag on empty space to draw a new rectangle,
//! - drag a corner handle to resize diagonally,
//! - drag an edge to resize that side,
//! - drag inside the rectangle to move it.
//!
//! A plain click (press+release without a real drag) keeps the current
//! selection. It dims everything outside the selection and draws an accent
//! border + corner handles, reporting the rectangle in GLOBAL compositor
//! coordinates (which the native screencopy capture crops to). Single-surface
//! for now.

use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::{Tree, tree};
use cosmic::iced::core::{
    Border, Clipboard, Color, Event, Layout, Length, Point, Rectangle, Shadow, Shell, Size,
    keyboard, mouse,
};
use cosmic::widget::Widget;
use crate::geometry::{Corner, Edge, GlobalRect, point_in_quad};

/// Callback for a right-click on an OCR word: `(reading index, global x, global y)`.
type WordMenuFn<Msg> = dyn Fn(usize, i32, i32) -> Msg;

/// Callback for a multi-click expand: `(word index, click count)` — 2 selects the
/// word's line, 3 (or more) selects everything.
type TextExpandFn<Msg> = dyn Fn(usize, u8) -> Msg;

const HANDLE_GRAB: f32 = 16.0; // corner hit radius (px)
const EDGE_GRAB: f32 = 8.0; // edge hit thickness (px)
const NEW_THRESHOLD: f32 = 4.0; // px of movement before a click becomes a new drag
// DRAGON-206/210 monitor-edge WALL: a dragged edge STOPS at the border and stays pinned
// there until the cursor pushes `EDGE_BREAK` px past it, then it breaks through and tracks
// the cursor 1:1 (offset by the break, so it is continuous with no jump). You must
// deliberately shove past to cross, and can then position any amount into the next display.
const EDGE_BREAK: i32 = 40;

// DRAGON-208/209 selection-box design (logical px). Each side is drawn as
// `corner - gap - line - gap - corner`: an L-bracket at each end and a single centered line
// between them. Corners and the line share an inner edge (EDGE_THICK in from the selection
// boundary) and grow their thickness OUTWARD, so the encompassing rectangle stays
// pixel-exact. A small selection scales the corner arms down (keeping MIN_SEG of line), then
// falls back to a plain fully-connected rectangle when even a stub won't fit.
const EDGE_THICK: f32 = 1.0; // inner-edge inset reference (no hairline is drawn at it)
const DEFAULT_BOX_THICK: f32 = 4.0; // DRAGON-209 default box thickness (corners + lines, matched)
const CORNER_ARM: f32 = 33.0; // corner arm length ceiling (scales down on small boxes)
const CORNER_RADIUS: f32 = 6.0; // outer-corner rounding
const CORNER_TIP: f32 = 4.0; // arm-tip / line-end rounding
// Rounding applied to the line ends / arm tips: the tip value, but pointy ONLY when NO
// rounding at all is chosen — if EITHER the tip or the outer radius is set, the ends stay
// rounded (falling back to the radius when the tip alone is zero).
const END_ROUND: f32 = if CORNER_TIP > 0.0 { CORNER_TIP } else { CORNER_RADIUS };
const EDGE_GAP: f32 = 7.0; // DRAGON-209: gap between a corner bracket and the side line
const MIN_SEG: f32 = 8.0; // arms shrink to keep at least this much line per side
const MIN_ARM: f32 = 7.0; // below this the brackets are dropped for a plain box
const GRAB_PAD: f32 = 6.0; // hit-zone padding past the resize targets
const WALL_HANDLE_LEN: f32 = 52.0; // edge-resize easy-hit span, centered on each side
const HANDLE_GRAB_PERP: f32 = 12.0; // edge-resize hit tolerance perpendicular to the edge

/// DRAGON-208: the two arm quads of a corner bracket as `(x, y, w, h, [tl,tr,br,bl] radii)`
/// in LOCAL output coords — `[0]` horizontal, `[1]` vertical. The bracket's outer corner is
/// pushed OUT by `p` past the boundary so its inner edges land on the thin lines; arms are
/// `ct` thick and `arm` long, outer corner rounded `radius`, tips/inner-square per the
/// viewfinder look. `ll,tt,rr,bb` are the selection edges. Pure.
fn corner_arms(
    corner: Corner,
    rect: (f32, f32, f32, f32),
    p: f32,
    arm: f32,
    ct: f32,
    radius: f32,
    cap: f32,
) -> [(f32, f32, f32, f32, [f32; 4]); 2] {
    let (ll, tt, rr, bb) = rect;
    // Flip the local (NW) radii for a corner mirrored in x and/or y.
    let flip = |mut r: [f32; 4], fx: bool, fy: bool| {
        if fx {
            r.swap(0, 1);
            r.swap(3, 2);
        }
        if fy {
            r.swap(0, 3);
            r.swap(1, 2);
        }
        r
    };
    // Outer corner point + inward directions per corner.
    let (bx, by, dx, dy) = match corner {
        Corner::Nw => (ll - p, tt - p, 1.0_f32, 1.0_f32),
        Corner::Ne => (rr + p, tt - p, -1.0, 1.0),
        Corner::Sw => (ll - p, bb + p, 1.0, -1.0),
        Corner::Se => (rr + p, bb + p, -1.0, -1.0),
    };
    let (fx, fy) = (dx < 0.0, dy < 0.0);
    // horizontal arm: local (0,0,arm,ct); vertical arm: local (0,0,ct,arm).
    let hx = if dx > 0.0 { bx } else { bx - arm };
    let hy = if dy > 0.0 { by } else { by - ct };
    let vx = if dx > 0.0 { bx } else { bx - ct };
    let vy = if dy > 0.0 { by } else { by - arm };
    [
        (hx, hy, arm, ct, flip([radius, cap, cap, 0.0], fx, fy)),
        (vx, vy, ct, arm, flip([radius, 0.0, cap, cap], fx, fy)),
    ]
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Grab {
    #[default]
    None,
    /// Pressed empty space; not yet a drag (keeps the current selection).
    PendingNew,
    New,
    Move,
    Resize(Corner),
    ResizeEdge(Edge),
    /// Dragging over OCR text to select it (anchored at `State::anchor_word`).
    TextSelect,
}

#[derive(Default)]
struct State {
    grab: Grab,
    /// Global point where the press began.
    press: (i32, i32),
    /// The selection rectangle at press time (global, normalized), as a `(l, t, r, b)`
    /// tuple so `State` can stay `#[derive(Default)]` (the resize math reads it as one).
    orig: (i32, i32, i32, i32),
    /// Whether the current grab moved past the click threshold (a real drag).
    moved: bool,
    /// Mark under the press, if any (activated on a release that stayed a click).
    press_mark: Option<usize>,
    /// Last hover index we published, to avoid republishing every cursor move.
    hover_mark: Option<usize>,
    /// Reading index of the word a text selection is anchored at.
    anchor_word: usize,
    /// Whether `anchor_word` has been set (so shift-click has something to extend from).
    has_anchor: bool,
    /// A ctrl-press on a word, pending: a release without a drag toggles it; a drag
    /// turns it into an additive range selection.
    ctrl_pending: Option<usize>,
    /// Last hovered word index we published (de-dupes cursor-move spam).
    hover_word: Option<usize>,
    /// Latest keyboard modifiers (tracked for ctrl/shift-click multi-select).
    mods: keyboard::Modifiers,
    /// Time + position of the last left press, to detect double/triple clicks.
    last_click: Option<(std::time::Instant, (i32, i32))>,
    /// Consecutive-click count for the current click streak (1=single, 2=double, …).
    click_count: u32,
}

fn corner_cursor(c: Corner) -> mouse::Interaction {
    match c {
        Corner::Nw | Corner::Se => mouse::Interaction::ResizingDiagonallyDown,
        Corner::Ne | Corner::Sw => mouse::Interaction::ResizingDiagonallyUp,
    }
}

fn edge_cursor(e: Edge) -> mouse::Interaction {
    match e {
        Edge::N | Edge::S => mouse::Interaction::ResizingVertically,
        Edge::E | Edge::W => mouse::Interaction::ResizingHorizontally,
    }
}

/// DRAGON-210 monitor-edge WALL. Coordinate `v` is measured against the axis's two borders
/// `lo`/`hi` (the output's left/right or top/bottom). An edge INSIDE the bounds is left
/// exactly where it is. Once `v` crosses OUTSIDE a border it is PINNED to that border until
/// its overshoot exceeds `brk` px (a firm stop you must push through), then it breaks free
/// and tracks the cursor 1:1 offset by `brk` — continuous, no jump, and any amount past is
/// then reachable so you can extend onto the next display once you have committed to it.
fn wall(v: i32, lo: i32, hi: i32, brk: i32) -> i32 {
    if v < lo {
        lo - (lo - v - brk).max(0)
    } else if v > hi {
        hi + (v - hi - brk).max(0)
    } else {
        v
    }
}

/// DRAGON-206/210: apply the monitor-edge wall to the dragged rectangle `(l, t, r, b)`
/// against the output's borders `bounds = (l, t, r, b)` (global coords). An edge inside the
/// display is untouched; a crossed edge is pinned to the border until pushed past `brk` by
/// [`wall`]. A `Move` walls the leading edge that crossed and translates (size preserved); a
/// `New` walls all four edges; a corner/edge resize walls only the edge(s) that moved. The
/// caller skips this entirely while Option/Alt is held. Pure. The `New` tuple may be
/// un-normalized (anchor vs cursor in either order) — walling each edge against both parallel
/// borders handles that.
fn wall_rect(
    rect: (i32, i32, i32, i32),
    grab: Grab,
    bounds: (i32, i32, i32, i32),
    brk: i32,
) -> (i32, i32, i32, i32) {
    let (l, t, r, b) = rect;
    let (bl, bt, br, bb) = bounds;
    match grab {
        Grab::Move => {
            // Size-preserving: wall the leading edge that crossed its border (left/top toward
            // the low border, right/bottom toward the high one). One edge leads per axis, so
            // there is no double count.
            let dx = if l < bl {
                wall(l, bl, br, brk) - l
            } else if r > br {
                wall(r, bl, br, brk) - r
            } else {
                0
            };
            let dy = if t < bt {
                wall(t, bt, bb, brk) - t
            } else if b > bb {
                wall(b, bt, bb, brk) - b
            } else {
                0
            };
            (l + dx, t + dy, r + dx, b + dy)
        }
        Grab::New => (wall(l, bl, br, brk), wall(t, bt, bb, brk), wall(r, bl, br, brk), wall(b, bt, bb, brk)),
        Grab::Resize(Corner::Nw) => (wall(l, bl, br, brk), wall(t, bt, bb, brk), r, b),
        Grab::Resize(Corner::Ne) => (l, wall(t, bt, bb, brk), wall(r, bl, br, brk), b),
        Grab::Resize(Corner::Sw) => (wall(l, bl, br, brk), t, r, wall(b, bt, bb, brk)),
        Grab::Resize(Corner::Se) => (l, t, wall(r, bl, br, brk), wall(b, bt, bb, brk)),
        Grab::ResizeEdge(Edge::N) => (l, wall(t, bt, bb, brk), r, b),
        Grab::ResizeEdge(Edge::S) => (l, t, r, wall(b, bt, bb, brk)),
        Grab::ResizeEdge(Edge::W) => (wall(l, bl, br, brk), t, r, b),
        Grab::ResizeEdge(Edge::E) => (l, t, wall(r, bl, br, brk), b),
        Grab::None | Grab::PendingNew | Grab::TextSelect => rect,
    }
}

pub struct RegionSelection<Msg> {
    origin: (i32, i32),
    region: Option<GlobalRect>,
    on_change: Box<dyn Fn(GlobalRect) -> Msg>,
    on_done: Msg,
    interactive: bool,
    /// Opacity of the black dim outside the selection.
    dim_alpha: f32,
    /// Opacity of the accent selection lines.
    line_alpha: f32,
    /// Draw the border lines just OUTSIDE the selection instead of inside it, so
    /// they frame the region without landing in a recording's cropped pixels.
    outer_border: bool,
    /// DRAGON-209: the interactive viewfinder box thickness (logical px) applied to the
    /// corner brackets AND the side lines uniformly (persisted appearance setting).
    box_thickness: f32,
    /// Detected marks to hit-test: `(app index, global rect x/y/w/h)`. Drawn by a
    /// separate layer; we own hover + click here so a mark never blocks region drag.
    marks: Vec<(usize, (i32, i32, i32, i32))>,
    on_hover_mark: Option<Box<dyn Fn(Option<usize>) -> Msg>>,
    on_activate_mark: Option<Box<dyn Fn(usize) -> Msg>>,
    /// OCR words to hit-test for text selection: `(reading index, global 4-corner
    /// poly)`, in reading order (the poly follows the text slant). Pressing one starts a
    /// selection; dragging extends it by index.
    words: Vec<(usize, [(i32, i32); 4])>,
    on_hover_word: Option<Box<dyn Fn(Option<usize>) -> Msg>>,
    /// Begin a range selection at a word; the bool is additive (ctrl+shift).
    on_text_begin: Option<Box<dyn Fn(usize, bool) -> Msg>>,
    /// Extend the in-progress range selection to a word (drag / shift target).
    on_text_to: Option<Box<dyn Fn(usize) -> Msg>>,
    /// Ctrl-click toggles a single word in/out of the selection.
    on_text_toggle: Option<Box<dyn Fn(usize) -> Msg>>,
    /// Double-click selects the line; triple-click selects all.
    on_text_expand: Option<Box<TextExpandFn<Msg>>>,
    /// Right-click on a word — opens its copy menu.
    on_word_menu: Option<Box<WordMenuFn<Msg>>>,
    /// Right-click on a code mark (QR/barcode) — opens its "Copy contents" menu.
    on_code_menu: Option<Box<WordMenuFn<Msg>>>,
}

impl<Msg> RegionSelection<Msg> {
    pub fn new(
        origin: (i32, i32),
        region: Option<GlobalRect>,
        on_change: impl Fn(GlobalRect) -> Msg + 'static,
        on_done: Msg,
    ) -> Self {
        Self {
            origin,
            region,
            on_change: Box::new(on_change),
            on_done,
            interactive: true,
            dim_alpha: 0.70,
            line_alpha: 1.0,
            outer_border: false,
            box_thickness: DEFAULT_BOX_THICK,
            marks: Vec::new(),
            on_hover_mark: None,
            on_activate_mark: None,
            words: Vec::new(),
            on_hover_word: None,
            on_text_begin: None,
            on_text_to: None,
            on_text_toggle: None,
            on_text_expand: None,
            on_word_menu: None,
            on_code_menu: None,
        }
    }

    /// Right-click on a code mark opens its contents menu at the cursor (global coords).
    pub fn code_menu(mut self, on_menu: impl Fn(usize, i32, i32) -> Msg + 'static) -> Self {
        self.on_code_menu = Some(Box::new(on_menu));
        self
    }

    /// Hit-test these marks for hover/click (the region widget owns them so dragging
    /// still works when a press starts on a mark — a click activates, a drag drags).
    pub fn marks(
        mut self,
        marks: Vec<(usize, (i32, i32, i32, i32))>,
        on_hover: impl Fn(Option<usize>) -> Msg + 'static,
        on_activate: impl Fn(usize) -> Msg + 'static,
    ) -> Self {
        self.marks = marks;
        self.on_hover_mark = Some(Box::new(on_hover));
        self.on_activate_mark = Some(Box::new(on_activate));
        self
    }

    /// The app index of the mark containing global point `g`, if any.
    fn mark_at(&self, g: (i32, i32)) -> Option<usize> {
        self.marks.iter().find_map(|(i, (x, y, w, h))| {
            (g.0 >= *x && g.0 < x + w && g.1 >= *y && g.1 < y + h).then_some(*i)
        })
    }

    /// Hit-test OCR words for text selection. `on_select(anchor, end)` reports the
    /// reading-order span while dragging; the selection persists after release (the
    /// user copies via keyboard or the right-click menu). `on_menu(word, pos)` fires on
    /// a right-click over a word.
    #[allow(clippy::too_many_arguments)] // a builder wiring up the text-layer callbacks
    pub fn words(
        mut self,
        words: Vec<(usize, [(i32, i32); 4])>,
        on_hover: impl Fn(Option<usize>) -> Msg + 'static,
        on_begin: impl Fn(usize, bool) -> Msg + 'static,
        on_to: impl Fn(usize) -> Msg + 'static,
        on_toggle: impl Fn(usize) -> Msg + 'static,
        on_expand: impl Fn(usize, u8) -> Msg + 'static,
        on_menu: impl Fn(usize, i32, i32) -> Msg + 'static,
    ) -> Self {
        self.words = words;
        self.on_hover_word = Some(Box::new(on_hover));
        self.on_text_begin = Some(Box::new(on_begin));
        self.on_text_to = Some(Box::new(on_to));
        self.on_text_toggle = Some(Box::new(on_toggle));
        self.on_text_expand = Some(Box::new(on_expand));
        self.on_word_menu = Some(Box::new(on_menu));
        self
    }

    /// The reading index of the word whose (possibly skewed) quad contains global point
    /// `g`, if any.
    fn word_at(&self, g: (i32, i32)) -> Option<usize> {
        self.words
            .iter()
            .find_map(|(i, poly)| point_in_quad(g, poly).then_some(*i))
    }

    /// The reading index of the word at `g`, or the nearest one by quad-centroid distance
    /// (so a drag into the gaps / onto another row still extends the selection sensibly).
    fn word_near(&self, g: (i32, i32)) -> Option<usize> {
        if let Some(i) = self.word_at(g) {
            return Some(i);
        }
        self.words
            .iter()
            .map(|(i, poly)| {
                let cx = poly.iter().map(|p| p.0).sum::<i32>() as f32 / 4.0;
                let cy = poly.iter().map(|p| p.1).sum::<i32>() as f32 / 4.0;
                (*i, (g.0 as f32 - cx).hypot(g.1 as f32 - cy))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
    }

    /// Draw-only: no input handling (used for the frozen countdown border).
    pub fn non_interactive(mut self) -> Self {
        self.interactive = false;
        self
    }

    /// Draw the selection border just outside the region (so it's visible during a
    /// recording without being captured in the cropped frame).
    pub fn outer_border(mut self) -> Self {
        self.outer_border = true;
        self
    }


    /// DRAGON-209: set the interactive viewfinder box thickness (logical px, clamped 1-8);
    /// applied to the corner brackets and the side lines uniformly.
    pub fn box_thickness(mut self, px: u32) -> Self {
        self.box_thickness = px.clamp(1, 8) as f32;
        self
    }

    /// Opacity of the black dim drawn outside the selection.
    pub fn dim_alpha(mut self, a: f32) -> Self {
        self.dim_alpha = a;
        self
    }

    /// Opacity of the accent selection lines.
    pub fn line_alpha(mut self, a: f32) -> Self {
        self.line_alpha = a;
        self
    }
}

impl<Msg: Clone + 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for RegionSelection<Msg> {
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

    fn mouse_interaction(
        &self,
        tree: &Tree,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        if !self.interactive {
            return mouse::Interaction::default();
        }
        let state = tree.state.downcast_ref::<State>();
        match state.grab {
            Grab::Resize(c) => return corner_cursor(c),
            Grab::ResizeEdge(e) => return edge_cursor(e),
            Grab::Move => return mouse::Interaction::Grabbing,
            Grab::New | Grab::PendingNew => return mouse::Interaction::Crosshair,
            Grab::TextSelect => return mouse::Interaction::Text,
            Grab::None => {}
        }
        if let (Some(p), Some(rect)) = (cursor.position(), self.region.map(GlobalRect::normalize)) {
            let g = (p.x as i32 + self.origin.0, p.y as i32 + self.origin.1);
            if let Some(c) = rect.corner_at(g, HANDLE_GRAB) {
                return corner_cursor(c);
            }
            // DRAGON-208: the wall handle is a bigger, easier target than the thin edge...
            if let Some(e) = rect
                .edge_handle_at(g, WALL_HANDLE_LEN / 2.0 + GRAB_PAD, HANDLE_GRAB_PERP)
                .or_else(|| rect.edge_at(g, EDGE_GRAB))
            {
                // ...but the whole edge still resizes.
                return edge_cursor(e);
            }
            // A detected code (QR/barcode) is clickable — show the crosshair.
            if self.mark_at(g).is_some() {
                return mouse::Interaction::Crosshair;
            }
            // OCR text is selectable — show the I-beam.
            if self.word_at(g).is_some() {
                return mouse::Interaction::Text;
            }
            if rect.contains(g) {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::Crosshair
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
        if !self.interactive {
            return;
        }
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let to_global = |p: Point| (p.x as i32 + self.origin.0, p.y as i32 + self.origin.1);
        match event {
            // Right-click on a word opens its copy menu.
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                let g = to_global(p);
                // A code mark's contents menu takes priority over a word menu.
                if let Some(mi) = self.mark_at(g)
                    && let Some(cb) = &self.on_code_menu
                {
                    shell.publish(cb(mi, g.0, g.1));
                    shell.capture_event();
                } else if let Some(wi) = self.word_at(g)
                    && let Some(cb) = &self.on_word_menu
                {
                    shell.publish(cb(wi, g.0, g.1));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                let g = to_global(p);
                state.press = g;
                state.press_mark = self.mark_at(g);
                state.moved = false;
                // Count consecutive clicks (same spot, within 400ms) for double/triple.
                let now = std::time::Instant::now();
                state.click_count = match state.last_click {
                    Some((t, lp))
                        if now.duration_since(t).as_millis() <= 400
                            && (lp.0 - g.0).abs() <= 5
                            && (lp.1 - g.1).abs() <= 5 =>
                    {
                        state.click_count + 1
                    }
                    _ => 1,
                };
                state.last_click = Some((now, g));
                if let Some(rect) = self.region.map(GlobalRect::normalize) {
                    if let Some(c) = rect.corner_at(g, HANDLE_GRAB) {
                        state.orig = rect.to_tuple();
                        state.grab = Grab::Resize(c);
                        return shell.capture_event();
                    }
                    // DRAGON-208: the wall handle is the easy target, but the whole edge
                    // still starts an edge resize.
                    if let Some(e) = rect
                        .edge_handle_at(g, WALL_HANDLE_LEN / 2.0 + GRAB_PAD, HANDLE_GRAB_PERP)
                        .or_else(|| rect.edge_at(g, EDGE_GRAB))
                    {
                        state.orig = rect.to_tuple();
                        state.grab = Grab::ResizeEdge(e);
                        return shell.capture_event();
                    }
                    // Pressing on OCR text selects it:
                    //  • double-click = the line, triple-click = everything,
                    //  • ctrl+shift = add the range from the anchor (additive),
                    //  • ctrl       = toggle the single word,
                    //  • shift      = replace with the range from the anchor,
                    //  • plain      = start a fresh selection the drag extends.
                    if let Some(wi) = self.word_at(g) {
                        let (ctrl, shift) = (state.mods.control(), state.mods.shift());
                        if !ctrl && !shift && state.click_count >= 2 {
                            state.anchor_word = wi;
                            state.has_anchor = true;
                            if let Some(cb) = &self.on_text_expand {
                                shell.publish(cb(wi, state.click_count.min(3) as u8));
                            }
                        } else if ctrl && shift && state.has_anchor {
                            // Additive range from the anchor — grab so the drag keeps
                            // extending it continuously.
                            state.grab = Grab::TextSelect;
                            if let Some(cb) = &self.on_text_begin {
                                shell.publish(cb(state.anchor_word, true));
                            }
                            if let Some(cb) = &self.on_text_to {
                                shell.publish(cb(wi));
                            }
                        } else if ctrl {
                            // Defer: a plain release toggles this word, a drag turns it
                            // into an additive range (handled on move / release).
                            state.anchor_word = wi;
                            state.has_anchor = true;
                            state.grab = Grab::TextSelect;
                            state.ctrl_pending = Some(wi);
                        } else if shift && state.has_anchor {
                            state.grab = Grab::TextSelect;
                            if let Some(cb) = &self.on_text_begin {
                                shell.publish(cb(state.anchor_word, false));
                            }
                            if let Some(cb) = &self.on_text_to {
                                shell.publish(cb(wi));
                            }
                        } else {
                            state.anchor_word = wi;
                            state.has_anchor = true;
                            state.grab = Grab::TextSelect;
                            if let Some(cb) = &self.on_text_begin {
                                shell.publish(cb(wi, false));
                            }
                        }
                        return shell.capture_event();
                    }
                    if rect.contains(g) {
                        state.orig = rect.to_tuple();
                        state.grab = Grab::Move;
                        return shell.capture_event();
                    }
                }
                // Empty space: pending until a real drag, so a plain click keeps
                // the current selection.
                state.grab = Grab::PendingNew;
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.grab != Grab::None => {
                let Some(p) = cursor.position() else {
                    return;
                };
                let q = to_global(p);
                // Text selection: extend the span to the word under (or nearest to)
                // the cursor — dragging down/up grabs the next rows.
                if state.grab == Grab::TextSelect {
                    let Some(end) = self.word_near(q) else {
                        return shell.capture_event();
                    };
                    // A ctrl-press becomes an additive drag only once it really moves
                    // (otherwise the release toggles the single word).
                    if state.ctrl_pending.is_some() {
                        let (dx, dy) = ((q.0 - state.press.0) as f32, (q.1 - state.press.1) as f32);
                        if dx.hypot(dy) <= NEW_THRESHOLD {
                            return shell.capture_event();
                        }
                        state.ctrl_pending = None;
                        if let Some(cb) = &self.on_text_begin {
                            shell.publish(cb(state.anchor_word, true));
                        }
                    }
                    if let Some(cb) = &self.on_text_to {
                        shell.publish(cb(end));
                    }
                    return shell.capture_event();
                }
                // A grab that moves past the threshold is a real drag (not a click),
                // so a press that started on a mark won't activate it on release.
                if !state.moved {
                    let dx = (q.0 - state.press.0) as f32;
                    let dy = (q.1 - state.press.1) as f32;
                    if dx.hypot(dy) > NEW_THRESHOLD {
                        state.moved = true;
                    }
                }
                let (l, t, r, b) = state.orig;
                let new = match state.grab {
                    Grab::PendingNew => {
                        let dx = (q.0 - state.press.0) as f32;
                        let dy = (q.1 - state.press.1) as f32;
                        if dx.hypot(dy) <= NEW_THRESHOLD {
                            return shell.capture_event();
                        }
                        state.grab = Grab::New;
                        (state.press.0, state.press.1, q.0, q.1)
                    }
                    Grab::New => (state.press.0, state.press.1, q.0, q.1),
                    Grab::Move => {
                        let dx = q.0 - state.press.0;
                        let dy = q.1 - state.press.1;
                        (l + dx, t + dy, r + dx, b + dy)
                    }
                    Grab::Resize(Corner::Nw) => (q.0, q.1, r, b),
                    Grab::Resize(Corner::Ne) => (l, q.1, q.0, b),
                    Grab::Resize(Corner::Sw) => (q.0, t, r, q.1),
                    Grab::Resize(Corner::Se) => (l, t, q.0, q.1),
                    Grab::ResizeEdge(Edge::N) => (l, q.1, r, b),
                    Grab::ResizeEdge(Edge::S) => (l, t, r, q.1),
                    Grab::ResizeEdge(Edge::W) => (q.0, t, r, b),
                    Grab::ResizeEdge(Edge::E) => (l, t, q.0, b),
                    Grab::None | Grab::TextSelect => return,
                };
                // DRAGON-206/210: the dragged edge hits a wall at the monitor border and
                // only crosses once pushed EDGE_BREAK px past it. Holding Option/Alt
                // suppresses it live (read per move, never latched at drag start).
                let new = if state.mods.alt() {
                    new
                } else {
                    let ob = (
                        self.origin.0,
                        self.origin.1,
                        self.origin.0 + bounds.width.round() as i32,
                        self.origin.1 + bounds.height.round() as i32,
                    );
                    wall_rect(new, state.grab, ob, EDGE_BREAK)
                };
                shell.publish((self.on_change)(GlobalRect::from_tuple(new)));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.grab != Grab::None =>
            {
                let was = state.grab;
                // A press that stayed a click (no drag) and landed on a mark
                // activates the mark instead of being treated as a region gesture.
                let clicked_mark = if state.moved { None } else { state.press_mark };
                state.grab = Grab::None;
                state.moved = false;
                state.press_mark = None;
                // A ctrl-press released without a drag toggles the single word.
                if let Some(w) = state.ctrl_pending.take()
                    && let Some(cb) = &self.on_text_toggle
                {
                    shell.publish(cb(w));
                }
                // A finished text selection just persists (the user copies it via
                // keyboard / the right-click menu); it isn't a region gesture.
                if was != Grab::TextSelect {
                    if let (Some(idx), Some(cb)) = (clicked_mark, &self.on_activate_mark) {
                        shell.publish(cb(idx));
                    } else if was != Grab::PendingNew {
                        shell.publish(self.on_done.clone());
                    }
                }
                shell.capture_event();
            }
            // Not grabbing: hover-test the marks (tooltip) + the words (highlight).
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let g = cursor.position_over(bounds).map(to_global);
                let hm = g.and_then(|g| self.mark_at(g));
                if hm != state.hover_mark {
                    state.hover_mark = hm;
                    if let Some(cb) = &self.on_hover_mark {
                        shell.publish(cb(hm));
                    }
                }
                let hw = g.and_then(|g| self.word_at(g));
                if hw != state.hover_word {
                    state.hover_word = hw;
                    if let Some(cb) = &self.on_hover_word {
                        shell.publish(cb(hw));
                    }
                }
            }
            // Track modifiers for ctrl/shift-click multi-select.
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                state.mods = *m;
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use cosmic::iced::core::Renderer as _;
        let b = layout.bounds();
        let (ox, oy, w, h) = (b.x, b.y, b.width, b.height);
        let dim = Color {
            a: self.dim_alpha,
            ..Color::BLACK
        };
        let mut fill = |x: f32, y: f32, fw: f32, fh: f32, color: Color, border: Border| {
            if fw <= 0.0 || fh <= 0.0 {
                return;
            }
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle::new(Point::new(x, y), Size::new(fw, fh)),
                    border,
                    shadow: Shadow::default(),
                    snap: true,
                },
                color,
            );
        };

        let local = self.region.and_then(|reg| {
            let (l, t, r, bm) = reg.normalize().to_tuple();
            let ll = ((l - self.origin.0) as f32).clamp(0.0, w);
            let tt = ((t - self.origin.1) as f32).clamp(0.0, h);
            let rr = ((r - self.origin.0) as f32).clamp(0.0, w);
            let bb = ((bm - self.origin.1) as f32).clamp(0.0, h);
            (rr - ll >= 1.0 && bb - tt >= 1.0).then_some((ll, tt, rr, bb))
        });

        match local {
            None => fill(ox, oy, w, h, dim, Border::default()),
            Some((ll, tt, rr, bb)) => {
                fill(ox, oy, w, tt, dim, Border::default()); // top
                fill(ox, oy + bb, w, h - bb, dim, Border::default()); // bottom
                fill(ox, oy + tt, ll, bb - tt, dim, Border::default()); // left
                fill(ox + rr, oy + tt, w - rr, bb - tt, dim, Border::default()); // right

                // Draw each border side only when the rect's TRUE edge lies on
                // this monitor. An edge that falls on a monitor boundary (the
                // selection continues onto an adjacent output) is hidden, so a
                // cross-monitor selection looks seamless.
                let (gl, gt, gr, gb) = self.region.map(|r| r.normalize().to_tuple()).unwrap_or_default();
                let mx0 = self.origin.0;
                let my0 = self.origin.1;
                let mx1 = mx0 + w as i32;
                let my1 = my0 + h as i32;
                let (show_l, show_r, show_t, show_b) = (gl >= mx0, gr <= mx1, gt >= my0, gb <= my1);
                let mut accent = crate::app::theme::accent(theme);
                accent.a = self.line_alpha;

                if !self.interactive {
                    // Countdown / recording frame: the original thin outline, optionally
                    // outset (`o`) so it doesn't intrude on the recorded crop. Kept
                    // byte-identical — DRAGON-208 restyles only the INTERACTIVE selection,
                    // which never sets outer_border.
                    let bw = 2.0;
                    let o = if self.outer_border { bw } else { 0.0 };
                    if show_l {
                        fill(ox + ll - o, oy + tt - o, bw, (bb - tt) + 2.0 * o, accent, Border::default());
                    }
                    if show_r {
                        fill(ox + rr - bw + o, oy + tt - o, bw, (bb - tt) + 2.0 * o, accent, Border::default());
                    }
                    if show_t {
                        fill(ox + ll - o, oy + tt - o, (rr - ll) + 2.0 * o, bw, accent, Border::default());
                    }
                    if show_b {
                        fill(ox + ll - o, oy + bb - bw + o, (rr - ll) + 2.0 * o, bw, accent, Border::default());
                    }
                } else {
                    // DRAGON-209 viewfinder: corner brackets + a single centered line per
                    // side (corner - gap - line - gap - corner). Corners and the line share
                    // the inner edge and grow their thickness outward; small selections scale
                    // the arms down, then fall back to a plain fully-connected rectangle.
                    let bw = EDGE_THICK;
                    // Corners and lines share ONE thickness so they match exactly.
                    let ct = self.box_thickness;
                    let lt = ct;
                    let p = (ct - bw).max(0.0); // corner outward push
                    let pl = (lt - bw).max(0.0); // line outward push
                    let sw = rr - ll;
                    let sh = bb - tt;
                    let round = |r: [f32; 4]| Border {
                        radius: r.into(),
                        ..Border::default()
                    };
                    let fit_arm = CORNER_ARM
                        .min((sw - 2.0 * EDGE_GAP - MIN_SEG) / 2.0)
                        .min((sh - 2.0 * EDGE_GAP - MIN_SEG) / 2.0);

                    if fit_arm < MIN_ARM {
                        // Too small for corners: a plain fully-connected rectangle at the box
                        // thickness.
                        if show_t {
                            fill(ox + ll, oy + tt, sw, lt, accent, Border::default());
                        }
                        if show_b {
                            fill(ox + ll, oy + bb - lt, sw, lt, accent, Border::default());
                        }
                        if show_l {
                            fill(ox + ll, oy + tt, lt, sh, accent, Border::default());
                        }
                        if show_r {
                            fill(ox + rr - lt, oy + tt, lt, sh, accent, Border::default());
                        }
                    } else {
                        let line = round([END_ROUND; 4]);
                        // one centered line per side, from a corner arm end + gap to the far
                        // arm end - gap; shares the inner edge, overhangs outward by pl.
                        let (hx0, hx1) = (ll + fit_arm + EDGE_GAP, rr - fit_arm - EDGE_GAP);
                        let (vy0, vy1) = (tt + fit_arm + EDGE_GAP, bb - fit_arm - EDGE_GAP);
                        if hx1 - hx0 > 1.0 {
                            if show_t {
                                fill(ox + hx0, oy + tt - pl, hx1 - hx0, lt, accent, line);
                            }
                            if show_b {
                                fill(ox + hx0, oy + bb - bw, hx1 - hx0, lt, accent, line);
                            }
                        }
                        if vy1 - vy0 > 1.0 {
                            if show_l {
                                fill(ox + ll - pl, oy + vy0, lt, vy1 - vy0, accent, line);
                            }
                            if show_r {
                                fill(ox + rr - bw, oy + vy0, lt, vy1 - vy0, accent, line);
                            }
                        }
                        // corner brackets: the horizontal arm is gated by its top/bottom
                        // edge, the vertical arm by its left/right edge (a cross-monitor
                        // corner then shows only the arm whose edge is on this output).
                        for (corner, arm_h, arm_v) in [
                            (Corner::Nw, show_t, show_l),
                            (Corner::Ne, show_t, show_r),
                            (Corner::Sw, show_b, show_l),
                            (Corner::Se, show_b, show_r),
                        ] {
                            let arms = corner_arms(corner, (ll, tt, rr, bb), p, fit_arm, ct, CORNER_RADIUS, END_ROUND);
                            if arm_h {
                                let (x, y, aw, ah, r) = arms[0];
                                fill(ox + x, oy + y, aw, ah, accent, round(r));
                            }
                            if arm_v {
                                let (x, y, aw, ah, r) = arms[1];
                                fill(ox + x, oy + y, aw, ah, accent, round(r));
                            }
                        }
                    }
                }
            }
        }
    }
}

impl<'a, Msg: Clone + 'static> From<RegionSelection<Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: RegionSelection<Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::{corner_arms, wall_rect, Grab};
    use crate::geometry::{Corner, Edge};

    // A 1920x1080 output at global origin (0,0); break-through distance 20px.
    const OUT: (i32, i32, i32, i32) = (0, 0, 1920, 1080);
    const BRK: i32 = 20;

    #[test]
    fn inside_edges_are_untouched() {
        // The wall only engages OUTSIDE a border: an edge just inside stays exactly put, so
        // you can position freely right up against an edge.
        let inside = (2, 2, 1918, 1078);
        assert_eq!(wall_rect(inside, Grab::New, OUT, BRK), inside);
        assert_eq!(wall_rect(inside, Grab::Resize(Corner::Se), OUT, BRK), inside);
        assert_eq!(wall_rect(inside, Grab::Move, OUT, BRK), inside);
    }

    #[test]
    fn within_break_pins_to_the_border() {
        // Every corner 10px past its border (< BRK) -> pinned flush: a full-display grab.
        let nudged = (-10, -10, 1930, 1090);
        assert_eq!(wall_rect(nudged, Grab::New, OUT, BRK), (0, 0, 1920, 1080));
    }

    #[test]
    fn past_the_break_crosses_offset_by_the_break() {
        // 30px past (> BRK 20) -> breaks through to 30 - 20 = 10px past, continuous.
        let past = (-30, -30, 1950, 1110);
        assert_eq!(wall_rect(past, Grab::New, OUT, BRK), (-10, -10, 1930, 1090));
    }

    #[test]
    fn far_past_tracks_the_cursor_once_broken() {
        // A big overshoot crosses freely (offset by the break): 100px past -> 80px past.
        let far = (-100, 300, 900, 700);
        assert_eq!(wall_rect(far, Grab::ResizeEdge(Edge::W), OUT, BRK), (-80, 300, 900, 700));
    }

    #[test]
    fn corner_resize_walls_only_its_two_edges() {
        // SE corner 10px past bottom-right (< BRK) -> r and b pin to the border; l and t
        // (inside) are untouched.
        let rect = (5, 5, 1930, 1090);
        assert_eq!(wall_rect(rect, Grab::Resize(Corner::Se), OUT, BRK), (5, 5, 1920, 1080));
    }

    #[test]
    fn edge_resize_walls_only_that_edge() {
        // West edge 10px past (< BRK) pins to the left border; a broken push (40px) crosses.
        assert_eq!(wall_rect((-10, 300, 900, 700), Grab::ResizeEdge(Edge::W), OUT, BRK), (0, 300, 900, 700));
        assert_eq!(wall_rect((-40, 300, 900, 700), Grab::ResizeEdge(Edge::W), OUT, BRK), (-20, 300, 900, 700));
        // An East-edge grab leaves the crossed left edge alone.
        assert_eq!(wall_rect((-10, 300, 900, 700), Grab::ResizeEdge(Edge::E), OUT, BRK), (-10, 300, 900, 700));
    }

    #[test]
    fn move_walls_leading_edge_preserving_size() {
        // A 500-wide box shoved 10px past the left border (< BRK) -> pinned so the left edge
        // sits AT the border, whole box translated, size intact.
        let rect = (-10, 500, 490, 900);
        let out = wall_rect(rect, Grab::Move, OUT, BRK);
        assert_eq!(out, (0, 500, 500, 900));
        assert_eq!(out.2 - out.0, rect.2 - rect.0);
        assert_eq!(out.3 - out.1, rect.3 - rect.1);
    }

    #[test]
    fn non_drag_grabs_are_identity() {
        let rect = (-5, -5, 10, 10);
        assert_eq!(wall_rect(rect, Grab::None, OUT, BRK), rect);
        assert_eq!(wall_rect(rect, Grab::PendingNew, OUT, BRK), rect);
        assert_eq!(wall_rect(rect, Grab::TextSelect, OUT, BRK), rect);
    }

    // ── DRAGON-208 selection-box geometry ────────────────────────────────────

    #[test]
    fn corner_arms_share_the_inner_edge_and_push_out() {
        // NW corner of a 0,0..1000,800 selection, p=4 (ct 5 - bw 1), arm 33, ct 5.
        let [(hx, hy, hw, hh, _), (vx, vy, vw, vh, _)] =
            corner_arms(Corner::Nw, (0.0, 0.0, 1000.0, 800.0), 4.0, 33.0, 5.0, 6.0, 4.0);
        // horizontal arm: outer corner pushed to (-4,-4), inner edge at y = -4+5 = 1 = bw.
        assert_eq!((hx, hy, hw, hh), (-4.0, -4.0, 33.0, 5.0));
        assert_eq!(hy + hh, 1.0, "inner edge lands on the thin line (+bw)");
        // vertical arm: inner edge at x = -4+5 = 1 = bw.
        assert_eq!((vx, vy, vw, vh), (-4.0, -4.0, 5.0, 33.0));
        assert_eq!(vx + vw, 1.0, "inner edge lands on the thin line (+bw)");
    }

    #[test]
    fn corner_arms_se_mirrors_to_the_far_corner() {
        // SE corner should sit at the bottom-right, pushed out past (1000,800).
        let [(hx, hy, hw, hh, _), (vx, vy, vw, vh, _)] =
            corner_arms(Corner::Se, (0.0, 0.0, 1000.0, 800.0), 4.0, 33.0, 5.0, 6.0, 4.0);
        // horizontal arm spans left from the pushed corner (1004): x in [1004-33, 1004].
        assert_eq!((hx, hy, hw, hh), (1004.0 - 33.0, 800.0 - 1.0, 33.0, 5.0));
        assert_eq!(hy, 799.0, "inner edge at bottom - bw");
        assert_eq!((vx, vy, vw, vh), (1004.0 - 5.0, 804.0 - 33.0, 5.0, 33.0));
        assert_eq!(vx, 999.0, "inner edge at right - bw");
    }
}
