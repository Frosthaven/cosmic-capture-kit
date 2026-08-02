//! The image-annotation interaction canvas: a transparent leaf `Widget` layered OVER
//! the preview's [`crate::widgets::ZoomPan`] that owns pointer handling for the
//! annotation editor — click-to-select, drag-to-move, drag-handle-to-resize,
//! drag-endpoint and drag-bend (arrow, DRAGON-470), draw-a-new-shape, and right-click. A press
//! over an existing item
//! manipulates it, with two exceptions that BYPASS hit-testing entirely: Ctrl held with a draw
//! tool armed flips that precedence so a new shape can be drawn on TOP of existing ones
//! ([`force_new_draw`], DRAGON-339), and the two whole-canvas tools — the eraser (DRAGON-338)
//! and the PENCIL (DRAGON-346) — never select anything at all. A pencil press is ink, full
//! stop: selection belongs to [`Tool::Pointer`].
//!
//! # Selection (DRAGON-341)
//! The selection is a SET (`selection`, in selection order) rather than one id. Its LAST member
//! is the PRIMARY: for a SINGLE selection it is the one wearing resize handles, so a [`Grab`]
//! edits exactly one item. For a MULTI-selection the handles move to a GROUP box (DRAGON-388) and
//! a resize scales every member in unison; single-select resize is untouched. [`Tool::Pointer`] is the
//! pure-selection mode that makes the set reachable — Ctrl/Shift-click toggles members
//! ([`additive_select`]), an empty-canvas drag rubber-bands ([`Pending::Band`] →
//! [`AnnotEvent::BoxSelect`]), and dragging any selected body emits the ordinary
//! [`Grab::Move`], which the app applies to the WHOLE set. Pointer mode is also the only state
//! in which freehand PEN groups are body-selectable (see `pen_selectable`): ink covers a picture
//! and must not swallow clicks meant for what is under it.
//!
//! The pointer never borrows a DRAW affordance (DRAGON-468). Its rubber band is a washed accent
//! box with a 1px solid border ([`band_rect`]) rather than the dashed marquee it wore, and the
//! drag keeps the plain arrow instead of swapping to the crosshair (the [`Pending::Band`] arm of
//! `mouse_interaction`). Both said "a shape is about to land here" for a gesture that only
//! selects; the dashed style stays where it belongs, on the selection chrome.
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
//! # Text rides along, in z-order (DRAGON-373)
//! Text annotations are RASTERS (their glyphs must be pixel-identical to the bake), and they are
//! handed to this widget as draw-only layers — one per box — rather than stacked under it, so
//! [`draw_passes`] can interleave them with the vector runs in ITEM order. Anything drawn under
//! this widget necessarily sits under EVERY vector, which is why bringing a caption to the front
//! used to do nothing on screen while the export honoured it. Those layers never receive an
//! event: hit-testing works off the item model, so input stays entirely here.
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
use cosmic::iced::core::input_method::{self, InputMethod, Purpose};
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
/// (`crate::app::preview::annotate::draw_arrow`, same 0.125 / 0.40·39.75 / 39.75 values).
const ARROW_HEAD_FRAC: f32 = 0.125;
/// The historical 53 cut by 25% at the owner's request (DRAGON-477). The floor above is a
/// FRACTION of this, so short arrows scale down with it and the head's proportions hold.
const ARROW_HEAD_MAX: f32 = 39.75;
const ARROW_HEAD_MIN_FRAC: f32 = 0.40;
/// Arrows render this many SOURCE px THICKER than the set stroke width, so an arrow reads as bolder
/// than a same-width box (mirrored in the bake, `annotate::draw_arrow`).
const ARROW_STROKE_BONUS: f32 = 2.0;
/// How closely a BENT arrow's shaft (DRAGON-470) is followed when hit-testing it, in SCREEN px.
/// A tenth of [`ARROW_GRAB`]: the polyline can then only shift the boundary of a 10px-slop grab
/// by 1px at the very worst, which is far below the hand's own precision. Only the hit-test
/// flattens at all — the DRAWN shaft is a true quadratic handed straight to the renderer.
const ARROW_CURVE_TOL: f32 = ARROW_GRAB * 0.1;
/// Dash + gap lengths for the selection outline (screen px).
const DASH: f32 = 6.0;
const DASH_GAP: f32 = 4.0;
/// The pointer's rubber band (DRAGON-468): a 1px SOLID accent border around a mostly
/// TRANSLUCENT accent interior. It used to be a dashed marquee, which is the "something will be
/// PLACED here" idiom the draw tools own; a washed box reads as "these items are being SELECTED".
/// The wash multiplies the accent's own alpha, so a translucent accent stays proportional.
const BAND_BORDER: f32 = 1.0;
const BAND_FILL_ALPHA: f32 = 0.18;

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
    /// The HAND (DRAGON-392): draws nothing and manipulates nothing — while it is armed a plain
    /// left-drag PANS the picture, exactly as Alt-drag does under every other tool. It replaced
    /// the old pointer/pan MODE toggle: panning is now a tool you arm like any other (`H`,
    /// Photoshop's Hand key), so the editor has ONE selection model instead of a tool plus a
    /// parallel mode flag. The canvas forwards every pointer event to its `ZoomPan` child while
    /// it is armed — see the `pan_mode` field, which the app now feeds from this tool.
    Hand,
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
    /// Place a TEXT annotation (DRAGON-354): a CLICK drops an auto-sizing box at the point and
    /// opens the in-canvas editor (blinking caret); a DRAG lays out a fixed-width box the text
    /// wraps within. Both enter edit mode on release. A press that lands on an existing text box
    /// re-opens IT instead of creating a new one, and (under the pointer) a double-click on a
    /// text box re-opens editing too. Geometry is a Box-like rect; the glyphs render through the
    /// shared embedded-font rasterizer (`crate::app::preview::text_annot`), not this widget.
    Text,
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
            Tool::Hand => "hand",
            Tool::Arrow => "arrow",
            Tool::Badge => "badge",
            Tool::Rect => "box",
            Tool::Highlight => "highlight",
            Tool::BoxHighlight => "box-highlight",
            Tool::Spotlight => "spotlight",
            Tool::Pixelate => "pixelate",
            Tool::Blur => "blur",
            Tool::Pen => "pen",
            Tool::Text => "text",
            Tool::Eraser => "eraser",
        }
    }
    /// Parse the persisted string; unknown values yield `None` (neutral).
    pub fn from_str(s: &str) -> Option<Tool> {
        match s {
            "pointer" => Some(Tool::Pointer),
            "hand" => Some(Tool::Hand),
            "arrow" => Some(Tool::Arrow),
            "badge" => Some(Tool::Badge),
            "box" => Some(Tool::Rect),
            "highlight" => Some(Tool::Highlight),
            "box-highlight" => Some(Tool::BoxHighlight),
            "spotlight" => Some(Tool::Spotlight),
            "pixelate" => Some(Tool::Pixelate),
            "blur" => Some(Tool::Blur),
            "pen" => Some(Tool::Pen),
            "text" => Some(Tool::Text),
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

    /// Whether this is the HAND (DRAGON-392) — the tool whose press PANS the picture instead of
    /// reaching the annotation model at all. The app reads it to drive the canvas's / `ZoomPan`'s
    /// `pan_mode`, so "the hand is armed" and "a plain drag pans" are one fact.
    pub fn is_hand(self) -> bool {
        matches!(self, Tool::Hand)
    }

    /// Whether this tool CREATES geometry on a drag. The three non-creating tools are the
    /// [`Tool::Eraser`] (it removes), the [`Tool::Pointer`] (it only selects) and the
    /// [`Tool::Hand`] (it pans) — everything that keys off "a drag will draw something" (the
    /// crosshair cursor, the Ctrl draw-over-items override) must ask this rather than
    /// `tool.is_some()`.
    pub fn draws(self) -> bool {
        !matches!(self, Tool::Eraser | Tool::Pointer | Tool::Hand)
    }

    /// Whether a plain CLICK (a press that never crosses the drag threshold) is already a
    /// COMPLETE gesture for this tool — the canvas then runs the whole `DrawBegin` +
    /// `GestureEnd` pair on release instead of letting the press pass through as a bare click.
    /// Two tools place rather than drag out a region:
    ///   * the PENCIL, whose tap is a deliberate round DOT (DRAGON-342);
    ///   * the STEP MARKER (`Tool::Badge`), dropped at a point and sized from the last one
    ///     placed or resized rather than from a rubber-band.
    ///   * the TEXT tool (`Tool::Text`, DRAGON-354): a bare click drops an auto-sizing text box
    ///     at the point and opens the editor. (A real drag instead lays out a fixed-width box —
    ///     `Tool::Text` also `draws()`.)
    ///
    /// Every other tool still needs a real drag to make a shape, so a stray click on the canvas
    /// stays a stray click. Pure — unit-tested.
    pub fn click_places(self) -> bool {
        matches!(self, Tool::Pen | Tool::Badge | Tool::Text)
    }

    /// Whether this tool creates TEXT — the one tool whose press re-opens an existing text box
    /// under the cursor (rather than always drawing a brand-new item). Pure.
    pub fn is_text(self) -> bool {
        matches!(self, Tool::Text)
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
/// drag threshold).
///
/// `over_handle` is [`HitKind::Resize`] — a press on the selected item's resize handle, which no
/// MODIFIER may claim ([`handle_press_beats_modifiers`], DRAGON-370). The PENCIL is deliberately
/// checked first and is not gated by it: DRAGON-346's rule is that a pencil press is ink, full
/// stop, and that is unconditional, not a modifier. Pure — unit-tested.
pub fn draw_bypassing_items(tool: Option<Tool>, ctrl: bool, over_handle: bool) -> Option<Tool> {
    match tool {
        Some(Tool::Pen) => Some(Tool::Pen),
        _ if handle_press_beats_modifiers(over_handle) => None,
        other => force_new_draw(other, ctrl),
    }
}

/// THE resize-handle rule (DRAGON-370): a press that lands on a selected item's RESIZE HANDLE
/// starts a resize, and NO MODIFIER GATE may claim it. Every modifier path in the press ladder
/// consults this, so there is exactly one statement of it.
///
/// WHY a handle is special. A handle only exists on an item that is ALREADY selected, and it is a
/// few px across sitting on top of that item. So each thing a modifier would otherwise claim
/// there is either a no-op or a near-impossible aim:
///
/// * Ctrl/Shift + pointer would TOGGLE the item into the multi-selection ([`additive_select`]) —
///   but it is selected already, which is the only reason the handle is on screen at all.
/// * Shift + any other tool would do the same ([`shift_selects_with_tool`]).
/// * Ctrl + a draw tool would start a new shape on top ([`draw_bypassing_items`]) — landing that
///   exactly on a handle rather than anywhere else on the item is not something anyone aims for.
///
/// What it BUYS is the DRAGON-370 override: Ctrl held during a text box's handle drag scales the
/// TYPE instead of reflowing the box (Photoshop's paragraph-vs-point modifier). Without it the
/// modifier could only be pressed AFTER the drag was already in flight, which is a trap — you
/// hold Ctrl, press, and nothing happens.
///
/// Two things it deliberately does NOT touch, both because they are unconditional behaviours
/// rather than modifier meanings, and the repo states each as a rule of its own:
///
/// * the PENCIL still inks over a handle (DRAGON-346, "a pencil press is ink, full stop");
/// * the ERASER still sweeps over one (it captures before this whole ladder).
///
/// And it is about the HANDLE, not the item: a modifier press on the item's BODY still
/// selects/toggles exactly as it did, so nothing a user could do is gone — only moved a few px.
pub fn handle_press_beats_modifiers(over_handle: bool) -> bool {
    over_handle
}

/// The band hit-test [`AnnotationCanvas::band_hits`] holds: `(x0, y0, x1, y1)` in image SOURCE
/// px → the ids that band would take. Named because the boxed closure spells out awkwardly at
/// every use.
type BandHits<'a> = Box<dyn Fn(f32, f32, f32, f32) -> Vec<u64> + 'a>;

/// **What a live rubber band WOULD select** (DRAGON-397): the ids that release would leave
/// selected, given what is `existing`ly selected, the `hits` the band currently covers, and
/// whether the band is `additive` (Ctrl/Shift at press — [`additive_select`]).
///
/// This mirrors the app's commit exactly: a plain band REPLACES (`Selection::set_all`), an
/// additive one KEEPS what was selected and appends the new ids, skipping duplicates
/// (`Selection::add_all`) — so a Ctrl-band sweeping an ALREADY-selected item previews it as
/// staying selected, which is what will happen, rather than as a fresh hit. Order matches the
/// commit's too (existing first, then hits in scene order), so the previewed set and the
/// committed one are the same list.
///
/// Deliberately NOT a toggle: only a Ctrl/Shift CLICK toggles ([`AnnotEvent::SelectToggle`]); a
/// band adds. Pure — unit-tested.
pub fn band_preview_ids(existing: &[u64], hits: &[u64], additive: bool) -> Vec<u64> {
    let mut out: Vec<u64> = if additive { existing.to_vec() } else { Vec::new() };
    for id in hits {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    out
}

/// Whether the armed tool owns the WHOLE canvas for CURSOR purposes: one crosshair everywhere
/// over the content, item bodies and resize handles included. True exactly when the next press
/// will not manipulate whatever is under the pointer — the eraser (which sweeps) or any press
/// that bypasses hit-testing ([`draw_bypassing_items`]). The cursor must promise what the press
/// will actually do, so this is derived from the press rule rather than restated. Pure —
/// unit-tested.
pub fn whole_canvas_crosshair(tool: Option<Tool>, ctrl: bool, over_handle: bool) -> bool {
    // The ERASER and the PENCIL are not gated on the handle — both genuinely act over one (see
    // [`handle_press_beats_modifiers`]), so promising a crosshair there is still the truth.
    // Everything else defers to [`draw_bypassing_items`], which now refuses a handle, so Ctrl +
    // a SHAPE tool over a handle shows the resize cursor — which is what that press will do
    // (DRAGON-370).
    tool.is_some_and(Tool::is_eraser) || draw_bypassing_items(tool, ctrl, over_handle).is_some()
}

/// Whether a press with `tool` armed and `ctrl`/`shift` held is an ADDITIVE selection click
/// (DRAGON-341): toggle the hit item into the multi-selection, or extend a rubber band, instead
/// of replacing the selection. ONLY the pointer tool multi-selects — with a draw tool armed Ctrl
/// still means "draw over what's under the cursor" ([`force_new_draw`]), so the two can never
/// both claim the same press.
///
/// `over_handle` is [`HitKind::Resize`], which is never additive ([`handle_press_beats_modifiers`],
/// DRAGON-370) — the item is already selected, so the toggle was a no-op anyway. Pure —
/// unit-tested.
pub fn additive_select(tool: Option<Tool>, ctrl: bool, shift: bool, over_handle: bool) -> bool {
    !handle_press_beats_modifiers(over_handle)
        && tool.is_some_and(Tool::is_pointer)
        && (ctrl || shift)
}

/// Whether a SHIFT-press claims the press for SELECTION while a NON-pointer tool is armed
/// (DRAGON-356): shift is the universal selection modifier, so with the pencil / text / a shape /
/// the eraser (or the neutral state) armed a shift-press toggles the hit item into the
/// multi-selection instead of running the tool. The POINTER tool is EXCLUDED here because it owns
/// its own ctrl/shift path ([`additive_select`], the additive rubber band + toggle) — the two
/// never both claim the same press. Only shift qualifies: Ctrl with a draw tool still means "draw
/// over what's under the cursor" ([`force_new_draw`]).
///
/// `over_handle` is [`HitKind::Resize`], excluded for the same reason [`additive_select`] excludes
/// it (DRAGON-370): a handle only exists on an already-selected item. Gating BOTH is what makes
/// the rule statable as one sentence — a press on a handle resizes, full stop — instead of
/// leaving shift meaning one thing over a handle with the pointer armed and another with the text
/// tool armed. Pure — unit-tested.
pub fn shift_selects_with_tool(tool: Option<Tool>, shift: bool, over_handle: bool) -> bool {
    !handle_press_beats_modifiers(over_handle) && shift && !tool.is_some_and(Tool::is_pointer)
}

/// Whether a press belongs to the TEXT EDITOR (DRAGON-354 item 12 × DRAGON-356) — returns the
/// edited box's id when the press BODY-hits the box that is currently being edited, else `None`.
/// This decision runs BEFORE [`shift_selects_with_tool`], so an in-box press (shift or not) is
/// caret placement / drag-select / shift-extend of the TEXT selection, never an annotation
/// select/toggle. A press that misses the body (a different item, empty canvas, or the edited
/// box's own resize handle) returns `None` and falls through to the normal gates. Pure —
/// unit-tested.
fn text_edit_press_target(
    editing_text: Option<u64>,
    hit_id: Option<u64>,
    hit_is_body: bool,
) -> Option<u64> {
    editing_text.filter(|&eid| hit_is_body && hit_id == Some(eid))
}

/// Whether a plain left press with `tool` armed drops a BRAND-NEW text box WITHOUT consulting
/// what is under the cursor (DRAGON-354 × DRAGON-364).
///
/// The DRAGON-364 change lives here. The text tool used to claim EVERY press over an existing
/// text box and re-open its editor immediately, which left a settled box un-draggable and
/// un-resizable: entering an edit arms the Text tool (`App::edit_existing_text`) and settling
/// does not disarm it, so the very next click re-entered editing instead of grabbing a handle.
/// Now the text tool only claims presses that are NOT over a text box; a press that IS falls
/// through to the shared item lane, where it selects + arms move/resize exactly like the
/// pointer tool and a DOUBLE-click re-opens the editor ([`text_body_reopens_editor`]).
///
/// A press over a NON-text item (a rect, an arrow) still places a new box — unchanged, and the
/// reason this asks about text items specifically rather than "any hit". Ctrl is not consulted:
/// Ctrl + a draw tool is [`draw_bypassing_items`]'s job, checked after this, so Ctrl-pressing an
/// existing text box still lays a new one on top (DRAGON-339). Pure — unit-tested.
pub fn text_press_places_new(tool: Option<Tool>, over_text_item: bool) -> bool {
    tool.is_some_and(Tool::is_text) && !over_text_item
}

/// Whether a plain left press on an EXISTING item re-opens a text editor rather than arming a
/// move/resize (DRAGON-354 × DRAGON-364): a text item's BODY, double-clicked.
///
/// Requiring the BODY is what keeps the two states usable together — a second click on a
/// resize HANDLE is a resize, never an accidental edit. Pure — unit-tested.
pub fn text_body_reopens_editor(is_text_item: bool, hit_is_body: bool, second_click: bool) -> bool {
    is_text_item && hit_is_body && second_click
}

/// Whether the TEXT tool's I-beam holds at a hover (DRAGON-354 × DRAGON-364).
///
/// Derived from the press rule so the cursor can never promise what the press won't do: the
/// I-beam means "this press starts text entry", which after DRAGON-364 is everywhere EXCEPT over
/// an existing text box (where the press selects / moves / resizes). The one exception is the box
/// currently being EDITED, whose body press really does place the caret. Pure — unit-tested.
pub fn text_tool_ibeam(tool: Option<Tool>, over_text_item: bool, over_edited_body: bool) -> bool {
    tool.is_some_and(Tool::is_text) && (!over_text_item || over_edited_body)
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
    /// Drag an arrow's BEND handle (DRAGON-470): the third node, sitting on the middle of the
    /// shaft. It stays under the pointer — the shaft is the quadratic Bezier through tail,
    /// handle and head (`crate::arrow_curve`) — so dragging it bows the arrow and dropping it
    /// back on the chord straightens it again.
    ArrowBend,
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
    /// An arrow from A `(ax, ay)` to B `(bx, by)` (source px), bowed by `bend` (DRAGON-470).
    ///
    /// `bend` is the model's own [`crate::arrow_curve::Bend`], carried verbatim: this widget
    /// derives the drawn shaft's control point, the head's tangent, the bend handle's position
    /// and the hit-test polyline from it through that shared module — the SAME one the
    /// full-resolution bake reads, so what is drawn here and what is exported are one curve.
    /// [`crate::arrow_curve::Bend::STRAIGHT`] takes the original straight-line path everywhere.
    Arrow { ax: f32, ay: f32, bx: f32, by: f32, bend: crate::arrow_curve::Bend },
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
    /// Marks a [`ItemKind::Rect`] item as a TEXT box (DRAGON-354): its glyphs are drawn by the
    /// shared embedded-font raster layer, so [`draw_shapes`] draws NO outline for it — but it
    /// hit-tests, chromes and resizes as the ordinary rect it is (exactly like [`Self::badge`]).
    /// A double-click on a text box re-opens its editor.
    pub text: bool,
}

/// A pointer gesture the canvas publishes — every point is in IMAGE SOURCE pixels
/// (already mapped through [`CanvasMap`]), except [`Self::Menu`] which carries a
/// widget-LOCAL point for placing the context-menu popover.
///
/// Not `Copy`: [`Self::ImeCommit`] carries the OS-composed string (DRAGON-359). Every
/// variant is still constructed inline and consumed once, so `Clone` is enough.
#[derive(Clone, Debug)]
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
    /// Drag update to image point `(x, y)`. `scale_type` is the DRAGON-370 override, sampled
    /// FRESH on every motion event rather than latched at press: Ctrl held during a RESIZE means
    /// "scale the type" — Photoshop's paragraph-vs-point-text modifier. Only a text box acts on
    /// it (`edited_kind`); for every other kind, and for a move or a draw, it is `false` and
    /// carries no meaning. Sampling per event is deliberately better than Photoshop: the user can
    /// change their mind mid-drag and watch the box flip between reflowing and scaling.
    GestureTo(f32, f32, bool),
    /// The active gesture committed (pointer released after a real drag).
    GestureEnd,
    /// Re-open the in-canvas editor on the existing TEXT item `id` (DRAGON-354): emitted by a
    /// press on a text box with the Text tool armed, or a double-click on one under the pointer.
    EditText(u64),
    /// A PRESS inside the actively-edited text box (DRAGON-354 item 12): place the caret at
    /// image point `(x, y)`. `extend` = Shift held (extend the selection from the caret);
    /// `word` = a double-click (select the word under the point); `all` = a TRIPLE-click (select
    /// the whole box, the same target as Cmd/Ctrl+A). `word` and `all` are mutually exclusive.
    TextClick { x: f32, y: f32, extend: bool, word: bool, all: bool },
    /// A drag inside the actively-edited text box (item 12): extend the text selection to image
    /// point `(x, y)`.
    TextDragTo(f32, f32),
    /// The OS input method committed a string while a text box was being edited (DRAGON-359):
    /// insert it at the caret (replacing any selection). This is how the macOS/Windows emoji
    /// picker and CJK composition deliver their result — the OS calls `insertText:`, winit
    /// turns it into an `Ime::Commit`, and iced routes it here as an
    /// [`input_method::Event::Commit`]. The in-flight (uncommitted) composition rides the
    /// over-the-spot preedit overlay instead and never reaches the app.
    ImeCommit(String),
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
    /// DISPLAYED image source pixel dims (fw, fh) — the CROP's size when a crop frames the view
    /// (DRAGON-385), else the whole frame.
    pub source: (f32, f32),
    /// The DISPLAY frame's top-left offset within the full source (SOURCE px), DRAGON-385: the
    /// crop origin when a crop frames the view, else `(0, 0)`. Model coordinates stay FULL-SOURCE
    /// (annotations never move when a crop is applied); this shifts between them and the cropped
    /// on-screen content. `(0, 0)` = un-cropped, byte-identical to before.
    pub offset: (f32, f32),
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
        // Crop-local source px, then shift back into FULL-source coords (DRAGON-385).
        ((q.0 - o.0) * sx + self.offset.0, (q.1 - o.1) * sy + self.offset.1)
    }

    /// FULL-source image pixel → widget-local screen point.
    pub fn to_canvas(self, img: (f32, f32)) -> (f32, f32) {
        let t = self.translate();
        let o = self.origin();
        let dx = if self.source.0 > 0.0 { self.disp.0 / self.source.0 } else { 0.0 };
        let dy = if self.source.1 > 0.0 { self.disp.1 / self.source.1 } else { 0.0 };
        // Full-source → crop-local (DRAGON-385) before the centred fit placement.
        let (ix, iy) = (img.0 - self.offset.0, img.1 - self.offset.1);
        let q = (o.0 + ix * dx, o.1 + iy * dy);
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
    /// Pressed INSIDE the actively-edited text box (DRAGON-354 item 12): a drag extends the
    /// text selection (caret placement already emitted on press).
    TextSelect,
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
    /// The ids the live band WOULD select if released now (DRAGON-397) — the result of
    /// [`band_preview_ids`] over the injected [`AnnotationCanvas::band_hits`], recomputed on each
    /// motion of a moved band and cleared when the gesture ends.
    ///
    /// **Chrome only.** The committed selection still changes exactly once, on release, through
    /// the single `BoxSelect` event: nothing here reaches the app, so the group box, the toolbar's
    /// enabled state and undo are untouched while the band grows.
    band_preview: Vec<u64>,
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
    /// The last left-press that landed on an item (time + id + running consecutive-click count),
    /// for the text-box click LADDER (DRAGON-354 item 12: double = word, triple = select all) and
    /// the double-click-to-reopen. Cleared after a re-open fires or on any non-matching press.
    last_click: Option<(Instant, u64, u8)>,
    /// The IN-FLIGHT OS input-method composition (DRAGON-359), i.e. the uncommitted preedit an
    /// IME shows while composing (CJK, dead keys). Set from `Ime::Preedit`, cleared on
    /// `Ime::Commit`/`Ime::Disabled`. Republished into the `InputMethod::Enabled` strategy each
    /// redraw so iced_winit paints it as an over-the-spot overlay at the cursor area — the
    /// widget's own raster never has to splice it in. `None` when nothing is being composed.
    preedit: Option<input_method::Preedit>,
}

/// The window within which two presses on the SAME item count as a double-click (re-open a
/// text box for editing). Matches the tray's own double-click window for consistency.
const TEXT_DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// What a text-edit press resolves to on the click ladder (DRAGON-354 item 12): a single click
/// places the caret, a double selects the word under it, a triple (or more) selects the whole
/// box. Deliberately capped at "select all" — no line/paragraph step for a 4th click.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TextClickKind {
    Caret,
    Word,
    All,
}

/// The running consecutive-click COUNT for a text-edit press (item 12): `same_within_window` is
/// whether the previous press was on the SAME box within [`TEXT_DOUBLE_CLICK`], `prev_count` its
/// running count. A same-and-in-time press advances the ladder; anything else restarts at 1.
/// Saturates so a very fast repeated click can't wrap. Pure — unit-tested.
fn text_click_count(same_within_window: bool, prev_count: u8) -> u8 {
    if same_within_window {
        prev_count.saturating_add(1)
    } else {
        1
    }
}

/// Map a consecutive-click count to its [`TextClickKind`]: 1 → caret, 2 → word, ≥3 → select all.
/// Pure — unit-tested.
fn text_click_kind(count: u8) -> TextClickKind {
    match count {
        0 | 1 => TextClickKind::Caret,
        2 => TextClickKind::Word,
        _ => TextClickKind::All,
    }
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
    /// The DISPLAY frame's offset within the full source (SOURCE px), DRAGON-385 — see
    /// [`CanvasMap::offset`]. `(0, 0)` un-cropped.
    offset: (f32, f32),
    /// The HAND tool ([`Tool::Hand`]) is armed — a press then belongs to the ZoomPan. Fed by the
    /// app from the armed tool since DRAGON-392 (it was a separate pan MODE flag before), so this
    /// widget's behaviour is unchanged: one bool, one meaning — "a plain drag pans".
    pan_mode: bool,
    accent: Color,
    /// While a text box is being EDITED (DRAGON-354): the blinking caret geometry in image
    /// SOURCE px `(x, y_top, height)`, box-relative to the primary selection's rect. `None`
    /// when not editing OR on a blink-off tick (the app gates the blink by passing `None`), so
    /// the widget just draws it when present.
    text_caret: Option<(f32, f32, f32)>,
    /// The UN-blinked caret geometry (image SOURCE px `(x, y_top, height)`, box-relative to the
    /// primary selection's rect) while a text box is being edited (DRAGON-359). Unlike
    /// [`text_caret`] this is never gated by the blink, so it can drive the OS IME cursor area
    /// (`set_ime_cursor_area`) every frame — the emoji picker / composition candidate window
    /// anchors here. `None` when not editing.
    ime_caret: Option<(f32, f32, f32)>,
    /// The id of the text box currently being EDITED, if any (DRAGON-356 + DRAGON-354 item 12).
    /// Set for the WHOLE edit (never gated by the blink, unlike [`text_caret`]), so a press can
    /// tell an in-box click — which belongs to the TEXT EDITOR (caret placement / drag-select) —
    /// from a click on a DIFFERENT item (a multi-select / shift-toggle). `None` when no box is
    /// being edited.
    editing_text: Option<u64>,
    /// The selection-highlight rectangles of the edited box (image SOURCE px `(x0, y_top, x1,
    /// height)`, box-relative to that box's rect) — painted as a translucent accent wash behind
    /// the glyphs (DRAGON-354 item 12). Empty when there is no text selection.
    text_selection: Vec<(f32, f32, f32, f32)>,
    /// The TEXT annotations' raster layers (DRAGON-373), as `(item id, a DRAW-ONLY element, the
    /// caption's SOURCE-px region `(x, y, w, h)`)`.
    ///
    /// The REGION is the placement (DRAGON-396): each layer is laid out at its own rect, mapped
    /// through the same [`CanvasMap`] the vector geometry uses, and its raster fills that rect.
    /// Previously the layer was stretched across the whole picture with `dest` fractions locating
    /// the caption inside it — equivalent on screen (pinned by
    /// `text_region_placement_matches_the_picture_fraction_form`), but it bounded every caption BY
    /// the picture: a shader primitive is clipped to its own widget rect, so a caption outside the
    /// image could not be drawn at all, whatever the clip said. Placing each at its own rect is
    /// what lets the crop session show them (see [`Self::marks_outside_image`]).
    ///
    /// # Why the canvas draws them
    /// Text is a raster (the glyphs must be pixel-identical to the bake, and re-rendering them
    /// per keystroke must not churn iced's atlas — see `preview/layers.rs`), while box / arrow /
    /// pen / badge are vector geometry drawn HERE, over everything this widget's child drew. A
    /// raster stacked under this widget therefore sat under EVERY vector whatever its depth, so
    /// bringing a caption to the front did nothing on screen even though the export honoured it
    /// (`rasterize_scene` walks all kinds in ONE in-order loop). Drawing the rasters here, at
    /// their own place in [`Self::items`], is what makes the two agree.
    ///
    /// They are DRAW-ONLY: never in the widget tree, never handed an event, laid out by
    /// [`Self::draw`] against the picture rect. Hit-testing works off the item model, so input
    /// stays entirely with this widget — an interactive sibling would swallow presses.
    text_layers: Vec<TextLayerMount<'a, Msg>>,
    /// DISPLAY-ONLY (DRAGON-387): draw the committed scene (vector runs + text rasters) over the
    /// media but own NO interaction — every pointer event forwards to the wrapped ZoomPan and no
    /// selection chrome is drawn. The crop SESSION layers its own overlay on top and owns the
    /// pointer, so the annotations must show through non-interactively. `false` = the ordinary
    /// interactive editor canvas, byte-identical to before.
    display_only: bool,
    /// Draw the committed scene BEYOND the picture rectangle (DRAGON-396) — out to the whole
    /// content area rather than cut at the image edge.
    ///
    /// The crop SESSION sets this: the user is deciding what the crop should include, so a mark
    /// lying outside the current image (in an over-crop's extension, or outside a tightened crop)
    /// has to be visible and identifiable to be judged — "so that we can easily recrop later".
    /// `false` — every other canvas — cuts at the picture, which is where the BAKE cuts.
    marks_outside_image: bool,
    /// Resolve which items a rubber BAND touches, for the LIVE sweep preview (DRAGON-397):
    /// `(x0, y0, x1, y1)` in image SOURCE px (un-normalized, exactly the corners
    /// [`AnnotEvent::BoxSelect`] carries) → the ids that band would take, in scene order.
    ///
    /// It is INJECTED rather than implemented here on purpose: the release path already owns
    /// that rule (`preview::annotate::items_in_band` — arrows test their shaft, pen groups test
    /// every stroke, everything else its drawn bounds), and a preview computed from a second
    /// implementation would eventually lie about what release will do. The app hands in a
    /// closure over the SAME function, so there is exactly one rule. `None` (a display-only
    /// canvas, or any caller that doesn't want it) simply draws no preview.
    band_hits: Option<BandHits<'a>>,
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
            offset: (0.0, 0.0),
            pan_mode,
            accent,
            text_caret: None,
            ime_caret: None,
            editing_text: None,
            text_selection: Vec::new(),
            text_layers: Vec::new(),
            display_only: false,
            marks_outside_image: false,
            band_hits: None,
            on_event: Box::new(on_event),
        }
    }

    /// Make this a DISPLAY-ONLY canvas (DRAGON-387): it draws the committed annotations over the
    /// media but intercepts no pointer event and draws no selection chrome — used by the crop
    /// SESSION, which owns the pointer through its own overlay yet must still show the annotations.
    /// Builder so the constructor arg list stays fixed; an interactive canvas simply never calls it.
    pub fn display_only(mut self, on: bool) -> Self {
        self.display_only = on;
        self
    }

    /// Draw the committed scene beyond the picture rectangle (DRAGON-396) — see
    /// [`Self::marks_outside_image`]. Builder; every canvas but the crop session's leaves it off.
    pub fn marks_outside_image(mut self, on: bool) -> Self {
        self.marks_outside_image = on;
        self
    }

    /// Supply the band hit-test the LIVE sweep preview reads (DRAGON-397) — see
    /// [`Self::band_hits`]. Builder so the constructor arg list stays fixed; a canvas that never
    /// calls it behaves exactly as before (the band's own box alone, everything lighting up on
    /// release).
    pub fn band_hits(
        mut self,
        hits: impl Fn(f32, f32, f32, f32) -> Vec<u64> + 'a,
    ) -> Self {
        self.band_hits = Some(Box::new(hits));
        self
    }

    /// The DISPLAY frame's offset within the full source (SOURCE px), DRAGON-385: the crop origin
    /// when a crop frames the view (`source` is then the crop's size), else `(0, 0)`. Builder so
    /// the constructor arg list stays fixed; an un-cropped canvas simply never calls it.
    pub fn crop_offset(mut self, offset: (f32, f32)) -> Self {
        self.offset = offset;
        self
    }

    /// Supply the TEXT annotations' raster layers as `(item id, draw-only element, SOURCE-px
    /// region)` — see [`Self::text_layers`]. Builder so the constructor arg list stays fixed.
    pub fn text_layers(mut self, layers: Vec<TextLayerMount<'a, Msg>>) -> Self {
        self.text_layers = layers;
        self
    }

    /// The id of the box being edited + its selection-highlight rects (DRAGON-354 item 12).
    /// Builder so the constructor arg list stays fixed.
    pub fn text_editing(
        mut self,
        editing: Option<u64>,
        selection: Vec<(f32, f32, f32, f32)>,
    ) -> Self {
        self.editing_text = editing;
        self.text_selection = selection;
        self
    }

    /// Supply the blinking caret geometry (image SOURCE px `(x, y_top, height)`, box-relative
    /// to the primary selection's rect) while a text box is being edited — `None` on a
    /// blink-off tick or when not editing. Builder so the constructor arg list stays fixed.
    pub fn text_caret(mut self, caret: Option<(f32, f32, f32)>) -> Self {
        self.text_caret = caret;
        self
    }

    /// Supply the UN-blinked caret geometry (image SOURCE px `(x, y_top, height)`, box-relative
    /// to the primary selection's rect) while a text box is being edited (DRAGON-359). Same
    /// geometry as [`text_caret`] but never gated by the blink, so it can position the OS IME
    /// cursor area every frame. `None` when not editing. Builder so the arg list stays fixed.
    pub fn ime_caret(mut self, caret: Option<(f32, f32, f32)>) -> Self {
        self.ime_caret = caret;
        self
    }

    /// The caret rectangle in GLOBAL (window-logical) coordinates for the OS IME cursor area
    /// (DRAGON-359). Mirrors the caret DRAW: box-relative source px → canvas-local via
    /// [`CanvasMap`], offset by the widget's bounds origin so it is window-relative like every
    /// iced layout bound (what `set_ime_cursor_area` expects). `None` until the app supplies an
    /// un-blinked caret for the primary text box.
    fn ime_cursor_rect(&self, bounds: Rectangle, map: CanvasMap) -> Option<Rectangle> {
        let (cx, cy, ch) = self.ime_caret?;
        let primary = self.primary();
        let &ItemKind::Rect { x, y, .. } = self
            .items
            .iter()
            .find(|i| Some(i.id) == primary && i.text)
            .map(|i| &i.kind)?
        else {
            return None;
        };
        let top = map.to_canvas((x + cx, y + cy));
        let bot = map.to_canvas((x + cx, y + cy + ch));
        Some(ime_cursor_rect_from(top, bot, (bounds.x, bounds.y)))
    }

    fn map(&self, bounds: Rectangle) -> CanvasMap {
        CanvasMap {
            bounds: (bounds.width, bounds.height),
            zoom: self.zoom,
            pan: self.pan,
            disp: self.disp,
            source: self.source,
            offset: self.offset,
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

    /// Whether `id` is a TEXT box (DRAGON-354) — the kind a press/double-click re-opens for
    /// editing.
    fn is_text_item(&self, id: u64) -> bool {
        self.items.iter().any(|i| i.id == id && i.text)
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

    /// The screen-px rect `(left, top, right, bottom)` an item's selection chrome sits on — the
    /// SAME rect the draw + hit-test use per kind (DRAGON-388): a rect/pen grows by
    /// [`box_chrome_rect`], an arrow by its span padded like the secondary-arrow chrome. Shared
    /// so the group box (below) unions exactly what each member draws.
    fn item_chrome_screen_rect(&self, map: &CanvasMap, item: &Item) -> (f32, f32, f32, f32) {
        let from_global = |r: GlobalRect| (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
        match &item.kind {
            ItemKind::Rect { x, y, w, h } => {
                from_global(box_chrome_rect(map, *x, *y, *w, *h, item.stroke_w))
            }
            ItemKind::Path { paths, .. } => {
                let (x, y, w, h) = path_bounds(paths);
                from_global(box_chrome_rect(map, x, y, w, h, item.stroke_w))
            }
            ItemKind::Arrow { ax, ay, bx, by, bend } => {
                // The CURVE's box (DRAGON-470), so a bow's chrome wraps the ink rather than the
                // chord. A straight arrow's box is its two endpoints, exactly as before.
                let (x0, y0, x1, y1) =
                    crate::arrow_curve::spanned_bounds((*ax, *ay), (*bx, *by), *bend);
                let r = box_screen_rect(map, x0, y0, x1 - x0, y1 - y0);
                let pad =
                    (HIT_PAD + item.stroke_w * map.img_to_screen_scale() * 0.5).round() as i32;
                (
                    (r.left - pad) as f32,
                    (r.top - pad) as f32,
                    (r.right + pad) as f32,
                    (r.bottom + pad) as f32,
                )
            }
        }
    }

    /// The GROUP selection box (DRAGON-388): the union of every SELECTED item's chrome rect in
    /// screen px `(left, top, right, bottom)`, or `None` when nothing is selected. Derived state —
    /// recomputed each draw/hit-test as the selection or geometry changes, never stored.
    fn group_chrome_rect(&self, map: &CanvasMap) -> Option<(f32, f32, f32, f32)> {
        let mut acc: Option<(f32, f32, f32, f32)> = None;
        for item in self.items.iter().filter(|i| self.is_selected(i.id)) {
            let (l, t, r, b) = self.item_chrome_screen_rect(map, item);
            acc = Some(match acc {
                None => (l, t, r, b),
                Some((al, at, ar, ab)) => (al.min(l), at.min(t), ar.max(r), ab.max(b)),
            });
        }
        acc
    }

    /// Hit-test in precedence order: the PRIMARY selected item's resize HANDLES first (they
    /// exist ONLY for it, drawn HIT_PAD outside it), then ANY item's BODY top-most first (a body
    /// press selects + moves), then empty. So an unselected item has no grabbable handles — you
    /// select it (body-click) to reveal them. In a MULTI-selection the handles ride the GROUP
    /// BOX (DRAGON-388), so a resize scales the whole set in unison; single selection keeps its
    /// handles on the item itself. Then (DRAGON-390) the group box's own border + empty interior
    /// grab as a Body hit on the primary, so dragging the box moves the whole group.
    /// The raster layer belonging to item `id` — the element and its SOURCE-px region — if it has
    /// one (DRAGON-373). A text box that is still blank has none — there is nothing to draw — so it
    /// just rides the vector run.
    #[allow(clippy::type_complexity)]
    fn text_layer_for(&self, id: u64) -> Option<(&cosmic::Element<'a, Msg>, (f32, f32, f32, f32))> {
        self.text_layers.iter().find(|(lid, ..)| *lid == id).map(|(_, el, region)| (el, *region))
    }

    fn hit_at(&self, map: &CanvasMap, p: (f32, f32)) -> Option<(u64, HitKind)> {
        let g = (p.0 as i32, p.1 as i32);
        // 1. The SELECTED item's HANDLES win over everything (top precedence). Handles are
        //    the 8 drawn circles (corners + edge midpoints) on the chrome rect / the arrow's
        //    two endpoint nodes — ONLY those resize, NOT the whole perimeter (the rest of the
        //    stroke moves, via Body in step 2).
        if self.selection.len() > 1 {
            // A MULTI-selection wears its handles on the GROUP BOX (DRAGON-388), the union of
            // every selected item's chrome rect — NOT the primary. A hit on one opens a group
            // SCALE (the app scales the whole set), so the id is just the primary for routing.
            if let Some((l, t, r, b)) = self.group_chrome_rect(map)
                && let Some(grab) = rect_handle_grab(l, t, r, b, g)
                && let Some(pid) = self.primary()
            {
                return Some((pid, HitKind::Resize(grab)));
            }
        } else if let Some(sel) = self.selected_item() {
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
                ItemKind::Arrow { ax, ay, bx, by, bend } => {
                    let (an, bn) = arrow_nodes(map, ax, ay, bx, by, bend, sel.stroke_w);
                    if (p.0 - an.0).hypot(p.1 - an.1) <= HANDLE_GRAB {
                        return Some((sel.id, HitKind::Resize(Grab::ArrowA)));
                    }
                    if (p.0 - bn.0).hypot(p.1 - bn.1) <= HANDLE_GRAB {
                        return Some((sel.id, HitKind::Resize(Grab::ArrowB)));
                    }
                    // The BEND node last (DRAGON-470): near the length threshold its disc can
                    // still clip an endpoint node's, and the endpoints are what a user reaches
                    // for there. It is DRAWN under them for the same reason, so what wins the
                    // click is what looks like it is on top.
                    if let Some(cn) = arrow_bend_node(map, ax, ay, bx, by, bend)
                        && (p.0 - cn.0).hypot(p.1 - cn.1) <= HANDLE_GRAB
                    {
                        return Some((sel.id, HitKind::Resize(Grab::ArrowBend)));
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
                &ItemKind::Arrow { ax, ay, bx, by, bend } => {
                    // Shaft grab tolerance from the OUTER drawn stroke edge: ARROW_GRAB + stroke/2,
                    // plus HIT_PAD ONLY for the selected arrow (breathing room), strict otherwise.
                    let pad = if selected { HIT_PAD } else { 0.0 };
                    let tol = ARROW_GRAB + pad + item.stroke_w * map.img_to_screen_scale() * 0.5;
                    // Measured against the SHAFT AS DRAWN (DRAGON-470): the chord when straight
                    // (the historical single-segment test), the flattened curve when bent — a
                    // bowed arrow must be grabbable along its ink, not along a chord the ink left.
                    if arrow_distance(map, (ax, ay), (bx, by), bend, p) <= tol {
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
        // 3. The GROUP BOX itself (DRAGON-390): with a multi-selection, a press on the dashed
        //    border or the empty interior inside the union — anything the group handles (step 1)
        //    and every annotation body (step 2) missed — grabs the WHOLE group. Routed through the
        //    primary id as a Body hit, so the existing "a press inside a multi-selection keeps it
        //    whole" lane arms MoveMany and the right-click lane opens the shared menu, both with
        //    the selection left intact — no new event, no app-side change.
        if self.selection.len() > 1
            && let Some((l, t, r, b)) = self.group_chrome_rect(map)
            && group_box_grab(l, t, r, b, g)
            && let Some(pid) = self.primary()
        {
            return Some((pid, HitKind::Body));
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
///
/// On a BENT arrow (DRAGON-470) "the axis" is each end's own TANGENT, so both nodes stay in line
/// with the shaft as it actually leaves and arrives. A straight arrow keeps the chord direction,
/// computed exactly as it always was.
fn arrow_nodes(
    map: &CanvasMap,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    bend: crate::arrow_curve::Bend,
    stroke_src: f32,
) -> ((f32, f32), (f32, f32)) {
    let a = map.to_canvas((ax, ay));
    let b = map.to_canvas((bx, by));
    let off = HIT_PAD + stroke_src * map.img_to_screen_scale() * 0.5;
    if bend.is_straight() {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / len, dy / len);
        return ((a.0 - ux * off, a.1 - uy * off), (b.0 + ux * off, b.1 + uy * off));
    }
    let c = map.to_canvas(crate::arrow_curve::control((ax, ay), (bx, by), bend));
    let t = crate::arrow_curve::tail_dir(a, c, b);
    let h = crate::arrow_curve::head_dir(a, c, b);
    ((a.0 - t.0 * off, a.1 - t.1 * off), (b.0 + h.0 * off, b.1 + h.1 * off))
}

/// The shortest ON-SCREEN shaft (px) that still gets a BEND node (DRAGON-470).
///
/// The three handle discs each grab within [`HANDLE_GRAB`] of their centre, and the bend disc sits
/// in the MIDDLE of the shaft, where the body-drag (move) hit lives. Below `4 · HANDLE_GRAB` the
/// bend disc's 2·`HANDLE_GRAB` span leaves less than `HANDLE_GRAB` of shaft on either side of it,
/// so between the three of them a short arrow has nowhere left to grab for a MOVE — and a
/// single-selected arrow has no group box to fall back on, which would leave the keyboard as the
/// only way to shift it. So a short arrow (or any arrow at a zoom that makes it short) simply does
/// not offer the node: it cannot be bent until it is big enough to be bent usefully, which is the
/// same trade the editor already makes for a shape too small to show its handles.
const BEND_NODE_MIN_SHAFT: f32 = 4.0 * HANDLE_GRAB;

/// Whether an arrow whose DRAWN shaft measures `shaft_screen_len` px on screen offers its bend
/// node at all — see [`BEND_NODE_MIN_SHAFT`]. Consulted by the draw AND the hit-test through the
/// one [`arrow_bend_node`], so what is painted and what is grabbable can never disagree. Pure —
/// unit-tested.
fn bend_node_offered(shaft_screen_len: f32) -> bool {
    shaft_screen_len.is_finite() && shaft_screen_len >= BEND_NODE_MIN_SHAFT
}

/// The screen position of an arrow's BEND node (DRAGON-470): the middle of the drawn shaft, i.e.
/// the curve's own midpoint. Unlike the endpoint nodes it is NOT pushed outward — it is the point
/// the curve is defined to pass through, so it must sit exactly where the ink does, and dragging
/// it takes the shaft with it. On a straight arrow it is the chord midpoint.
///
/// `None` when the shaft is too short on screen to carry a third disc ([`bend_node_offered`]).
/// This is the ONE place that decision is made, so the node is drawn exactly when it is grabbable.
fn arrow_bend_node(
    map: &CanvasMap,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    bend: crate::arrow_curve::Bend,
) -> Option<(f32, f32)> {
    let a = map.to_canvas((ax, ay));
    let b = map.to_canvas((bx, by));
    let c = map.to_canvas(crate::arrow_curve::control((ax, ay), (bx, by), bend));
    // The shaft AS DRAWN, in screen px: the chord when straight, the arc when bent.
    bend_node_offered(crate::arrow_curve::arc_len(a, c, b))
        .then(|| map.to_canvas(crate::arrow_curve::handle((ax, ay), (bx, by), bend)))
}

/// The smallest screen-px distance from `p` (widget-local) to an arrow's SHAFT AS DRAWN: the
/// chord for a straight arrow — the identical single-segment measure this widget always used —
/// and the flattened quadratic for a bent one (DRAGON-470).
///
/// The polyline reads up to [`ARROW_CURVE_TOL`] further from the curve than the curve itself, a
/// tenth of [`ARROW_GRAB`]. That holds until the curve needs more than `arrow_curve::FLATTEN_MAX`
/// segments, past which the error is `|a − 2c + b| / (4 · FLATTEN_MAX²)` in screen px instead —
/// see [`crate::arrow_curve::flatten`]. Reaching that needs a `|a − 2c + b|` over 260 000 screen
/// px, orders past what a bent arrow can produce at this editor's zoom ceiling, so in practice
/// the tolerance is the tenth of a grab radius.
fn arrow_distance(
    map: &CanvasMap,
    a_src: (f32, f32),
    b_src: (f32, f32),
    bend: crate::arrow_curve::Bend,
    p: (f32, f32),
) -> f32 {
    let a = map.to_canvas(a_src);
    let b = map.to_canvas(b_src);
    if bend.is_straight() {
        return point_near_segment(p, a, b);
    }
    let c = map.to_canvas(crate::arrow_curve::control(a_src, b_src, bend));
    crate::arrow_curve::flatten(a, c, b, ARROW_CURVE_TOL)
        .windows(2)
        .fold(f32::INFINITY, |best, w| best.min(point_near_segment(p, w[0], w[1])))
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
        // DRAGON-387: a display-only canvas owns no cursor — defer entirely to the wrapped ZoomPan
        // (the crop overlay above resolves the crop cursors itself).
        if self.display_only {
            return self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            );
        }
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
            // A live rubber band keeps the POINTER's own cursor (DRAGON-468). It used to swap to
            // the crosshair for the drag (DRAGON-341, "a band reads as a marquee draw"), which is
            // the draw tools' cursor and made a selection sweep look like it was about to leave a
            // shape behind. Deferring to the ZoomPan is exactly what the idle pointer over empty
            // canvas already resolves to (see the `None` arm below), so the band itself adds NO
            // cursor change: press, drag and release resolve to whatever the hover already did.
            // (The ZoomPan can still answer Grab/Grabbing for its OWN reasons mid-band, e.g. Alt
            // pressed during the drag — that is its cursor, unchanged by this arm, and it is what
            // the same hover would show.)
            //
            // Positionally this returns BEFORE the `cursor_reassert` dip gate below (DRAGON-331),
            // as every live-gesture arm does. That is benign here rather than a bypass: the value
            // returned IS what the dip would return, so a band that starts inside the post-enter
            // window still resolves to the default cursor and the re-assert schedule (driven from
            // `update`) is untouched.
            Pending::Band { .. } => return child(),
            // Drag-selecting text (item 12) keeps the I-beam.
            Pending::TextSelect => return mouse::Interaction::Text,
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
        let map = self.map(bounds);
        let hit = self.hit_at(&map, local);
        let over_handle = matches!(hit, Some((_, HitKind::Resize(_))));
        // The eraser, the PENCIL (DRAGON-346) and Ctrl + a draw tool (DRAGON-339) each own the
        // WHOLE canvas: their press never manipulates what is under it, so one crosshair holds
        // everywhere over the content — above item bodies and, EXCEPT for the eraser, up to but
        // not including a selected item's resize handle (DRAGON-370: that press resizes). Derived
        // from the press rule itself ([`whole_canvas_crosshair`]) so the cursor can never promise
        // something the press won't do — which is why the hit test now runs first.
        if whole_canvas_crosshair(self.tool, state.mods.control(), over_handle) {
            return mouse::Interaction::Crosshair;
        }
        // The TEXT tool (DRAGON-354) shows an I-beam wherever a press starts TEXT ENTRY, which
        // since DRAGON-364 is everywhere EXCEPT over an existing text box — those presses now
        // select / move / resize, so they wear the ordinary manipulation cursors from the match
        // below. The box actually being EDITED keeps the I-beam over its body: that press really
        // does place the caret. Derived through [`text_tool_ibeam`] from the press rule itself.
        if text_tool_ibeam(
            self.tool,
            hit.map(|(id, _)| id).is_some_and(|id| self.is_text_item(id)),
            text_edit_press_target(
                self.editing_text,
                hit.map(|(id, _)| id),
                matches!(hit, Some((_, HitKind::Body))),
            )
            .is_some(),
        ) {
            return mouse::Interaction::Text;
        }
        // Idle hover: the selected item's handle shows its resize cursor, any item's body
        // the open-hand grab; empty canvas shows the draw crosshair when a draw tool is
        // active, else defer to the ZoomPan.
        match hit {
            Some((_, HitKind::Resize(g))) => grab_cursor(g),
            Some((_, HitKind::Body)) => mouse::Interaction::Grab,
            None => {
                // Only a tool that actually DRAWS promises a crosshair over empty canvas; the
                // pointer (DRAGON-341) rubber-bands, which reads as the plain arrow — and since
                // DRAGON-468 stays that way THROUGH the drag (see the `Pending::Band` arm above),
                // so the pointer never borrows a draw cursor. The neutral state defers likewise.
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
        // DRAGON-387: a display-only canvas owns no interaction — every event goes straight to the
        // wrapped ZoomPan (the crop overlay above owns the pointer). Returns before any modifier
        // tracking / hit-testing / capture, so it can never claim a press.
        if self.display_only {
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
            return;
        }
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
        // ── OS input-method bridge (DRAGON-359) ───────────────────────────────────────────
        // A custom canvas editor never told the OS a text field was focused, so the emoji
        // picker (Ctrl+Cmd+Space / fn-E on macOS) and CJK composition had nothing to target.
        // While a text box is being edited we PUBLISH an `InputMethod::Enabled` strategy every
        // redraw — iced_winit reads it there and calls `set_ime_allowed(true)` +
        // `set_ime_cursor_area(caret)` — and turn the OS commit/preedit events (winit `Ime::*`,
        // routed here as `Event::InputMethod`) into an insertion / an over-the-spot overlay.
        // The insertion piggybacks the same `on_event` channel as every pointer gesture; the
        // in-flight preedit lives in this widget's `State` so no app/model change is needed.
        if self.editing_text.is_some() {
            match event {
                Event::Window(cosmic::iced::core::window::Event::RedrawRequested(_)) => {
                    // Mirror `text_input`: the strategy is only harvested on the redraw pass
                    // (`user_interface::State::Updated { input_method, .. }`), so publish here.
                    if let Some(cursor) = self.ime_cursor_rect(bounds, map) {
                        let state = tree.state.downcast_ref::<State>();
                        shell.request_input_method(&InputMethod::Enabled {
                            cursor,
                            purpose: Purpose::Normal,
                            preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
                        });
                    }
                }
                Event::InputMethod(ime) => {
                    let state = tree.state.downcast_mut::<State>();
                    match ime {
                        input_method::Event::Opened => {
                            state.preedit = Some(input_method::Preedit::new());
                        }
                        input_method::Event::Closed => {
                            state.preedit = None;
                        }
                        input_method::Event::Preedit(content, selection) => {
                            state.preedit = Some(input_method::Preedit {
                                content: content.clone(),
                                selection: selection.clone(),
                                text_size: None,
                            });
                        }
                        input_method::Event::Commit(text) => {
                            state.preedit = None;
                            self.emit(shell, AnnotEvent::ImeCommit(text.clone()));
                        }
                    }
                    // A composing/committing IME event is fully handled here; keep it off the
                    // wrapped ZoomPan and force a redraw so the strategy + overlay refresh.
                    shell.request_redraw();
                    shell.capture_event();
                    consumed = true;
                }
                _ => {}
            }
        } else {
            // The edit session ended (settle/Escape) — possibly MID-composition, where no
            // Ime::Closed has arrived yet. Drop any stale preedit so the next session's first
            // publish can't flash the old composition overlay.
            let state = tree.state.downcast_mut::<State>();
            if state.preedit.is_some() {
                state.preedit = None;
            }
        }
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
                                // ── Press decision matrix while a text box is being EDITED
                                // (DRAGON-354 item 12 × DRAGON-356 reconciliation) ──────────
                                // A press whose BODY hit is the actively-edited box belongs to
                                // the TEXT EDITOR, and takes PRIORITY over DRAGON-356's shift
                                // gate below: it places the caret (Shift EXTENDS the text
                                // selection, a double-click selects the word) and arms a
                                // drag-select. A shift-press inside the edited box therefore
                                // extends the TEXT selection — it does NOT toggle the box in the
                                // annotation multi-selection. Presses that DON'T body-hit the
                                // edited box fall through: a shift-press on a DIFFERENT item (or
                                // the edited box's own resize handle) goes to DRAGON-356's gate;
                                // everything else to the tool gates. (Resizing a text box still
                                // needs the pointer tool, as before — the Text tool's own press
                                // path re-opens the editor rather than resizing.)
                                let hit = self.hit_at(&map, local);
                                // DRAGON-370: no MODIFIER gate may claim a press on a selected
                                // item's RESIZE HANDLE — see [`handle_press_beats_modifiers`].
                                // Every modifier gate below consults it, which is what lets Ctrl
                                // be held BEFORE the press to arm the text scale override.
                                let over_handle = matches!(hit, Some((_, HitKind::Resize(_))));
                                let edit_body_hit = text_edit_press_target(
                                    self.editing_text,
                                    hit.map(|(id, _)| id),
                                    matches!(hit, Some((_, HitKind::Body))),
                                );
                                if let Some(eid) = edit_body_hit {
                                    let now = Instant::now();
                                    // The click ladder (item 12): a same-box press within the
                                    // window advances the count — 1 caret / 2 word / 3+ select all.
                                    let same = state.last_click.is_some_and(|(t, lid, _)| {
                                        lid == eid && now.duration_since(t) <= TEXT_DOUBLE_CLICK
                                    });
                                    let prev_count = state.last_click.map_or(0, |(_, _, c)| c);
                                    let count = text_click_count(same, prev_count);
                                    state.last_click = Some((now, eid, count));
                                    let (word, all) = match text_click_kind(count) {
                                        TextClickKind::Caret => (false, false),
                                        TextClickKind::Word => (true, false),
                                        TextClickKind::All => (false, true),
                                    };
                                    self.emit(
                                        shell,
                                        AnnotEvent::TextClick {
                                            x: state.press_img.0,
                                            y: state.press_img.1,
                                            extend: state.mods.shift(),
                                            word,
                                            all,
                                        },
                                    );
                                    state.pending = Pending::TextSelect;
                                    shell.capture_event();
                                    consumed = true;
                                } else if shift_selects_with_tool(
                                    self.tool,
                                    state.mods.shift(),
                                    over_handle,
                                ) {
                                    // SHIFT-SELECT FROM ANY TOOL (DRAGON-356): shift is the
                                    // universal selection modifier, so while a NON-pointer tool
                                    // (pencil, text, a shape, the eraser) is armed a shift-press
                                    // claims the press for SELECTION instead of running the tool.
                                    // The decision matrix (an in-box text edit takes priority):
                                    //   * hit the box being EDITED  → no-op: the press belongs to
                                    //     the text editor (caret/selection); captured so the tool
                                    //     can't also act on it.
                                    //   * hit any OTHER item         → TOGGLE it into the
                                    //     multi-selection (the app settles a live edit of a
                                    //     different box first, like a click-away). No stroke, no
                                    //     new text box — the modifier claims the press.
                                    //   * hit EMPTY canvas           → no-op: no draw, no band (a
                                    //     miss must never surprise with a stroke); the event
                                    //     forwards so the ZoomPan can still pan/scroll.
                                    // Pen groups stay pointer-only (`pen_selectable`), so shift
                                    // over ink with a non-pointer tool reads as empty. The armed
                                    // tool is NEVER switched. The pointer tool keeps its own
                                    // ctrl/shift path (`additive_select`, additive band + toggle)
                                    // untouched below.
                                    state.pending = Pending::None;
                                    if let Some(hit_id) = self.topmost_at(&map, local) {
                                        if self.editing_text != Some(hit_id) {
                                            self.emit(shell, AnnotEvent::SelectToggle(hit_id));
                                        }
                                        shell.capture_event();
                                        consumed = true;
                                    }
                                } else if self.tool.is_some_and(Tool::is_eraser) {
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
                                } else if text_press_places_new(
                                    self.tool,
                                    hit.map(|(id, _)| id).is_some_and(|id| self.is_text_item(id)),
                                ) {
                                    // TEXT tool (DRAGON-354), press NOT over an existing text box:
                                    // start a brand-new one WITHOUT hit-testing (a bare click
                                    // drops an auto box, a drag lays out a fixed-width one) — the
                                    // same lazy-draw path the pencil uses, so a click that never
                                    // drags still settles through the click-place branch on
                                    // release. A press that IS over a text box deliberately falls
                                    // THROUGH to the shared item lane below (DRAGON-364), where it
                                    // selects + arms move/resize and a double-click re-opens the
                                    // editor — the two-state model.
                                    self.emit(shell, AnnotEvent::Select(None));
                                    state.pending = Pending::Draw(Tool::Text);
                                } else if let Some(t) =
                                    draw_bypassing_items(self.tool, state.mods.control(), over_handle)
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
                                } else if let Some((id, hit)) = hit {
                                    // An existing item is CANVAS-owned: capture + own it. Since
                                    // DRAGON-364 the TEXT tool reaches here too (over a text box),
                                    // so this ONE lane now serves both tools: single click =
                                    // selected-not-editing (drag + resize), double click = edit.
                                    if additive_select(
                                        self.tool,
                                        state.mods.control(),
                                        state.mods.shift(),
                                        over_handle,
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
                                        // DOUBLE-CLICK a text box body → re-open its editor
                                        // (DRAGON-354); otherwise arm the normal move/resize.
                                        let now = Instant::now();
                                        let dbl = text_body_reopens_editor(
                                            self.is_text_item(id),
                                            matches!(hit, HitKind::Body),
                                            state.last_click.is_some_and(|(t, lid, _)| {
                                                lid == id
                                                    && now.duration_since(t) <= TEXT_DOUBLE_CLICK
                                            }),
                                        );
                                        state.last_click = Some((now, id, 1));
                                        if dbl {
                                            self.emit(shell, AnnotEvent::EditText(id));
                                            state.last_click = None;
                                            state.pending = Pending::None;
                                        } else {
                                            state.pending = match hit {
                                                HitKind::Resize(g) => Pending::Resize(g),
                                                HitKind::Body => Pending::Move,
                                            };
                                        }
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
                                    // Empty canvas — there is no hit at all, so no handle.
                                    let additive = additive_select(
                                        self.tool,
                                        state.mods.control(),
                                        state.mods.shift(),
                                        false,
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
                                        self.emit(shell, AnnotEvent::GestureTo(img.0, img.1, false));
                                    }
                                    Pending::Move => {
                                        if !state.begun {
                                            self.emit(shell, AnnotEvent::GrabBegin(Grab::Move, state.press_img.0, state.press_img.1));
                                            state.begun = true;
                                        }
                                        self.emit(shell, AnnotEvent::GestureTo(img.0, img.1, false));
                                    }
                                    Pending::Resize(g) => {
                                        if !state.begun {
                                            self.emit(shell, AnnotEvent::GrabBegin(g, state.press_img.0, state.press_img.1));
                                            state.begun = true;
                                        }
                                        // DRAGON-370: the ONE place the override is read, and it
                                        // is read NOW rather than remembered from the press — see
                                        // `AnnotEvent::GestureTo`.
                                        self.emit(
                                            shell,
                                            AnnotEvent::GestureTo(img.0, img.1, state.mods.control()),
                                        );
                                    }
                                    // The rubber band publishes NOTHING while it grows — the
                                    // selection still lands in ONE `BoxSelect` on release
                                    // (DRAGON-341). What it DOES do is re-resolve which items it
                                    // is covering (DRAGON-397) so `draw` can put the selection
                                    // box on them live; that stays inside this widget's own
                                    // state, so no message, no app update, no re-raster rides on
                                    // a motion event (DRAGON-376's lesson).
                                    Pending::Band { additive } => {
                                        state.band_to = local;
                                        state.band_preview = match &self.band_hits {
                                            Some(hits) => band_preview_ids(
                                                &self.selection,
                                                &hits(
                                                    state.press_img.0,
                                                    state.press_img.1,
                                                    img.0,
                                                    img.1,
                                                ),
                                                additive,
                                            ),
                                            None => Vec::new(),
                                        };
                                    }
                                    // Drag-select inside the edited text box (item 12): extend
                                    // the text selection to the current point every motion.
                                    Pending::TextSelect => {
                                        self.emit(shell, AnnotEvent::TextDragTo(img.0, img.1));
                                    }
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
                        // The live sweep preview belongs to the gesture: the committed selection
                        // takes over the instant `BoxSelect` lands (DRAGON-397).
                        state.band_preview.clear();
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
        // The picture's on-screen rectangle (GLOBAL coords) — the clip source above and the
        // placement the text layers' `dest` fractions are relative to.
        // The DISPLAYED picture is the crop REGION (offset..offset+source) in full-source
        // coords (DRAGON-385) — offset `(0, 0)` un-cropped, byte-identical to the old corners.
        let picture = region_on_screen(
            &map,
            (ox, oy),
            (map.offset.0, map.offset.1, map.source.0, map.source.1),
        );
        // DRAGON-396: a crop SESSION lifts that cut. The user is choosing what the crop should
        // contain, so a mark lying outside the current image must be visible to be judged — it is
        // drawn out to the whole content rect instead (still bounded away from the scrollbars, and
        // the crop overlay's own scrim above dims whatever falls outside the crop rect, so the two
        // states stay distinguishable WITHOUT rendering the marks any differently).
        let shape_clip = if self.marks_outside_image {
            clip
        } else {
            picture
                .intersection(&clip)
                .unwrap_or(Rectangle { x: 0.0, y: 0.0, width: 0.0, height: 0.0 })
        };
        // 2. The committed scene IN ITEM ORDER, CLIPPED to the IMAGE rect: runs of vector
        //    geometry (crisp at any zoom) with each TEXT item's raster layer drawn at its own
        //    place between them (DRAGON-373). A canvas `Frame` builds the geometry in
        //    widget-LOCAL coords; the `with_translation` maps it to global, and iced scissors the
        //    geometry to the surrounding `with_layer` clip. Vector redraw each frame = no atlas
        //    churn = no flicker (the whole reason the raster display layer was retired).
        //
        //    WHY each run gets its own `with_layer`: within ONE iced layer the primitive TYPES
        //    draw in a fixed order (quads, then geometry, then shader primitives, then images,
        //    then text) — submission order does NOT decide it. A layer BOUNDARY does, and iced's
        //    layer merge only fuses neighbours whose type ranges already agree, so the order
        //    written here is the order drawn. That is what lets a rectangle sit over one caption
        //    and under another, exactly as `rasterize_scene` bakes it.
        for pass in draw_passes(&self.items, |id| self.text_layer_for(id).is_some()) {
            match pass {
                DrawPass::Shapes(from, to) => {
                    let run = &self.items[from..to];
                    renderer.with_layer(shape_clip, |renderer| {
                        let mut frame =
                            canvas::Frame::new(renderer, Size::new(bounds.width, bounds.height));
                        draw_shapes(&mut frame, &map, run);
                        let geometry = frame.into_geometry();
                        renderer.with_translation(Vector::new(ox, oy), |renderer| {
                            renderer.draw_geometry(geometry);
                        });
                    });
                }
                DrawPass::TextLayer(i) => {
                    if let Some((layer, region)) = self.text_layer_for(self.items[i].id) {
                        // Ordinarily the layer is stretched across the PICTURE and its `dest`
                        // fractions locate the caption inside it. In a crop session it is placed at
                        // ITS OWN region instead, through the same map as the vectors (DRAGON-396):
                        // a shader is clipped to its own widget rect, so a picture-wide layer could
                        // never draw a caption that sits outside the image. The two forms put it in
                        // the same place — see the placement-equivalence test.
                        let place = if self.marks_outside_image {
                            region_on_screen(&map, (ox, oy), region)
                        } else {
                            picture
                        };
                        draw_placed(layer, renderer, theme, style, place, shape_clip, viewport);
                    }
                }
            }
        }
        // A display-only canvas (DRAGON-387) stops after the committed scene: no selection chrome,
        // no caret, no rubber band — the crop session shows the annotations, not the editing UI.
        if self.display_only {
            return;
        }
        // 3. The annotation CHROME (selection boxes + handles) and the pointer's rubber band on
        //    top — same content clip. A single selection gets a dashed box with handles on the
        //    item; a multi-selection (DRAGON-388) gives each member a half-opacity solid box with
        //    NO handles and adds ONE dashed GROUP box, wearing the handles, around their union.
        let state = tree.state.downcast_ref::<State>();
        let band = match state.pending {
            Pending::Band { .. } if state.moved => Some((state.press_screen, state.band_to)),
            _ => None,
        };
        // (A live band always draws — its own washed box, and since DRAGON-397 the swept items'
        // boxes — so the `band.is_none()` guard already covers the preview.)
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
            // The pointer's live rubber band (DRAGON-341), restyled by DRAGON-468: a 1px solid
            // accent border around a mostly translucent accent wash, drawn even when nothing is
            // selected yet. The dashed marquee it replaced looked like the draw tools' "a shape
            // lands here" affordance rather than a selection sweep.
            if let Some((a, b)) = band {
                band_rect(
                    &mut fill,
                    a.0.min(b.0),
                    a.1.min(b.1),
                    (b.0 - a.0).abs(),
                    (b.1 - a.1).abs(),
                    accent,
                );
            }
            // The LIVE sweep preview (DRAGON-397): every item the band is currently covering wears
            // the same half-opacity solid box a selected member does, so the user watches items
            // take the selection box as the band reaches them and drop it as it retreats. Items
            // ALREADY selected are skipped — the committed chrome below draws them (a plain band
            // deselected everything at press, so in practice this is only the additive case).
            //
            // It is CHROME ONLY: `self.selection` is untouched until `BoxSelect` lands on release,
            // so the group box, the handles and everything the app derives from the selection stay
            // put while the band grows. The set is resolved on motion (see `update`), never here —
            // `draw` does no hit-testing.
            if band.is_some() && !state.band_preview.is_empty() {
                let mut half = accent;
                half.a *= 0.5;
                for item in self
                    .items
                    .iter()
                    .filter(|i| state.band_preview.contains(&i.id) && !self.is_selected(i.id))
                {
                    let (l, t, rr, bb) = self.item_chrome_screen_rect(&map, item);
                    solid_rect(&mut fill, l, t, rr - l, bb - t, half);
                }
            }
            // With more than one item selected (DRAGON-388) each member wears a SOLID box at half
            // opacity with NO handles, and a single GROUP box (below) carries the handles. Single
            // selection keeps the historical dashed box + on-item handles, byte-identical.
            let multi = self.selection.len() > 1;
            for item in self.items.iter().filter(|i| self.is_selected(i.id)) {
                if multi {
                    let (l, t, rr, bb) = self.item_chrome_screen_rect(&map, item);
                    let mut half = accent;
                    half.a *= 0.5;
                    solid_rect(&mut fill, l, t, rr - l, bb - t, half);
                    continue;
                }
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
                    ItemKind::Arrow { ax, ay, bx, by, bend } => {
                        if is_primary {
                            // The BEND node (DRAGON-470): a third handle on the middle of the
                            // shaft, offered only once the shaft is long enough on screen to
                            // spare the room ([`arrow_bend_node`]). Drawn FIRST so the endpoint
                            // discs paint over it where they overlap, which is the order the
                            // hit-test resolves them in — the dot you see on top is the one that
                            // takes the click.
                            if let Some(cn) = arrow_bend_node(&map, ax, ay, bx, by, bend) {
                                handle(&mut fill, cn.0, cn.1);
                            }
                            let (an, bn) = arrow_nodes(&map, ax, ay, bx, by, bend, item.stroke_w);
                            handle(&mut fill, an.0, an.1);
                            handle(&mut fill, bn.0, bn.1);
                        } else {
                            // A secondary arrow has no endpoint nodes to show, so it wears the
                            // same dashed box as everything else — "this is selected too".
                            let (l, t, rr, bb) = self.item_chrome_screen_rect(&map, item);
                            dashed_rect(&mut fill, l, t, rr - l, bb - t, accent);
                        }
                    }
                    // Mapped to its bounding Rect above — unreachable.
                    ItemKind::Path { .. } => {}
                }
            }
            // The GROUP box (DRAGON-388): a dashed accent rect wrapping the union of the members'
            // boxes, wearing the 8 resize handles — the ONE thing a group scale drags. Derived
            // each frame from the live selection, so it tracks any edit.
            if multi && let Some((l, t, rr, bb)) = self.group_chrome_rect(&map) {
                dashed_rect(&mut fill, l, t, rr - l, bb - t, accent);
                let (mx, my) = ((l + rr) / 2.0, (t + bb) / 2.0);
                for (hx, hy) in [
                    (l, t), (rr, t), (l, bb), (rr, bb),
                    (mx, t), (mx, bb), (l, my), (rr, my),
                ] {
                    handle(&mut fill, hx, hy);
                }
            }
            // The text SELECTION highlight (DRAGON-354 item 12): a translucent accent wash over
            // the selected glyphs, box-relative to the primary text item's rect (source px →
            // canvas). Drawn BEFORE the caret so the caret stays crisp on top.
            if !self.text_selection.is_empty()
                && let Some(&ItemKind::Rect { x, y, .. }) = self
                    .items
                    .iter()
                    .find(|i| Some(i.id) == primary && i.text)
                    .map(|i| &i.kind)
            {
                let mut wash = accent;
                wash.a = 0.30;
                for (x0, yt, x1, h) in self.text_selection.iter().copied() {
                    let tl = map.to_canvas((x + x0, y + yt));
                    let br = map.to_canvas((x + x1, y + yt + h));
                    fill(
                        tl.0.min(br.0),
                        tl.1.min(br.1),
                        (br.0 - tl.0).abs(),
                        (br.1 - tl.1).abs(),
                        wash,
                        0.0,
                    );
                }
            }
            // The blinking text caret (DRAGON-354): a thin accent bar, box-relative to the
            // primary text item's rect (source px → canvas). Present only on a blink-on tick
            // while editing (the app gates the blink by passing `None`).
            if let Some((cx, cy, ch)) = self.text_caret
                && let Some(&ItemKind::Rect { x, y, .. }) = self
                    .items
                    .iter()
                    .find(|i| Some(i.id) == primary && i.text)
                    .map(|i| &i.kind)
            {
                let top = map.to_canvas((x + cx, y + cy));
                let bot = map.to_canvas((x + cx, y + cy + ch));
                let h = (bot.1 - top.1).abs().max(2.0);
                fill(top.0, top.1.min(bot.1), 1.5, h, accent, 0.0);
            }
        });
    }
}

/// One pass of the committed scene's draw (DRAGON-373), in ITEM order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DrawPass {
    /// The half-open item range `from..to`, drawn as ONE batch of vector geometry.
    Shapes(usize, usize),
    /// The text annotation at this index, drawn as its own raster layer.
    TextLayer(usize),
}

/// Split the scene into draw passes: maximal runs of vector items, with each text item that
/// OWNS a raster layer (`has_layer`) standing alone between them.
///
/// This is the z-order fix in one function. The vector kinds are drawn by this widget and the
/// text kinds are rasters; drawing all of one and then all of the other — which is what a raster
/// layer stacked under the canvas amounts to — pins every text box either above or below every
/// vector, so bringing a caption to the front had no effect on screen even though the export
/// honoured it (`rasterize_scene` walks every kind in ONE in-order loop). Splitting at each text
/// item and drawing the passes in order reproduces that loop exactly, so live and bake agree at
/// any depth, in any arrangement.
///
/// A text box with no layer (still blank — nothing to draw) stays inside the surrounding vector
/// run: `draw_shapes` skips it anyway, and not splitting there keeps a scene with no text on
/// exactly ONE pass, as it was before. Pure — unit-tested.
fn draw_passes(items: &[Item], has_layer: impl Fn(u64) -> bool) -> Vec<DrawPass> {
    let mut passes = Vec::new();
    let mut run_start = 0usize;
    for (i, item) in items.iter().enumerate() {
        if !has_layer(item.id) {
            continue;
        }
        if i > run_start {
            passes.push(DrawPass::Shapes(run_start, i));
        }
        passes.push(DrawPass::TextLayer(i));
        run_start = i + 1;
    }
    if items.len() > run_start {
        passes.push(DrawPass::Shapes(run_start, items.len()));
    }
    passes
}

/// A picture REGION (full-source px `(x, y, w, h)`) as its GLOBAL on-screen rectangle: mapped
/// through `map` — the same transform the vector geometry rides — and shifted by the widget's
/// bounds origin. THE placement kernel for the picture rect itself and for each text caption's
/// raster (DRAGON-396), so a caption can never drift from the shapes around it. Pure.
/// One TEXT annotation's draw-only raster layer as the canvas receives it: the item id, the
/// element, and the caption's SOURCE-px region `(x, y, w, h)`. See
/// [`AnnotationCanvas::text_layers`].
pub type TextLayerMount<'a, Msg> = (u64, cosmic::Element<'a, Msg>, (f32, f32, f32, f32));

pub(crate) fn region_on_screen(map: &CanvasMap, origin: (f32, f32), region: (f32, f32, f32, f32)) -> Rectangle {
    let (x, y, w, h) = region;
    let a = map.to_canvas((x, y));
    let b = map.to_canvas((x + w, y + h));
    Rectangle {
        x: origin.0 + a.0.min(b.0),
        y: origin.1 + a.1.min(b.1),
        width: (b.0 - a.0).abs(),
        height: (b.1 - a.1).abs(),
    }
}

/// Draw one DRAW-ONLY element (a text annotation's raster layer, DRAGON-373) over `place` —
/// the region's on-screen rectangle — clipped to `clip`.
///
/// The element is not in the widget tree (see [`AnnotationCanvas::text_layers`]): it is laid out
/// here, against the picture rather than by a parent, and handed a throwaway state tree. That is
/// sound precisely because it is draw-only — a `LayerStack` shader program is stateless
/// (`State = ()`), it is never sent an event, and its GPU textures live in iced's per-process
/// pipeline storage keyed by `LayerKey`, not in the widget tree. Placing it here is also what
/// makes the placement right: the fractions in `Layer::dest` are fractions of the PICTURE, and
/// `place` is that picture under the current zoom/pan (the same [`CanvasMap`] the vectors use),
/// so the raster and the geometry can never drift apart.
fn draw_placed<Msg>(
    element: &cosmic::Element<'_, Msg>,
    renderer: &mut cosmic::Renderer,
    theme: &cosmic::Theme,
    style: &cosmic::iced::core::renderer::Style,
    place: Rectangle,
    clip: Rectangle,
    viewport: &Rectangle,
) {
    use cosmic::iced::core::Renderer as _;
    use cosmic::iced::core::layout;
    if place.width <= 0.0 || place.height <= 0.0 || clip.width <= 0.0 || clip.height <= 0.0 {
        return;
    }
    let node = layout::Node::new(Size::new(place.width, place.height))
        .move_to(Point::new(place.x, place.y));
    let layout = Layout::new(&node);
    let tree = Tree::new(element.as_widget());
    renderer.with_layer(clip, |renderer| {
        element.as_widget().draw(
            &tree,
            renderer,
            theme,
            style,
            layout,
            mouse::Cursor::Unavailable,
            viewport,
        );
    });
}

/// Assemble the OS IME cursor rectangle in WINDOW-GLOBAL coords (DRAGON-359) from the caret's
/// top/bottom canvas-LOCAL endpoints and the widget's bounds origin. Pure so the offset +
/// normalization (top = the smaller y; a non-negative height floored to 2px so a degenerate
/// caret still gives the OS a real anchor) is unit-testable away from the widget tree.
fn ime_cursor_rect_from(top: (f32, f32), bot: (f32, f32), origin: (f32, f32)) -> Rectangle {
    Rectangle {
        x: origin.0 + top.0,
        y: origin.1 + top.1.min(bot.1),
        width: 2.0,
        height: (bot.1 - top.1).abs().max(2.0),
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
        // An arrow endpoint repositions freely — a move cursor. So does the bend handle
        // (DRAGON-470), which follows the pointer in both axes.
        Grab::ArrowA | Grab::ArrowB | Grab::ArrowBend => mouse::Interaction::Move,
    }
}

/// Which resize [`Grab`] a point `g` (screen px) lands on for a chrome rect whose corners are
/// `(l, t)`..`(r, b)` (DRAGON-388): the 8 handle circles (4 corners + 4 edge midpoints), each
/// within [`HANDLE_GRAB`]. Corners take precedence over edges (they sit at the intersections).
/// The group box hit-tests through this; the single-item path keeps its own inline test so it
/// stays byte-identical. Pure — unit-tested.
fn rect_handle_grab(l: f32, t: f32, r: f32, b: f32, g: (i32, i32)) -> Option<Grab> {
    let (gx, gy) = (g.0 as f32, g.1 as f32);
    let near = |cx: f32, cy: f32| (gx - cx).hypot(gy - cy) <= HANDLE_GRAB;
    let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
    if near(l, t) {
        Some(Grab::Corner(Corner::Nw))
    } else if near(r, t) {
        Some(Grab::Corner(Corner::Ne))
    } else if near(l, b) {
        Some(Grab::Corner(Corner::Sw))
    } else if near(r, b) {
        Some(Grab::Corner(Corner::Se))
    } else if near(mx, t) {
        Some(Grab::Edge(Edge::N))
    } else if near(mx, b) {
        Some(Grab::Edge(Edge::S))
    } else if near(l, my) {
        Some(Grab::Edge(Edge::W))
    } else if near(r, my) {
        Some(Grab::Edge(Edge::E))
    } else {
        None
    }
}

/// Whether `g` (widget-local px) falls on the GROUP BOX body itself (DRAGON-390): anywhere inside
/// the union rect `(l, t, r, b)`, or within [`HIT_PAD`] of its dashed border (the same comfortable
/// tolerance the per-item box hit-tests already breathe by). The CALLER owns precedence — this is
/// consulted only AFTER the group handles and every annotation body have missed, so a `true` here
/// is the group's empty interior or its border, which drags the whole selection (MoveMany) or
/// opens its context menu. Pure — unit-tested.
fn group_box_grab(l: f32, t: f32, r: f32, b: f32, g: (i32, i32)) -> bool {
    let (gx, gy) = (g.0 as f32, g.1 as f32);
    gx >= l - HIT_PAD && gx <= r + HIT_PAD && gy >= t - HIT_PAD && gy <= b + HIT_PAD
}

/// Draw a SOLID rounded rectangle outline (4 sides), 1.5px thick (DRAGON-388) — the box each
/// member of a multi-selection wears (drawn at half opacity, no handles); the group box keeps
/// the dashed style + handles.
fn solid_rect(
    fill: &mut dyn FnMut(f32, f32, f32, f32, Color, f32),
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Color,
) {
    let thick = 1.5;
    let rad = thick / 2.0;
    fill(x, y, w, thick, color, rad); // top
    fill(x, y + h - thick, w, thick, color, rad); // bottom
    fill(x, y, thick, h, color, rad); // left
    fill(x + w - thick, y, thick, h, color, rad); // right
}

/// Draw the pointer tool's live rubber BAND (DRAGON-468): a mostly translucent accent interior
/// with a [`BAND_BORDER`]-thick solid accent outline on top.
///
/// Deliberately NOT [`dashed_rect`]: a marching-ants outline is the "content will be placed
/// inside this" idiom every draw tool uses, so the pointer wearing it read as an in-progress
/// SHAPE. A filled wash reads as a sweep over what it covers, which is what the band does. The
/// selection chrome (per-item boxes, the group box) keeps its dashed style, so the band and the
/// chrome stay easy to tell apart while a band grows over already-selected items.
///
/// The wash SCALES the accent's alpha rather than replacing it, so it stays proportional if the
/// theme ever hands us a non-opaque accent. The border is drawn LAST, over the wash, and its four
/// sides overlap at the corners — harmless, it is one opaque color.
fn band_rect(
    fill: &mut dyn FnMut(f32, f32, f32, f32, Color, f32),
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Color,
) {
    let mut wash = color;
    wash.a *= BAND_FILL_ALPHA;
    fill(x, y, w, h, wash, 0.0);
    let t = BAND_BORDER;
    fill(x, y, w, t, color, 0.0); // top
    fill(x, y + h - t, w, t, color, 0.0); // bottom
    fill(x, y, t, h, color, 0.0); // left
    fill(x + w - t, y, t, h, color, 0.0); // right
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
        // A TEXT box (DRAGON-354) renders its glyphs through the shared embedded-font raster
        // layer, so this widget draws NO outline for it (only its selection/edit chrome, drawn
        // separately). Same skip as the shader-drawn effect kinds above.
        if item.text {
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
            &ItemKind::Arrow { ax, ay, bx, by, bend } => {
                let a = map.to_canvas((ax, ay));
                let b = map.to_canvas((bx, by));
                // Arrows render +ARROW_STROKE_BONUS source px thicker than the set width (matches
                // the bake), so an arrow is bolder than a same-width box.
                let asw = ((item.stroke_w + ARROW_STROKE_BONUS) * iss).max(0.5);
                // The BENT shaft's control point (DRAGON-470), mapped like the endpoints so the
                // canvas and the bake flatten the same parabola. `None` = the straight arrow,
                // which takes the untouched line path.
                let c = (!bend.is_straight())
                    .then(|| map.to_canvas(crate::arrow_curve::control((ax, ay), (bx, by), bend)));
                draw_arrow_vec(frame, a, b, c, asw, curve, iss, item.color);
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

/// `ctrl` is the BENT shaft's quadratic control point (DRAGON-470), already in screen px;
/// `None` draws the straight line this has always drawn. When it is present the head is sized
/// against the ARC (a bow is longer than its chord) and the barbs splay around the curve's end
/// TANGENT, so the head points where the shaft actually arrives.
#[allow(clippy::too_many_arguments)]
fn draw_arrow_vec(
    frame: &mut canvas::Frame,
    a: (f32, f32),
    b: (f32, f32),
    ctrl: Option<(f32, f32)>,
    sw: f32,
    curve: f32,
    iss: f32,
    color: Color,
) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = match ctrl {
        None => (dx * dx + dy * dy).sqrt(),
        Some(c) => crate::arrow_curve::arc_len(a, c, b),
    };
    if len < 0.5 {
        return;
    }
    let (ux, uy) = match ctrl {
        None => (dx / len, dy / len),
        Some(c) => crate::arrow_curve::head_dir(a, c, b),
    };
    let stroke = shape_stroke(color, sw, curve);
    // Shaft: tail all the way to the tip.
    let shaft = match ctrl {
        None => Path::line(Point::new(a.0, a.1), Point::new(b.0, b.1)),
        Some(c) => Path::new(|p| {
            p.move_to(Point::new(a.0, a.1));
            p.quadratic_curve_to(Point::new(c.0, c.1), Point::new(b.0, b.1));
        }),
    };
    frame.stroke(&shaft, stroke);
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
    // ── DRAGON-373: text draws at its own depth, not above (or below) every vector ──────

    /// A scene of `n` items where those in `text` are TEXT boxes, in item (z) order.
    fn z_scene(n: u64, text: &[u64]) -> Vec<super::Item> {
        (0..n)
            .map(|id| super::Item {
                id,
                kind: super::ItemKind::Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
                stroke_w: 1.0,
                color: super::Color::WHITE,
                fill: None,
                fx: super::FxKind::None,
                curve_radius: 0.0,
                badge: None,
                text: text.contains(&id),
            })
            .collect()
    }

    /// The reported bug, as a draw order: a rectangle between two captions must be drawn AFTER
    /// the one it covers and BEFORE the one that covers it. A single text layer (however it is
    /// stacked) cannot express that — it draws all text at one depth — which is why the layers
    /// are per box and the canvas splits its geometry around each one.
    #[test]
    fn a_vector_between_two_captions_draws_between_them() {
        use super::DrawPass::*;
        // text(0), rect(1), text(2), rect(3), rect(4)
        let items = z_scene(5, &[0, 2]);
        let passes = super::draw_passes(&items, |id| id == 0 || id == 2);
        assert_eq!(passes, vec![TextLayer(0), Shapes(1, 2), TextLayer(2), Shapes(3, 5)]);
    }

    /// The general invariant that makes live and bake agree: the passes visit EVERY item exactly
    /// once, in item order — the same single in-order walk `rasterize_scene` bakes with. So any
    /// arrangement of any depth is drawn the way it exports, not just the reported one.
    #[test]
    fn the_passes_cover_every_item_exactly_once_in_scene_order() {
        for text in [
            vec![],
            vec![0],
            vec![3],
            vec![0, 1, 2, 3],
            vec![1, 2],
            vec![0, 3],
        ] {
            let items = z_scene(4, &text);
            let passes = super::draw_passes(&items, |id| text.contains(&id));
            let visited: Vec<usize> = passes
                .iter()
                .flat_map(|p| match *p {
                    super::DrawPass::Shapes(a, b) => (a..b).collect::<Vec<_>>(),
                    super::DrawPass::TextLayer(i) => vec![i],
                })
                .collect();
            assert_eq!(visited, (0..items.len()).collect::<Vec<_>>(), "text at {text:?}");
        }
    }

    /// A scene with no text (or with a blank box, which owns no raster) keeps the historical
    /// SINGLE geometry pass — the default path stays exactly what it was.
    #[test]
    fn a_scene_without_text_layers_is_one_pass() {
        let items = z_scene(4, &[2]);
        // The text box at index 2 is still blank, so it has no layer: one run covers everything
        // (`draw_shapes` skips text items itself).
        assert_eq!(super::draw_passes(&items, |_| false), vec![super::DrawPass::Shapes(0, 4)]);
        assert!(super::draw_passes(&[], |_| false).is_empty(), "an empty scene draws nothing");
    }

    use super::*;

    /// DRAGON-354 item 12: the text-edit click ladder. A same-box, in-window press advances the
    /// count (caret → word → select-all, capped); anything else restarts at a single caret click.
    #[test]
    fn text_click_ladder_caret_word_all() {
        // A fresh press (nothing before, or out of window / different box) is a single caret click.
        assert_eq!(text_click_count(false, 0), 1);
        assert_eq!(text_click_kind(text_click_count(false, 0)), TextClickKind::Caret);
        // Second same-box in-window press → word.
        let c2 = text_click_count(true, 1);
        assert_eq!(c2, 2);
        assert_eq!(text_click_kind(c2), TextClickKind::Word);
        // Third → select all, and it STAYS select-all for a fourth (no line/paragraph step).
        let c3 = text_click_count(true, c2);
        assert_eq!(text_click_kind(c3), TextClickKind::All);
        let c4 = text_click_count(true, c3);
        assert_eq!(text_click_kind(c4), TextClickKind::All);
        // A gap (different box or too slow) restarts the ladder at a single caret click.
        assert_eq!(text_click_kind(text_click_count(false, c4)), TextClickKind::Caret);
        // The count saturates rather than wrapping on a very long same-box burst.
        assert_eq!(text_click_count(true, u8::MAX), u8::MAX);
    }

    fn map(bounds: (f32, f32), zoom: f32, pan: (f32, f32), disp: (f32, f32), source: (f32, f32)) -> CanvasMap {
        CanvasMap { bounds, zoom, pan, disp, source, offset: (0.0, 0.0) }
    }

    /// DRAGON-385: a view-crop offset shifts between the cropped on-screen content and the
    /// FULL-source model coords. `to_image(to_canvas(p)) == p` still round-trips, and the crop
    /// origin lands at the content's top-left while a full-source point inside the crop maps to
    /// its cropped-local place.
    #[test]
    fn crop_offset_shifts_between_full_source_and_cropped_content() {
        // A 400x300 crop taken at source (500, 200) of a larger image, shown 1:1 (disp == source
        // size), centred in an 800x600 canvas at fit zoom, no pan.
        let m = CanvasMap {
            bounds: (800.0, 600.0),
            zoom: 1.0,
            pan: (0.0, 0.0),
            disp: (400.0, 300.0),
            source: (400.0, 300.0),
            offset: (500.0, 200.0),
        };
        // The crop's top-left (full-source 500,200) sits at the content box's top-left corner:
        // centred 400x300 in 800x600 → (200, 150).
        assert_close(m.to_canvas((500.0, 200.0)), (200.0, 150.0), 1e-3, "crop origin → content TL");
        // A point 100px into the crop maps 100px into the content box.
        assert_close(m.to_canvas((600.0, 250.0)), (300.0, 200.0), 1e-3, "inside the crop");
        // Round-trip through the offset both ways.
        for p in [(210.0f32, 160.0f32), (590.0, 440.0), (400.0, 300.0)] {
            let back = m.to_canvas(m.to_image(p));
            assert_close(back, p, 1e-2, "offset round-trip");
        }
        // to_image of the content top-left recovers the full-source crop origin.
        assert_close(m.to_image((200.0, 150.0)), (500.0, 200.0), 1e-2, "content TL → crop origin");
    }

    fn assert_close(a: (f32, f32), b: (f32, f32), eps: f32, what: &str) {
        assert!((a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps, "{what}: {a:?} vs {b:?}");
    }

    /// DRAGON-359: the OS IME cursor rect offsets caret-local coords by the widget bounds
    /// origin (so it is window-global like `set_ime_cursor_area` expects), takes the SMALLER y
    /// as the top, and floors the height to a real anchor even for a degenerate caret.
    #[test]
    fn ime_cursor_rect_offsets_and_normalizes() {
        // Normal caret: top above bottom, bounds origin added.
        let r = ime_cursor_rect_from((10.0, 20.0), (10.0, 44.0), (100.0, 200.0));
        assert_eq!((r.x, r.y), (110.0, 220.0));
        assert!((r.height - 24.0).abs() < 1e-3, "height {}", r.height);
        assert!((r.width - 2.0).abs() < 1e-3);

        // Inverted endpoints (top y > bottom y): the min is still the rect top.
        let r = ime_cursor_rect_from((5.0, 60.0), (5.0, 30.0), (0.0, 0.0));
        assert_eq!(r.y, 30.0);
        assert!((r.height - 30.0).abs() < 1e-3);

        // Degenerate (zero-height) caret still yields the 2px floor so the OS gets an anchor.
        let r = ime_cursor_rect_from((0.0, 8.0), (0.0, 8.0), (0.0, 0.0));
        assert!((r.height - 2.0).abs() < 1e-3, "floored height {}", r.height);
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
    fn rect_handle_grab_picks_the_corner_or_edge_under_the_point() {
        // Chrome rect (0,0)-(100,50): corners, edge midpoints, and a miss (DRAGON-388).
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (0, 0)), Some(Grab::Corner(Corner::Nw)));
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (100, 50)), Some(Grab::Corner(Corner::Se)));
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (100, 0)), Some(Grab::Corner(Corner::Ne)));
        // Edge midpoints (50,0) top, (0,25) left.
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (50, 0)), Some(Grab::Edge(Edge::N)));
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (0, 25)), Some(Grab::Edge(Edge::W)));
        // Dead centre is no handle.
        assert_eq!(rect_handle_grab(0.0, 0.0, 100.0, 50.0, (50, 25)), None);
    }

    #[test]
    fn group_box_grab_covers_the_interior_and_a_border_band() {
        // Union (0,0)-(100,50): the dead centre and every corner/edge are ON the group body,
        // and so is a point within HIT_PAD (8) of the border; anything past that band misses
        // (DRAGON-390).
        assert!(group_box_grab(0.0, 0.0, 100.0, 50.0, (50, 25)), "empty interior grabs");
        assert!(group_box_grab(0.0, 0.0, 100.0, 50.0, (0, 0)), "corner grabs");
        assert!(group_box_grab(0.0, 0.0, 100.0, 50.0, (100, 50)), "far corner grabs");
        // Just outside the outline but inside the tolerance band → still the border.
        assert!(group_box_grab(0.0, 0.0, 100.0, 50.0, (-8, 25)), "left border band");
        assert!(group_box_grab(0.0, 0.0, 100.0, 50.0, (50, 58)), "bottom border band");
        // Past the band on any side → a miss (marquee/deselect territory).
        assert!(!group_box_grab(0.0, 0.0, 100.0, 50.0, (-9, 25)), "past the left band");
        assert!(!group_box_grab(0.0, 0.0, 100.0, 50.0, (50, 59)), "past the bottom band");
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
            text: false,
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
        let straight = crate::arrow_curve::Bend::STRAIGHT;
        let (an, bn) = arrow_nodes(&m, 20.0, 50.0, 80.0, 50.0, straight, 8.0);
        assert_close(an, (8.0, 50.0), 1e-3, "tail node pushed back beyond the cap");
        assert_close(bn, (92.0, 50.0), 1e-3, "head node pushed forward beyond the tip");
    }

    // ── DRAGON-470: the arrow's third (bend) handle ────────────────────────────────────

    #[test]
    fn a_straight_arrows_bend_node_sits_on_the_chord_midpoint() {
        // 300px wide at 1:1 so the 200px shaft clears BEND_NODE_MIN_SHAFT.
        let m = map((300.0, 300.0), 1.0, (0.0, 0.0), (300.0, 300.0), (300.0, 300.0));
        let n = arrow_bend_node(&m, 20.0, 50.0, 220.0, 50.0, crate::arrow_curve::Bend::STRAIGHT)
            .expect("a 200px shaft carries the node");
        assert_close(n, (120.0, 50.0), 1e-3, "bend node on the middle of the shaft");
    }

    #[test]
    fn the_bend_node_is_withheld_from_a_short_on_screen_shaft() {
        // The rule itself: three HANDLE_GRAB discs must not be able to cover the whole shaft.
        assert!(!bend_node_offered(BEND_NODE_MIN_SHAFT - 0.1));
        assert!(bend_node_offered(BEND_NODE_MIN_SHAFT));
        assert!(!bend_node_offered(f32::NAN));
        assert!(!bend_node_offered(0.0));
        // And the widget honours it: a 30px arrow has no node to draw or grab…
        let m = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let straight = crate::arrow_curve::Bend::STRAIGHT;
        assert!(arrow_bend_node(&m, 20.0, 50.0, 50.0, 50.0, straight).is_none(), "30px shaft");
        // …nor does a long one seen at a zoom that shrinks it below the bar (0.2 × 200 = 40px).
        let zoomed = map((300.0, 300.0), 0.2, (0.0, 0.0), (300.0, 300.0), (300.0, 300.0));
        assert!(
            arrow_bend_node(&zoomed, 20.0, 50.0, 220.0, 50.0, straight).is_none(),
            "zoomed out below the bar"
        );
        // The SAME arrow at 1:1 does offer it — the rule is on-screen length, not source length.
        let full = map((300.0, 300.0), 1.0, (0.0, 0.0), (300.0, 300.0), (300.0, 300.0));
        assert!(arrow_bend_node(&full, 20.0, 50.0, 220.0, 50.0, straight).is_some());
    }

    #[test]
    fn a_bent_shaft_earns_its_node_by_arc_length() {
        // A chord under the bar whose BOW takes the drawn shaft over it: the node follows the ink.
        let m = map((300.0, 300.0), 1.0, (0.0, 0.0), (300.0, 300.0), (300.0, 300.0));
        let (ax, ay, bx, by) = (20.0, 150.0, 70.0, 150.0); // 50px chord, under the 64px bar
        let straight = crate::arrow_curve::Bend::STRAIGHT;
        assert!(arrow_bend_node(&m, ax, ay, bx, by, straight).is_none(), "straight: too short");
        let bowed = crate::arrow_curve::Bend { along: 0.0, across: -1.2 };
        assert!(
            arrow_bend_node(&m, ax, ay, bx, by, bowed).is_some(),
            "the arc is long enough even though the chord is not"
        );
    }

    #[test]
    fn a_bent_arrows_nodes_follow_the_curve() {
        let m = map((300.0, 300.0), 1.0, (0.0, 0.0), (300.0, 300.0), (300.0, 300.0));
        // A bow 50px above the chord: the handle IS the curve's apex, and the endpoint nodes are
        // pushed out along the TANGENTS, so they no longer sit on the chord's own line.
        let bend = crate::arrow_curve::Bend { along: 0.0, across: -0.25 };
        let n = arrow_bend_node(&m, 20.0, 100.0, 220.0, 100.0, bend).expect("long enough");
        assert_close(n, (120.0, 50.0), 1e-3, "bend node at the bow's apex");
        let (an, bn) = arrow_nodes(&m, 20.0, 100.0, 220.0, 100.0, bend, 8.0);
        assert!(an.1 > 100.0, "tail node swings below the chord: {an:?}");
        assert!(bn.1 > 100.0, "head node swings below the chord: {bn:?}");
        // Still exactly HIT_PAD + stroke/2 away from the endpoint it belongs to.
        assert!(((an.0 - 20.0).hypot(an.1 - 100.0) - 12.0).abs() < 1e-3, "{an:?}");
        assert!(((bn.0 - 220.0).hypot(bn.1 - 100.0) - 12.0).abs() < 1e-3, "{bn:?}");
    }

    #[test]
    fn a_bent_arrow_hit_tests_along_its_ink_not_its_chord() {
        let m = map((100.0, 100.0), 1.0, (0.0, 0.0), (100.0, 100.0), (100.0, 100.0));
        let bend = crate::arrow_curve::Bend { along: 0.0, across: -0.25 };
        let straight = crate::arrow_curve::Bend::STRAIGHT;
        let (a, b) = ((20.0, 50.0), (80.0, 50.0));
        // The bow's apex is 15px above the chord midpoint. The curve is THERE, the chord is not.
        assert!(arrow_distance(&m, a, b, bend, (50.0, 35.0)) < 0.5, "on the bow");
        assert!(arrow_distance(&m, a, b, bend, (50.0, 50.0)) > 10.0, "chord midpoint is empty now");
        // The straight arrow is untouched: its own midpoint is right on it.
        assert!(arrow_distance(&m, a, b, straight, (50.0, 50.0)) < 1e-3, "straight shaft");
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
            text: false,
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
            text: false,
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
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), false, false), Some(Tool::Pen), "plain pencil inks");
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), true, false), Some(Tool::Pen), "ctrl changes nothing");
        // Every SHAPE tool keeps press-selects: it hit-tests unless Ctrl is held (DRAGON-339).
        for t in [Tool::Rect, Tool::Arrow, Tool::Highlight, Tool::BoxHighlight, Tool::Spotlight, Tool::Pixelate, Tool::Blur] {
            assert_eq!(draw_bypassing_items(Some(t), false, false), None, "{t:?} presses select as before");
            assert_eq!(draw_bypassing_items(Some(t), true, false), Some(t), "{t:?} + ctrl draws over items");
        }
        // The pointer and the neutral state always hit-test (the pointer IS selection); the
        // eraser never reaches this branch (handled earlier), so it must not claim one here.
        for t in [None, Some(Tool::Pointer), Some(Tool::Eraser)] {
            for ctrl in [false, true] {
                assert_eq!(draw_bypassing_items(t, ctrl, false), None, "{t:?} ctrl={ctrl} hit-tests");
            }
        }
        // DRAGON-370: over a selected item's RESIZE HANDLE the Ctrl override stops claiming the
        // press, so it can arm the text scale modifier instead. The PENCIL is untouched — its
        // bypass is unconditional (DRAGON-346), not a modifier meaning.
        for t in [None, Some(Tool::Pointer), Some(Tool::Rect), Some(Tool::Text)] {
            for ctrl in [false, true] {
                assert_eq!(
                    draw_bypassing_items(t, ctrl, true),
                    None,
                    "{t:?} ctrl={ctrl}: a handle press must resize",
                );
            }
        }
        assert_eq!(
            draw_bypassing_items(Some(Tool::Pen), false, true),
            Some(Tool::Pen),
            "the pencil still inks over a handle — DRAGON-346 is unconditional",
        );
    }

    #[test]
    fn the_cursor_promises_what_the_press_will_do() {
        // DRAGON-346: with the pencil armed the crosshair must hold EVERYWHERE — over an item's
        // body (which used to show the open-hand grab, promising a move that never happened) and
        // over the selected item's resize handles too, since neither is reachable any more.
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false, false), "the pencil owns the whole canvas");
        assert!(whole_canvas_crosshair(Some(Tool::Pen), true, false));
        assert!(whole_canvas_crosshair(Some(Tool::Eraser), false, false), "so does the eraser");
        // A shape tool shows per-item cursors until Ctrl flips it to draw-over-anything.
        assert!(!whole_canvas_crosshair(Some(Tool::Rect), false, false));
        assert!(whole_canvas_crosshair(Some(Tool::Rect), true, false));
        // The pointer and the neutral state always show the per-item cursors.
        for t in [None, Some(Tool::Pointer)] {
            for ctrl in [false, true] {
                assert!(!whole_canvas_crosshair(t, ctrl, false), "{t:?} ctrl={ctrl} keeps item cursors");
            }
        }
        // DRAGON-370 — the invariant, restated where the handle rule now bites: Ctrl + a SHAPE
        // tool over a RESIZE HANDLE will resize, so the crosshair must stop being promised there.
        // This is the whole reason `whole_canvas_crosshair` is derived from the press rule rather
        // than written out twice.
        for t in [Some(Tool::Rect), Some(Tool::Text), Some(Tool::Pointer), None] {
            for ctrl in [false, true] {
                assert!(
                    !whole_canvas_crosshair(t, ctrl, true),
                    "{t:?} ctrl={ctrl}: promised a crosshair over a handle the press will resize",
                );
            }
        }
        // The PENCIL and the ERASER genuinely still act over a handle (their bypass is
        // unconditional, not a modifier), so their crosshair stays the truth there.
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false, true), "the pencil still inks");
        assert!(whole_canvas_crosshair(Some(Tool::Eraser), false, true), "the eraser still sweeps");
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
            text: false,
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
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), false, false), Some(Tool::Pen));
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false, false));
        // ...and DRAGON-370 does not carve the pencil out: its bypass is unconditional, so it
        // still inks over the handle too. Only the CTRL override defers there.
        assert_eq!(draw_bypassing_items(Some(Tool::Pen), false, true), Some(Tool::Pen));
        assert!(whole_canvas_crosshair(Some(Tool::Pen), false, true));
        assert_eq!(draw_bypassing_items(Some(Tool::Rect), true, true), None, "ctrl defers");
    }

    // ── DRAGON-397: the live band preview ────────────────────────────────────────────

    /// The preview is the RESULT of the modifier, not the raw intersection: a plain band
    /// replaces, so what is currently selected has no say in what the boxes show.
    #[test]
    fn a_plain_band_previews_exactly_what_it_covers() {
        assert_eq!(band_preview_ids(&[7, 8], &[1, 2], false), vec![1, 2]);
        // Sweeping off everything previews an EMPTY selection — the boxes drop as the band
        // retreats, which is the behaviour the ticket is about.
        assert_eq!(band_preview_ids(&[7, 8], &[], false), Vec::<u64>::new());
    }

    /// An ADDITIVE band (Ctrl/Shift at press) keeps the existing selection and appends —
    /// and an already-selected item swept by it previews as STAYING selected, exactly once
    /// and in its original position, which is what release will commit (`Selection::add_all`).
    /// It must never read as a toggle: only a Ctrl/Shift CLICK removes.
    #[test]
    fn an_additive_band_keeps_the_selection_and_never_toggles() {
        assert_eq!(band_preview_ids(&[7, 8], &[1, 2], true), vec![7, 8, 1, 2]);
        // The already-selected item is covered by the band: still selected, not dropped, and
        // not duplicated.
        assert_eq!(band_preview_ids(&[7, 8], &[8, 1], true), vec![7, 8, 1]);
        // Covering nothing leaves the selection alone.
        assert_eq!(band_preview_ids(&[7, 8], &[], true), vec![7, 8]);
    }

    /// The hits themselves are de-duplicated too, so a preview list can never carry an id
    /// twice (which would double-draw its half-opacity box and read as a darker one).
    #[test]
    fn the_preview_set_is_deduplicated() {
        assert_eq!(band_preview_ids(&[], &[3, 3, 4], false), vec![3, 4]);
        assert_eq!(band_preview_ids(&[3], &[3, 3, 4], true), vec![3, 4]);
    }

    #[test]
    fn additive_select_is_pointer_only_and_never_fights_ctrl_draw() {
        // DRAGON-341 × DRAGON-339: Ctrl means "multi-select" in POINTER mode and "draw over
        // whatever is under the cursor" with a draw tool — never both for one press.
        assert!(additive_select(Some(Tool::Pointer), true, false, false), "ctrl-click adds");
        assert!(additive_select(Some(Tool::Pointer), false, true, false), "shift-click adds");
        assert!(!additive_select(Some(Tool::Pointer), false, false, false), "a plain click replaces");
        for t in [Tool::Rect, Tool::Arrow, Tool::Pen, Tool::Eraser] {
            assert!(!additive_select(Some(t), true, true, false), "{t:?} never multi-selects");
        }
        assert!(!additive_select(None, true, true, false), "the neutral state never multi-selects");
        // The two Ctrl meanings are mutually exclusive for every tool.
        for t in [None, Some(Tool::Pointer), Some(Tool::Rect), Some(Tool::Pen), Some(Tool::Eraser)] {
            assert!(
                !(additive_select(t, true, false, false) && force_new_draw(t, true).is_some()),
                "{t:?}: ctrl must claim exactly one meaning"
            );
        }
        // DRAGON-370 adds a THIRD Ctrl meaning — "scale the type" during a text box's handle
        // drag — and the rule that keeps all three from colliding is positional, not modal: over
        // a RESIZE HANDLE the press resizes and no modifier gate fires at all. So Ctrl still
        // claims exactly one meaning per press, now per (tool, what is under the cursor).
        for t in [None, Some(Tool::Pointer), Some(Tool::Rect), Some(Tool::Text)] {
            for shift in [false, true] {
                assert!(
                    !additive_select(t, true, shift, true),
                    "{t:?}: a handle press must resize, not toggle the selection",
                );
            }
            assert!(
                !(additive_select(t, true, false, true)
                    || draw_bypassing_items(t, true, true).is_some()
                    || shift_selects_with_tool(t, true, true)),
                "{t:?}: no MODIFIER gate may claim a handle press",
            );
        }
        // …and the handle exclusion is exactly that — positional. The item's BODY is untouched,
        // so nothing the user could previously do with a modifier is gone, only moved a few px.
        assert!(additive_select(Some(Tool::Pointer), true, false, false));
        assert!(shift_selects_with_tool(Some(Tool::Text), true, false));
    }

    #[test]
    fn shift_selects_from_any_non_pointer_tool_never_the_pointer() {
        // DRAGON-356: shift is the universal selection modifier — with any NON-pointer tool (or
        // the neutral state) armed a shift-press claims the press for selection.
        for t in [None, Some(Tool::Rect), Some(Tool::Arrow), Some(Tool::Pen), Some(Tool::Text), Some(Tool::Eraser)] {
            assert!(shift_selects_with_tool(t, true, false), "{t:?}: shift claims the press");
            assert!(!shift_selects_with_tool(t, false, false), "{t:?}: no shift, the tool acts");
            // DRAGON-370: never over a resize handle, whatever the tool.
            assert!(!shift_selects_with_tool(t, true, true), "{t:?}: a handle press resizes");
        }
        // The POINTER tool is excluded — it owns its own ctrl/shift path (`additive_select`), so
        // the two paths never both fire for one press.
        assert!(!shift_selects_with_tool(Some(Tool::Pointer), true, false), "pointer keeps its own path");
        // Shift-select rides the SHIFT modifier; the Ctrl-draw override rides CTRL — so a shape
        // tool with Ctrl (only) still draws over what is under the cursor, never selects.
        assert!(!shift_selects_with_tool(Some(Tool::Rect), false, false));
        assert_eq!(force_new_draw(Some(Tool::Rect), true), Some(Tool::Rect));
    }

    #[test]
    fn text_edit_press_target_claims_only_an_in_box_body_press() {
        // DRAGON-354 item 12 × DRAGON-356: a BODY press on the edited box (id 7) belongs to the
        // text editor and takes priority over the shift gate — regardless of shift.
        assert_eq!(text_edit_press_target(Some(7), Some(7), true), Some(7));
        // A press on the edited box's RESIZE HANDLE (not body) falls through (→ None).
        assert_eq!(text_edit_press_target(Some(7), Some(7), false), None);
        // A body press on a DIFFERENT item falls through to the shift/tool gates.
        assert_eq!(text_edit_press_target(Some(7), Some(3), true), None);
        // Empty canvas, or nothing being edited, never targets the editor.
        assert_eq!(text_edit_press_target(Some(7), None, true), None);
        assert_eq!(text_edit_press_target(None, Some(7), true), None);
    }

    #[test]
    fn a_multi_selection_wears_its_handles_on_the_group_box() {
        // DRAGON-388 (superseding the DRAGON-341 primary-only rule): with more than one item
        // selected the resize handles ride the GROUP box — the union of every member's chrome
        // rect — so a corner drag scales the whole set. Members are still BODY-hittable (a body
        // drag moves the group) and no per-item handle exists anymore.
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
            text: false,
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
        // Chrome rects (pad = HIT_PAD 8 + stroke/2 4 = 12): item 1 (8,8)-(72,72), item 2
        // (108,8)-(172,72) → group box (8,8)-(172,72).
        assert_eq!(canvas.group_chrome_rect(&cmap), Some((8.0, 8.0, 172.0, 72.0)));
        // The GROUP's NW corner is a resize handle (routed via the primary id)...
        assert!(
            matches!(
                canvas.hit_at(&cmap, (8.0, 8.0)),
                Some((2, HitKind::Resize(Grab::Corner(Corner::Nw))))
            ),
            "the group box's corner resizes the whole selection"
        );
        // ...and so is its SE corner, which belongs to no single item's old chrome.
        assert!(
            matches!(
                canvas.hit_at(&cmap, (172.0, 72.0)),
                Some((2, HitKind::Resize(Grab::Corner(Corner::Se))))
            ),
        );
        // The old per-item handle spot (the primary's own NW chrome corner at (108, 8)) now sits
        // on the group box's TOP edge, between handles — it is just item 2's grabbable BODY.
        assert!(
            matches!(canvas.hit_at(&cmap, (110.0, 30.0)), Some((2, HitKind::Body))),
            "a member's body still moves the group"
        );
    }

    fn boxed_item(id: u64, x: f32, y: f32) -> Item {
        Item {
            id,
            kind: ItemKind::Rect { x, y, w: 40.0, h: 40.0 },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
            text: false,
        }
    }

    #[test]
    fn the_group_box_body_grabs_the_whole_selection() {
        // DRAGON-390: the derived group box is itself grabbable — its empty interior + border drag
        // the whole selection, routed as a Body hit on the PRIMARY so the existing "press inside a
        // multi-selection keeps it whole" lane arms MoveMany. Members 1 & 2 (2 primary) → group box
        // (8,8)-(172,72), with a clear vertical gap between the two 40px rects at x 72..108.
        let cmap = map((200.0, 200.0), 1.0, (0.0, 0.0), (200.0, 200.0), (200.0, 200.0));
        let canvas = AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![boxed_item(1, 20.0, 20.0), boxed_item(2, 120.0, 20.0)],
            vec![1, 2],
            Some(Tool::Pointer),
            1.0,
            (0.0, 0.0),
            (200.0, 200.0),
            (200.0, 200.0),
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        );
        // A truly EMPTY spot mid-gap (clear of both member chromes and of every handle) grabs the
        // group as a Body hit on the primary.
        assert!(
            matches!(canvas.hit_at(&cmap, (90.0, 40.0)), Some((2, HitKind::Body))),
            "the group's empty interior moves the whole selection"
        );
        // A group HANDLE still wins over the box body (precedence step 1 over step 3).
        assert!(
            matches!(
                canvas.hit_at(&cmap, (8.0, 8.0)),
                Some((2, HitKind::Resize(Grab::Corner(Corner::Nw))))
            ),
            "handles outrank the group body"
        );
        // Outside the union entirely → no hit (marquee/deselect territory, unchanged).
        assert!(
            canvas.hit_at(&cmap, (185.0, 100.0)).is_none(),
            "past the group box is empty canvas"
        );
    }

    #[test]
    fn a_non_selected_item_inside_the_union_still_hits_before_the_group_body() {
        // DRAGON-390 precedence step 2 over step 3: an UNSELECTED annotation sitting inside the
        // union still takes the press (changing the selection exactly as before) rather than the
        // group body swallowing it. Item 9 fills the gap between the two selected members.
        let cmap = map((200.0, 200.0), 1.0, (0.0, 0.0), (200.0, 200.0), (200.0, 200.0));
        let canvas = AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![boxed_item(1, 20.0, 20.0), boxed_item(2, 120.0, 20.0), boxed_item(9, 78.0, 24.0)],
            vec![1, 2],
            Some(Tool::Pointer),
            1.0,
            (0.0, 0.0),
            (200.0, 200.0),
            (200.0, 200.0),
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        );
        // Item 9 drawn bounds (pad = stroke/2 = 4): (74,20)-(122,68); (98,44) is its centre — a
        // handle-free spot inside the union that belongs to item 9, not the group.
        assert!(
            matches!(canvas.hit_at(&cmap, (98.0, 44.0)), Some((9, HitKind::Body))),
            "a non-selected item inside the union still hits first"
        );
    }

    #[test]
    fn a_single_selection_has_no_grabbable_group_body() {
        // The group-box body lane is MULTI-selection only (DRAGON-390): with one item selected the
        // empty space around it stays empty canvas, so the marquee/deselect path is untouched.
        let cmap = map((200.0, 200.0), 1.0, (0.0, 0.0), (200.0, 200.0), (200.0, 200.0));
        let one = Item {
            id: 3,
            kind: ItemKind::Rect { x: 20.0, y: 20.0, w: 40.0, h: 40.0 },
            stroke_w: 8.0,
            color: Color::WHITE,
            fill: None,
            fx: FxKind::None,
            curve_radius: 8.0,
            badge: None,
            text: false,
        };
        let canvas = AnnotationCanvas::new(
            cosmic::widget::Space::new(),
            vec![one],
            vec![3],
            Some(Tool::Pointer),
            1.0,
            (0.0, 0.0),
            (200.0, 200.0),
            (200.0, 200.0),
            false,
            Color::WHITE,
            |_ev: AnnotEvent| (),
        );
        // A point well outside the single item's chrome (8,8)-(72,72) is empty canvas, not a body.
        assert!(canvas.hit_at(&cmap, (150.0, 150.0)).is_none());
    }

    // ── DRAGON-468: the band is a washed box, not a marquee ──────────────────────────────

    /// Every quad `f` would draw for the band, as `(x, y, w, h, alpha)` — the drawing helpers
    /// take the renderer as a closure, so what they emit is inspectable without a renderer.
    fn band_quads(x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        {
            let mut rec = |qx: f32, qy: f32, qw: f32, qh: f32, c: Color, _r: f32| {
                if qw <= 0.0 || qh <= 0.0 {
                    return; // the real `fill` closure drops empty quads too
                }
                out.push((qx, qy, qw, qh, c.a));
            };
            band_rect(&mut rec, x, y, w, h, Color::from_rgba(0.0, 0.5, 1.0, 1.0));
        }
        out
    }

    /// The pointer's rubber band draws a translucent accent WASH over its whole area plus four
    /// 1px solid sides — not the dashed outline it wore before, which is the draw tools' "a
    /// shape lands here" idiom and made a selection sweep read as a pending rectangle. Five
    /// quads, so a dash tiling can never creep back in unnoticed.
    #[test]
    fn the_band_is_a_translucent_wash_inside_a_one_px_border() {
        let q = band_quads(10.0, 20.0, 100.0, 40.0);
        assert_eq!(q.len(), 5, "one wash + four sides, no dash tiling: {q:?}");
        // 1. The interior covers the WHOLE band and is mostly transparent.
        assert_eq!((q[0].0, q[0].1, q[0].2, q[0].3), (10.0, 20.0, 100.0, 40.0));
        assert!(q[0].4 < 0.5, "the centre must be mostly translucent, got alpha {}", q[0].4);
        assert!(q[0].4 > 0.0, "…but still visible");
        // 2. The four sides are 1px thick and fully opaque, drawn ON TOP of the wash.
        for side in &q[1..] {
            assert_eq!(side.4, 1.0, "the border keeps the accent's own alpha");
            assert!(
                side.2 == BAND_BORDER || side.3 == BAND_BORDER,
                "a border side is {BAND_BORDER}px thick on one axis: {side:?}",
            );
        }
        // The sides bound the band exactly: top/bottom span its width, left/right its height.
        assert_eq!((q[1].2, q[1].3), (100.0, BAND_BORDER), "top");
        assert_eq!((q[2].1, q[2].3), (20.0 + 40.0 - BAND_BORDER, BAND_BORDER), "bottom");
        assert_eq!((q[3].2, q[3].3), (BAND_BORDER, 40.0), "left");
        assert_eq!((q[4].0, q[4].2), (10.0 + 100.0 - BAND_BORDER, BAND_BORDER), "right");
    }

    /// A band dragged out and brought back to the press pixel collapses to zero size, and then it
    /// draws nothing at all: the empty quads are dropped, so it can never flash a 1px accent cross
    /// where the wash and the four sides would otherwise overlap. (`draw` gates the band on
    /// `state.moved`, so the un-dragged press never reaches here in the first place; this is the
    /// case that does.)
    #[test]
    fn a_band_collapsed_back_to_its_press_point_draws_nothing() {
        assert!(band_quads(10.0, 20.0, 0.0, 0.0).is_empty());
    }

    // ── DRAGON-364: the text element's two states (selected vs. editing) ─────────────────

    #[test]
    fn the_text_tool_only_claims_presses_that_are_not_over_a_text_box() {
        // BEFORE DRAGON-364 the text tool claimed EVERY press over a text box and re-opened its
        // editor, which is why a settled box could not be dragged or resized: entering an edit
        // arms the Text tool and settling does not disarm it, so the next click re-entered
        // editing. Now a press over a text box falls through to the shared item lane.
        assert!(
            !text_press_places_new(Some(Tool::Text), true),
            "a press over an existing text box must NOT place a new one — it manipulates",
        );
        assert!(
            text_press_places_new(Some(Tool::Text), false),
            "empty canvas (or a non-text item) still places a new box",
        );
        // No other tool ever places text, whatever is under the cursor.
        for t in [
            None,
            Some(Tool::Pointer),
            Some(Tool::Rect),
            Some(Tool::Arrow),
            Some(Tool::Pen),
            Some(Tool::Badge),
            Some(Tool::Eraser),
            Some(Tool::Highlight),
        ] {
            for over in [false, true] {
                assert!(!text_press_places_new(t, over), "{t:?} never places text");
            }
        }
    }

    #[test]
    fn a_text_box_edits_on_the_second_body_click_and_resizes_on_a_handle() {
        // The two-state model: single click = selected-not-editing (drag + resize), double
        // click on the BODY = immediate editing.
        assert!(text_body_reopens_editor(true, true, true), "double-click a text body → edit");
        assert!(!text_body_reopens_editor(true, true, false), "a FIRST click only selects it");
        assert!(
            !text_body_reopens_editor(true, false, true),
            "a second click on a resize HANDLE is a resize, never an accidental edit",
        );
        // Only text items have an editor to re-open, so a double-clicked box/arrow keeps its
        // ordinary move/resize — which is also what keeps DRAGON-339 placement untouched for
        // every other tool.
        assert!(!text_body_reopens_editor(false, true, true), "a non-text item has no editor");
    }

    #[test]
    fn the_text_tool_cursor_promises_exactly_what_its_press_will_do() {
        // The I-beam means "this press starts text entry". After DRAGON-364 that is everywhere
        // EXCEPT over an existing text box, whose press now selects/moves/resizes — so the
        // cursor rule is derived from the press rule and the two can never disagree.
        assert!(text_tool_ibeam(Some(Tool::Text), false, false), "empty canvas: text entry");
        assert!(
            !text_tool_ibeam(Some(Tool::Text), true, false),
            "over a settled text box the press manipulates, so no I-beam",
        );
        assert!(
            text_tool_ibeam(Some(Tool::Text), true, true),
            "the box being EDITED keeps the I-beam — that press really does place the caret",
        );
        // Every other tool keeps its own cursor.
        for t in [None, Some(Tool::Pointer), Some(Tool::Rect), Some(Tool::Pen)] {
            assert!(!text_tool_ibeam(t, false, false), "{t:?} is not the text tool");
        }
        // The press rule and the cursor rule agree wherever the text tool is armed and no edit
        // is live: an I-beam iff the press places a new box.
        for over in [false, true] {
            assert_eq!(
                text_tool_ibeam(Some(Tool::Text), over, false),
                text_press_places_new(Some(Tool::Text), over),
                "cursor and press must agree (over_text_item = {over})",
            );
        }
    }

    #[test]
    fn double_click_to_place_survives_for_the_tools_that_have_it() {
        // DRAGON-339/342/354: a click PLACES for the pen, the badge and text — a text press on
        // empty canvas still drops a box, so double-clicking empty canvas keeps placing.
        assert!(Tool::Text.click_places());
        assert!(Tool::Pen.click_places());
        assert!(Tool::Badge.click_places());
        // And Ctrl + a draw tool still lays a NEW shape over an existing item (checked AFTER
        // the text lane, so Ctrl-pressing a text box still draws rather than manipulating).
        assert_eq!(draw_bypassing_items(Some(Tool::Text), true, false), Some(Tool::Text));
        assert_eq!(draw_bypassing_items(Some(Tool::Text), false, false), None);
    }
}
