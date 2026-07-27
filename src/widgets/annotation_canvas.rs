//! The image-annotation interaction canvas: a transparent leaf `Widget` layered OVER
//! the preview's [`crate::widgets::ZoomPan`] that owns pointer handling for the
//! annotation editor — click-to-select, drag-to-move, drag-handle-to-resize,
//! drag-endpoint (arrow), draw-a-new-shape, and right-click. A press over an existing item
//! manipulates it, with two exceptions that BYPASS hit-testing entirely: Ctrl held with a draw
//! tool armed flips that precedence so a new shape can be drawn on TOP of existing ones
//! ([`force_new_draw`], DRAGON-339), and the two whole-canvas tools — the eraser (DRAGON-338)
//! and the PENCIL (DRAGON-346) — never select anything at all. A pencil press is ink, full
//! stop: selection belongs to [`Tool::Pointer`].
//!
//! # Selection (DRAGON-341)
//! The selection is a SET (`selection`, in selection order) rather than one id. Its LAST member
//! is the PRIMARY: the only one wearing resize handles, so every [`Grab`] still edits exactly one
//! item and the whole resize machinery is untouched by multi-select. [`Tool::Pointer`] is the
//! pure-selection mode that makes the set reachable — Ctrl/Shift-click toggles members
//! ([`additive_select`]), an empty-canvas drag rubber-bands ([`Pending::Band`] →
//! [`AnnotEvent::BoxSelect`]), and dragging any selected body emits the ordinary
//! [`Grab::Move`], which the app applies to the WHOLE set. Pointer mode is also the only state
//! in which freehand PEN groups are body-selectable (see `pen_selectable`): ink covers a picture
//! and must not swallow clicks meant for what is under it.
//!
//! # Why a sibling overlay (not a child of ZoomPan)
//! ZoomPan transforms its content VISUALLY in `draw` but passes RAW (untransformed)
//! cursor coordinates to its children. A hit-testing widget nested inside it would be
//! drawn zoomed while seeing un-zoomed coordinates — a mismatch the moment you zoom.
//! Instead this widget is stacked as a SIBLING over the ZoomPan (same bounds, since
//! iced's `stack` sizes to its first child and lays the rest inside it) and applies the
//! SAME transform itself ([`CanvasMap`], which inverts ZoomPan's `transform` then the
//! centered fit placement). So its hit-testing and the picture stay in lock-step at any
//! zoom/pan. This widget draws the COMMITTED shapes as TRUE VECTOR geometry (DRAGON-324) —
//! recomputed each frame at the current zoom, so they stay razor-crisp at any magnification
//! (no preview-resolution raster to blur) and flicker-free (a vector redraw churns no
//! texture atlas) — PLUS the editing CHROME (selection box + handles) on top. Both are
//! clipped to ZoomPan's content rect (no bleed past the flush scrollbars) and never baked;
//! the full-resolution bake rasterizes the same scene independently.
//!
//! # Pass-through
//! The widget captures pointer events ONLY when it is actually acting (a drawing tool is
//! down, or a Select gesture grabbed an item/handle). Idle hovers, wheel scroll, and
//! empty-space presses while the pan tool / Alt is engaged fall through to the ZoomPan
//! below, so zoom + pan keep working.
//!
//! The widget is app-agnostic: it emits [`AnnotEvent`]s through one `on_event` closure
//! (all points already mapped to IMAGE SOURCE pixels), and the app maps each to its own
//! message. Model types (the persisted scene, undo, raster, bake) live in
//! `crate::app::preview::annotate`; this module owns only the interaction + the pure
//! coordinate mapping.

use crate::geometry::{Corner, Edge, GlobalRect};
use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::time::Instant;
use cosmic::iced::core::widget::{tree, Tree};
use cosmic::iced::core::{
    Border, Clipboard, Color, Event, Layout, Length, Point, Rectangle, Shadow, Shell, Size,
    Vector, keyboard, mouse,
};
use cosmic::widget::Widget;
use cosmic::widget::canvas::{self, LineCap, LineJoin, Path, Stroke};

/// Screen-px hit radius for a corner / endpoint handle (matches region_selection's
/// `HANDLE_GRAB`).
const HANDLE_GRAB: f32 = 16.0;
/// Screen-px hit tolerance to the body of an arrow (its shaft).
const ARROW_GRAB: f32 = 10.0;
/// Screen-px hit tolerance to a pen STROKE (DRAGON-338) — tighter than the arrow's, since a
/// scribble can wander anywhere and shouldn't swallow clicks meant for what's behind it.
const PEN_GRAB: f32 = 6.0;
/// Movement (screen px) before a press becomes a real drag rather than a click.
const NEW_THRESHOLD: f32 = 4.0;
/// Screen-px padding added around an item's OUTER drawn extent (geometry + stroke/2) for
/// the selection chrome, handle offsets, and the body/select hit area — so the dashed box
/// and circular handles float a clear gap beyond the visible stroke, and the whole padded
/// region selects. The total offset from geometry is `HIT_PAD + stroke_w/2`.
const HIT_PAD: f32 = 8.0;

/// The drawn size of a resize/endpoint handle square (screen px).
const HANDLE_SIZE: f32 = 9.0;
/// Arrow-head barb length = `ARROW_HEAD_FRAC` of the shaft (grows with the line), floored at
/// `ARROW_HEAD_MIN_FRAC` of `ARROW_HEAD_MAX` so a short arrow still shows a substantial head, and
/// capped at `ARROW_HEAD_MAX` (SOURCE px) so a long line doesn't get a huge head — INDEPENDENT of
/// stroke width (that only changes barb THICKNESS). Kept in sync with the bake
/// (`crate::app::preview::annotate::draw_arrow`, same 0.125 / 0.30·53 / 53 values).
const ARROW_HEAD_FRAC: f32 = 0.125;
const ARROW_HEAD_MAX: f32 = 53.0;
const ARROW_HEAD_MIN_FRAC: f32 = 0.40;
/// Arrows render this many SOURCE px THICKER than the set stroke width, so an arrow reads as bolder
/// than a same-width box (mirrored in the bake, `annotate::draw_arrow`).
const ARROW_STROKE_BONUS: f32 = 2.0;
/// Dash + gap lengths for the selection outline (screen px).
const DASH: f32 = 6.0;
const DASH_GAP: f32 = 4.0;

/// Which annotation DRAW tool is active. Kept here (not in the app) so this widget stays
/// app-agnostic; the app holds the current tool as `Option<Tool>` where `None` is the
/// NEUTRAL (no-draw) default — select/move/resize of existing items works in either state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    /// PURE SELECTION (DRAGON-341): draws nothing at all. A click picks the topmost item of ANY
    /// kind, Ctrl/Shift-click toggles items into a MULTI-selection, a drag on empty canvas
    /// rubber-bands everything it touches, and dragging a selected item's body moves the WHOLE
    /// selection. It is also the ONLY tool under which freehand PEN groups are click-selectable
    /// (see [`AnnotationCanvas::pen_selectable`]).
    Pointer,
    /// Draw arrows.
    Arrow,
    /// Place a STEP MARKER (DRAGON-340): an auto-numbered disc + ring. Geometry is a square rect
    /// like a Box, except it is FORCED 1:1 on placement and every resize — the one annotation
    /// that ignores free aspect — and it is PLACED by a click rather than dragged out (see
    /// [`Tool::click_places`]). The ordinal is never stored: it is DERIVED from the badge's
    /// position among the scene's badges (`crate::app::preview::annotate::badge_numbers`), so
    /// deleting one renumbers the rest with no bookkeeping.
    Badge,
    /// Draw rectangles ("Box").
    Rect,
    /// Draw a MULTIPLY-blended highlighter box — a Box variant, same geometry/interaction.
    Highlight,
    /// Draw a box-highlight — a highlighter FILL plus a box OUTLINE on top (DRAGON-333).
    BoxHighlight,
    /// Draw a spotlight knockout box (DRAGON-329): a Box-geometry region that renders NOTHING
    /// of its own (no stroke/fill) but PUNCHES a hole in the global dim so the underlying image
    /// shows through at full brightness. Same geometry/interaction as a Box.
    Spotlight,
    /// Draw a DESTRUCTIVE pixelate box (block mosaic) — a Box variant.
    Pixelate,
    /// Draw a DESTRUCTIVE blur box — a Box variant.
    Blur,
    /// Draw FREEHAND pen strokes (DRAGON-338): a drag traces a vector polyline at the current
    /// stroke width. Strokes that TOUCH merge into one selectable item (see
    /// `crate::app::preview::annotate::merge_connected_pens`).
    Pen,
    /// ERASE pen strokes (DRAGON-338). Not a draw tool at all: press-and-drag MARKS every pen
    /// item the eraser passes over (dimmed to ERASE_PREVIEW_ALPHA — the pending-deletion preview) and
    /// RELEASE deletes them all in one undo entry. Presses never select/move an item, so the
    /// whole canvas erases while it's armed.
    Eraser,
}

impl Tool {
    /// A stable string form for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Pointer => "pointer",
            Tool::Arrow => "arrow",
            Tool::Badge => "badge",
            Tool::Rect => "box",
            Tool::Highlight => "highlight",
            Tool::BoxHighlight => "box-highlight",
            Tool::Spotlight => "spotlight",
            Tool::Pixelate => "pixelate",
            Tool::Blur => "blur",
            Tool::Pen => "pen",
            Tool::Eraser => "eraser",
        }
    }
    /// Parse the persisted string; unknown values yield `None` (neutral).
    pub fn from_str(s: &str) -> Option<Tool> {
        match s {
            "pointer" => Some(Tool::Pointer),
            "arrow" => Some(Tool::Arrow),
            "badge" => Some(Tool::Badge),
            "box" => Some(Tool::Rect),
            "highlight" => Some(Tool::Highlight),
            "box-highlight" => Some(Tool::BoxHighlight),
            "spotlight" => Some(Tool::Spotlight),
            "pixelate" => Some(Tool::Pixelate),
            "blur" => Some(Tool::Blur),
            "pen" => Some(Tool::Pen),
            "eraser" => Some(Tool::Eraser),
            _ => None,
        }
    }

    /// Whether this tool ERASES rather than draws — its press never selects, moves or resizes
    /// an existing item (it always starts an erase sweep instead). Since DRAGON-346 the PENCIL
    /// shares that "the press never selects" property (its press is always ink), so the two are
    /// the whole-canvas tools; only the eraser's press ALSO skips the drag threshold.
    pub fn is_eraser(self) -> bool {
        matches!(self, Tool::Eraser)
    }

    /// Whether this is the pure-SELECTION pointer (DRAGON-341) — the tool that never creates
    /// anything and owns multi-select / rubber-band / group-move.
    pub fn is_pointer(self) -> bool {
        matches!(self, Tool::Pointer)
    }

    /// Whether this tool CREATES geometry on a drag. The two non-creating tools are the
    /// [`Tool::Eraser`] (it removes) and the [`Tool::Pointer`] (it only selects) — everything
    /// that keys off "a drag will draw something" (the crosshair cursor, the Ctrl
    /// draw-over-items override) must ask this rather than `tool.is_some()`.
    pub fn draws(self) -> bool {
        !matches!(self, Tool::Eraser | Tool::Pointer)
    }

    /// Whether a plain CLICK (a press that never crosses the drag threshold) is already a
    /// COMPLETE gesture for this tool — the canvas then runs the whole `DrawBegin` +
    /// `GestureEnd` pair on release instead of letting the press pass through as a bare click.
    /// Two tools place rather than drag out a region:
    ///   * the PENCIL, whose tap is a deliberate round DOT (DRAGON-342);
    ///   * the STEP MARKER (`Tool::Badge`), dropped at a point and sized from the last one
    ///     placed or resized rather than from a rubber-band.
    ///
    /// Every other tool still needs a real drag to make a shape, so a stray click on the canvas
    /// stays a stray click. Pure — unit-tested.
    pub fn click_places(self) -> bool {
        matches!(self, Tool::Pen | Tool::Badge)
    }
}

/// Whether a left press must start a BRAND-NEW shape, ignoring whatever item sits under the
/// cursor (DRAGON-339). Normally a press over an existing item selects/moves/resizes it, so
/// there is no way to draw ON TOP of one; holding Ctrl with a draw tool armed flips that
/// precedence — the press draws, the item below is left alone. With no tool armed (the neutral
/// pointer) Ctrl means nothing, so manipulation stays the behavior. The non-DRAWING tools —
/// the eraser and the DRAGON-341 pointer — have nothing to force, so Ctrl means nothing there
/// either (in pointer mode Ctrl-click is multi-select instead). Pure — unit-tested.
pub fn force_new_draw(tool: Option<Tool>, ctrl: bool) -> Option<Tool> {
    if ctrl { tool.filter(|t| t.draws()) } else { None }
}

/// The tool a left press must draw with WITHOUT ever consulting what sits under the cursor —
/// `None` when the press should hit-test normally (select / move / resize). Two cases bypass
/// hit-testing, and both behave exactly like an empty-canvas press (deselect, then arm a LAZY
/// draw):
///   * the PENCIL, always (DRAGON-346): a pencil press is ink, full stop. Drawing over a shape
///     used to select that shape — chrome and all — while the stroke landed, which reads as a
///     manipulation that isn't happening. Selection belongs to [`Tool::Pointer`].
///   * any other DRAWING tool while Ctrl is held ([`force_new_draw`], DRAGON-339), so a new
///     shape can be laid on TOP of existing ones.
///
/// The eraser is not here: its press is handled earlier still (it captures immediately, with no
/// drag threshold). Pure — unit-tested.
pub fn draw_bypassing_items(tool: Option<Tool>, ctrl: bool) -> Option<Tool> {
    match tool {
        Some(Tool::Pen) => Some(Tool::Pen),
        other => force_new_draw(other, ctrl),
    }
}

/// Whether the armed tool owns the WHOLE canvas for CURSOR purposes: one crosshair everywhere
/// over the content, item bodies and resize handles included. True exactly when the next press
/// will not manipulate whatever is under the pointer — the eraser (which sweeps) or any press
/// that bypasses hit-testing ([`draw_bypassing_items`]). The cursor must promise what the press
/// will actually do, so this is derived from the press rule rather than restated. Pure —
/// unit-tested.
pub fn whole_canvas_crosshair(tool: Option<Tool>, ctrl: bool) -> bool {
    tool.is_some_and(Tool::is_eraser) || draw_bypassing_items(tool, ctrl).is_some()
}

/// Whether a press with `tool` armed and `ctrl`/`shift` held is an ADDITIVE selection click
/// (DRAGON-341): toggle the hit item into the multi-selection, or extend a rubber band, instead
/// of replacing the selection. ONLY the pointer tool multi-selects — with a draw tool armed Ctrl
/// still means "draw over what's under the cursor" ([`force_new_draw`]), so the two can never
/// both claim the same press. Pure — unit-tested.
pub fn additive_select(tool: Option<Tool>, ctrl: bool, shift: bool) -> bool {
    tool.is_some_and(Tool::is_pointer) && (ctrl || shift)
}

/// How the canvas RENDERS an item. Box/Arrow draw as vector geometry in [`draw_shapes`]; the
/// region effects (highlight multiply, pixelate, blur) are rendered by dedicated shader passes
/// UNDER this widget (DRAGON-326/327/328), so the canvas SKIPS drawing them here — it still
/// hit-tests + chromes them (they are ordinary rects). Fixed z: the two blend modes live in
/// separate passes, so destructive → highlight-multiply → box/arrow is the on-screen order
/// regardless of a shape's per-item index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FxKind {
    /// A box or arrow — drawn as vector geometry by this widget.
    #[default]
    None,
    /// A highlight — rendered by the multiply shader pass.
    Highlight,
    /// A box-highlight (DRAGON-333): its highlight FILL renders via the multiply shader pass,
    /// but — unlike the pure effects — its box OUTLINE is STILL drawn as vector geometry by
    /// this widget (so [`draw_shapes`] draws the outline yet skips the fill).
    BoxHighlight,
    /// A spotlight knockout (DRAGON-329): contributes a knockout rect to the global dim shader
    /// pass but renders NO geometry of its own — [`draw_shapes`] skips it (invisible when
    /// unselected; only its selection chrome shows). Hit-tests as an ordinary rect.
    Spotlight,
    /// A pixelate redaction — rendered by the region shader pass.
    Pixelate,
    /// A blur redaction — rendered by the region shader pass.
    Blur,
}

/// What a Select-tool drag is manipulating on the selected item. Emitted in
/// [`AnnotEvent::GrabBegin`] so the app can apply the right geometry edit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grab {
    /// Translate the whole item.
    Move,
    /// Drag a box corner (the opposite corner stays put).
    Corner(Corner),
    /// Drag a box edge.
    Edge(Edge),
    /// Drag arrow endpoint A (the tail).
    ArrowA,
    /// Drag arrow endpoint B (the head).
    ArrowB,
}

/// What a hit-test landed on: a resize handle (with its grab), or the item body.
#[derive(Clone, Copy)]
enum HitKind {
    Resize(Grab),
    Body,
}

/// One item's GEOMETRY in image SOURCE pixels — enough for the widget to hit-test and to
/// draw selection chrome. Appearance (color/stroke/fill) is the raster's job.
#[derive(Clone, PartialEq, Debug)]
pub enum ItemKind {
    /// A rectangle at `(x, y)` sized `w`×`h` (source px; may be un-normalized during a draw).
    Rect { x: f32, y: f32, w: f32, h: f32 },
    /// An arrow from A `(ax, ay)` to B `(bx, by)` (source px).
    Arrow { ax: f32, ay: f32, bx: f32, by: f32 },
    /// A freehand PEN item (DRAGON-338): one or more polylines (source px) that all belong to
    /// the SAME selectable unit — every stroke the user drew that touches another one in the
    /// group. Selection chrome + resize handles sit on the group's bounding box; the BODY
    /// hit-test follows the strokes themselves (a scribble's bbox is mostly empty space).
    ///
    /// The points are the SMOOTHED centerline and `pressure` the parallel per-point speed
    /// signal (DRAGON-342) — together they build the pseudo-pressure width profile this widget
    /// FILLS as a ribbon (`crate::pen_stroke::stroke_fill_polygons`), the identical geometry the
    /// full-res bake rasterizes. `pressure[i]` belongs to `paths[i]`; an empty entry reads as
    /// neutral pressure.
    Path { paths: Vec<Vec<(f32, f32)>>, pressure: Vec<Vec<f32>> },
}

/// A hit-testable, DRAWABLE item: a stable id, its geometry, its stroke width (SOURCE px —
/// the stroke half-width offsets the selection chrome so it clears the VISIBLE stroke), and
/// the appearance the canvas draws it with. The shapes are drawn as TRUE VECTOR geometry by
/// this widget (DRAGON-324), so they stay crisp at ANY zoom instead of sampling a
/// preview-resolution raster; the full-res bake still rasterizes the same scene on demand.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: u64,
    pub kind: ItemKind,
    pub stroke_w: f32,
    /// The stroke color for a box outline / arrow. Unused by the region-effect kinds (they
    /// render through shader passes, not this widget).
    pub color: Color,
    /// A box's optional interior fill (straight alpha).
    pub fill: Option<Color>,
    /// How this item renders: `None` = box/arrow drawn here as vectors; the region-effect
    /// kinds are drawn by shader passes and SKIPPED by [`draw_shapes`].
    pub fx: FxKind,
    /// The shared corner curve as an ABSOLUTE radius (SOURCE px): rounds the box corners and
    /// softens the arrow caps/joins (round when > 0). Scaled to screen px at draw time.
    pub curve_radius: f32,
    /// `Some(n)` marks a [`ItemKind::Rect`] item as a SEQUENCE BADGE (DRAGON-340) carrying
    /// ordinal `n`: [`draw_shapes`] renders it as a disc + numeral + ring
    /// (`crate::badge`) instead of a box outline, and `stroke_w` becomes the RING weight.
    /// A render flag, exactly like [`Self::fx`] — the badge hit-tests, chromes and resizes as
    /// the ordinary square rect it is, so nothing else in this widget needs to know about it.
    /// The ordinal is derived scene-side and re-stamped on every view build, so it is never
    /// stale; the numeral's ink colour is derived here from [`Self::color`].
    pub badge: Option<u32>,
}

/// A pointer gesture the canvas publishes — every point is in IMAGE SOURCE pixels
/// (already mapped through [`CanvasMap`]), except [`Self::Menu`] which carries a
/// widget-LOCAL point for placing the context-menu popover.
#[derive(Clone, Copy, Debug)]
pub enum AnnotEvent {
    /// Plain click: select `Some(id)` (topmost hit) or deselect (`None`). REPLACES the
    /// selection.
    Select(Option<u64>),
    /// Ctrl/Shift-click in POINTER mode (DRAGON-341): TOGGLE `id` in the multi-selection,
    /// keeping the rest. A newly added id becomes the PRIMARY (the one wearing resize handles).
    SelectToggle(u64),
    /// A pointer-mode RUBBER BAND finished (DRAGON-341): select every item the band
    /// `(x0, y0)`–`(x1, y1)` (image source px, un-normalized) TOUCHES. `additive` (Ctrl/Shift
    /// held at press) keeps the existing selection and adds to it.
    BoxSelect(f32, f32, f32, f32, bool),
    /// Begin drawing a brand-new shape of `tool` from image point `(x, y)`.
    DrawBegin(Tool, f32, f32),
    /// Begin manipulating the selected item (grab kind + press image point).
    GrabBegin(Grab, f32, f32),
    /// Drag update to image point `(x, y)`.
    GestureTo(f32, f32),
    /// The active gesture committed (pointer released after a real drag).
    GestureEnd,
    /// Right-click context menu, anchored at widget-LOCAL `(x, y)`.
    Menu(f32, f32),
}

/// Pure mapping between canvas (widget-local screen px) and image SOURCE pixels.
///
/// Inverts [`ZoomPan::transform`] — `q' = zoom*q + (c*(1-zoom) + pan)`, `c` = bounds
/// centre — then the centered fit placement (`disp` is the image's on-screen size at
/// zoom 1, centred in the bounds; `source` its pixel dims). Unit-tested; the correctness
/// lynchpin for every gesture.
#[derive(Clone, Copy, Debug)]
pub struct CanvasMap {
    /// Widget bounds size (local origin at 0,0), screen px.
    pub bounds: (f32, f32),
    pub zoom: f32,
    pub pan: (f32, f32),
    /// Image on-screen display size at zoom 1 (dw, dh).
    pub disp: (f32, f32),
    /// Image source pixel dims (fw, fh).
    pub source: (f32, f32),
}

impl CanvasMap {
    fn center(&self) -> (f32, f32) {
        (self.bounds.0 / 2.0, self.bounds.1 / 2.0)
    }
    fn translate(&self) -> (f32, f32) {
        let c = self.center();
        (c.0 * (1.0 - self.zoom) + self.pan.0, c.1 * (1.0 - self.zoom) + self.pan.1)
    }
    /// Image origin (top-left) in local zoom-1 content coords.
    fn origin(&self) -> (f32, f32) {
        let c = self.center();
        (c.0 - self.disp.0 / 2.0, c.1 - self.disp.1 / 2.0)
    }

    /// Widget-local screen point → image source pixel.
    pub fn to_image(self, p: (f32, f32)) -> (f32, f32) {
        let t = self.translate();
        let z = if self.zoom.abs() < f32::EPSILON { 1.0 } else { self.zoom };
        let q = ((p.0 - t.0) / z, (p.1 - t.1) / z);
        let o = self.origin();
        let sx = if self.disp.0 > 0.0 { self.source.0 / self.disp.0 } else { 0.0 };
        let sy = if self.disp.1 > 0.0 { self.source.1 / self.disp.1 } else { 0.0 };
        ((q.0 - o.0) * sx, (q.1 - o.1) * sy)
    }

    /// Image source pixel → widget-local screen point.
    pub fn to_canvas(self, img: (f32, f32)) -> (f32, f32) {
        let t = self.translate();
        let o = self.origin();
        let dx = if self.source.0 > 0.0 { self.disp.0 / self.source.0 } else { 0.0 };
        let dy = if self.source.1 > 0.0 { self.disp.1 / self.source.1 } else { 0.0 };
        let q = (o.0 + img.0 * dx, o.1 + img.1 * dy);
        (self.zoom * q.0 + t.0, self.zoom * q.1 + t.1)
    }

    /// The scale from image SOURCE px to on-screen px (`disp/source · zoom`) — so a
    /// source-space stroke width maps to its rendered screen thickness. Aspect is
    /// preserved, so the x factor suffices.
    pub fn img_to_screen_scale(self) -> f32 {
        let s = if self.source.0 > 0.0 { self.disp.0 / self.source.0 } else { 1.0 };
        s * self.zoom
    }
}

#[derive(Default, Clone, Copy)]
enum Pending {
    #[default]
    None,
    /// A drawing tool is down (draw a new shape on drag).
    Draw(Tool),
    /// Pressed an item body: a click selects (already emitted), a drag moves it.
    Move,
    /// Pressed a handle of the selected item: a drag resizes / moves the endpoint.
    Resize(Grab),
    /// POINTER mode, pressed empty canvas (DRAGON-341): a drag rubber-bands a selection.
    /// `additive` (Ctrl/Shift at press) keeps whatever was already selected.
    Band { additive: bool },
}

#[derive(Default)]
struct State {
    pending: Pending,
    /// Press point in widget-LOCAL screen px (for the click/drag threshold).
    press_screen: (f32, f32),
    /// Press point in IMAGE source px (the gesture anchor).
    press_img: (f32, f32),
    /// The rubber band's live far corner in widget-LOCAL screen px (DRAGON-341), meaningful
    /// only while `pending` is [`Pending::Band`] and the press has moved.
    band_to: (f32, f32),
    /// Whether the press has moved past the click threshold.
    moved: bool,
    /// Whether a `DrawBegin` / `GrabBegin` has been emitted for this gesture yet.
    begun: bool,
    /// Latest modifiers (Alt lets an empty press fall through to the ZoomPan pan).
    mods: keyboard::Modifiers,
    /// Whether a MIDDLE-button pan drag is live in the wrapped ZoomPan (DRAGON-347). Tracked
    /// here (never consumed) only so `mouse_interaction` can show the grabbing cursor over
    /// the tool cursors while the button is held.
    mmb_pan: bool,
    /// When the pointer last entered the surface, driving the post-enter cursor re-assert (DRAGON-331;
    /// see [`crate::widgets::cursor_reassert`]). `None` after the pointer leaves.
    entered_at: Option<Instant>,
}


/// The annotation interaction canvas widget. It WRAPS the preview's [`ZoomPan`] as its
/// child (`content`): iced's `stack` does NOT reliably propagate an Ignored mouse event
/// from a top sibling down to a lower one, so a sibling-over approach left ZoomPan's pan +
/// scrollbar handlers unreachable. As the OWNER, this widget explicitly FORWARDS every
/// event it doesn't consume to the wrapped ZoomPan (`self.content.update(...)`), and only
/// intercepts+captures the gestures the annotation layer genuinely handles.
pub struct AnnotationCanvas<'a, Msg> {
    /// The wrapped ZoomPan (draws the image + shape raster, owns pan/zoom/scrollbars).
    content: cosmic::Element<'a, Msg>,
    items: Vec<Item>,
    /// The current MULTI-selection (DRAGON-341), in selection order — the LAST id is the
    /// PRIMARY (the only one wearing resize handles; see [`Self::primary`]). Empty = nothing
    /// selected. A single-item selection behaves exactly as the old `Option<u64>` did.
    selection: Vec<u64>,
    /// The active draw tool, or `None` for neutral (no drawing on empty-canvas drag).
    tool: Option<Tool>,
    zoom: f32,
    pan: (f32, f32),
    disp: (f32, f32),
    source: (f32, f32),
    /// The pan tool (grabby hand) is active — a press then belongs to the ZoomPan.
    pan_mode: bool,
    accent: Color,
    on_event: Box<dyn Fn(AnnotEvent) -> Msg>,
}

impl<'a, Msg> AnnotationCanvas<'a, Msg> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        items: Vec<Item>,
        selection: Vec<u64>,
        tool: Option<Tool>,
        zoom: f32,
        pan: (f32, f32),
        disp: (f32, f32),
        source: (f32, f32),
        pan_mode: bool,
        accent: Color,
        on_event: impl Fn(AnnotEvent) -> Msg + 'static,
    ) -> Self {
        Self {
            content: content.into(),
            items,
            selection,
            tool,
            zoom,
            pan,
            disp,
            source,
            pan_mode,
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
        }
    }

    /// The content rectangle (global) the wrapped ZoomPan draws + clips to — the full
    /// bounds MINUS the scrollbar strips. The SINGLE source shared by the strip test, the
    /// chrome clip, and (indirectly, via the same `SCROLLBAR_TOTAL`) the pan-bounds clamp,
    /// so the scrollbars, content edge, and pan limit all agree.
    fn content_rect(&self, bounds: Rectangle) -> Rectangle {
        crate::widgets::zoom_pan::content_bounds(self.disp, self.zoom, bounds)
    }

    /// Whether a widget-LOCAL point falls OUTSIDE the content rectangle (i.e. in a scrollbar
    /// strip). Presses there belong to the ZoomPan — forward them, and never draw there.
    fn in_scrollbar_strip(&self, bounds: Rectangle, local: (f32, f32)) -> bool {
        let cb = self.content_rect(bounds);
        local.0 >= cb.width || local.1 >= cb.height
    }

    /// The PRIMARY selected id — the last one added (DRAGON-341). It is the only member of a
    /// multi-selection that wears resize handles, so the existing single-item [`Grab`] machinery
    /// keeps working unchanged: a resize drag always edits exactly one item.
    fn primary(&self) -> Option<u64> {
        self.selection.last().copied()
    }

    /// Whether `id` is part of the current selection.
    fn is_selected(&self, id: u64) -> bool {
        self.selection.contains(&id)
    }

    /// The PRIMARY selected item, if any (the one the chrome hangs handles on).
    fn selected_item(&self) -> Option<&Item> {
        self.primary().and_then(|id| self.items.iter().find(|i| i.id == id))
    }

    /// Whether freehand PEN groups may be picked up by a BODY click right now (DRAGON-341):
    /// only in POINTER mode. Ink is scribbled all over a picture and would otherwise swallow
    /// clicks meant for the shapes (and the drawing) beneath it, so outside the pointer tool a
    /// pen group is inert. (With the PENCIL armed the press never reaches hit-testing at all —
    /// it always draws, DRAGON-346 — so this gating matters for the shape tools and the neutral
    /// state, where a click over ink must fall through to what is under it.) Nor can a pen be
    /// selected WHILE another
    /// tool is armed: arming a non-pointer tool prunes pen ids out of the selection
    /// (`EditState::drop_pen_selection`), so this widget never receives one to chrome.
    fn pen_selectable(&self) -> bool {
        self.tool.is_some_and(Tool::is_pointer)
    }

    /// Hit-test in precedence order: the PRIMARY selected item's resize HANDLES first (they
    /// exist ONLY for it, drawn HIT_PAD outside it), then ANY item's BODY top-most first (a body
    /// press selects + moves), then empty. So an unselected item has no grabbable handles — you
    /// select it (body-click) to reveal them. In a MULTI-selection (DRAGON-341) only the primary
    /// carries handles, so a resize still edits exactly one item.
    fn hit_at(&self, map: &CanvasMap, p: (f32, f32)) -> Option<(u64, HitKind)> {
        let g = (p.0 as i32, p.1 as i32);
        // 1. The SELECTED item's HANDLES win over everything (top precedence). Handles are
        //    the 8 drawn circles (corners + edge midpoints) on the chrome rect / the arrow's
        //    two endpoint nodes — ONLY those resize, NOT the whole perimeter (the rest of the
        //    stroke moves, via Body in step 2).
        if let Some(sel) = self.selected_item() {
            // A pen group's handles sit on its BOUNDING BOX, exactly like a rect's.
            let sel_kind = match &sel.kind {
                ItemKind::Path { paths, .. } => {
                    let (x, y, w, h) = path_bounds(paths);
                    ItemKind::Rect { x, y, w, h }
                }
                other => other.clone(),
            };
            match sel_kind {
                ItemKind::Rect { x, y, w, h } => {
                    let r = box_chrome_rect(map, x, y, w, h, sel.stroke_w);
                    if let Some(c) = r.corner_at(g, HANDLE_GRAB) {
                        return Some((sel.id, HitKind::Resize(Grab::Corner(c))));
                    }
                    // Edge-MIDPOINT handle circles resize that side (not the whole edge band).
                    let (mx, my) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                    let near = |cx: i32, cy: i32| {
                        ((g.0 - cx) as f32).hypot((g.1 - cy) as f32) <= HANDLE_GRAB
                    };
                    if near(mx, r.top) {
                        return Some((sel.id, HitKind::Resize(Grab::Edge(Edge::N))));
                    }
                    if near(mx, r.bottom) {
                        return Some((sel.id, HitKind::Resize(Grab::Edge(Edge::S))));
                    }
                    if near(r.left, my) {
                        return Some((sel.id, HitKind::Resize(Grab::Edge(Edge::W))));
                    }
                    if near(r.right, my) {
                        return Some((sel.id, HitKind::Resize(Grab::Edge(Edge::E))));
                    }
                }
                ItemKind::Arrow { ax, ay, bx, by } => {
                    let (an, bn) = arrow_nodes(map, ax, ay, bx, by, sel.stroke_w);
                    if (p.0 - an.0).hypot(p.1 - an.1) <= HANDLE_GRAB {
                        return Some((sel.id, HitKind::Resize(Grab::ArrowA)));
                    }
                    if (p.0 - bn.0).hypot(p.1 - bn.1) <= HANDLE_GRAB {
                        return Some((sel.id, HitKind::Resize(Grab::ArrowB)));
                    }
                }
                // Mapped to its bounding Rect above — unreachable.
                ItemKind::Path { .. } => {}
            }
        }
        // 2. Any item's BODY, top-most (reverse z-order) first. The SELECTED item keeps the
        //    padded region (geometry + stroke/2 + HIT_PAD — the same rect its chrome/handles sit
        //    on, so its whole padded body moves); every OTHER item uses the STRICT drawn bounds
        //    (geometry + stroke/2, NO HIT_PAD) so a click just outside a shape lands in the gap
        //    instead of grabbing it. Clicking ON the visible stroke still selects. `<=` so the
        //    outer boundary counts too.
        for item in self.items.iter().rev() {
            let selected = self.is_selected(item.id);
            match &item.kind {
                &ItemKind::Rect { x, y, w, h } => {
                    let r = if selected {
                        box_chrome_rect(map, x, y, w, h, item.stroke_w)
                    } else {
                        box_drawn_rect(map, x, y, w, h, item.stroke_w)
                    };
                    if g.0 >= r.left && g.0 <= r.right && g.1 >= r.top && g.1 <= r.bottom {
                        return Some((item.id, HitKind::Body));
                    }
                }
                &ItemKind::Arrow { ax, ay, bx, by } => {
                    let a = map.to_canvas((ax, ay));
                    let b = map.to_canvas((bx, by));
                    // Shaft grab tolerance from the OUTER drawn stroke edge: ARROW_GRAB + stroke/2,
                    // plus HIT_PAD ONLY for the selected arrow (breathing room), strict otherwise.
                    let pad = if selected { HIT_PAD } else { 0.0 };
                    let tol = ARROW_GRAB + pad + item.stroke_w * map.img_to_screen_scale() * 0.5;
                    if point_near_segment(p, a, b) <= tol {
                        return Some((item.id, HitKind::Body));
                    }
                }
                // A pen group hits along its STROKES (never its mostly-empty bbox), so a click
                // in the gap between two scribbles falls through to whatever is under it — and
                // only in POINTER mode at all (DRAGON-341, see `pen_selectable`). The reach
                // rides `max_width` — the widest a pressure-swelled stretch draws — so a heavy
                // loop is grabbable everywhere it is inked.
                ItemKind::Path { paths, .. } => {
                    if !self.pen_selectable() {
                        continue;
                    }
                    let pad = if selected { HIT_PAD } else { 0.0 };
                    let tol = PEN_GRAB
                        + pad
                        + crate::pen_stroke::max_width(item.stroke_w)
                            * map.img_to_screen_scale()
                            * 0.5;
                    if path_distance(map, paths, p) <= tol {
                        return Some((item.id, HitKind::Body));
                    }
                }
            }
        }
        None
    }

    /// The top-most item under `p` (for the right-click menu).
    fn topmost_at(&self, map: &CanvasMap, p: (f32, f32)) -> Option<u64> {
        self.hit_at(map, p).map(|(id, _)| id)
    }

    fn emit(&self, shell: &mut Shell<'_, Msg>, ev: AnnotEvent) {
        shell.publish((self.on_event)(ev));
    }
}

/// The screen-space (widget-local, i32) bounding rect of a box item's raw geometry.
fn box_screen_rect(map: &CanvasMap, x: f32, y: f32, w: f32, h: f32) -> GlobalRect {
    let a = map.to_canvas((x, y));
    let b = map.to_canvas((x + w, y + h));
    GlobalRect::new(a.0 as i32, a.1 as i32, b.0 as i32, b.1 as i32).normalize()
}

/// The screen-space rect a box's chrome + handles sit on: the raw box grown OUTWARD by
/// [`HIT_PAD`] PLUS half the (screen-space) stroke width, so the dashed outline + handles
/// clear the VISIBLE stroke (which straddles the geometry by ~stroke/2) with a true
/// ~HIT_PAD gap all around — and the same rect is hit-tested, so you grab what you see.
fn box_chrome_rect(map: &CanvasMap, x: f32, y: f32, w: f32, h: f32, stroke_src: f32) -> GlobalRect {
    let r = box_screen_rect(map, x, y, w, h);
    let pad = (HIT_PAD + stroke_src * map.img_to_screen_scale() * 0.5).round() as i32;
    GlobalRect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad)
}

/// The STRICT drawn bounds of a box item: geometry grown ONLY by half the (screen-space)
/// stroke width — the outer edge of the visible stroke, with NO [`HIT_PAD`]. This is the hit
/// region for an UNSELECTED item, so clicking just outside a shape's stroke lands in the gap
/// (deselects / draws) instead of grabbing it; only the SELECTED item gets the padded
/// [`box_chrome_rect`] (breathing room for its handles + body move).
fn box_drawn_rect(map: &CanvasMap, x: f32, y: f32, w: f32, h: f32, stroke_src: f32) -> GlobalRect {
    let r = box_screen_rect(map, x, y, w, h);
    let pad = (stroke_src * map.img_to_screen_scale() * 0.5).round() as i32;
    GlobalRect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad)
}

/// The screen positions of an arrow's two endpoint NODES (tail, head): each pushed OUTWARD
/// along the arrow axis by `HIT_PAD + stroke/2` (screen), so the tail node clears the tail
/// round cap and the head node sits just BEYOND the arrowhead TIP (in front of the barbs),
/// instead of on them. Dragging a node moves the true endpoint (the constant offset cancels
/// in the relative-drag model — no jump on grab).
fn arrow_nodes(
    map: &CanvasMap,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    stroke_src: f32,
) -> ((f32, f32), (f32, f32)) {
    let a = map.to_canvas((ax, ay));
    let b = map.to_canvas((bx, by));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let off = HIT_PAD + stroke_src * map.img_to_screen_scale() * 0.5;
    ((a.0 - ux * off, a.1 - uy * off), (b.0 + ux * off, b.1 + uy * off))
}

/// The SOURCE-space bounding box `(x, y, w, h)` of a pen group's polylines — the rect its
/// selection chrome + resize handles sit on. An empty group is a zero rect at the origin.
/// Pure — unit-tested.
pub fn path_bounds(paths: &[Vec<(f32, f32)>]) -> (f32, f32, f32, f32) {
    let (mut lo_x, mut lo_y) = (f32::INFINITY, f32::INFINITY);
    let (mut hi_x, mut hi_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in paths.iter().flatten() {
        lo_x = lo_x.min(p.0);
        lo_y = lo_y.min(p.1);
        hi_x = hi_x.max(p.0);
        hi_y = hi_y.max(p.1);
    }
    if !lo_x.is_finite() || !lo_y.is_finite() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (lo_x, lo_y, hi_x - lo_x, hi_y - lo_y)
}

/// The smallest screen-px distance from `p` (widget-local) to any segment of a pen group,
/// mapping the SOURCE-px polylines through `map`. A single-point stroke measures to the point.
fn path_distance(map: &CanvasMap, paths: &[Vec<(f32, f32)>], p: (f32, f32)) -> f32 {
    let mut best = f32::INFINITY;
    for path in paths {
        match path.len() {
            0 => {}
            1 => {
                let a = map.to_canvas(path[0]);
                best = best.min((p.0 - a.0).hypot(p.1 - a.1));
            }
            _ => {
                for w in path.windows(2) {
                    let a = map.to_canvas(w[0]);
                    let b = map.to_canvas(w[1]);
                    best = best.min(point_near_segment(p, a, b));
                }
            }
        }
    }
    best
}

/// Distance from point `p` to the segment `a`–`b` (screen px).
fn point_near_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    let proj = (a.0 + t * dx, a.1 + t * dy);
    (p.0 - proj.0).hypot(p.1 - proj.1)
}

impl<'a, Msg: Clone + 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer>
    for AnnotationCanvas<'a, Msg>
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

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

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn cosmic::iced::core::widget::Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: cosmic::iced::Vector,
    ) -> Option<cosmic::iced::core::overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &cosmic::iced::core::layout::Limits,
    ) -> cosmic::iced::core::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        let state = tree.state.downcast_ref::<State>();
        // The ZoomPan's cursor (pan grab / scrollbar / default) is the fallback for anything
        // the annotation layer doesn't own.
        let child = || {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        };
        // A live middle-button pan (DRAGON-347) outranks every tool cursor — the hand is
        // what the drag is actually doing, exactly like the alt/pan-mode grab.
        if state.mmb_pan {
            return mouse::Interaction::Grabbing;
        }
        // Active annotation gesture cursors.
        match state.pending {
            Pending::Draw(_) => return mouse::Interaction::Crosshair,
            Pending::Move => return mouse::Interaction::Grabbing,
            Pending::Resize(g) => return grab_cursor(g),
            // A live rubber band reads as a marquee draw (DRAGON-341).
            Pending::Band { .. } => return mouse::Interaction::Crosshair,
            _ => {}
        }
        // Fresh-enter re-assert dip (DRAGON-331; see `crate::widgets::cursor_reassert`): we claim the
        // real cursor immediately, then defer to the default for one dip window just before the
        // deadline so the deadline redraw RE-issues `set_cursor` past cosmic-comp's post-enter drop.
        if crate::widgets::cursor_reassert::in_dip(state.entered_at) {
            return child();
        }
        // Panning (pan tool / Alt) or the scrollbar strips belong to the ZoomPan.
        if self.pan_mode || state.mods.alt() {
            return child();
        }
        let Some(p) = cursor.position_over(bounds) else {
            return child();
        };
        let local = (p.x - bounds.x, p.y - bounds.y);
        if self.in_scrollbar_strip(bounds, local) {
            return child();
        }
        // The eraser, the PENCIL (DRAGON-346) and Ctrl + a draw tool (DRAGON-339) each own the
        // WHOLE canvas: their press never manipulates what is under it, so one crosshair holds
        // everywhere over the content — above item bodies and resize handles included. Derived
        // from the press rule itself ([`whole_canvas_crosshair`]) so the cursor can never
        // promise something the press won't do.
        if whole_canvas_crosshair(self.tool, state.mods.control()) {
            return mouse::Interaction::Crosshair;
        }
        // Idle hover: the selected item's handle shows its resize cursor, any item's body
        // the open-hand grab; empty canvas shows the draw crosshair when a draw tool is
        // active, else defer to the ZoomPan.
        let map = self.map(bounds);
        match self.hit_at(&map, local) {
            Some((_, HitKind::Resize(g))) => grab_cursor(g),
            Some((_, HitKind::Body)) => mouse::Interaction::Grab,
            None => {
                // Only a tool that actually DRAWS promises a crosshair over empty canvas; the
                // pointer (DRAGON-341) rubber-bands, which reads as the plain arrow until the
                // drag starts, and the neutral state defers to the ZoomPan.
                if self.tool.is_some_and(Tool::draws) {
                    mouse::Interaction::Crosshair
                } else {
                    child()
                }
            }
        }
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
        let to_local = |p: Point| (p.x - bounds.x, p.y - bounds.y);
        // Cursor RE-ENTRY fix (DRAGON-331): iced applies the pointer icon ONLY on a redraw, and
        // only when the resolved `mouse::Interaction` DIFFERS from the last one it sent
        // (`window::update_mouse` diff-gates `set_cursor`). Over the empty media a draw tool always
        // resolves to `Crosshair`, so leaving and re-entering is `Crosshair -> Crosshair` — no
        // change, so iced never re-issues `set_cursor`. But Wayland resets the compositor pointer
        // to the DEFAULT on the re-enter, leaving the arrow showing under our unchanged cache. The
        // earlier DRAGON-324 attempt redrew on ENTER only (still `Crosshair == cache`, so skipped);
        // a continuous redraw-on-move likewise recomputes the SAME value. The cure is to reset the
        // cache on the way OUT: redrawing on `CursorLeft`/`Unfocused` recomputes `Idle` (pointer
        // gone), so the NEXT enter is a real `Idle -> Crosshair` change that DOES re-issue
        // `set_cursor`. Redraw on enter/focus too so that change is applied immediately.
        // Non-consuming: the event still forwards to the ZoomPan below.
        if matches!(
            event,
            Event::Mouse(mouse::Event::CursorEntered | mouse::Event::CursorLeft)
                | Event::Window(
                    cosmic::iced::core::window::Event::Focused
                        | cosmic::iced::core::window::Event::Unfocused
                )
        ) {
            shell.request_redraw();
        }
        // Drive the post-enter cursor re-assert (DRAGON-331; see `crate::widgets::cursor_reassert`):
        // maintain the entry stamp + schedule the dip/deadline redraws.
        crate::widgets::cursor_reassert::arm(
            &mut tree.state.downcast_mut::<State>().entered_at,
            event,
            shell,
        );
        // Decide whether the ANNOTATION layer consumes this event. If not, it's FORWARDED to
        // the wrapped ZoomPan below so pan/zoom/scrollbars work (iced's `stack` didn't do
        // this reliably as siblings — hence the wrap). Modifiers are both tracked AND
        // forwarded (the ZoomPan tracks Alt too).
        let mut consumed = false;
        {
            let state = tree.state.downcast_mut::<State>();
            // Middle-button pan tracking (DRAGON-347): mirror the ZoomPan's drag lifecycle so
            // the cursor can promise it — press over the content arms it, release anywhere
            // ends it. Never consumed; the event still forwards to the ZoomPan below, which
            // owns the actual panning.
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) => {
                    state.mmb_pan = cursor.is_over(bounds);
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Middle)) => {
                    state.mmb_pan = false;
                }
                _ => {}
            }
            if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
                state.mods = *m;
            } else if self.pan_mode || state.mods.alt() {
                // Pan mode / Alt: forward everything; reset any pending gesture.
                if matches!(event, Event::Mouse(_)) {
                    state.pending = Pending::None;
                }
            } else {
                match event {
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                        if let Some(p) = cursor.position_over(bounds) {
                            let local = to_local(p);
                            if !self.in_scrollbar_strip(bounds, local)
                                && let Some(id) = self.topmost_at(&map, local)
                            {
                                // Right-clicking INSIDE an existing multi-selection keeps it
                                // whole (DRAGON-341) — the menu then acts on everything
                                // selected; anywhere else the press selects what it hit first.
                                if !(self.selection.len() > 1 && self.is_selected(id)) {
                                    self.emit(shell, AnnotEvent::Select(Some(id)));
                                }
                                self.emit(shell, AnnotEvent::Menu(local.0, local.1));
                                shell.capture_event();
                                consumed = true;
                            }
                        }
                    }
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                        if let Some(p) = cursor.position_over(bounds) {
                            let local = to_local(p);
                            if self.in_scrollbar_strip(bounds, local) {
                                // Scrollbar strip → forward (the ZoomPan drags the thumb).
                                state.pending = Pending::None;
                            } else {
                                state.press_screen = local;
                                state.press_img = map.to_image(local);
                                state.moved = false;
                                state.begun = false;
                                if self.tool.is_some_and(Tool::is_eraser) {
                                    // ERASER (DRAGON-338): never selects/moves — the press ITSELF
                                    // starts the sweep (so a plain CLICK on a stroke marks it,
                                    // with no drag threshold to cross) and captures immediately.
                                    self.emit(shell, AnnotEvent::Select(None));
                                    self.emit(
                                        shell,
                                        AnnotEvent::DrawBegin(
                                            Tool::Eraser,
                                            state.press_img.0,
                                            state.press_img.1,
                                        ),
                                    );
                                    state.pending = Pending::Draw(Tool::Eraser);
                                    state.moved = true;
                                    state.begun = true;
                                    shell.capture_event();
                                    consumed = true;
                                } else if let Some(t) =
                                    draw_bypassing_items(self.tool, state.mods.control())
                                {
                                    // The press DRAWS without looking at what is under it: the
                                    // pencil always (DRAGON-346 — a pencil press is ink, full
                                    // stop), or Ctrl + a draw tool (DRAGON-339 — lay a new shape
                                    // on TOP of existing ones). Either way this is exactly the
                                    // empty-canvas path: deselect (so no chrome implies a
                                    // manipulation that isn't happening) and arm the draw, which
                                    // captures LAZILY once it's a genuine drag — so a plain
                                    // click still just deselects and the ZoomPan keeps working.
                                    self.emit(shell, AnnotEvent::Select(None));
                                    state.pending = Pending::Draw(t);
                                } else if let Some((id, hit)) = self.hit_at(&map, local) {
                                    // An existing item is CANVAS-owned: capture + own it.
                                    if additive_select(
                                        self.tool,
                                        state.mods.control(),
                                        state.mods.shift(),
                                    ) {
                                        // POINTER + Ctrl/Shift (DRAGON-341): TOGGLE this item in
                                        // the multi-selection and stop there — a modifier click
                                        // is a pure selection edit, never the start of a drag
                                        // (dragging one you just toggled OFF would be a lie).
                                        self.emit(shell, AnnotEvent::SelectToggle(id));
                                        state.pending = Pending::None;
                                    } else {
                                        // A plain press INSIDE an existing multi-selection keeps
                                        // it whole, so the drag moves every selected item
                                        // together (DRAGON-341); anything else REPLACES the
                                        // selection with the pressed item.
                                        if !(self.selection.len() > 1 && self.is_selected(id)) {
                                            self.emit(shell, AnnotEvent::Select(Some(id)));
                                        }
                                        // (The pencil never reaches here — its press is handled
                                        // above and always draws, DRAGON-346.)
                                        state.pending = match hit {
                                            HitKind::Resize(g) => Pending::Resize(g),
                                            HitKind::Body => Pending::Move,
                                        };
                                    }
                                    shell.capture_event();
                                    consumed = true;
                                } else {
                                    // Empty: DESELECT (no capture — forward so the ZoomPan
                                    // can still act); a draw tool arms a draw that captures
                                    // LAZILY once it's a genuine drag. In POINTER mode
                                    // (DRAGON-341) the same press arms a RUBBER BAND instead —
                                    // also lazily, so a plain click still just deselects — and
                                    // Ctrl/Shift makes it ADDITIVE (the existing selection is
                                    // kept, so nothing is deselected up front).
                                    let additive = additive_select(
                                        self.tool,
                                        state.mods.control(),
                                        state.mods.shift(),
                                    );
                                    if !additive {
                                        self.emit(shell, AnnotEvent::Select(None));
                                    }
                                    state.band_to = local;
                                    state.pending = match self.tool {
                                        Some(Tool::Pointer) => Pending::Band { additive },
                                        Some(t) => Pending::Draw(t),
                                        None => Pending::None,
                                    };
                                }
                            }
                        }
                    }
                    Event::Mouse(mouse::Event::CursorMoved { .. })
                        if !matches!(state.pending, Pending::None) =>
                    {
                        if let Some(p) = cursor.position() {
                            let local = to_local(p);
                            if !state.moved {
                                let d = (local.0 - state.press_screen.0)
                                    .hypot(local.1 - state.press_screen.1);
                                if d > NEW_THRESHOLD {
                                    state.moved = true;
                                }
                            }
                            // A confirmed drag consumes + captures; pre-threshold moves
                            // forward (keeping the ZoomPan free until the gesture is real).
                            if state.moved {
                                let img = map.to_image(local);
                                match state.pending {
                                    Pending::Draw(tool) => {
                                        if !state.begun {
                                            self.emit(shell, AnnotEvent::DrawBegin(tool, state.press_img.0, state.press_img.1));
                                            state.begun = true;
                                        }
                                        self.emit(shell, AnnotEvent::GestureTo(img.0, img.1));
                                    }
                                    Pending::Move => {
                                        if !state.begun {
                                            self.emit(shell, AnnotEvent::GrabBegin(Grab::Move, state.press_img.0, state.press_img.1));
                                            state.begun = true;
                                        }
                                        self.emit(shell, AnnotEvent::GestureTo(img.0, img.1));
                                    }
                                    Pending::Resize(g) => {
                                        if !state.begun {
                                            self.emit(shell, AnnotEvent::GrabBegin(g, state.press_img.0, state.press_img.1));
                                            state.begun = true;
                                        }
                                        self.emit(shell, AnnotEvent::GestureTo(img.0, img.1));
                                    }
                                    // The rubber band publishes NOTHING while it grows — the
                                    // drawn marquee is the whole feedback, and the selection
                                    // lands in one `BoxSelect` on release (DRAGON-341).
                                    Pending::Band { .. } => state.band_to = local,
                                    Pending::None => {}
                                }
                                shell.capture_event();
                                consumed = true;
                            }
                        }
                    }
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                        if !matches!(state.pending, Pending::None) =>
                    {
                        let begun = state.begun;
                        let pending = state.pending;
                        let moved = state.moved;
                        let band = (state.press_img, map.to_image(state.band_to));
                        state.pending = Pending::None;
                        state.moved = false;
                        state.begun = false;
                        if let Pending::Band { additive } = pending {
                            // A real rubber band selects everything it touched; a band that
                            // never moved was just the deselecting click already emitted.
                            if moved {
                                let ((x0, y0), (x1, y1)) = band;
                                self.emit(shell, AnnotEvent::BoxSelect(x0, y0, x1, y1, additive));
                                shell.capture_event();
                                consumed = true;
                            }
                        } else if begun {
                            self.emit(shell, AnnotEvent::GestureEnd);
                            shell.capture_event();
                            consumed = true;
                        } else if let Pending::Draw(t) = pending
                            && t.click_places()
                        {
                            // A pencil TAP inks a DOT (DRAGON-342); a BADGE click drops a
                            // marker. Every other tool needs a real drag to make a shape, so a
                            // no-drag press stays a click — but for these two a press is the
                            // whole deliberate gesture, so run it right here: begin at the
                            // press point and commit. The app side finishes it (the pen
                            // normalizes to a dot; the badge is already a finished square).
                            self.emit(
                                shell,
                                AnnotEvent::DrawBegin(t, state.press_img.0, state.press_img.1),
                            );
                            self.emit(shell, AnnotEvent::GestureEnd);
                            shell.capture_event();
                            consumed = true;
                        }
                        // else: a click that didn't drag → forward (ends any ZoomPan drag).
                    }
                    _ => {}
                }
            }
        }
        // Forward every non-consumed event to the wrapped ZoomPan.
        if !consumed {
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
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &cosmic::iced::core::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use cosmic::iced::advanced::graphics::geometry::Renderer as _;
        use cosmic::iced::core::Renderer as _;
        // 1. The wrapped ZoomPan draws the image (+ covermark) clipped to its content, with
        //    the scrollbars on top.
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        let bounds = layout.bounds();
        let map = self.map(bounds);
        let clip = self.content_rect(bounds);
        let (ox, oy) = (bounds.x, bounds.y);
        // The drawn SHAPES are clipped to the actual IMAGE rect — the content area can extend
        // BEYOND the picture (letterbox bars in a windowed preview taller/wider than the image),
        // and a stroke/cap sitting at the image edge must be cut exactly where the BAKE cuts it
        // (its pixmap is image-sized), not bleed into the letterbox. `clip` (the full content
        // rect) still bounds it away from the scrollbars; the chrome/handles keep `clip` so they
        // can float outside the picture.
        let shape_clip = {
            let a = map.to_canvas((0.0, 0.0));
            let b = map.to_canvas((map.source.0, map.source.1));
            let img = Rectangle {
                x: ox + a.0.min(b.0),
                y: oy + a.1.min(b.1),
                width: (b.0 - a.0).abs(),
                height: (b.1 - a.1).abs(),
            };
            img.intersection(&clip)
                .unwrap_or(Rectangle { x: 0.0, y: 0.0, width: 0.0, height: 0.0 })
        };
        // 2. The committed shapes as TRUE VECTOR geometry (crisp at any zoom), CLIPPED to the
        //    IMAGE rect. A canvas `Frame` builds the geometry in widget-LOCAL coords; the
        //    `with_translation` maps it to global, and iced scissors the geometry to the
        //    surrounding `with_layer` clip. Vector redraw each frame = no atlas churn = no
        //    flicker (the whole reason the raster display layer was retired).
        if !self.items.is_empty() {
            renderer.with_layer(shape_clip, |renderer| {
                let mut frame = canvas::Frame::new(renderer, Size::new(bounds.width, bounds.height));
                draw_shapes(&mut frame, &map, &self.items);
                let geometry = frame.into_geometry();
                renderer.with_translation(Vector::new(ox, oy), |renderer| {
                    renderer.draw_geometry(geometry);
                });
            });
        }
        // 3. The annotation CHROME (selection boxes + the primary's handles) and the pointer's
        //    rubber band on top — same content clip. Every SELECTED item gets the dashed
        //    outline (so a multi-selection is legible, DRAGON-341); only the PRIMARY carries
        //    handles, because a resize always edits exactly one item.
        let state = tree.state.downcast_ref::<State>();
        let band = match state.pending {
            Pending::Band { .. } if state.moved => Some((state.press_screen, state.band_to)),
            _ => None,
        };
        if band.is_none() && self.selection.is_empty() {
            return;
        }
        let primary = self.primary();
        let accent = self.accent;
        renderer.with_layer(clip, |renderer| {
            let mut fill = |x: f32, y: f32, w: f32, h: f32, color: Color, radius: f32| {
                if w <= 0.0 || h <= 0.0 {
                    return;
                }
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle::new(Point::new(ox + x, oy + y), Size::new(w, h)),
                        border: Border { radius: radius.into(), ..Default::default() },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    color,
                );
            };
            // A filled ROUND handle (a full-radius square = a circle) centred at a point.
            let s = HANDLE_SIZE;
            let handle = |fill: &mut dyn FnMut(f32, f32, f32, f32, Color, f32), cx: f32, cy: f32| {
                fill(cx - s / 2.0, cy - s / 2.0, s, s, accent, s / 2.0);
            };
            // The pointer's live rubber band (DRAGON-341): a plain dashed marquee in the accent,
            // drawn even when nothing is selected yet.
            if let Some((a, b)) = band {
                dashed_rect(
                    &mut fill,
                    a.0.min(b.0),
                    a.1.min(b.1),
                    (b.0 - a.0).abs(),
                    (b.1 - a.1).abs(),
                    accent,
                );
            }
            for item in self.items.iter().filter(|i| self.is_selected(i.id)) {
                let is_primary = primary == Some(item.id);
                // A pen group chromes on its BOUNDING BOX — the same dashed rect + 8 handles a
                // rectangle gets, so it moves/resizes with the familiar affordances.
                let chrome_kind = match &item.kind {
                    ItemKind::Path { paths, .. } => {
                        let (x, y, w, h) = path_bounds(paths);
                        ItemKind::Rect { x, y, w, h }
                    }
                    other => other.clone(),
                };
                match chrome_kind {
                    ItemKind::Rect { x, y, w, h } => {
                        let r = box_chrome_rect(&map, x, y, w, h, item.stroke_w);
                        let (l, t, rr, bb) =
                            (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                        dashed_rect(&mut fill, l, t, rr - l, bb - t, accent);
                        if !is_primary {
                            continue; // secondary members of a multi-selection show no handles
                        }
                        let (mx, my) = ((l + rr) / 2.0, (t + bb) / 2.0);
                        for (hx, hy) in [
                            (l, t), (rr, t), (l, bb), (rr, bb),
                            (mx, t), (mx, bb), (l, my), (rr, my),
                        ] {
                            handle(&mut fill, hx, hy);
                        }
                    }
                    ItemKind::Arrow { ax, ay, bx, by } => {
                        if is_primary {
                            let (an, bn) = arrow_nodes(&map, ax, ay, bx, by, item.stroke_w);
                            handle(&mut fill, an.0, an.1);
                            handle(&mut fill, bn.0, bn.1);
                        } else {
                            // A secondary arrow has no endpoint nodes to show, so it wears the
                            // same dashed box as everything else — "this is selected too".
                            let r = box_screen_rect(&map, ax, ay, bx - ax, by - ay);
                            let pad =
                                (HIT_PAD + item.stroke_w * map.img_to_screen_scale() * 0.5).round()
                                    as i32;
                            let (l, t) = ((r.left - pad) as f32, (r.top - pad) as f32);
                            let (rr, bb) = ((r.right + pad) as f32, (r.bottom + pad) as f32);
                            dashed_rect(&mut fill, l, t, rr - l, bb - t, accent);
                        }
                    }
                    // Mapped to its bounding Rect above — unreachable.
                    ItemKind::Path { .. } => {}
                }
            }
        });
    }
}

/// The resize/move cursor for a grab kind.
fn grab_cursor(g: Grab) -> mouse::Interaction {
    match g {
        Grab::Move => mouse::Interaction::Grabbing,
        Grab::Corner(Corner::Nw | Corner::Se) => mouse::Interaction::ResizingDiagonallyDown,
        Grab::Corner(Corner::Ne | Corner::Sw) => mouse::Interaction::ResizingDiagonallyUp,
        Grab::Edge(Edge::N | Edge::S) => mouse::Interaction::ResizingVertically,
        Grab::Edge(Edge::E | Edge::W) => mouse::Interaction::ResizingHorizontally,
        // An arrow endpoint repositions freely — a move cursor.
        Grab::ArrowA | Grab::ArrowB => mouse::Interaction::Move,
    }
}

/// Draw a dashed rounded rectangle outline (4 sides tiled with short quads), 1.5px thick.
fn dashed_rect(
    fill: &mut dyn FnMut(f32, f32, f32, f32, Color, f32),
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Color,
) {
    let thick = 1.5;
    let step = DASH + DASH_GAP;
    // Horizontal sides.
    let mut dx = 0.0;
    while dx < w {
        let seg = DASH.min(w - dx);
        fill(x + dx, y, seg, thick, color, thick / 2.0);
        fill(x + dx, y + h - thick, seg, thick, color, thick / 2.0);
        dx += step;
    }
    // Vertical sides.
    let mut dy = 0.0;
    while dy < h {
        let seg = DASH.min(h - dy);
        fill(x, y + dy, thick, seg, color, thick / 2.0);
        fill(x + w - thick, y + dy, thick, seg, color, thick / 2.0);
        dy += step;
    }
}

/// Draw every committed shape as TRUE VECTOR geometry into `frame`, in z-order (vector
/// order = bottom-to-top), mapping image SOURCE px → widget-LOCAL screen px via `map`.
/// Stroke widths and corner radii scale with zoom (`img_to_screen_scale`), so a shape stays
/// visually identical to the full-res bake at every magnification — just resolution-free.
fn draw_shapes(frame: &mut canvas::Frame, map: &CanvasMap, items: &[Item]) {
    let iss = map.img_to_screen_scale();
    for item in items {
        // The PURE region effects (highlight multiply, pixelate, blur) render through shader
        // passes UNDER this widget — never as vector geometry here (DRAGON-326/327/328).
        // BoxHighlight is the exception: its highlight fill renders via the shader too, but its
        // box OUTLINE is an always-on-top vector drawn here (DRAGON-333), so it does NOT skip.
        // Spotlight (DRAGON-329) renders NOTHING of its own — a pure knockout region — so it
        // skips here too (only its selection chrome, drawn separately, ever appears).
        if matches!(item.fx, FxKind::Highlight | FxKind::Pixelate | FxKind::Blur | FxKind::Spotlight)
        {
            continue;
        }
        let curve = (item.curve_radius * iss).max(0.0);
        let sw = (item.stroke_w * iss).max(0.5);
        // A SEQUENCE BADGE (DRAGON-340) is a Rect item with a render flag: draw the disc +
        // numeral + ring and skip the box outline entirely. Same geometry source, same `iss`
        // scaling the bake applies with its raster scale — see `crate::badge`.
        if let (Some(number), &ItemKind::Rect { x, y, w, h }) = (item.badge, &item.kind) {
            draw_badge(frame, map, (x, y, w, h), number, item.stroke_w, item.color, iss);
            continue;
        }
        match &item.kind {
            &ItemKind::Rect { x, y, w, h } => {
                let a = map.to_canvas((x, y));
                let b = map.to_canvas((x + w, y + h));
                let (l, t) = (a.0.min(b.0), a.1.min(b.1));
                let (rw, rh) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
                if rw <= 0.0 || rh <= 0.0 {
                    continue;
                }
                // ABSOLUTE corner radius, only shrunk when the box is too small to fit it
                // (matches the raster's `round_rect_path` clamp to half the smaller side).
                let r = curve.min(rw * 0.5).min(rh * 0.5);
                let path = Path::rounded_rectangle(Point::new(l, t), Size::new(rw, rh), r.into());
                if let Some(fill) = item.fill {
                    frame.fill(&path, fill);
                }
                frame.stroke(&path, shape_stroke(item.color, sw, curve));
            }
            &ItemKind::Arrow { ax, ay, bx, by } => {
                let a = map.to_canvas((ax, ay));
                let b = map.to_canvas((bx, by));
                // Arrows render +ARROW_STROKE_BONUS source px thicker than the set width (matches
                // the bake), so an arrow is bolder than a same-width box.
                let asw = ((item.stroke_w + ARROW_STROKE_BONUS) * iss).max(0.5);
                draw_arrow_vec(frame, a, b, asw, curve, iss, item.color);
            }
            // Freehand pen (DRAGON-338 + DRAGON-342): a pseudo-pressure RIBBON, not a
            // fixed-width polyline — smoothed centerline, tapered tips, heavier through slow
            // and curving stretches. `crate::pen_stroke::stroke_fill_polygons` builds the
            // pieces (one quad per segment + round caps/joins) from the SAME stored geometry
            // and speed signal the bake reads, mapped through this canvas's zoom instead of the
            // bake's scale — so the two are the same drawing at two resolutions.
            //
            // The whole GROUP goes into ONE path filled with the default NON-ZERO rule: every
            // piece is wound alike, so a self-crossing scribble unions instead of cancelling
            // into holes, and a partially transparent color (an erase-marked group draws at
            // ERASE_PREVIEW_ALPHA) composites exactly once instead of darkening at overlaps.
            ItemKind::Path { paths, pressure } => {
                let ribbon = Path::new(|b| {
                    for (i, path) in paths.iter().enumerate() {
                        let press = pressure.get(i).filter(|p| p.len() == path.len());
                        let polys = crate::pen_stroke::stroke_fill_polygons(
                            path,
                            item.stroke_w,
                            press.map_or(&[][..], |p| p.as_slice()),
                            |p| map.to_canvas(p),
                            iss,
                        );
                        for poly in polys {
                            let Some(first) = poly.first() else { continue };
                            b.move_to(Point::new(first.0, first.1));
                            for q in &poly[1..] {
                                b.line_to(Point::new(q.0, q.1));
                            }
                            b.close();
                        }
                    }
                });
                frame.fill(&ribbon, item.color);
            }
        }
    }
}

/// Draw one SEQUENCE BADGE (DRAGON-340) as vector geometry: a filled disc in the annotation
/// colour, a clear gap, an outer ring at the current line weight, and the ordinal centred on
/// the disc in the contrast ink.
///
/// `rect` is the item's SOURCE-px square; its inscribed circle is the ring's centreline. All
/// the figures come from [`crate::badge::metrics`] in SOURCE px and are multiplied by the ONE
/// image→screen factor `iss` — the exact mirror of the bake, which multiplies the same metrics
/// by its raster scale. The number's ink is derived from `color` at draw time, so a colour
/// change re-picks it with no stored state.
fn draw_badge(
    frame: &mut canvas::Frame,
    map: &CanvasMap,
    rect: (f32, f32, f32, f32),
    number: u32,
    ring_w: f32,
    color: Color,
    iss: f32,
) {
    let (x, y, w, h) = rect;
    // The square is forced 1:1 scene-side; a mid-drag rect can still be un-normalized, so read
    // the side off the extent and place the centre from the two corners.
    let side = w.abs().min(h.abs());
    let m = crate::badge::metrics(side, ring_w, crate::badge::digit_count(number));
    if m.disc_r <= 0.0 {
        return;
    }
    let c = map.to_canvas((x + w * 0.5, y + h * 0.5));
    let centre = Point::new(c.0, c.1);
    // The filled disc.
    frame.fill(&Path::circle(centre, m.disc_r * iss), color);
    // The outer ring — stroked ON the centreline circle, so it straddles the model square's
    // inscribed circle exactly like a box outline straddles its rect.
    if m.ring_w > 0.0 && m.outer_r > 0.0 {
        frame.stroke(
            &Path::circle(centre, m.outer_r * iss),
            Stroke {
                style: canvas::Style::Solid(color),
                width: (m.ring_w * iss).max(0.5),
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            },
        );
    }
    // The ordinal, in whichever tone actually contrasts with the disc.
    let ink = if crate::badge::prefers_dark_ink([color.r, color.g, color.b]) {
        crate::badge::INK_DARK
    } else {
        crate::badge::INK_LIGHT
    };
    let mut ink = Color::from_rgb8(ink[0], ink[1], ink[2]);
    ink.a = color.a;
    let numeral = Stroke {
        style: canvas::Style::Solid(ink),
        width: (m.digit_stroke * iss).max(0.5),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    for poly in crate::badge::number_polylines(number, &m, (x + w * 0.5, y + h * 0.5)) {
        let Some(first) = poly.first() else { continue };
        let path = Path::new(|p| {
            let a = map.to_canvas(*first);
            p.move_to(Point::new(a.0, a.1));
            for q in &poly[1..] {
                let b = map.to_canvas(*q);
                p.line_to(Point::new(b.0, b.1));
            }
        });
        frame.stroke(&path, numeral);
    }
}

/// The stroke style shapes share: round caps/joins when the curve is > 0 (soft corners),
/// else butt/miter (sharp) — mirroring the raster's `(cap, join)` choice.
fn shape_stroke(color: Color, width: f32, curve: f32) -> Stroke<'static> {
    let (line_cap, line_join) = if curve > 0.0 {
        (LineCap::Round, LineJoin::Round)
    } else {
        (LineCap::Butt, LineJoin::Miter)
    };
    Stroke { style: canvas::Style::Solid(color), width, line_cap, line_join, ..Default::default() }
}

/// Draw one arrow as vector geometry (screen px): a shaft to the tip plus an OPEN "V" head
/// (two barbs diverging from the tip back along the shaft) — the SAME geometry the raster's
/// `draw_arrow` builds, so display and bake match. `iss` (image→screen scale) sizes the
/// head's minimum length in the same source-px terms as the raster's `6.0 * scale` floor.
/// The arrow-head length: a FIXED source-px `base_len` (independent of stroke width — a thicker
/// line makes the barbs THICKER, not LONGER), floored at `min_len` (so a thin arrow still shows a
/// visible head) yet never longer than `max_len` (70% of the shaft, so the head can't outgrow a
/// short arrow). CRITICAL (DRAGON-324): a just-started / very short arrow has `min_len > max_len`,
/// and `f32::clamp` PANICS when its `min > max` — the arrow-draw crash. `min_len.min(max_len)`
/// keeps the clamp bounds ordered so the head simply shrinks to the cap on a tiny arrow instead of
/// panicking. The caller already returns early for a near-zero shaft (`len < 0.5`), so `max_len`
/// here is finite and ≥ 0.
fn arrow_head_len(base_len: f32, min_len: f32, max_len: f32) -> f32 {
    base_len.clamp(min_len.min(max_len), max_len)
}

fn draw_arrow_vec(
    frame: &mut canvas::Frame,
    a: (f32, f32),
    b: (f32, f32),
    sw: f32,
    curve: f32,
    iss: f32,
    color: Color,
) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let stroke = shape_stroke(color, sw, curve);
    // Shaft: tail all the way to the tip.
    frame.stroke(&Path::line(Point::new(a.0, a.1), Point::new(b.0, b.1)), stroke);
    // Open-"V" head: two barbs from the tip, splayed back along the shaft direction. The head
    // GROWS with the shaft (25% of it), capped at ARROW_HEAD_MAX and never past 70% of the shaft;
    // stroke width changes barb THICKNESS, not length. Kept in sync with the bake.
    let head_cap = (ARROW_HEAD_MAX * iss).min(len * 0.7);
    let head = arrow_head_len(len * ARROW_HEAD_FRAC, ARROW_HEAD_MAX * ARROW_HEAD_MIN_FRAC * iss, head_cap);
    let ang = 0.52_f32; // ~30° half-angle
    let (ca, sa) = (ang.cos(), ang.sin());
    let back = (-ux, -uy);
    let lft = (back.0 * ca - back.1 * sa, back.0 * sa + back.1 * ca);
    let rgt = (back.0 * ca + back.1 * sa, -back.0 * sa + back.1 * ca);
    let lp = (b.0 + lft.0 * head, b.1 + lft.1 * head);
    let rp = (b.0 + rgt.0 * head, b.1 + rgt.1 * head);
    let head_path = Path::new(|p| {
        p.move_to(Point::new(lp.0, lp.1));
        p.line_to(Point::new(b.0, b.1));
        p.line_to(Point::new(rp.0, rp.1));
    });
    frame.stroke(&head_path, stroke);
}

impl<'a, Msg: Clone + 'static> From<AnnotationCanvas<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: AnnotationCanvas<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(bounds: (f32, f32), zoom: f32, pan: (f32, f32), disp: (f32, f32), source: (f32, f32)) -> CanvasMap {
        CanvasMap { bounds, zoom, pan, disp, source }
    }

    fn assert_close(a: (f32, f32), b: (f32, f32), eps: f32, what: &str) {
        assert!((a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps, "{what}: {a:?} vs {b:?}");
    }

    /// Round-trip: to_canvas(to_image(p)) == p, across zoom/pan/non-square fits.
    #[test]
    fn round_trip_screen_to_image_to_screen() {
        let cases = [
            // bounds, zoom, pan, disp, source
            map((800.0, 600.0), 1.0, (0.0, 0.0), (400.0, 300.0), (800.0, 600.0)),
            map((800.0, 600.0), 2.5, (37.0, -12.0), (400.0, 300.0), (1600.0, 1200.0)),
            map((1000.0, 500.0), 1.0, (0.0, 0.0), (600.0, 200.0), (1920.0, 640.0)), // non-square fit
            map((640.0, 480.0), 3.0, (-80.0, 40.0), (320.0, 240.0), (3200.0, 2400.0)),
        ];
        for m in cases {
            for p in [(0.0, 0.0), (123.0, 456.0), (400.0, 300.0), (799.0, 1.0)] {
                let img = m.to_image(p);
                let back = m.to_canvas(img);
                assert_close(back, p, 1e-2, "round trip");
            }
        }
    }

    /// At zoom 1 / no pan, the image is centred: the bounds centre maps to the source
    /// centre, and the image origin maps to (0,0) source.
    #[test]
    fn centered_fit_anchors_at_zoom_one() {
        let m = map((800.0, 600.0), 1.0, (0.0, 0.0), (400.0, 300.0), (2000.0, 1500.0));
        assert_close(m.to_image((400.0, 300.0)), (1000.0, 750.0), 1e-3, "centre → source centre");
        // Image top-left is centred: origin at (200,150) local.
        assert_close(m.to_image((200.0, 150.0)), (0.0, 0.0), 1e-3, "image origin → (0,0)");
        assert_close(m.to_image((600.0, 450.0)), (2000.0, 1500.0), 1e-3, "image br → source br");
    }

    /// Panning shifts the mapping by the pan amount at zoom 1.
    #[test]
    fn pan_shifts_the_image_point() {
        let base = map((800.0, 600.0), 1.0, (0.0, 0.0), (400.0, 300.0), (400.0, 300.0));
        let panned = map((800.0, 600.0), 1.0, (50.0, 20.0), (400.0, 300.0), (400.0, 300.0));
        // At zoom 1, disp==source, so 1 screen px == 1 source px; a +50,+20 pan moves the
        // picture right/down, so the same screen point maps to a smaller source coordinate.
        let p = (400.0, 300.0);
        let a = base.to_image(p);
        let b = panned.to_image(p);
        assert_close((a.0 - b.0, a.1 - b.1), (50.0, 20.0), 1e-3, "pan delta");
    }

    /// Zoom scales source-per-screen: zooming in halves the source span a screen delta covers.
    #[test]
    fn zoom_scales_source_per_screen() {
        let m1 = map((800.0, 600.0), 1.0, (0.0, 0.0), (400.0, 300.0), (400.0, 300.0));
        let m2 = map((800.0, 600.0), 2.0, (0.0, 0.0), (400.0, 300.0), (400.0, 300.0));
        // Two screen points 100px apart.
        let span1 = m1.to_image((400.0, 300.0)).0 - m1.to_image((300.0, 300.0)).0;
        let span2 = m2.to_image((400.0, 300.0)).0 - m2.to_image((300.0, 300.0)).0;
        assert!((span1 - 100.0).abs() < 1e-3, "zoom 1: 100 screen px = 100 source px");
        assert!((span2 - 50.0).abs() < 1e-3, "zoom 2: 100 screen px = 50 source px");
    }

    #[test]
    fn box_chrome_rect_offsets_by_hit_pad_plus_stroke_half() {
        // A scale-1 map (screen == image coords), so the offset is read directly.
        let m = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        // Raw box (0,0)-(100,100). With NO stroke the chrome is offset only by HIT_PAD (8).
        let r0 = box_chrome_rect(&m, 0.0, 0.0, 100.0, 100.0, 0.0);
        assert_eq!((r0.left, r0.top, r0.right, r0.bottom), (-8, -8, 108, 108));
        // With stroke 8 the offset grows by stroke/2 = 4 → 12 all around, clearing the
        // VISIBLE stroke (which straddles the geometry by ~stroke/2).
        let r8 = box_chrome_rect(&m, 0.0, 0.0, 100.0, 100.0, 8.0);
        assert_eq!((r8.left, r8.top, r8.right, r8.bottom), (-12, -12, 112, 112));
    }

    #[test]
    fn unselected_hit_is_strict_selected_keeps_hit_pad() {
        // scale-1 map (screen == image coords). Box geometry (20,20)-(80,80), stroke 8.
        let cmap = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let item = || Item {
            id: 7,
            kind: ItemKind::Rect { x: 20.0, y: 20.0, w: 60.0, h: 60.0 },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
        };
        let make = |selected: Vec<u64>| {
            AnnotationCanvas::new(
                cosmic::widget::Space::new(),
                vec![item()],
                selected,
                None,
                1.0,
                (0.0, 0.0),
                (100.0, 100.0),
                (100.0, 100.0),
                false,
                Color::WHITE,
                |_ev: AnnotEvent| (),
            )
        };
        // The STRICT (unselected) region = geometry ± stroke/2 = ±4 → left edge at x=16; the
        // PADDED (selected) region adds HIT_PAD → left edge at x=8.
        assert_eq!(box_drawn_rect(&cmap, 20.0, 20.0, 60.0, 60.0, 8.0).left, 16, "strict = stroke/2");
        assert_eq!(box_chrome_rect(&cmap, 20.0, 20.0, 60.0, 60.0, 8.0).left, 8, "padded = +HIT_PAD");

        let unselected = make(Vec::new());
        // Clicking the visible LEFT STROKE (x=20) selects (Body) even when unselected.
        assert!(matches!(unselected.hit_at(&cmap, (20.0, 50.0)), Some((7, HitKind::Body))));
        // But the HIT_PAD band just OUTSIDE the drawn stroke (x=13, inside the old ±12 pad,
        // outside the strict ±4) NO LONGER selects — it's a clean gap now.
        assert!(unselected.hit_at(&cmap, (13.0, 50.0)).is_none(), "pad band is a gap when unselected");
        assert!(unselected.hit_at(&cmap, (8.0, 8.0)).is_none(), "outer corner is a gap when unselected");

        // Once SELECTED, the same padded band is live again: the outer corner is a resize
        // handle, and the pad-band body is grabbable (HIT_PAD breathing room restored).
        let selected = make(vec![7]);
        assert!(matches!(selected.hit_at(&cmap, (8.0, 8.0)), Some((7, HitKind::Resize(_)))), "handle at pad");
        // A padded-body point clear of the corner/edge handles (x=13 is in the ±HIT_PAD band,
        // y=30 keeps it away from the W edge-midpoint handle at (8,50)).
        assert!(matches!(selected.hit_at(&cmap, (13.0, 30.0)), Some((7, HitKind::Body))), "padded body");
    }

    #[test]
    fn arrow_nodes_push_endpoints_outward_by_hit_pad_plus_stroke_half() {
        let m = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        // Horizontal arrow (20,50)->(80,50), stroke 8: each node pushed 8+4=12 along the
        // axis — the tail node BACK past its cap, the head node FORWARD beyond the tip.
        let (an, bn) = arrow_nodes(&m, 20.0, 50.0, 80.0, 50.0, 8.0);
        assert_close(an, (8.0, 50.0), 1e-3, "tail node pushed back beyond the cap");
        assert_close(bn, (92.0, 50.0), 1e-3, "head node pushed forward beyond the tip");
    }

    fn canvas(zoom: f32, disp: (f32, f32), source: (f32, f32)) -> AnnotationCanvas<'static, ()> {
        AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![],
            Vec::new(),
            None,
            zoom,
            (0.0, 0.0),
            disp,
            source,
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        )
    }

    #[test]
    fn scrollbar_strip_passthrough_only_when_overflowing() {
        let bounds = Rectangle { x: 0.0, y: 0.0, width: 300.0, height: 200.0 };
        let total = crate::widgets::zoom_pan::SCROLLBAR_TOTAL;
        // Zoom 2 on 300×200 disp → 600×400 content overflows both axes → both strips exist.
        let c = canvas(2.0, (300.0, 200.0), (300.0, 200.0));
        assert!(c.in_scrollbar_strip(bounds, (300.0 - total / 2.0, 100.0)), "right strip");
        assert!(c.in_scrollbar_strip(bounds, (150.0, 200.0 - total / 2.0)), "bottom strip");
        assert!(!c.in_scrollbar_strip(bounds, (150.0, 100.0)), "centre is not a strip");
        // No overflow (content fits) → no strips, even at the very edge (so drawing near the
        // edge is not blocked when there are no scrollbars).
        let c2 = canvas(1.0, (200.0, 150.0), (200.0, 150.0));
        assert!(!c2.in_scrollbar_strip(bounds, (299.0, 100.0)), "no overflow → no strip");
        // PER-AXIS gating (DRAGON-326): a tall+narrow content overflows ONLY vertically →
        // ONLY the right strip is reserved; the bottom edge stays fully drawable/selectable
        // (no phantom horizontal-scrollbar reservation). Content 100×400 in 300×200 bounds:
        // width fits with room to spare (the vertical bar can't tip it into h-overflow).
        let c3 = canvas(1.0, (100.0, 400.0), (100.0, 400.0));
        assert!(c3.in_scrollbar_strip(bounds, (300.0 - total / 2.0, 100.0)), "right strip present");
        assert!(
            !c3.in_scrollbar_strip(bounds, (150.0, 200.0 - total / 2.0)),
            "no bottom strip when only the vertical axis overflows — draw to the bottom edge"
        );
    }

    #[test]
    fn point_near_segment_measures_perpendicular_distance() {
        // A horizontal segment from (0,0) to (100,0); a point 5 above its middle is 5 away.
        assert!((point_near_segment((50.0, 5.0), (0.0, 0.0), (100.0, 0.0)) - 5.0).abs() < 1e-3);
        // Past the end clamps to the endpoint distance.
        assert!((point_near_segment((110.0, 0.0), (0.0, 0.0), (100.0, 0.0)) - 10.0).abs() < 1e-3);
        // A degenerate segment is just the point distance.
        assert!((point_near_segment((3.0, 4.0), (0.0, 0.0), (0.0, 0.0)) - 5.0).abs() < 1e-3);
    }

    #[test]
    fn tool_persistence_round_trips_every_variant() {
        // Every tool must survive a save/restore cycle (the persisted `annot_tool`), including
        // the DRAGON-338 pencil + eraser; an unknown string stays neutral.
        for t in [
            Tool::Pointer,
            Tool::Arrow,
            Tool::Rect,
            Tool::Highlight,
            Tool::BoxHighlight,
            Tool::Spotlight,
            Tool::Pixelate,
            Tool::Blur,
            Tool::Pen,
            Tool::Eraser,
        ] {
            assert_eq!(Tool::from_str(t.as_str()), Some(t), "{t:?} round-trips");
        }
        assert_eq!(Tool::from_str("not-a-tool"), None);
        assert!(Tool::Eraser.is_eraser());
        assert!(!Tool::Pen.is_eraser());
        // DRAGON-341: exactly two tools create nothing — the eraser removes, the pointer selects.
        assert!(Tool::Pointer.is_pointer() && !Tool::Arrow.is_pointer());
        assert!(!Tool::Pointer.draws() && !Tool::Eraser.draws());
        for t in [Tool::Arrow, Tool::Rect, Tool::Highlight, Tool::BoxHighlight, Tool::Spotlight, Tool::Pixelate, Tool::Blur, Tool::Pen] {
            assert!(t.draws(), "{t:?} draws");
        }
    }

    /// Exactly TWO tools complete their gesture on a plain click — the pencil (a tap is a dot)
    /// and the step marker (it is placed at a point, not dragged out). Every region tool still
    /// needs a real drag, so a stray click can never leave a zero-size shape behind.
    #[test]
    fn only_the_placing_tools_complete_a_gesture_on_a_bare_click() {
        assert!(Tool::Pen.click_places());
        assert!(Tool::Badge.click_places());
        for t in [
            Tool::Pointer,
            Tool::Arrow,
            Tool::Rect,
            Tool::Highlight,
            Tool::BoxHighlight,
            Tool::Spotlight,
            Tool::Pixelate,
            Tool::Blur,
            Tool::Eraser,
        ] {
            assert!(!t.click_places(), "{t:?} must still need a drag");
        }
    }

    #[test]
    fn path_bounds_spans_every_stroke_in_the_group() {
        let paths = vec![
            vec![(10.0, 5.0), (20.0, 5.0)],
            vec![(12.0, 25.0), (12.0, 30.0)],
        ];
        assert_eq!(path_bounds(&paths), (10.0, 5.0, 10.0, 25.0));
        // An empty group is a zero rect (never NaN/infinite chrome).
        assert_eq!(path_bounds(&[]), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn pen_hits_along_its_strokes_not_across_its_empty_bbox() {
        // A pen group's bbox is mostly empty space, so only the STROKES select — a click in
        // the gap between two scribbles must fall through to whatever is under it. And since
        // DRAGON-341 a pen group is body-selectable ONLY in POINTER mode.
        let cmap = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let item = || Item {
            id: 5,
            // An "L": along the top edge, then down the left — the bbox interior stays empty.
            kind: ItemKind::Path {
                paths: vec![vec![(10.0, 10.0), (90.0, 10.0)], vec![(10.0, 10.0), (10.0, 90.0)]],
                pressure: Vec::new(),
            },
            stroke_w: 4.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
        };
        let make = |tool: Option<Tool>| {
            AnnotationCanvas::new(
                cosmic::widget::Space::new(),
                vec![item()],
                Vec::new(),
                tool,
                1.0,
                (0.0, 0.0),
                (100.0, 100.0),
                (100.0, 100.0),
                false,
                Color::WHITE,
                |_ev: AnnotEvent| (),
            )
        };
        let canvas = make(Some(Tool::Pointer));
        // On a stroke → the body (selects / moves).
        assert!(matches!(canvas.hit_at(&cmap, (50.0, 10.0)), Some((5, HitKind::Body))), "on the top stroke");
        assert!(matches!(canvas.hit_at(&cmap, (10.0, 50.0)), Some((5, HitKind::Body))), "on the left stroke");
        // Deep inside the bounding box but far from any ink → no hit.
        assert!(canvas.hit_at(&cmap, (60.0, 60.0)).is_none(), "the empty bbox interior is not the item");
        // Outside the group entirely → no hit.
        assert!(canvas.hit_at(&cmap, (95.0, 95.0)).is_none());
        // path_distance measures to the nearest stroke (screen px at this 1:1 map).
        assert!((path_distance(&cmap, &[vec![(0.0, 0.0), (100.0, 0.0)]], (50.0, 12.0)) - 12.0).abs() < 1e-3);
        // DRAGON-341 gating: outside pointer mode the very same ink is INERT — neutral, a draw
        // tool, the pencil itself and the eraser all fall straight through it.
        for tool in [None, Some(Tool::Rect), Some(Tool::Pen), Some(Tool::Eraser)] {
            let c = make(tool);
            assert!(
                c.hit_at(&cmap, (50.0, 10.0)).is_none(),
                "{tool:?}: pen ink must not be click-selectable outside pointer mode"
            );
        }
    }

    #[test]
    fn a_selected_pen_gets_bounding_box_handles() {
        // A pen group chromes + resizes on its bbox, exactly like a rectangle: once SELECTED,
        // its outer corner is a resize handle.
        let cmap = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let item = Item {
            id: 7,
            kind: ItemKind::Path { paths: vec![vec![(20.0, 20.0), (80.0, 80.0)]], pressure: Vec::new() },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
        };
        let canvas = AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![item],
            vec![7],
            None,
            1.0,
            (0.0, 0.0),
            (100.0, 100.0),
            (100.0, 100.0),
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        );
        // The bbox is (20,20)-(80,80); with stroke 8 the chrome sits at ±(8 + 4) → the NW
        // handle is at (8,8), the same offsets a box of that geometry would use.
        assert!(
            matches!(canvas.hit_at(&cmap, (8.0, 8.0)), Some((7, HitKind::Resize(Grab::Corner(Corner::Nw))))),
            "the pen's bbox corner resizes it"
        );
    }

    #[test]
    fn arrow_head_len_is_panic_free_on_short_and_zero_arrows() {
        // DRAGON-324 regression: drawing an arrow starts with a ZERO-length shaft, and a
        // short shaft has floor (6*iss) > cap (len*0.7). The old `clamp(floor, cap)` hit
        // `f32::clamp`'s min > max PANIC — this crashed the preview on the MAIN render thread
        // (the old raster path swallowed the same panic off-thread). The head must simply
        // shrink to the cap, never panic.
        // `base` here stands in for the shaft-proportional head length (len * ARROW_HEAD_FRAC).
        // Zero-length shaft cap → head 0 (nothing drawn), no panic.
        assert_eq!(arrow_head_len(20.0, 6.0, 0.0), 0.0);
        // 1px arrow: cap = 0.7 < floor 6 → head clamps to the cap, no panic.
        assert!((arrow_head_len(20.0, 6.0, 0.7) - 0.7).abs() < 1e-6);
        // A normal arrow (floor ≤ base ≤ cap): the base head is returned unchanged.
        assert!((arrow_head_len(20.0, 6.0, 100.0) - 20.0).abs() < 1e-6);
        // The floor still applies when the base is below it.
        assert!((arrow_head_len(3.0, 6.0, 100.0) - 6.0).abs() < 1e-6);
        // Result is always finite and within [0, cap].
        for (base, floor, cap) in [(20.0, 6.0, 0.5), (60.0, 6.0, 2.0), (6.0, 6.0, 6.0)] {
            let h = arrow_head_len(base, floor, cap);
            assert!(h.is_finite() && h >= 0.0 && h <= cap + 1e-6);
        }
    }

    #[test]
    fn ctrl_forces_a_new_draw_only_while_a_tool_is_armed() {
        // DRAGON-339: Ctrl flips the press precedence so a new shape can be drawn ON TOP of
        // existing items (which normally capture the press to move/resize).
        assert_eq!(force_new_draw(Some(Tool::Rect), true), Some(Tool::Rect), "ctrl + tool draws");
        assert_eq!(force_new_draw(Some(Tool::Rect), false), None, "no ctrl → manipulate as before");
        // The NEUTRAL pointer has nothing to draw, so Ctrl changes nothing there.
        assert_eq!(force_new_draw(None, true), None, "ctrl without a tool still manipulates");
        assert_eq!(force_new_draw(None, false), None);
        // Whatever tool is armed is the one forced (tool-agnostic — future tools inherit it).
        assert_eq!(force_new_draw(Some(Tool::Blur), true), Some(Tool::Blur));
        // DRAGON-341: the NON-drawing tools have nothing to force. In pointer mode Ctrl-click is
        // multi-select, so `force_new_draw` must stay out of its way entirely.
        assert_eq!(force_new_draw(Some(Tool::Pointer), true), None, "the pointer never draws");
        assert_eq!(force_new_draw(Some(Tool::Eraser), true), None, "the eraser never draws");
    }

    #[test]
    fn a_pencil_press_always_draws_and_never_consults_what_is_under_it() {
        // DRAGON-346: the pencil press is INK, full stop. `draw_bypassing_items` is the branch
        // the press arm takes BEFORE `hit_at`, so a Some(_) here means the item under the cursor
        // is never looked at — the press can only ever emit Select(None) + arm the draw, never
        // Select(Some(id)) and never a Move/Resize grab.
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), false), Some(Tool::Pen), "plain pencil inks");
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), true), Some(Tool::Pen), "ctrl changes nothing");
        // Every SHAPE tool keeps press-selects: it hit-tests unless Ctrl is held (DRAGON-339).
        for t in [Tool::Rect, Tool::Arrow, Tool::Highlight, Tool::BoxHighlight, Tool::Spotlight, Tool::Pixelate, Tool::Blur] {
            assert_eq!(draw_bypassing_items(Some(t), false), None, "{t:?} presses select as before");
            assert_eq!(draw_bypassing_items(Some(t), true), Some(t), "{t:?} + ctrl draws over items");
        }
        // The pointer and the neutral state always hit-test (the pointer IS selection); the
        // eraser never reaches this branch (handled earlier), so it must not claim one here.
        for t in [None, Some(Tool::Pointer), Some(Tool::Eraser)] {
            for ctrl in [false, true] {
                assert_eq!(draw_bypassing_items(t, ctrl), None, "{t:?} ctrl={ctrl} hit-tests");
            }
        }
    }

    #[test]
    fn the_cursor_promises_what_the_press_will_do() {
        // DRAGON-346: with the pencil armed the crosshair must hold EVERYWHERE — over an item's
        // body (which used to show the open-hand grab, promising a move that never happened) and
        // over the selected item's resize handles too, since neither is reachable any more.
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false), "the pencil owns the whole canvas");
        assert!(whole_canvas_crosshair(Some(Tool::Pen), true));
        assert!(whole_canvas_crosshair(Some(Tool::Eraser), false), "so does the eraser");
        // A shape tool shows per-item cursors until Ctrl flips it to draw-over-anything.
        assert!(!whole_canvas_crosshair(Some(Tool::Rect), false));
        assert!(whole_canvas_crosshair(Some(Tool::Rect), true));
        // The pointer and the neutral state always show the per-item cursors.
        for t in [None, Some(Tool::Pointer)] {
            for ctrl in [false, true] {
                assert!(!whole_canvas_crosshair(t, ctrl), "{t:?} ctrl={ctrl} keeps item cursors");
            }
        }
    }

    #[test]
    fn a_pencil_press_over_a_shape_body_still_bypasses_it() {
        // The companion to the pure rule above, on a real canvas: a box sits under the press
        // point and IS hit-testable (a shape tool would grab it), yet with the pencil armed the
        // press arm never gets that far — `draw_bypassing_items` short-circuits first.
        let cmap = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let boxed = Item {
            id: 3,
            kind: ItemKind::Rect { x: 20.0, y: 20.0, w: 60.0, h: 60.0 },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
        };
        let make = |tool: Option<Tool>, selection: Vec<u64>| {
            AnnotationCanvas::new(
                cosmic::widget::Space::new(),
                vec![boxed.clone()],
                selection,
                tool,
                1.0,
                (0.0, 0.0),
                (100.0, 100.0),
                (100.0, 100.0),
                false,
                Color::WHITE,
                |_ev: AnnotEvent| (),
            )
        };
        // The point is genuinely ON the box (a shape tool would select + move it)...
        let with_rect = make(Some(Tool::Rect), Vec::new());
        assert!(matches!(with_rect.hit_at(&cmap, (20.0, 50.0)), Some((3, HitKind::Body))));
        // ...and even its resize handle is live once selected...
        let selected = make(Some(Tool::Rect), vec![3]);
        assert!(matches!(selected.hit_at(&cmap, (8.0, 8.0)), Some((3, HitKind::Resize(_)))));
        // ...but with the PENCIL armed the press bypasses hit-testing entirely, so neither the
        // body nor the handle can be reached, and the cursor says so.
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), false), Some(Tool::Pen));
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false));
    }

    #[test]
    fn additive_select_is_pointer_only_and_never_fights_ctrl_draw() {
        // DRAGON-341 × DRAGON-339: Ctrl means "multi-select" in POINTER mode and "draw over
        // whatever is under the cursor" with a draw tool — never both for one press.
        assert!(additive_select(Some(Tool::Pointer), true, false), "ctrl-click adds");
        assert!(additive_select(Some(Tool::Pointer), false, true), "shift-click adds");
        assert!(!additive_select(Some(Tool::Pointer), false, false), "a plain click replaces");
        for t in [Tool::Rect, Tool::Arrow, Tool::Pen, Tool::Eraser] {
            assert!(!additive_select(Some(t), true, true), "{t:?} never multi-selects");
        }
        assert!(!additive_select(None, true, true), "the neutral state never multi-selects");
        // The two Ctrl meanings are mutually exclusive for every tool.
        for t in [None, Some(Tool::Pointer), Some(Tool::Rect), Some(Tool::Pen), Some(Tool::Eraser)] {
            assert!(
                !(additive_select(t, true, false) && force_new_draw(t, true).is_some()),
                "{t:?}: ctrl must claim exactly one meaning"
            );
        }
    }

    #[test]
    fn only_the_primary_of_a_multi_selection_wears_handles() {
        // DRAGON-341: the LAST selected id is the primary — the only one with resize handles, so
        // every Grab still edits exactly one item. The others are still BODY-hittable (they move
        // with the group) and keep the padded selected hit region.
        let cmap = map((200.0, 200.0), 1.0, (0.0, 0.0), (200.0, 200.0), (200.0, 200.0));
        let boxed = |id: u64, x: f32, y: f32| Item {
            id,
            kind: ItemKind::Rect { x, y, w: 40.0, h: 40.0 },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
        };
        let canvas = AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![boxed(1, 20.0, 20.0), boxed(2, 120.0, 20.0)],
            vec![1, 2], // 2 is the PRIMARY (added last)
            Some(Tool::Pointer),
            1.0,
            (0.0, 0.0),
            (200.0, 200.0),
            (200.0, 200.0),
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        );
        assert_eq!(canvas.primary(), Some(2));
        assert!(canvas.is_selected(1) && canvas.is_selected(2));
        // The PRIMARY's NW chrome corner (120-12, 20-12) is a resize handle...
        assert!(
            matches!(canvas.hit_at(&cmap, (108.0, 8.0)), Some((2, HitKind::Resize(_)))),
            "the primary resizes"
        );
        // ...while the secondary's matching corner is only its (padded) BODY — no handle.
        assert!(
            matches!(canvas.hit_at(&cmap, (8.0, 8.0)), Some((1, HitKind::Body))),
            "a secondary member has no handles, only a grabbable body"
        );
    }
}
