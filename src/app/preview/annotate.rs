//! The image-annotation scene: the persisted model (shapes in SOURCE-pixel
//! coordinates), off-thread rasterization, full-resolution bake compositing, z-order
//! operations, and the app-side pointer-gesture handlers driving the interaction canvas
//! ([`crate::widgets::annotation_canvas`]).
//!
//! Shapes are stored in SOURCE pixels so the bake is direct and zoom-independent. The
//! preview DISPLAYS them as TRUE VECTOR geometry drawn by the canvas widget (DRAGON-324) —
//! crisp at any zoom, no preview-resolution raster to blur — via [`widget_items`], which
//! carries each shape's geometry PLUS its appearance to the widget. The editing chrome
//! (selection box + handles) is drawn there too and never baked. This module still owns the
//! off-thread [`rasterize_scene`]/[`apply_annotations`] used ONLY by the full-resolution
//! bake (`share.rs`), which composites the same scene at scale 1.0.
//!
//! # Adding a new tool (the whole recipe)
//! Two tools ship as the foundation — Box (rectangle) and Arrow. To add another
//! (line / ellipse / text / numbered / …):
//! 1. **Model**: add a variant to [`AnnotKind`] carrying its SOURCE-pixel geometry.
//! 2. **Rasterize**: add a match arm in [`rasterize_scene`] drawing it with tiny-skia
//!    (scaled by `scale`). The bake reuses this at full resolution — nothing else needed
//!    for baking overlays (destructive kinds like blur/pixelate composite FIRST — see
//!    [`apply_annotations`]).
//! 3. **Toolbar**: add a [`Tool`] variant, then a `TrayItem::Tool` row to the group it belongs
//!    to in `chrome.rs`'s declared `ANNOT_TRAY` layout. The tray is data-driven (DRAGON-340):
//!    each group renders as one bordered `Tb::tool_cluster`, and the buttons inside it are
//!    bare glyphs whose COLOUR alone shows which tool is armed (`Tb::tool_toggle`) — there is
//!    no per-icon ring any more.
//! 4. **Hotkey**: add an `Action` (shortcuts.rs, contiguous in the "Annotation Tools"
//!    group, and BEFORE the slot actions — see `Action::ALL`) mapped to
//!    `PreviewMsg::SelectTool` in keyboard.rs. If the tool joins a family that already
//!    shares one key, give it no default bind and just drop it into that tray group: the
//!    group's `cycle` action picks it up, in the order you declared it (DRAGON-369).
//! 5. **Gesture**: if it draws by drag (like Box/Arrow) it already works via
//!    `DrawBegin`/`GestureTo` — teach [`App::annot_gesture_to`]'s `New` arm how to shape
//!    it, and the canvas widget's hit-testing/chrome how to select it. A click-placed
//!    tool (sticker/numbered) adds a `DrawBegin`-on-press path instead.
//! 6. **Pre-placement** (DRAGON-339): add a match arm in [`spawn_kind`] shaping the tool into
//!    the shared [`default_placement_rect`], so DOUBLE-CLICKING its tray button drops one
//!    ready-made in the middle of the picture. Return `None` there for a tool with no
//!    pre-placeable form (a freehand/stroke tool) — double-click then just picks it.
//!    Nothing else is owed: the Ctrl-overrides-manipulation draw path
//!    (`annotation_canvas::force_new_draw`) is tool-agnostic.
//!
//! A tool that DRAWS NOTHING skips 1, 2, 5 and 6 entirely and only does 3 + 4 — it declares
//! itself in `Tool::draws()` and gets `None`/`return` arms in the model matches. The
//! [`Tool::Pointer`] (pure selection) and the [`Tool::Hand`] (pan, DRAGON-392) are the two.
//!
//! # The freehand pencil + eraser (DRAGON-338), beautified (DRAGON-342)
//! [`AnnotKind::Pen`] is the first NON-rect, non-two-point kind, and shows how far the seams
//! stretch. A drag appends samples to the RAW trail on [`super::edit::EditState::pen_raw`]
//! (thinned by [`PEN_MIN_STEP`]) and re-fits [`crate::pen_stroke`]'s smoothed, pseudo-pressure
//! curve into the MODEL on every sample — so the stored points are always the beautified
//! stroke, the ink you watch being drawn is exactly what commit keeps, and display, bake,
//! hit-testing, the eraser and merge all read one geometry. The parallel `pressure` array is
//! the per-point SPEED signal the width profile rides (the one thing the resample throws away);
//! it is optional by construction — an empty entry reads as neutral. A pencil TAP is a
//! one-point stroke that inks as a firm round DOT ([`normalize_pen_tap`]); with the pencil
//! armed a press is always deliberate ink, so a pen gesture is never discarded as degenerate.
//! Every reach sized off the stroke width (eraser, hit-test, merge slack, the keep-it-inside-
//! the-picture margin) rides [`crate::pen_stroke::max_width`], since a pressure-swelled stretch
//! draws wider than its preset. On commit,
//! [`merge_connected_pens`] folds every same-looking pen group whose ink TOUCHES the new
//! stroke into it — so connected scribbles are ONE selectable item and disconnected ones stay
//! separate. Because a group has no rect of its own, its selection chrome + resize ride its
//! BOUNDING BOX ([`pen_bounds`]) and a resize maps the points affinely into the new box
//! ([`scale_pen`]); the canvas hit-tests along the STROKES, not the (mostly empty) box.
//!
//! # The sequence badge — user-facing "Step Marker" (DRAGON-340)
//! [`AnnotKind::Badge`] is the first kind with a HARD aspect constraint, the first whose
//! appearance depends on the REST of the scene, and the only rect kind that is PLACED rather
//! than dragged out, so it stretches three seams:
//!
//! * **Click to place, never drag to size.** A marker is dropped at a POINT, not swept over a
//!   region: the press drops a finished square centred on it at
//!   [`badge_placement_rect`]-clamped size, the drag does nothing at all (the `New` arm of
//!   `annot_gesture_to` skips it), and the canvas therefore completes the whole gesture on a
//!   bare click (`Tool::click_places`, shared with the pencil's tap-inks-a-dot rule). Click and
//!   click-drag land the same badge in the same place; every OTHER draw tool still needs a real
//!   drag, untouched.
//! * **Remembered size, PERSISTED.** The side comes from [`super::edit::EditState::badge_size`]
//!   — the last badge placed or resized, falling back to [`DEFAULT_BADGE_SIZE`] only when
//!   nothing has ever been remembered. Every placement and resize writes it back through
//!   [`App::remember_badge_size`] to the persisted `App::annot_badge_size`, so FUTURE editors
//!   (a new capture process, a later launch) spawn markers at it; the `EditState` field is the
//!   per-document working copy, seeded at document open. BOTH spawn paths — click-to-place and
//!   the double-click pre-placement ([`spawn_placement_rect`]) — size through the SAME
//!   [`badge_placement_rect`], so they can never drift apart. Undoing a resize does not
//!   un-remember it, since the remembered size is tool state rather than scene state.
//! * **Always 1:1.** Every path that can change its rect squares it: the placement
//!   ([`badge_placement_rect`], which shrinks BOTH axes together when the picture is too
//!   small), a resize ([`square_for_grab`], which also picks the anchor from the grab so the
//!   handle you are NOT holding stays put), the pre-placement ([`centered_square`]) and the
//!   image clamp ([`clamp_square`] — the ordinary `clamp_rect` would flatten a badge against an
//!   edge). It is deliberately NOT a member of the rect-conversion family
//!   ([`rect_family_id`]) — converting a wide box into a badge would silently reshape the
//!   user's geometry.
//! * **Derived numbering.** A badge stores NO number. [`badge_numbers`] hands out `1..N` over
//!   the badges in scene order, and every renderer resolves the ordinal through it on each
//!   draw. Deleting one renumbers the rest for free, and undo/redo restore correct numbers
//!   because they restore the item vector the numbering is a function of. Never add a stored
//!   index; it cannot survive either operation without bookkeeping that goes stale.
//!
//! Its DRAWING (disc / gap / ring / numerals / the contrast ink) is [`crate::badge`], the
//! canvas-and-bake shared module — the same split `crate::pen_stroke` uses for the pencil.
//!
//! A pencil press NEVER selects (DRAGON-346): it bypasses the canvas's hit-testing entirely and
//! always inks, even straight over an existing shape — a stroke landing while the shape under
//! the press wore selection chrome read as a manipulation that wasn't happening. Selection is
//! the pointer's job alone.
//!
//! The ERASER (`Tool::Eraser`) is not a draw tool at all — like the pencil its press never
//! selects or moves, and it is the only one that skips the drag threshold outright. It opens an
//! [`AnnotGesture::Erase`] sweep that MARKS the pen groups its
//! travelled SEGMENT touches ([`pen_hit_by_eraser`]) into
//! [`super::edit::EditState::erase_marks`]; marked groups draw at [`ERASE_PREVIEW_ALPHA`]
//! (the preview of what's going) and RELEASE deletes them all as ONE undo entry. Only pen
//! groups erase — a sweep must never silently take out a redaction it passed over.
//!
//! # The pointer + multi-selection (DRAGON-341)
//! `Tool::Pointer` creates NOTHING: it is the mode in which the selection
//! ([`super::edit::Selection`], an ordered set whose last member is the PRIMARY) is edited —
//! Ctrl/Shift-click toggles a member, an empty-canvas drag rubber-bands ([`items_in_band`]),
//! and dragging any selected body moves them all ([`AnnotGesture::MoveMany`], one delta clamped
//! once on the union bounds via [`group_move_delta`], committed as ONE undo entry). It is also
//! the ONLY tool under which pen groups are selectable AT ALL: ink never swallows clicks meant
//! for the shapes under it ([`crate::widgets::annotation_canvas`]'s `pen_selectable`), a drawn
//! stroke never selects itself ([`kind_selects_on_create`]), and arming any other tool prunes
//! pen ids out of the set ([`super::edit::EditState::drop_pen_selection`]) so the visible
//! selection and the real one can never disagree. Single-item operations (resize, duplicate,
//! reorder, kind conversion)
//! still act on the PRIMARY alone; whole-selection operations (move, delete, color, width) walk
//! the set. A new tool must decide its `spawn_kind` arm and, if it creates nothing, return
//! `None` there and answer `false` to `Tool::draws`.

use super::*;
use crate::widgets::annotation_canvas::{FxKind, Grab, Item, ItemKind, Tool};
use ::image::RgbaImage;

/// A straight-alpha RGBA color.
pub type AnnotColor = [u8; 4];

/// The SHARED default stroke width in logical POINTS (DRAGON-383), seeded onto every new box
/// AND arrow — the single source of truth a future width control drives (see
/// [`super::edit::EditState::stroke`]). A preset/preference is a POINT measure so a "4px" stroke
/// spans the SAME visual size on a 1x and a 2x capture; it becomes concrete SOURCE-pixel
/// geometry through [`points_to_source_px`] at the moment a shape is born. On any UNSCALED (1x)
/// output — on every platform — points == source px, so this is unchanged there.
pub const DEFAULT_ANNOT_STROKE: f32 = 4.0;

/// The selectable stroke-width presets in logical POINTS (DRAGON-383) the toggle group offers,
/// thin → thick (DRAGON-357 item 9: a 1px option leads and the run continues +2px past the
/// former 6px top, to 8/10/12). `DEFAULT_ANNOT_STROKE` (4pt) stays a preset. The WORKING default
/// ([`super::edit::EditState::annot_stroke_w`]) and the persisted preference are kept in these
/// same POINT units, so the ladder, [`stroke_width_nearest_index`] and the chrome flyout all
/// compare in ONE unit and the highlighted segment stays correct on a 2x document; only the
/// value re-stroked ONTO a shape is scaled to source px. Migration: a width persisted by an
/// older build was effectively source px — on an UNSCALED output that number is unchanged
/// (px == pt); on a scaled one (2x mac, a 200% Windows monitor, a scaled COSMIC output) it now
/// reads as points (numerically identical, so a "8" stays "8", just reinterpreted), which
/// self-corrects the instant a preset is picked.
pub const STROKE_WIDTHS: [f32; 7] = [1.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0];

/// Convert a preset / persisted annotation dimension expressed in logical POINTS into the SOURCE
/// pixels an annotation stores and renders in, for a document whose backing scale is `scale`
/// (macOS Retina = 2.0). Annotation GEOMETRY lives in SOURCE px (`AnnotRect`, a shape's own
/// `stroke_w`, a text box's `size_px`); every PRESET and PERSISTED PREFERENCE (the stroke ladder,
/// the badge/text sizes, the curve radius) is a POINT measure so the same preset spans the same
/// visual size across DPIs (DRAGON-383). `scale` is the SOURCE output's own reported scale on
/// EVERY platform (COSMIC buffer scale, macOS backing scale, Windows per-monitor DPI — see
/// `PreviewState::source_scale`); it is `1.0` on an UNSCALED panel, and there this is the
/// identity so every seeding path stays byte-identical. A non-positive scale is treated as
/// `1.0` (defensive). Pure — unit-tested.
pub fn points_to_source_px(points: f32, scale: f32) -> f32 {
    points * if scale > 0.0 { scale } else { 1.0 }
}

/// The inverse of [`points_to_source_px`]: bring a SOURCE-pixel annotation dimension back to the
/// logical POINTS the presets + persisted preferences are kept in — used when a badge RESIZE or a
/// placed badge's settled side seeds the remembered default, so the number stored matches the
/// preset ladder's unit and re-seeds correctly on a DIFFERENT-scale document (DRAGON-383).
/// Identity on an unscaled (1x) output, on every platform. Pure — unit-tested.
pub fn source_px_to_points(px: f32, scale: f32) -> f32 {
    px / if scale > 0.0 { scale } else { 1.0 }
}

/// The index into [`STROKE_WIDTHS`] whose preset is nearest `current` — which segment of the
/// width toggle group reads as active. Argmin of the absolute difference (ties pick the
/// lower index). Pure — unit-tested.
pub fn stroke_width_nearest_index(current: f32) -> usize {
    let mut best = 0;
    let mut best_d = f32::INFINITY;
    for (i, &w) in STROKE_WIDTHS.iter().enumerate() {
        let d = (w - current).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// The next stroke-width preset after `current`, wrapping (1 → 2 → 4 → 6 → 8 → 10 → 12 → 1 …) —
/// the `L` hotkey's cycle. Keyed off the nearest preset to `current`. Pure — unit-tested.
pub fn cycle_stroke_width(current: f32) -> f32 {
    let i = stroke_width_nearest_index(current);
    STROKE_WIDTHS[(i + 1) % STROKE_WIDTHS.len()]
}

/// The SHARED default corner curve as an ABSOLUTE radius in logical POINTS (DRAGON-383), read by
/// BOTH the box (a CONSTANT corner radius regardless of box size, reduced only when the box is
/// too small to fit it) and the arrow (round caps/joins when > 0) — the single source of truth a
/// future curve control drives (see [`super::edit::EditState::curve_radius`]). Like every other
/// preset it is a POINT measure, scaled to source px through [`points_to_source_px`] at each
/// render/bake site so the corner reads the same on a 1x and a 2x capture; identity on an
/// unscaled (1x) output.
pub const DEFAULT_ANNOT_CURVE_RADIUS: f32 = 8.0;

/// A stable per-item identity (scene z-order is the vector order, so ids only identify).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AnnotId(pub u64);

/// A point in image SOURCE pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AnnotPoint {
    pub x: f32,
    pub y: f32,
}

/// A rectangle in image SOURCE pixels. Stored normalized (`w`, `h` ≥ 0) once committed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AnnotRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl AnnotRect {
    /// A normalized rect spanning the two corner points.
    fn from_points(a: (f32, f32), b: (f32, f32)) -> Self {
        let x = a.0.min(b.0);
        let y = a.1.min(b.1);
        Self { x, y, w: (a.0 - b.0).abs(), h: (a.1 - b.1).abs() }
    }
    fn corners(&self) -> (f32, f32, f32, f32) {
        (self.x, self.y, self.x + self.w, self.y + self.h)
    }
}

/// The bounding-box UNION of two rects (DRAGON-389). Pure.
fn union_rect(a: AnnotRect, b: AnnotRect) -> AnnotRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    AnnotRect { x, y, w: right - x, h: bottom - y }
}

impl super::edit::EditState {
    /// The rectangle (SOURCE px) annotations may be placed / moved / resized within — DRAGON-389's
    /// single authoritative "annotatable bounds".
    ///
    /// It is the UNION of the source frame (origin `(0,0)`, size [`Self::frame`]) and the APPLIED
    /// crop rect ([`Self::crop`], which may have a NEGATIVE origin and/or extend PAST the source —
    /// the over-crop black extension, DRAGON-382/385). The union (not just the crop) keeps the
    /// WHOLE source annotatable when a crop sits wholly inside it: out-of-crop annotations
    /// legitimately exist and clip away, exactly as they have since DRAGON-385.
    ///
    /// DERIVED, never stored — recomputed per call so a crop accept / undo / redo updates it for
    /// free. During a LIVE crop session the working rect is UNCOMMITTED, so only the committed
    /// [`Self::crop`] counts (annotations are display-only during a session anyway, DRAGON-387).
    pub fn annot_bounds(&self) -> AnnotRect {
        let src = AnnotRect { x: 0.0, y: 0.0, w: self.frame.0 as f32, h: self.frame.1 as f32 };
        match self.crop {
            Some(c) => union_rect(src, AnnotRect { x: c.x, y: c.y, w: c.w, h: c.h }),
            None => src,
        }
    }
}

// ── DRAGON-389: origin-aware (annotatable-bounds) forms of the clamp/placement kernels ────────
//
// The pure clamp/placement/layout kernels below ([`edited_kind`], [`reflow_text`],
// [`badge_placement_rect`], [`spawn_placement_rect`], …) all bound against a SIZE `(fw, fh)` with
// an implicit `(0,0)` origin. The annotatable canvas (source ∪ crop) can have a NEGATIVE origin,
// so these thin wrappers shift the whole problem into bounds-origin space, run the proven
// origin-0 kernel at the bounds SIZE, and shift the result back. That routes every placement /
// move / resize through [`super::edit::EditState::annot_bounds`] without duplicating (and risking
// drift in) the clamp math — with a `bounds` at origin `(0,0)` each is byte-identical to the
// kernel it wraps. Pure — unit-tested (see the `dragon389_*` tests).

/// [`edited_kind`] clamped against annotatable `bounds` instead of a `(0,0)`-origin frame.
fn edited_kind_in_bounds(
    original: &AnnotKind,
    grab: Grab,
    press: (f32, f32),
    cur: (f32, f32),
    bounds: AnnotRect,
    scale_type: bool,
) -> AnnotKind {
    let (ox, oy) = (bounds.x, bounds.y);
    // Only `dx = cur - press` reaches the kernel's clamps, so shifting the geometry is enough;
    // press/cur ride through unchanged (their difference is origin-invariant).
    let shifted = translated_kind(original, -ox, -oy);
    let out = edited_kind(&shifted, grab, press, cur, (bounds.w, bounds.h), scale_type);
    translated_kind(&out, ox, oy)
}

/// [`reflow_text`] laid out and clamped against annotatable `bounds` instead of a `(0,0)`-origin
/// frame — so a caption's auto-wrap cap and its [`TEXT_MIN_ON_CANVAS_PX`] edge rule both track the
/// extended canvas.
fn reflow_text_in_bounds(
    text: &str,
    size_px: f32,
    font: super::text_annot::TextFont,
    rect: AnnotRect,
    constrained: bool,
    stroke_w: f32,
    bounds: AnnotRect,
) -> AnnotKind {
    let (ox, oy) = (bounds.x, bounds.y);
    let shifted = AnnotRect { x: rect.x - ox, y: rect.y - oy, ..rect };
    let k = reflow_text(text, size_px, font, shifted, constrained, stroke_w, (bounds.w, bounds.h));
    translated_kind(&k, ox, oy)
}

/// [`badge_placement_rect`] centred/clamped within annotatable `bounds` — so a badge can be
/// click-placed over the crop extension.
fn badge_placement_in_bounds(center: (f32, f32), want: f32, bounds: AnnotRect, margin: f32) -> AnnotRect {
    let (ox, oy) = (bounds.x, bounds.y);
    let size = (bounds.w.round().max(0.0) as u32, bounds.h.round().max(0.0) as u32);
    let r = badge_placement_rect((center.0 - ox, center.1 - oy), want, size, margin);
    AnnotRect { x: r.x + ox, y: r.y + oy, ..r }
}

/// [`spawn_placement_rect`] centred within annotatable `bounds` — so a double-click drop lands in
/// the middle of the extended canvas, not the source.
fn spawn_placement_in_bounds(tool: Tool, bounds: AnnotRect, margin: f32, badge_want: f32) -> AnnotRect {
    let (ox, oy) = (bounds.x, bounds.y);
    let size = (bounds.w.round().max(0.0) as u32, bounds.h.round().max(0.0) as u32);
    let r = spawn_placement_rect(tool, size, margin, badge_want);
    AnnotRect { x: r.x + ox, y: r.y + oy, ..r }
}

/// What an annotation draws. Add a variant here to add a tool (see the module doc).
/// The stroke COLOR lives on [`AnnotationItem`]; `fill` is a box's optional interior.
#[derive(Clone, PartialEq, Debug)]
pub enum AnnotKind {
    /// A rectangle outline (with optional fill).
    Box { rect: AnnotRect, stroke_w: f32, fill: Option<AnnotColor> },
    /// An arrow from `a` (tail) to `b` (head).
    Arrow { a: AnnotPoint, b: AnnotPoint, stroke_w: f32 },
    /// A highlighter (DRAGON-326): an ADAPTIVE multiply/screen-blended rounded box, weighted
    /// by [`HIGHLIGHT_ALPHA`]. Composites through the shared CPU core [`apply_one_effect`] on
    /// BOTH display and bake; NOT a source-over fill. Same geometry + interaction as
    /// [`Self::Box`].
    Highlight { rect: AnnotRect },
    /// A box-highlight (DRAGON-333): BOTH an adaptive-highlight FILL (identical to
    /// [`Self::Highlight`] — composites through [`apply_one_effect`] on display + bake) AND a
    /// box OUTLINE stroke on top (identical to [`Self::Box`], an always-on-top vector). Carries
    /// the box's `stroke_w`; the shared [`AnnotationItem::color`] tints BOTH the fill and the
    /// outline. Same geometry + interaction as [`Self::Box`].
    BoxHighlight { rect: AnnotRect, stroke_w: f32 },
    /// A spotlight knockout (DRAGON-329): a Box-geometry region that renders NOTHING of its own
    /// (no stroke, no fill, no effect) — it only contributes its rect to the global dim's
    /// knockout UNION, so the underlying image shows through at full brightness inside it. Same
    /// geometry + interaction as [`Self::Box`]; carries no color/stroke. When SELECTED it shows
    /// the box selection chrome so it can be moved/resized/deleted; unselected it is invisible
    /// on its own (but visible as the bright hole it punches whenever the dim is non-zero).
    Spotlight { rect: AnnotRect },
    /// A DESTRUCTIVE pixelate redaction (DRAGON-327): the region is replaced by its
    /// block-averaged mosaic, whose CELL SIZE adapts to the region's content
    /// ([`content_pixelate_block`], floored at [`PIXELATE_BLOCK`], capped at
    /// [`PIXELATE_BLOCK_MAX`]) so text of any size is obfuscated. Same geometry/interaction as a
    /// box; the sub-cell detail is unrecoverable once baked.
    Pixelate { rect: AnnotRect },
    /// A SEQUENCE BADGE (DRAGON-340): a filled disc in [`AnnotationItem::color`], a small clear
    /// gap, and an outer RING at `ring_w` (the current line weight) — with the badge's ORDINAL
    /// centred on the disc in whichever ink contrasts with that colour. All the drawing figures
    /// come from [`crate::badge::metrics`], the one module the canvas and the bake share.
    ///
    /// Three things make it unlike every other kind:
    /// * it is PLACED by a click, not dragged out ([`badge_placement_rect`], at the editor's
    ///   remembered side) — the only rect kind whose creation gesture is a point;
    /// * `rect` is ALWAYS 1:1 — [`badge_placement_rect`] / [`square_for_grab`] force it on
    ///   creation, on resize and on every clamp, so the badge can never be squashed into an
    ///   oval;
    /// * it stores NO number. The ordinal is DERIVED from the badge's position among the
    ///   scene's badges ([`badge_numbers`]) every time anything is drawn, which is what keeps
    ///   the set a contiguous `1..N` through deletes, undo and redo with no bookkeeping at all.
    Badge { rect: AnnotRect, ring_w: f32 },
    /// A DESTRUCTIVE blur redaction (DRAGON-328): the region is replaced by [`BLUR_PASSES`]
    /// stacked [`BLUR_BLOCK`] box blurs ([`box_blur_stack`]) — a strong smooth (≈ Gaussian) blur.
    /// Same geometry/interaction as a box; irreversible once baked.
    Blur { rect: AnnotRect },
    /// FREEHAND pen strokes (DRAGON-338): a GROUP of polylines (SOURCE px) that all read as one
    /// drawing — every stroke that TOUCHES another one in the group (see
    /// [`merge_connected_pens`]), so connected scribbles select/move/delete as a unit while
    /// disconnected ones stay separate items. Stored as pure VECTOR points at the shared stroke
    /// width, so it stays crisp at any zoom and only rasterizes at bake time. Selection chrome +
    /// resize ride the group's bounding box; a resize scales the points affinely into the new box.
    ///
    /// The points are the SMOOTHED centerline (DRAGON-342) — the drag re-fits
    /// [`crate::pen_stroke::smooth_path`] into the model on every sample, so the beautified
    /// stroke IS what was drawn (nothing re-shapes at commit) and hit-testing / the eraser /
    /// merge / the bake all read the one geometry the canvas draws. `pressure` is the parallel
    /// per-point SPEED signal the pseudo-pressure width profile rides
    /// ([`crate::pen_stroke::pressure_along`]) — the one thing the resample throws away and the
    /// bake cannot recompute. It is OPTIONAL by construction: an empty (or wrong-length) entry
    /// reads as neutral pressure, so a stroke built without a trail still profiles from its
    /// curvature alone. `pressure[i]` belongs to `paths[i]`; helpers keep them in step.
    Pen { paths: Vec<Vec<AnnotPoint>>, pressure: Vec<Vec<f32>>, stroke_w: f32 },
    /// A TEXT annotation (DRAGON-354): wrapped text in an EMBEDDED font, laid out and
    /// rasterized by [`super::text_annot`] (the ONE renderer both the live layer and the bake
    /// call, so they agree pixel-for-pixel). `rect` is the drawn box in SOURCE px, recomputed
    /// from the layout on every edit; `size_px` is the SOURCE-pixel font size (zooms with the
    /// picture like the stroke width); `font` picks the handwritten vs clean family; and
    /// `constrained` is `true` for a DRAG box (text wraps within the fixed `rect.w`) and
    /// `false` for a CLICK box (the box auto-sizes to the widest line). The stroke
    /// [`AnnotationItem::color`] is the ink. `stroke_w` is the active LINE WIDTH (SOURCE px)
    /// captured at creation (DRAGON-358): it maps through
    /// [`super::text_annot::text_stroke_width`] to an OUTLINE weight painted under the fill, so
    /// the width group thickens text the way it thickens a box/arrow stroke — and it re-styles
    /// on a selected box exactly like the color does. Outline-only, so the shared layout
    /// ([`text_kind_layout`]) is untouched and live/bake parity holds. Selection chrome +
    /// move/resize ride `rect` exactly like a [`Self::Box`]; the glyphs never draw on the vector
    /// canvas (the raster layer owns them).
    Text { rect: AnnotRect, text: String, size_px: f32, font: super::text_annot::TextFont, constrained: bool, stroke_w: f32 },
}

impl AnnotKind {
    /// Whether this kind has a user-facing COLOR (box outline/fill, arrow, highlight tint).
    /// The destructive redactions (pixelate/blur) derive their pixels from the base and carry
    /// no color, so a color change must skip them.
    pub fn is_colorable(&self) -> bool {
        matches!(
            self,
            AnnotKind::Box { .. }
                | AnnotKind::Arrow { .. }
                | AnnotKind::Highlight { .. }
                | AnnotKind::BoxHighlight { .. }
                | AnnotKind::Pen { .. }
                // A badge's colour drives BOTH its disc/ring and (through the contrast rule)
                // its numeral ink, so recolouring it is meaningful.
                | AnnotKind::Badge { .. }
                // Text ink IS its colour (DRAGON-354).
                | AnnotKind::Text { .. }
        )
    }

    /// Whether this is a SEQUENCE BADGE (DRAGON-340) — the auto-numbered, always-square kind.
    pub fn is_badge(&self) -> bool {
        matches!(self, AnnotKind::Badge { .. })
    }

    /// Whether this is a freehand PEN group (DRAGON-338) — the only kind the eraser removes.
    pub fn is_pen(&self) -> bool {
        matches!(self, AnnotKind::Pen { .. })
    }

    /// Whether this kind is a REGION EFFECT (highlight / pixelate / blur) — the kinds that
    /// composite through the true-z-order CPU stack ([`apply_one_effect`]), as opposed to the
    /// always-on-top vector shapes (box / arrow) the [`crate::widgets::annotation_canvas`]
    /// draws. The effect walk visits only these, in scene z-order.
    pub fn is_effect(&self) -> bool {
        matches!(
            self,
            AnnotKind::Highlight { .. }
                | AnnotKind::Pixelate { .. }
                | AnnotKind::Blur { .. }
                // BoxHighlight's FILL is a highlight effect (the outline is a separate vector,
                // drawn on top by the always-on-top canvas — DRAGON-333).
                | AnnotKind::BoxHighlight { .. }
        )
    }
}

/// Highlighter multiply weight (≈ 38.5% of 255): how strongly the highlight color multiplies
/// the content under it.
pub const HIGHLIGHT_ALPHA: u8 = 98;

/// The pixelate mosaic cell-size FLOOR (SOURCE px): the smallest content-aware cell
/// [`content_pixelate_block`] ever returns, so a small/clean region still shows a genuine
/// mosaic (never a no-op) — and the historical fixed size a flat region collapses to. Each cell
/// is replaced by its block mean; detail finer than the chosen cell is destroyed. Grid-aligned
/// so the display shader (NEAREST sample of the block-mean texture) and the bake share the exact
/// same blocks.
///
/// DRAGON-383 audit: the pixelate cell is CONTENT-adaptive — it resolves feature spacing in
/// source px, so a 2x capture (2x-resolution content) already produces a proportionally larger
/// cell by construction. It is not preset/preference driven, so it stays SOURCE px and is left
/// unscaled (mirroring [`BLUR_BLOCK`]'s audit); the points-to-source-px rule targets presets.
pub const PIXELATE_BLOCK: u32 = 8;

/// The pixelate cell-size CEILING (SOURCE px): the content-aware size never exceeds this, so the
/// GPU shader's per-fragment O(block²) mosaic loop stays bounded even on a 4K image (a big region
/// can't explode) while still destroying large glyphs. Sits in the same cost envelope as the blur
/// low-pass ([`BLUR_BLOCK`]). A multiple of 4 (the block snap step in [`content_pixelate_block`]).
pub const PIXELATE_BLOCK_MAX: u32 = 48;

/// The blur cell size (SOURCE px): a COARSER block mean than pixelate, bilinearly upsampled
/// (display: LINEAR sample of the same block-mean texture; bake: `Triangle` resize). One pass is
/// too weak, so the standalone Blur effect STACKS [`BLUR_PASSES`] of them ([`box_blur_stack`]) —
/// three stacked box blurs approximate a Gaussian, reading as a strong SMOOTH blur that destroys
/// text/faces. The adaptive highlight's low-pass reuses this block at ONE pass (its own strength
/// is unchanged).
///
/// DRAGON-383 audit: this stays SOURCE px, NOT converted to points. It is a redaction-strength
/// constant (how much real image detail a blur destroys), not a user preset/preference, and
/// there is no picker driving it — so the points-to-source-px rule (which targets presets +
/// persisted prefs) does not apply. Fixed in source px, a 2x capture's blur destroys the same
/// COUNT of content pixels, which is the property that matters for a redaction.
pub const BLUR_BLOCK: u32 = 32;

/// How many single-pass box blurs the standalone Blur effect stacks (≈ a Gaussian). The highlight
/// low-pass deliberately does NOT stack — it stays a single pass so its look is unchanged.
pub const BLUR_PASSES: u32 = 3;

/// One annotation: a stable id, its stroke color, and its kind.
#[derive(Clone, PartialEq, Debug)]
pub struct AnnotationItem {
    pub id: AnnotId,
    pub color: AnnotColor,
    pub kind: AnnotKind,
}

/// The active pointer gesture on the canvas — set at `DrawBegin`/`GrabBegin`, consumed at
/// `GestureEnd`. Kept on [`super::edit::EditState`].
#[derive(Clone, Debug)]
pub enum AnnotGesture {
    /// Drawing a brand-new shape `id` from press point `press` (image px).
    New { press: (f32, f32), id: AnnotId },
    /// Editing existing item `id`: `grab` is what's dragged, `original` its geometry at
    /// grab start, `press` the grab's image-px anchor.
    Edit { press: (f32, f32), id: AnnotId, grab: Grab, original: AnnotKind },
    /// Moving a MULTI-selection as one (DRAGON-341): every selected item's geometry at grab
    /// start, plus the union of their DRAWN bounds. The drag applies ONE delta — clamped once
    /// against `bounds` ([`group_move_delta`]) rather than per item — so the arrangement is
    /// rigid: nothing squeezes together when the group meets an image edge.
    MoveMany { press: (f32, f32), originals: Vec<(AnnotId, AnnotKind)>, bounds: AnnotRect },
    /// Scaling a MULTI-selection as one (DRAGON-388): every selected item's geometry at grab
    /// start, the union of their DRAWN bounds, and the group-box handle being dragged. The drag
    /// maps every member through ONE uniform scale ([`group_scaled_kind`]) about the union's
    /// fixed anchor corner ([`group_scale_anchor`]) — a SIMILARITY, so relative layout, overlaps
    /// and every per-kind aspect (badges square, text aspect-locked) are preserved by
    /// construction. The shared factor is clamped ONCE ([`clamp_group_scale`]) so no item
    /// collapses; committed as ONE undo entry, exactly like [`Self::MoveMany`].
    ScaleMany {
        press: (f32, f32),
        originals: Vec<(AnnotId, AnnotKind)>,
        bounds: AnnotRect,
        grab: Grab,
    },
    /// An ERASER sweep (DRAGON-338): `last` is the previous sampled point, so each update
    /// tests the SEGMENT the eraser travelled (never just the sampled points — a fast drag
    /// would otherwise jump clean over a stroke). Marks accumulate in
    /// [`super::edit::EditState::erase_marks`]; releasing deletes them as ONE undo entry.
    Erase { last: (f32, f32) },
}

// ── Pre-placed items: double-click a tool to spawn one (DRAGON-339) ─────────────────────

/// The default spawn WIDTH (SOURCE px) of a pre-placed item — the size a double-clicked tool
/// drops in the middle of the picture, unless 80% of the image is smaller (see
/// [`default_placement_rect`]).
pub const SPAWN_W: f32 = 200.0;
/// The default spawn HEIGHT (SOURCE px) of a pre-placed item. See [`SPAWN_W`].
pub const SPAWN_H: f32 = 100.0;
/// The fraction of the image a pre-placed item may occupy per axis when [`SPAWN_W`]/[`SPAWN_H`]
/// would not fit — so a tiny capture still gets a usable, clearly-inset item.
pub const SPAWN_MAX_FRAC: f32 = 0.8;

/// ONE axis of the pre-placement size rule (DRAGON-339): the WANTED extent, capped at
/// [`SPAWN_MAX_FRAC`] of that axis and at the room left once the item's DRAWN margin `m` is
/// reserved on BOTH sides. Never negative. The single source both the double-click spawn
/// ([`default_placement_rect`]) and the badge's click-to-place ([`badge_placement_rect`])
/// size themselves through, so "too big for this picture" means the same thing in both.
/// Pure — unit-tested.
fn placement_extent(full: f32, want: f32, m: f32) -> f32 {
    let room = (full - 2.0 * m).max(0.0);
    want.min(full * SPAWN_MAX_FRAC).min(room).max(0.0)
}

/// The rect a DOUBLE-CLICKED tool spawns its item in (DRAGON-339): [`SPAWN_W`]×[`SPAWN_H`] or
/// [`SPAWN_MAX_FRAC`] of the image per axis — whichever FITS — CENTERED in the frame, and
/// further shrunk so the item's DRAWN extent (geometry grown by `margin`, i.e. half the stroke —
/// the `kind_draw_margin` overhang) still lands inside the picture. Each axis is independent, so a
/// wide-but-short image gets a wide-but-short item. Degenerate frames yield a zero rect (the
/// caller discards it, exactly like a degenerate drag). Pure — unit-tested.
pub fn default_placement_rect(frame: (u32, u32), margin: f32) -> AnnotRect {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let m = margin.max(0.0);
    let w = placement_extent(fw, SPAWN_W, m);
    let h = placement_extent(fh, SPAWN_H, m);
    AnnotRect { x: (fw - w) * 0.5, y: (fh - h) * 0.5, w, h }
}

/// The side (SOURCE px) a sequence badge is born at when NOTHING has been remembered yet —
/// the fallback behind [`super::edit::EditState::badge_size`], reached only on a fresh install
/// (or after a config reset), since the remembered side is persisted.
///
/// SOURCE pixels, not screen pixels: every annotation geometry in this module is source-space
/// (see [`AnnotRect`]), as are [`DEFAULT_ANNOT_STROKE`] and [`SPAWN_W`]/[`SPAWN_H`]. That keeps
/// a placed badge a FIXED fraction of the picture — the same in the preview at any zoom, in a
/// re-opened preview, and in the bake, which composites at full source resolution. The
/// trade-off is the honest one: on a 4K grab a 75px badge reads smaller on screen than on a
/// 720p one. Sizing in screen px would invert that (consistent on screen, drifting against the
/// image), and the badge belongs to the image.
pub const DEFAULT_BADGE_SIZE: f32 = 75.0;

/// The square a CLICK-PLACED sequence badge takes (DRAGON-340 follow-up): side `want`,
/// CENTRED on the click, sized by the same pre-placement rule the double-click spawn uses
/// ([`placement_extent`] per axis, then the TIGHTER axis wins so the result is still 1:1),
/// then slid inside the picture by [`clamp_square`].
///
/// So a badge clicked into the corner of a big picture is exactly `want` wide and merely
/// nudged clear of the edge, while one clicked into a picture too small to hold `want` shrinks
/// on BOTH axes together — never flattened, which is the badge's whole invariant. A degenerate
/// frame yields a zero square, which the caller discards like a degenerate drag.
/// Pure — unit-tested.
pub fn badge_placement_rect(
    center: (f32, f32),
    want: f32,
    frame: (u32, u32),
    margin: f32,
) -> AnnotRect {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let m = margin.max(0.0);
    // 1:1 means ONE side: the smaller of what each axis allows.
    let s = placement_extent(fw, want.max(0.0), m).min(placement_extent(fh, want.max(0.0), m));
    let centred = AnnotRect { x: center.0 - s * 0.5, y: center.1 - s * 0.5, w: s, h: s };
    clamp_square(centred, fw, fh, m)
}

/// The rect a DOUBLE-CLICKED `tool` pre-places its item in — the geometry half of
/// [`App::spawn_annotation`], split out so it is pure and unit-tested.
///
/// Every ordinary tool takes the shared [`default_placement_rect`] (the 200×100 spawn box).
/// The SEQUENCE BADGE does NOT: a step marker has a REMEMBERED size, and dropping one from the
/// tray must produce exactly the marker a click would — so it goes through
/// [`badge_placement_rect`] at `badge_want` (the editor's [`super::edit::EditState::badge_size`])
/// and is merely CENTRED in the picture instead of dropped on the pointer. That is the whole
/// difference between the two spawn routes; the SIZE rule and the clamp are shared, so the two
/// can never drift (they used to: the badge inherited the spawn box's shorter axis, ignoring
/// the remembered side entirely).
pub fn spawn_placement_rect(
    tool: Tool,
    frame: (u32, u32),
    margin: f32,
    badge_want: f32,
) -> AnnotRect {
    if tool == Tool::Badge {
        let (fw, fh) = (frame.0 as f32, frame.1 as f32);
        badge_placement_rect((fw * 0.5, fh * 0.5), badge_want, frame, margin)
    } else {
        default_placement_rect(frame, margin)
    }
}

/// The kind a DOUBLE-CLICKED `tool` spawns inside `rect` (DRAGON-339), or `None` for a tool that
/// has NO pre-placeable form — a freehand/stroke tool draws only under the pointer, so
/// double-clicking it must stay a plain tool pick. Every rect-geometry tool maps straight onto
/// the placement rect; the arrow spans it corner-to-corner (NW → SE), so it reads as a real
/// arrow rather than a dot. Pure — unit-tested.
pub fn spawn_kind(tool: Tool, rect: AnnotRect, stroke_w: f32) -> Option<AnnotKind> {
    Some(match tool {
        Tool::Rect => AnnotKind::Box { rect, stroke_w, fill: None },
        Tool::Highlight => AnnotKind::Highlight { rect },
        Tool::BoxHighlight => AnnotKind::BoxHighlight { rect, stroke_w },
        Tool::Spotlight => AnnotKind::Spotlight { rect },
        Tool::Pixelate => AnnotKind::Pixelate { rect },
        Tool::Blur => AnnotKind::Blur { rect },
        Tool::Arrow => AnnotKind::Arrow {
            a: AnnotPoint { x: rect.x, y: rect.y },
            b: AnnotPoint { x: rect.x + rect.w, y: rect.y + rect.h },
            stroke_w,
        },
        // A badge is ALWAYS 1:1, so the placement rect is squared down to its shorter axis and
        // re-centred — it must never spawn as the 200×100 oval the shared rect would give.
        Tool::Badge => AnnotKind::Badge {
            rect: centered_square(rect),
            ring_w: stroke_w,
        },
        // A freehand stroke has no meaningful default geometry; the eraser creates no item at
        // all; the POINTER (DRAGON-341) is pure selection and the HAND (DRAGON-392) only pans;
        // and TEXT (DRAGON-354) is typed into, not pre-placed — double-clicking any of their
        // tray buttons just picks the tool.
        Tool::Pen | Tool::Eraser | Tool::Pointer | Tool::Hand | Tool::Text => return None,
    })
}

/// Re-lay-out a text box after any content / size / box change (DRAGON-354), the SINGLE seam
/// creation, per-keystroke edits and a resize all reflow through — so the drawn box always hugs
/// its text identically everywhere. Keeps the box ORIGIN; a `constrained` (dragged) box wraps
/// within its fixed width, an auto (clicked) box grows to its widest line capped at the
/// PICTURE's width ([`text_auto_cap`]); the height snaps to the wrapped line count. The origin
/// is clamped to keep the box on `frame` (DRAGON-368: 5px of it, not all of it —
/// [`clamp_text_rect_on_canvas`]). `stroke_w` (the active line width, SOURCE px) rides along
/// as the glyph OUTLINE weight (DRAGON-358) — it does NOT affect the layout (outline-only), so
/// the geometry is unchanged by it. Pure.
pub fn reflow_text(
    text: &str,
    size_px: f32,
    font: super::text_annot::TextFont,
    rect: AnnotRect,
    constrained: bool,
    stroke_w: f32,
    frame: (f32, f32),
) -> AnnotKind {
    let (fw, fh) = frame;
    let lay = text_kind_layout(text, size_px, font, rect, constrained, fw);
    let (w, h) = (lay.box_w, lay.box_h);
    // DRAGON-368: held to [`TEXT_MIN_ON_CANVAS_PX`] of the box on the picture rather than wholly
    // inside it. This is THE seam a text box's position is normalized at — creation, typing,
    // moving and scaling all pass through here — so putting the rule here is what keeps the
    // gestures from disagreeing about where a caption is allowed to be. The visible consequence
    // beyond dragging: typing at the picture's edge now grows the box OFF the picture instead of
    // sliding the caption inboard under the caret, which is the same trade the owner asked for
    // ("aligning text to an edge currently forces it to be clipped or blocked") applied to the
    // one other gesture that can grow a box.
    let placed = clamp_text_rect_on_canvas(AnnotRect { x: rect.x, y: rect.y, w, h }, fw, fh);
    AnnotKind::Text {
        rect: placed,
        text: text.to_string(),
        size_px,
        font,
        constrained,
        stroke_w,
    }
}

/// Resolve a caret MOVE (DRAGON-354 item 12) into `(new_text=None, caret, anchor)`:
/// * Shift held → EXTEND the selection: keep the anchor (seed it at the old caret if there was
///   none) and move the caret to `moved`.
/// * No shift, a selection exists:
///   * an ARROW (`travel = false`) COLLAPSES to the edge in the movement direction
///     (`to_start_side` = left/up → start, right/down → end); the caret doesn't travel further.
///   * HOME/END (`travel = true`) clear the selection AND travel to the computed line
///     boundary — collapsing to the selection edge instead is the classic Home-with-selection
///     bug (the caret would stop mid-line).
/// * No shift, no selection → just move to `moved`.
///
/// Pure — unit-tested.
fn caret_move(
    old_caret: usize,
    moved: usize,
    anchor: Option<usize>,
    sel: Option<(usize, usize)>,
    shift: bool,
    to_start_side: bool,
    travel: bool,
) -> (Option<String>, usize, Option<usize>) {
    if shift {
        (None, moved, Some(anchor.unwrap_or(old_caret)))
    } else if let Some((a, b)) = sel {
        if travel {
            (None, moved, None)
        } else {
            (None, if to_start_side { a } else { b }, None)
        }
    } else {
        (None, moved, None)
    }
}

/// Whether a press ENDS the live text-edit session (DRAGON-364) — Escape, as always, or
/// **numpad** Enter.
///
/// Why `location` and not the key alone: iced's LOGICAL key cannot tell the two Enters apart.
/// BOTH backends this app runs on map the keypad key to the same `Key::Named(Named::Enter)` as
/// the main one — the Wayland layer-shell path through `keysym_to_key(KP_Enter) -> Named::Enter`,
/// the winit path through winit's own logical mapping — and record the keypad ONLY in the
/// event's [`Location`] (`KP_Enter -> Location::Numpad`, set by `keysym_location` and by
/// winit's `KeyLocation::Numpad`). That field is plumbed from the event subscription for exactly
/// this predicate; `physical_key` would work equally well but is a per-layout code where
/// `Location` is already the normalized "which group of keys" answer both backends compute.
///
/// MAIN Enter is deliberately NOT an exit: the box is multi-line and Enter inserts a newline
/// (`text_edit_key`'s `Named::Enter` arm), so exiting on it would make a paragraph unwritable.
/// Modifiers are not consulted — the exit is the key itself, however it is qualified.
///
/// Pure — unit-tested.
pub fn text_edit_exits(
    key: &cosmic::iced::keyboard::Key,
    location: cosmic::iced::keyboard::Location,
) -> bool {
    use cosmic::iced::keyboard::{key::Named, Key, Location};
    match key {
        Key::Named(Named::Escape) => true,
        Key::Named(Named::Enter) => location == Location::Numpad,
        _ => false,
    }
}

/// What a PRIMARY-modifier press means to a LIVE text-annotation edit (DRAGON-354 item 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEditChord {
    /// Cmd/Ctrl+Z — step this session's text history back.
    Undo,
    /// Shift+Cmd/Ctrl+Z, or Cmd/Ctrl+Y — step it forward.
    Redo,
    /// Cmd/Ctrl+A — select the whole box.
    SelectAll,
    /// Cmd/Ctrl+C — copy the selected text.
    Copy,
    /// Cmd/Ctrl+X — cut the selected text.
    Cut,
    /// Cmd/Ctrl+V — paste at the caret.
    Paste,
}

/// Classify a press that arrived with the PRIMARY command modifier (Cmd on macOS, Ctrl
/// elsewhere) held while a text box is being edited. `None` = the chord is **swallowed**.
///
/// # Why this is a named, tested predicate and not an inline match (DRAGON-369)
///
/// A live text edit OWNS the keyboard: `keyboard.rs`'s `preview_modal_key` hands every press to
/// [`super::App::text_edit_key`] BEFORE the Preview keymap is consulted, so this list is the
/// complete inventory of primary chords that do anything at all while you are typing —
/// everything else is silently dropped and NEVER reaches an editor binding.
///
/// That is a SAFETY property, not an implementation detail, because the Preview context binds
/// two chords whose stray arrival mid-typing would be destructive: `Ctrl+D`
/// ([`crate::shortcuts::Action::PreviewDeselectAll`]) and `Ctrl+Shift+X`
/// ([`crate::shortcuts::Action::PreviewDelete`] — a hard `remove_file` on the capture). Neither
/// `d` nor a bare `x`-with-Shift-as-a-different-meaning is claimed here, so:
/// * `Ctrl+D` while typing does NOTHING (swallowed) — it cannot deselect out from under an
///   active edit;
/// * `Ctrl+Shift+X` while typing cuts the TEXT SELECTION (the [`TextEditChord::Cut`] arm — the
///   arms are modifier-insensitive apart from Shift+Z, so it behaves exactly like `Ctrl+X`) and
///   can never delete the capture file.
///
/// Pure — unit-tested, including both of those.
pub fn text_edit_chord(key: &cosmic::iced::keyboard::Key, shift: bool) -> Option<TextEditChord> {
    let cosmic::iced::keyboard::Key::Character(s) = key else {
        // A non-character primary chord (a bare modifier, an F-key, an arrow): swallowed.
        return None;
    };
    Some(match s.to_ascii_lowercase().as_str() {
        "z" if shift => TextEditChord::Redo,
        "z" => TextEditChord::Undo,
        "y" => TextEditChord::Redo,
        "a" => TextEditChord::SelectAll,
        "c" => TextEditChord::Copy,
        "x" => TextEditChord::Cut,
        "v" => TextEditChord::Paste,
        _ => return None,
    })
}

/// The most CHARS one paste may insert into a text annotation (32K). A caption is at most a
/// paragraph; an accidental multi-MB clipboard (a log file, a document) would otherwise drive
/// the per-edit reflow + resvg re-raster into a multi-second stall. The excess is silently
/// truncated at a grapheme boundary ([`super::text_annot::cap_graphemes`]) — no toast, by
/// review decision.
const TEXT_PASTE_MAX_CHARS: usize = 32 * 1024;

/// The wrap cap (SOURCE px) of an AUTO (click-placed) text box whose current width is `rect_w`,
/// set at `size_px`, on a picture `fw` wide: the PICTURE's own width, floored at the box's OWN
/// width and at one glyph, and ceilinged at [`super::text_annot::AUTO_WRAP_FALLBACK`] (resvg
/// coordinate sanity on absurd frames). Pure.
///
/// # Why the box's POSITION is not in it (DRAGON-378)
///
/// It used to be. The cap was `fw - rect.x - 4` — the room from the box's LEFT edge to the
/// picture's RIGHT edge — which made an auto box's wrap width an accident of where it happened
/// to be clicked (effectively unbounded on the left of the picture, a narrow column on the
/// right), and left it structurally blind to the room on the box's other side. Nothing could
/// widen it either: [`reflow_text`] keeps the box ORIGIN, so `rect.x` never moves on its own,
/// and dragging a WEST handle — which anchors the EAST edge and grows the box leftward — laid
/// the text out against the PRE-drag origin. Measured on a 1920px picture, a caption at x=1650
/// dragged 600px left: the type scaled 32 → 107px as asked, but the box grew 254 → 272px and the
/// caption fell apart into a four-line, one-word-per-line column. The identical gesture near the
/// LEFT edge grew it 254 → 854px on ONE line, because there the cap never bound at all.
///
/// The obvious repair — measure the room on the side the box is actually growing toward — cannot
/// work: the cap must be a pure function of the STORED box, and the stored origin of a scaled
/// box is itself derived from the layout the cap produced ([`anchor_scaled_text_rect`]). Any
/// position-dependent cap closes that circle, and it surfaces as a render deriving a different
/// wrap from the one the reflow stored — the exact divergence [`text_kind_layout`] exists to
/// prevent.
///
/// So the position term is gone — and DRAGON-368 had already made it obsolete. Once a caption
/// may hang off the picture ([`TEXT_MIN_ON_CANVAS_PX`]), the picture's right edge is no longer a
/// wall glyphs may not cross, so "the distance to that wall" was measuring something that no
/// longer exists. What survives is the cap's real job — a click-placed caption must not become
/// one unbounded line — and the picture's own width says exactly that without naming a position:
/// an auto box is POINT text, wrapping only when a single line would outrun the whole picture. A
/// column of a chosen width is what dragging a box out is for (`constrained`) — Photoshop's
/// split, the one DRAGON-364 already codified.
///
/// Two properties fall out, both load-bearing:
/// * a MOVE is now a pure translation ALWAYS. DRAGON-368's `rect_w` floor only stopped a
///   RIGHTWARD move from re-wrapping (the cap shrinking under the box); a LEFTWARD move grew the
///   cap and could silently un-wrap a caption mid-drag, off the raster-reuse fast path
///   ([`text_layer_xform`]). With no position term there is nothing left for a move to change.
/// * the cap is a FIXED POINT of its own layout: re-derived from the rect the reflow stored it
///   lands in `[widest line, the cap that produced that line]`, an interval over which greedy
///   wrap cannot move a single break — so the stored geometry, the live raster, the bake and the
///   caret math can never disagree about the wrap.
///
/// The `rect_w` floor stays exactly as DRAGON-368 left it, now guarding one case instead of
/// two: a box already WIDER than the picture (only a scale can make one) keeps its width, so
/// neither a move nor a re-render collapses it back into the picture.
///
/// # The floor is the layout's own output — read this before touching it (DRAGON-379)
///
/// `rect_w` is not an independent quantity for an auto box: its width IS the widest laid-out
/// line. So past the picture's width — the only regime where the floor binds — the cap is the
/// PREVIOUS layout fed back in as this one's input, and the wrap width and the line measured
/// against it are one number computed two ways (`Σ em_adv × s·k` versus `(Σ em_adv × s)·k`).
///
/// That is safe, but only conditionally, and the condition is not local to this function:
/// * it is a genuine FIXED POINT — a re-derivation lands in `[widest line, the cap that produced
///   it]`, where greedy wrap cannot move a break — so a caption re-flows at most once and then
///   never again, proved by iteration in
///   `the_auto_wrap_cap_settles_to_a_fixed_point_past_the_picture_width`;
/// * but the two ways of computing that one number differ by a rounding step, so the fit test
///   must not be exact. [`super::text_annot::WRAP_FIT_SLACK_REL`] is what makes it robust; before
///   it existed, one drag through this regime alternated between one line and two 23 times in 121
///   steps. `expanding_a_caption_past_the_media_never_alternates_between_wrapped_and_unwrapped`
///   is the net, and it is a SWEEP for that reason: every individual step was already a fixed
///   point, so iterating one of them could never have caught it.
///
/// Keeping the floor is a choice about the gesture, and it is the reason the loop is tolerated:
/// it is the only channel through which a scale can be a SIMILARITY once a caption grows past
/// the picture. Drop it and the cap becomes a true constant (`fw`) — the loop disappears — but
/// scaling a caption then re-wraps it at the picture's edge and shatters long words mid-word
/// instead of letting the caption grow off-canvas, which is precisely the "it keeps wrapping and
/// wont expand" DRAGON-378 was raised for. Measured both ways before choosing.
fn text_auto_cap(rect_w: f32, size_px: f32, fw: f32) -> f32 {
    fw.max(rect_w).max(size_px).min(super::text_annot::AUTO_WRAP_FALLBACK)
}

/// THE one layout derivation for a stored [`AnnotKind::Text`]: its display lines + box
/// metrics as a pure function of the stored fields and the picture width. EVERY consumer —
/// [`reflow_text`] (the stored geometry), [`render_text_layer`] (the live raster),
/// [`rasterize_scene`]'s Text arm (the bake), the caret geometry in `image.rs`, and the
/// caret-movement keys in [`App::text_edit_key`] — derives its layout HERE, so the wrap can
/// never diverge between the box geometry and what is actually drawn. (It used to: the render
/// paths passed the constant `AUTO_WRAP_FALLBACK` where the reflow used the frame-derived
/// cap, so a long caption near the right edge stored a wrapped box but rendered one clipped
/// line.) Pure — the parity is unit-tested.
pub fn text_kind_layout(
    text: &str,
    size_px: f32,
    font: super::text_annot::TextFont,
    rect: AnnotRect,
    constrained: bool,
    fw: f32,
) -> super::text_annot::TextLayout {
    use super::text_annot;
    let wrap_w = if constrained { Some(rect.w.max(text_annot::MIN_BOX_W)) } else { None };
    text_annot::layout(text, font, size_px, wrap_w, text_auto_cap(rect.w, size_px, fw))
}

/// The rect kinds that share Box GEOMETRY and interaction, as a small id — the axis
/// [`converted_rect_kind`] converts along. `None` for anything that isn't one of them.
///
/// The SEQUENCE BADGE is deliberately NOT a member even though it stores a rect: it is locked
/// 1:1, so converting a wide box into one (or a badge into a wide box) would silently reshape
/// the user's geometry. Picking the badge tool with a box selected just arms the tool.
fn rect_family_id(kind: &AnnotKind) -> Option<u8> {
    Some(match kind {
        AnnotKind::Box { .. } => 0,
        AnnotKind::Highlight { .. } => 1,
        AnnotKind::BoxHighlight { .. } => 2,
        AnnotKind::Pixelate { .. } => 3,
        AnnotKind::Blur { .. } => 4,
        AnnotKind::Spotlight { .. } => 5,
        // Text is NOT a rect-conversion member (DRAGON-354): converting a box into text (or vice
        // versa) would throw away the string / reshape the layout — arming the tool just arms it.
        AnnotKind::Arrow { .. }
        | AnnotKind::Pen { .. }
        | AnnotKind::Badge { .. }
        | AnnotKind::Text { .. } => return None,
    })
}

/// [`rect_family_id`]'s tool side.
fn rect_family_tool(tool: Tool) -> Option<u8> {
    Some(match tool {
        Tool::Rect => 0,
        Tool::Highlight => 1,
        Tool::BoxHighlight => 2,
        Tool::Pixelate => 3,
        Tool::Blur => 4,
        Tool::Spotlight => 5,
        Tool::Arrow
        | Tool::Pen
        | Tool::Eraser
        | Tool::Pointer
        | Tool::Hand
        | Tool::Badge
        | Tool::Text => return None,
    })
}

/// `rect` shrunk to the largest CENTRED 1:1 square inside it — how a pre-placed badge fits the
/// shared (200×100) placement rect. Pure — unit-tested.
pub fn centered_square(rect: AnnotRect) -> AnnotRect {
    let s = rect.w.abs().min(rect.h.abs());
    AnnotRect {
        x: rect.x + (rect.w - s) * 0.5,
        y: rect.y + (rect.h - s) * 0.5,
        w: s,
        h: s,
    }
}

/// How long after a tool button's first press a SECOND press on the SAME tool still counts as a
/// double-click (DRAGON-339). Matches the usual desktop double-click window.
pub const TOOL_DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// The action-tray double-click detector (DRAGON-339). libcosmic buttons report presses, not
/// click COUNTS, so the preview tracks the last tool press itself: pressing the SAME tool twice
/// within [`TOOL_DOUBLE_CLICK`] is a double-click (which spawns a pre-placed item), anything else
/// is a plain pick. A recognized double-click CONSUMES the record, so a third press starts a
/// fresh pair rather than firing again. Pure state machine — unit-tested.
#[derive(Default, Clone, Copy, Debug)]
pub struct ToolClicks {
    last: Option<(Tool, std::time::Instant)>,
}

impl ToolClicks {
    /// Record a press of `tool` at `now`, returning whether it completed a double-click.
    pub fn press(&mut self, tool: Tool, now: std::time::Instant) -> bool {
        let double = matches!(
            self.last,
            Some((prev, at)) if prev == tool && now.duration_since(at) <= TOOL_DOUBLE_CLICK
        );
        // A completed pair is consumed (no triple-fire); otherwise this press opens a new pair.
        self.last = if double { None } else { Some((tool, now)) };
        double
    }
}

/// The straight-alpha bytes of the user's current accent color. Resolved on the main
/// thread (reads the active theme), so the off-thread raster never touches the theme.
pub fn accent_color_bytes() -> AnnotColor {
    let c = crate::app::theme::accent(&cosmic::theme::active());
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(c.r), b(c.g), b(c.b), 255]
}

/// The DEFAULT annotation color: the COMPLEMENT of the live accent (its "companion" — hue
/// rotated 180°), so a fresh mark stands out against accent-colored UI. Computed from the
/// current accent at runtime, so it tracks theme changes.
pub fn default_annot_color() -> AnnotColor {
    complement(accent_color_bytes())
}

/// The accent's COMPANION colour as an iced [`Color`] — the accent's [`complement`] (hue
/// rotated 180°), read from `theme` so it tracks a live appearance override. The SAME
/// companion the default annotation colour uses ([`default_annot_color`]); the preview's
/// sharing actions (Save / Save As / Copy) tint with it while the document has unsaved
/// edits, so the "there is uncommitted work here" cue reads as the accent's partner rather
/// than an alarm colour.
pub fn companion(theme: &cosmic::Theme) -> cosmic::iced::Color {
    let a = crate::app::theme::accent(theme);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let c = complement([byte(a.r), byte(a.g), byte(a.b), 255]);
    cosmic::iced::Color::from_rgb(
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
    )
}

/// The COMPANION of an annotation color (DRAGON-386): its [`complement`] — hue rotated 180° in
/// HSL, saturation + lightness (and alpha) preserved. This IS the codebase's established
/// "companion" relationship: the very complement the default annotation color
/// ([`default_annot_color`]), the palette's lead pair ([`palette_entries`]) and the unsaved-edit
/// tint ([`companion`]) already speak. It is what the `X` swap hotkey toggles to, mirroring
/// Photoshop's foreground/background swap.
///
/// The mapping is TOTAL — every selectable color has a companion, and a GRAY (no hue to rotate)
/// is its own companion. In exact arithmetic it is an INVOLUTION (two rotations of 180° land back
/// on the original hue); across the u8 round-trip it is involutive up to a rounding unit, which is
/// why the swap TOGGLE ([`companion_swap`]) also carries a remembered partner for an exact return.
/// Pure — unit-tested.
pub fn companion_color(c: AnnotColor) -> AnnotColor {
    complement(c)
}

/// Resolve ONE press of the companion-swap hotkey (`X`; DRAGON-386): given the CURRENT active
/// annotation color and the REMEMBERED swap partner (`EditState::color_swap_back`), return the
/// `(new active color, new remembered partner)`.
///
/// The rule makes the toggle CLEAN despite [`companion_color`] being involutive only up to u8
/// rounding: when the current color is exactly the companion of the remembered partner we return
/// that partner VERBATIM (an exact "swap back"); otherwise it is a fresh swap to the companion.
/// The new remembered partner is ALWAYS the color we just left, so a second `X` returns to the
/// exact starting color and a third swaps forward again. A gray swaps to itself (its companion),
/// a harmless no-op. Pure — unit-tested.
pub fn companion_swap(
    current: AnnotColor,
    remembered: Option<AnnotColor>,
) -> (AnnotColor, AnnotColor) {
    let target = match remembered {
        Some(back) if companion_color(back)[..3] == current[..3] => back,
        _ => companion_color(current),
    };
    (target, current)
}

/// The complementary color of `rgb`: hue rotated 180° in HSL, same saturation + lightness,
/// alpha preserved. A gray (saturation 0) has no hue to rotate and is returned unchanged.
/// Pure — unit-tested.
pub fn complement(rgb: AnnotColor) -> AnnotColor {
    let (r, g, b) = (rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d.abs() < 1e-6 {
        return rgb; // gray: no hue
    }
    let l = (max + min) / 2.0;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let mut h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h = ((h / 6.0) + 0.5).fract(); // 0..1, rotated 180°
    let (nr, ng, nb) = hsl_to_rgb(h, s, l);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(nr), q(ng), q(nb), rgb[3]]
}

/// HSL (`h`,`s`,`l` all 0..1) → RGB (0..1). Standard reference conversion.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s.abs() < 1e-6 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let chan = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (chan(h + 1.0 / 3.0), chan(h), chan(h - 1.0 / 3.0))
}

// ── the sequence badge: numbering + the 1:1 constraint (DRAGON-340; all pure) ──────────

/// Every badge in the scene paired with its ORDINAL, in scene order: the first badge is `1`,
/// the second `2`, and so on. Non-badge items are skipped and never consume a number.
///
/// The numbering is DERIVED, never stored. That is the whole design: the set is a contiguous
/// `1..N` by construction, so deleting badge 2 makes the old 3 become 2 for free, and undo /
/// redo restore correct numbers automatically because they restore the item VECTOR (an ordinal
/// stored on an item could not survive either operation without bookkeeping that can go stale).
///
/// "Scene order" is the annotation vector's order, which is also the z-order: a badge is
/// appended on creation, so numbering follows the order badges were PLACED. The corollary is
/// that explicitly restacking a badge (Bring to Front / Send to Back) also renumbers it — the
/// honest consequence of the order being the single source of truth. Pure — unit-tested.
pub fn badge_numbers(items: &[AnnotationItem]) -> Vec<(AnnotId, u32)> {
    items
        .iter()
        .filter(|it| it.kind.is_badge())
        .enumerate()
        .map(|(i, it)| (it.id, i as u32 + 1))
        .collect()
}

/// `r` forced to a 1:1 SQUARE anchored at the corner the drag is NOT moving — the base of the
/// badge's always-square rule.
///
/// `anchor` names the fixed corner as `(right, bottom)` flags: `(false, false)` pins the
/// top-left (so the square grows toward +x/+y), `(true, true)` pins the bottom-right, and so
/// on. The side is the LARGER of the two extents, so the square follows whichever axis the
/// pointer dragged further — the behaviour a 1:1 drag reads as. Pure — unit-tested.
pub fn square_rect(r: AnnotRect, anchor: (bool, bool)) -> AnnotRect {
    let s = r.w.abs().max(r.h.abs());
    let x = if anchor.0 { r.x + r.w - s } else { r.x };
    let y = if anchor.1 { r.y + r.h - s } else { r.y };
    AnnotRect { x, y, w: s, h: s }
}

/// A SQUARE-PRESERVING clamp into the image `[0,fw]×[0,fh]` inset by the drawn margin `m`:
/// unlike [`clamp_rect`], which shrinks the axes independently (and would turn a badge into a
/// rectangle against an edge), the side shrinks to the TIGHTER axis so the result is still 1:1.
/// Pure — unit-tested.
pub fn clamp_square(r: AnnotRect, fw: f32, fh: f32, m: f32) -> AnnotRect {
    let s = r.w.abs().max(r.h.abs()).min((fw - 2.0 * m).max(0.0)).min((fh - 2.0 * m).max(0.0));
    AnnotRect {
        x: r.x.clamp(m, (fw - m - s).max(m)),
        y: r.y.clamp(m, (fh - m - s).max(m)),
        w: s,
        h: s,
    }
}

/// The square a badge takes after grab `grab` produced the (possibly non-square) rect `r`,
/// clamped inside the image — the ONE place the "always 1:1, during AND after any resize"
/// rule is enforced for an existing badge.
///
/// Which corner stays put follows the grab, so the badge grows/shrinks from the handle the
/// user is NOT holding, exactly like a box: a NW drag pins SE, an E drag pins the west edge,
/// and so on. Edge grabs still produce a square (that is the point) but keep the badge centred
/// on the OTHER axis, so dragging the top edge doesn't also slide the badge sideways. A Move
/// changes no size at all. Pure — unit-tested.
pub fn square_for_grab(r: AnnotRect, grab: Grab, fw: f32, fh: f32, m: f32) -> AnnotRect {
    use crate::geometry::{Corner, Edge};
    let squared = match grab {
        // A move never resizes: the rect is already square, just clamp it.
        Grab::Move | Grab::ArrowA | Grab::ArrowB => r,
        Grab::Corner(Corner::Nw) => square_rect(r, (true, true)),
        Grab::Corner(Corner::Ne) => square_rect(r, (false, true)),
        Grab::Corner(Corner::Sw) => square_rect(r, (true, false)),
        Grab::Corner(Corner::Se) => square_rect(r, (false, false)),
        // An edge drag sizes on ITS axis and recentres on the other, so the badge doesn't
        // wander sideways while you drag its top edge.
        Grab::Edge(e) => {
            let (cx, cy) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
            match e {
                Edge::N => {
                    let s = r.h.abs();
                    AnnotRect { x: cx - s * 0.5, y: r.y + r.h - s, w: s, h: s }
                }
                Edge::S => {
                    let s = r.h.abs();
                    AnnotRect { x: cx - s * 0.5, y: r.y, w: s, h: s }
                }
                Edge::W => {
                    let s = r.w.abs();
                    AnnotRect { x: r.x + r.w - s, y: cy - s * 0.5, w: s, h: s }
                }
                Edge::E => {
                    let s = r.w.abs();
                    AnnotRect { x: r.x, y: cy - s * 0.5, w: s, h: s }
                }
            }
        }
    };
    clamp_square(squared, fw, fh, m)
}

// ── text box resize: the two modes (DRAGON-364) ──────────────────────────────────────

/// The minimum of a text box's own `rect` (SOURCE px) that must remain ON the picture
/// (DRAGON-368) — the whole of what stops a caption being dragged out of existence.
///
/// WHY the box may leave the canvas at all: the owner asked for it, because a caption's ink
/// simply cannot be aligned FLUSH to an edge while the box is clamped wholly inside — the layout
/// box carries side bearings and line leading the ink does not fill, so "as far left as it goes"
/// still leaves a visible gap. Letting the box overhang lets the INK meet the edge.
///
/// WHY it is measured on `rect` and NOT on the padded region ([`text_padded_bounds`]): the
/// padding is slack for ink that might escape the box, up to `size_px * TEXT_INK_OVERHANG_EM`
/// plus the outline — 134 px per side at 512px type. Clamping on that is precisely what made
/// edge alignment impossible. `rect` is also the rect the canvas hit-tests, so "5 px of box left
/// on the picture" is the same 5 px you can still grab to drag it back: recoverability falls out
/// of the number instead of needing a second rule of its own.
///
/// SOURCE px, so it does not change meaning with zoom — at fit zoom on a 5K capture 5 source px
/// is a bit over 2 device px, which is small but reliably grabbable, and it is a floor on how far
/// out you can push rather than a target.
pub const TEXT_MIN_ON_CANVAS_PX: f32 = 5.0;

/// A text box's rect held to [`TEXT_MIN_ON_CANVAS_PX`] of itself on the picture (DRAGON-368) —
/// the MOVE clamp for text, in place of the "wholly inside the frame" [`clamp_rect`] every other
/// kind takes.
///
/// Per axis the rule is simply that the overlap of the box with the picture stays at least
/// `min(TEXT_MIN_ON_CANVAS_PX, extent)` — the `min` so a box narrower than the threshold (a
/// single hairline glyph) is held by its whole width rather than being unclampable. A degenerate
/// frame leaves the rect alone rather than inventing a position. Pure — unit-tested.
fn clamp_text_rect_on_canvas(r: AnnotRect, fw: f32, fh: f32) -> AnnotRect {
    let axis = |v: f32, extent: f32, full: f32| {
        // A degenerate (or NaN) frame leaves the rect alone rather than inventing a position;
        // spelled as a positive comparison so a NaN `full` falls through here too.
        if !(full.is_finite() && full > 0.0) || !v.is_finite() {
            return v;
        }
        let keep = TEXT_MIN_ON_CANVAS_PX.min(extent.max(0.0));
        v.clamp(keep - extent.max(0.0), full - keep)
    };
    AnnotRect { x: axis(r.x, r.w, fw), y: axis(r.y, r.h, fh), w: r.w, h: r.h }
}

/// The SIZE (SOURCE px) a handle drag SCALES an auto text box to: the drag's own factor, held
/// inside the guard bounds [`super::text_annot::TEXT_SCALE_MIN_PX`]..=
/// [`super::text_annot::TEXT_SCALE_MAX_PX`] and otherwise untouched.
///
/// DRAGON-367 made the size reach past the dropdown's span (a drag goes 64 → 96 → 128 → 192 and
/// on up, or below 12) and land on discrete LADDER rungs so a resize read as deliberate steps.
/// DRAGON-368 removed the stepping on the owner's instruction ("we need to remove snapping from
/// the sizer") — the type now tracks the pointer continuously, exactly as the box does. Only the
/// ladder's two ENDS survive, as guards: "no limit" cannot mean "no guard", since `size_px` feeds
/// the text layer's padded region and a runaway (or NaN) size has no rasterizable region at all.
///
/// The BOX scales with the type, and that is not a compromise: a normal (click-created) box has
/// no independent geometry — `reflow_text` derives its extent from the text at `size_px`, which
/// is the whole reason DRAGON-364 scales the type here instead of stretching the frame.
///
/// The chip reports the real number, and the dropdown highlights nothing while the size is
/// off-preset ([`super::text_annot::text_size_preset_index`]) — which is now the usual case, not
/// the exception — so an off-preset size is never misreported as a preset and is never silently
/// snapped back into the listed range.
fn clamp_scaled_text_size(px: f32) -> f32 {
    super::text_annot::text_scale_clamp(px)
}

/// The UNIFORM scale factor a resize grab implies for an AUTO ("normal") text box — the
/// DRAGON-364 rule that a click-created box grows its GLYPHS rather than its wrap frame.
///
/// A normal box has no independent geometry: `reflow_text` derives its `w`/`h` from the text at
/// `size_px`, so there is nothing to stretch — the only thing a handle CAN change is the type
/// size, and scaling that uniformly is what makes the resize aspect-ratio-locked by
/// construction (both axes come from one number, so the box can never be squashed).
///
/// The factor is the drag PROJECTED onto the direction the handle actually pulls, normalized by
/// the box's own extent:
///   * a CORNER projects onto that corner's outward diagonal, so both axes contribute and a
///     purely horizontal or purely vertical drag still resizes (a per-axis `max`/`min` would
///     dead-zone one of them, and the geometric mean would halve the response);
///   * an EDGE uses its own axis alone — dragging the bottom edge down grows, up shrinks.
///
/// Both are continuous through 1.0 and monotone in the drag, so the text tracks the pointer
/// without a jump when the drag direction crosses an axis.
///
/// `w`/`h` are the box's PRE-drag extent; a degenerate axis falls back to the other so a
/// one-line box with a hairline height can still be scaled. Clamped to a small positive floor —
/// the caller's [`clamp_scaled_text_size`] owns the real range; this only guarantees the factor
/// never goes zero or negative (which would invert the type). Pure — unit-tested.
pub fn text_scale_factor(w: f32, h: f32, grab: Grab, dx: f32, dy: f32) -> f32 {
    use crate::geometry::{Corner, Edge};
    // Guard degenerate extents: fall back to the other axis, then to 1px, so no division by ~0.
    let w = if w.abs() > 0.5 { w.abs() } else if h.abs() > 0.5 { h.abs() } else { 1.0 };
    let h = if h.abs() > 0.5 { h.abs() } else { w };
    // The outward direction of the dragged handle, in box units.
    let (sx, sy) = match grab {
        Grab::Corner(Corner::Nw) => (-1.0, -1.0),
        Grab::Corner(Corner::Ne) => (1.0, -1.0),
        Grab::Corner(Corner::Sw) => (-1.0, 1.0),
        Grab::Corner(Corner::Se) => (1.0, 1.0),
        Grab::Edge(Edge::N) => (0.0, -1.0),
        Grab::Edge(Edge::S) => (0.0, 1.0),
        Grab::Edge(Edge::W) => (-1.0, 0.0),
        Grab::Edge(Edge::E) => (1.0, 0.0),
        // A move never resizes; arrow grabs never apply to a box.
        Grab::Move | Grab::ArrowA | Grab::ArrowB => return 1.0,
    };
    // Project the drag onto that direction and divide by the extent ALONG it: for a corner that
    // is the squared diagonal (w² + h²), for an edge simply its own axis.
    let (ox, oy) = (sx * w, sy * h);
    let denom = ox * ox + oy * oy;
    if denom <= f32::EPSILON {
        return 1.0;
    }
    (1.0 + (dx * ox + dy * oy) / denom).max(0.05)
}

/// Re-place a rescaled AUTO text box so the grab's ANCHOR — the corner/edge OPPOSITE the handle
/// being dragged — stays put (DRAGON-364), then clamp the result inside `frame`.
///
/// Scaling changes the box's derived `w`/`h`, so without this the box would grow from its
/// top-left and the handle you are holding would slide out from under the pointer. Mirrors
/// [`edit_rect`]'s anchoring exactly: an NW drag pins SE, an E drag pins the west edge, and an
/// edge drag leaves the other axis's origin alone (unlike the badge's recentring — a caption
/// that slid sideways while you dragged its bottom edge would read as a move, not a resize).
/// Pure — unit-tested.
pub fn anchor_scaled_text_rect(
    orig: AnnotRect,
    new_w: f32,
    new_h: f32,
    grab: Grab,
    frame: (f32, f32),
) -> AnnotRect {
    use crate::geometry::{Corner, Edge};
    let (l, t, r, b) = orig.corners();
    // `(x, y)` of the new box, chosen so the anchor edge/corner is preserved.
    let (x, y) = match grab {
        Grab::Corner(Corner::Nw) => (r - new_w, b - new_h),
        Grab::Corner(Corner::Ne) => (l, b - new_h),
        Grab::Corner(Corner::Sw) => (r - new_w, t),
        Grab::Corner(Corner::Se) => (l, t),
        Grab::Edge(Edge::N) => (l, b - new_h),
        Grab::Edge(Edge::S) => (l, t),
        Grab::Edge(Edge::W) => (r - new_w, t),
        Grab::Edge(Edge::E) => (l, t),
        Grab::Move | Grab::ArrowA | Grab::ArrowB => (orig.x, orig.y),
    };
    let (fw, fh) = frame;
    // DRAGON-368: held to [`TEXT_MIN_ON_CANVAS_PX`] on the picture rather than wholly inside it,
    // the same rule a text MOVE takes. Applied here too so scaling a caption you have deliberately
    // aligned to an edge grows it past that edge instead of shoving it back inboard — the two
    // gestures would otherwise disagree about where a text box is allowed to be.
    clamp_text_rect_on_canvas(AnnotRect { x, y, w: new_w, h: new_h }, fw, fh)
}

// ── text style: display vs. remembered default (DRAGON-364) ──────────────────────────

/// WHERE a text size/font change came from — the axis that decides whether it is a user
/// PREFERENCE (persisted as the default new boxes are born with) or merely a REPORT about the
/// element under the cursor (the dropdown chips following the selection).
///
/// This exists as a named type rather than a bare `bool` so every call site has to say which
/// kind of change it is making, and the rule lives in exactly one place
/// ([`Self::writes_default`]) instead of being restated — the two must never quietly collapse
/// into "any style change persists", which would let merely clicking an old 96px caption re-set
/// the default for every future capture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextStyleSource {
    /// The user picked a value in the font/size dropdown menu — the only genuine statement of
    /// preference, and the only source that writes the persisted default.
    DropdownPick,
    /// The dropdowns are REPORTING the selected / newly-edited element's own style
    /// (DRAGON-364 task 3). A report, not a choice.
    SelectionSync,
    /// A handle drag scaled a NORMAL box, changing its font size (DRAGON-364 task 4). The user
    /// dragged geometry; they did not pick a size, so the default must not move — confirmed by
    /// the repo owner. The chip still follows the drag, so the number stays honest.
    HandleScale,
}

impl TextStyleSource {
    /// Whether a style change from this source updates the PERSISTED default for future text
    /// boxes. Only an explicit dropdown pick does. Pure — unit-tested in both directions.
    pub fn writes_default(self) -> bool {
        match self {
            TextStyleSource::DropdownPick => true,
            TextStyleSource::SelectionSync | TextStyleSource::HandleScale => false,
        }
    }
}

/// The `(size_px, font)` the dropdowns should DISPLAY for `target` within `annotations`
/// (DRAGON-364 task 3) — `None` when there is no target, it has vanished, or it is not a text
/// item, in which case the chips keep showing what a new box would take.
///
/// The caller resolves `target` as "the box being edited, else the PRIMARY selection", and the
/// primary is the LAST-selected item — which is precisely the ticket's "in the case of multiple
/// you can match the last one selected". Pure — unit-tested.
pub fn text_style_for_display(
    annotations: &[AnnotationItem],
    target: Option<AnnotId>,
) -> Option<(f32, super::text_annot::TextFont)> {
    let tid = target?;
    annotations.iter().find(|it| it.id == tid).and_then(|it| match &it.kind {
        AnnotKind::Text { size_px, font, .. } => Some((*size_px, *font)),
        _ => None,
    })
}

// ── color palette + custom color-wheel picker ────────────────────────────────────────

/// One entry in the ordered color-flyout list: a selectable color swatch, or the "+" that
/// opens the custom color-wheel picker. The list drives BOTH rendering and keyboard
/// navigation, so they index the same sequence.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PaletteEntry {
    Color(AnnotColor),
    Custom,
}

impl PaletteEntry {
    /// Whether this is a color entry matching `c` (RGB only).
    pub fn matches_color(&self, c: AnnotColor) -> bool {
        matches!(self, PaletteEntry::Color(x) if x[..3] == c[..3])
    }
}

/// Number of leading fixed entries (the complement + the accent) before the palette.
pub const PALETTE_LEAD: usize = 2;
/// Number of appearance-tab palette colors.
pub const PALETTE_COLOR_COUNT: usize = 9;

/// The nine appearance-tab accent colors as RGBA bytes.
pub fn appearance_palette() -> [AnnotColor; PALETTE_COLOR_COUNT] {
    let active = cosmic::theme::active();
    let pal = &active.cosmic().palette;
    let to = |c: &cosmic::cosmic_theme::palette::Srgba| {
        [
            (c.red * 255.0).round() as u8,
            (c.green * 255.0).round() as u8,
            (c.blue * 255.0).round() as u8,
            255u8,
        ]
    };
    [
        to(&pal.accent_blue),
        to(&pal.accent_indigo),
        to(&pal.accent_purple),
        to(&pal.accent_pink),
        to(&pal.accent_red),
        to(&pal.accent_orange),
        to(&pal.accent_yellow),
        to(&pal.accent_green),
        to(&pal.accent_warm_grey),
    ]
}

/// How many CUSTOM colors the recents queue holds before the oldest is replaced.
pub const RECENT_COLOR_CAP: usize = 5;

/// Rotate a freshly picked CUSTOM color into the recents strip (DRAGON-348): NEWEST-FIRST —
/// the new color lands at the FRONT of the strip, and once the strip is at cap the OLDEST
/// (the last entry) is always the one replaced. Re-picking a color already in the strip
/// (RGB match) moves it to the front instead of duplicating. Pure — unit-tested.
pub fn rotate_recent_color(recents: &mut Vec<AnnotColor>, c: AnnotColor) {
    recents.retain(|x| x[..3] != c[..3]);
    recents.insert(0, c);
    recents.truncate(RECENT_COLOR_CAP);
}

/// The full ordered color-flyout entry list: complement, accent, the nine palette colors,
/// the last-`recents` custom colors, then the "+" opener. Shared by the view and the
/// keyboard nav so they agree on index → entry.
pub fn palette_entries(recents: &[AnnotColor]) -> Vec<PaletteEntry> {
    let mut v = Vec::with_capacity(PALETTE_LEAD + PALETTE_COLOR_COUNT + recents.len() + 1);
    // Accent first, then its companion (the complement) — accent leads the flyout.
    v.push(PaletteEntry::Color(accent_color_bytes()));
    v.push(PaletteEntry::Color(default_annot_color()));
    for c in appearance_palette() {
        v.push(PaletteEntry::Color(c));
    }
    for &c in recents {
        v.push(PaletteEntry::Color(c));
    }
    v.push(PaletteEntry::Custom);
    v
}

// ── freehand pen geometry (DRAGON-338; all pure — unit-tested) ────────────────────────

/// The smallest SOURCE-px step between two recorded pen points. A pointer move closer than
/// this to the last point is dropped, so a slow drag doesn't pile up thousands of coincident
/// vertices (the stroke is a VECTOR — its cost is its point count, not its length).
pub const PEN_MIN_STEP: f32 = 1.5;

/// The SOURCE-px travel below which a committed pen gesture is a TAP, not a stroke
/// (DRAGON-342): it normalizes to its single anchor point and inks as a round DOT of
/// [`crate::pen_stroke::dot_width`]. Same 3px bar the old degeneracy rule used to DISCARD such
/// a gesture at — now it becomes a mark instead of nothing, because with the pencil armed a
/// press is always deliberate ink (selection lives on the pointer tool).
pub const PEN_DOT_MAX: f32 = 3.0;

/// Extra SOURCE-px slack added to the "do these two strokes touch?" test on TOP of their two
/// stroke half-widths — the ink of two strokes drawn to meet can leave a hairline gap from
/// pointer sampling, and a hair's gap should still read as connected.
pub const PEN_JOIN_SLACK: f32 = 2.0;

/// The eraser's SOURCE-px reach beyond a stroke's own half-width: how close the eraser path
/// must pass to a pen stroke to mark it. Deliberately generous — erasing is a sweep, not
/// surgery — but small enough that you can clear one scribble without catching its neighbour.
pub const ERASER_SLACK: f32 = 6.0;

/// Distance from point `p` to the segment `a`–`b`. Pure.
fn point_seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The smallest distance between the segments `a1`–`a2` and `b1`–`b2` — `0` when they cross.
/// Non-crossing segments are nearest at an ENDPOINT of one of them, so the four point-to-
/// segment distances cover every case. Pure.
fn seg_seg_dist(a1: (f32, f32), a2: (f32, f32), b1: (f32, f32), b2: (f32, f32)) -> f32 {
    // Proper-intersection test by orientation signs (the crossing case, distance 0).
    let cross = |o: (f32, f32), p: (f32, f32), q: (f32, f32)| {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    };
    let (d1, d2) = (cross(b1, b2, a1), cross(b1, b2, a2));
    let (d3, d4) = (cross(a1, a2, b1), cross(a1, a2, b2));
    if ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) {
        return 0.0;
    }
    point_seg_dist(a1, b1, b2)
        .min(point_seg_dist(a2, b1, b2))
        .min(point_seg_dist(b1, a1, a2))
        .min(point_seg_dist(b2, a1, a2))
}

/// The smallest distance between any segment of `paths` and the segment `a`–`b`. A single-point
/// stroke measures as a point. `f32::INFINITY` for an empty group. Pure.
fn pen_dist_to_segment(paths: &[Vec<AnnotPoint>], a: (f32, f32), b: (f32, f32)) -> f32 {
    let mut best = f32::INFINITY;
    for path in paths {
        match path.len() {
            0 => {}
            1 => best = best.min(point_seg_dist((path[0].x, path[0].y), a, b)),
            _ => {
                for w in path.windows(2) {
                    let d = seg_seg_dist((w[0].x, w[0].y), (w[1].x, w[1].y), a, b);
                    best = best.min(d);
                    if best <= 0.0 {
                        return 0.0;
                    }
                }
            }
        }
    }
    best
}

/// Whether the two pen groups TOUCH: any segment of one passes within `tol` of any segment of
/// the other (crossing counts as distance 0). `tol` is the sum of their stroke half-widths plus
/// [`PEN_JOIN_SLACK`] — i.e. their drawn INK overlaps or all but touches. Pure — the whole
/// definition of "connected" the merge is built on.
pub fn pen_groups_touch(a: &[Vec<AnnotPoint>], b: &[Vec<AnnotPoint>], tol: f32) -> bool {
    for path in a {
        match path.len() {
            0 => {}
            1 => {
                if pen_dist_to_segment(b, (path[0].x, path[0].y), (path[0].x, path[0].y)) <= tol {
                    return true;
                }
            }
            _ => {
                for w in path.windows(2) {
                    if pen_dist_to_segment(b, (w[0].x, w[0].y), (w[1].x, w[1].y)) <= tol {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether an ERASER sweep from `a` to `b` (SOURCE px) touches this pen group: any of its
/// segments within `max_width / 2 + ERASER_SLACK` of the sweep. The reach rides
/// [`crate::pen_stroke::max_width`], NOT the preset width — a heavy (pressure-swelled) stretch
/// draws wider than its nominal weight, and the eraser must reach every pixel that was inked.
/// A zero-length sweep (a plain click) is a point test, so clicking a stroke marks it. Pure —
/// the eraser's whole hit rule.
pub fn pen_hit_by_eraser(
    paths: &[Vec<AnnotPoint>],
    stroke_w: f32,
    a: (f32, f32),
    b: (f32, f32),
) -> bool {
    pen_dist_to_segment(paths, a, b) <= crate::pen_stroke::max_width(stroke_w) * 0.5 + ERASER_SLACK
}

/// A pen group's points as plain `(x, y)` tuples — the shape [`crate::pen_stroke`] and the
/// canvas widget speak. One allocation per stroke; the pen paths are short vectors.
pub fn pen_xy(path: &[AnnotPoint]) -> Vec<(f32, f32)> {
    path.iter().map(|p| (p.x, p.y)).collect()
}

/// The stored per-point speed signal for stroke `i` of a pen group, or an EMPTY slice when the
/// group carries none (or a stale/mismatched one) — which every consumer reads as neutral
/// pressure. The single guard against the parallel arrays ever being mis-indexed.
pub fn pen_pressure<'a>(
    pressure: &'a [Vec<f32>],
    paths: &[Vec<AnnotPoint>],
    i: usize,
) -> &'a [f32] {
    match (pressure.get(i), paths.get(i)) {
        (Some(p), Some(path)) if p.len() == path.len() => p,
        _ => &[],
    }
}

/// The SOURCE-px bounding box of a pen group (the rect its selection chrome + handles sit on).
/// An empty group is a zero rect at the origin. Pure.
pub fn pen_bounds(paths: &[Vec<AnnotPoint>]) -> AnnotRect {
    let (mut lo_x, mut lo_y) = (f32::INFINITY, f32::INFINITY);
    let (mut hi_x, mut hi_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in paths.iter().flatten() {
        lo_x = lo_x.min(p.x);
        lo_y = lo_y.min(p.y);
        hi_x = hi_x.max(p.x);
        hi_y = hi_y.max(p.y);
    }
    if !lo_x.is_finite() || !lo_y.is_finite() {
        return AnnotRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    }
    AnnotRect { x: lo_x, y: lo_y, w: hi_x - lo_x, h: hi_y - lo_y }
}

/// The total drawn LENGTH of a pen group (SOURCE px) — the "is this a real stroke or a stray
/// click?" measure. Pure.
pub fn pen_length(paths: &[Vec<AnnotPoint>]) -> f32 {
    paths
        .iter()
        .flat_map(|p| p.windows(2))
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum()
}

/// Map a pen group from the bounding box `from` into the box `to` — the affine a resize
/// applies (a Move is the degenerate same-size case, a pure translation). A zero-extent axis
/// can't scale (a perfectly straight line has no height), so it TRANSLATES on that axis
/// instead of dividing by zero. Pure — unit-tested.
pub fn scale_pen(paths: &[Vec<AnnotPoint>], from: AnnotRect, to: AnnotRect) -> Vec<Vec<AnnotPoint>> {
    let sx = if from.w.abs() > 1e-4 { to.w / from.w } else { 1.0 };
    let sy = if from.h.abs() > 1e-4 { to.h / from.h } else { 1.0 };
    paths
        .iter()
        .map(|path| {
            path.iter()
                .map(|p| AnnotPoint {
                    x: to.x + (p.x - from.x) * sx,
                    y: to.y + (p.y - from.y) * sy,
                })
                .collect()
        })
        .collect()
}

/// Collapse a freehand gesture that never really travelled (under [`PEN_DOT_MAX`] of ink) into
/// the single-point DOT it is, anchored on its own first point — or on `fallback` (the raw
/// trail's first sample) if it somehow has none. Returns whether the gesture WAS a tap (an
/// already-single point counts, and normalizes to itself); `false`, changing nothing, for a
/// real stroke and for every non-pen kind.
///
/// This is the whole "a pencil TAP inks a dot" rule (DRAGON-342). A one-point stroke renders as
/// a firm round press ([`crate::pen_stroke::dot_width`]) and behaves like any other pen item —
/// it merges with strokes that touch it, erases, resizes, undoes as one entry, and bakes at
/// full resolution. The boundary is deliberately the same 3px the OLD degeneracy rule used to
/// DISCARD such a gesture at: below it there is no stroke worth keeping, so it becomes the dot
/// the user meant rather than nothing at all. Pure — unit-tested.
pub fn normalize_pen_tap(kind: &mut AnnotKind, fallback: Option<AnnotPoint>) -> bool {
    let AnnotKind::Pen { paths, pressure, .. } = kind else {
        return false;
    };
    if pen_length(paths) >= PEN_DOT_MAX {
        return false;
    }
    let Some(anchor) = paths.iter().flatten().next().copied().or(fallback) else {
        return false;
    };
    // A dot carries no speed signal: its width is the firm-press dot width, by definition.
    *paths = vec![vec![anchor]];
    *pressure = vec![Vec::new()];
    true
}

/// Fold every OTHER pen group that CONNECTS to `id` into it, transitively (absorbing one group
/// grows the geometry, which may then reach a third), and drop the absorbed items. Returns
/// whether anything merged.
///
/// Two groups connect when their ink touches ([`pen_groups_touch`]) AND they LOOK the same —
/// identical color and stroke width. The appearance guard is deliberate: a group carries ONE
/// color + width, so merging across a color change would silently repaint the user's earlier
/// strokes. Same-looking strokes that touch are indistinguishable once drawn, so folding them
/// into one selectable item changes nothing on screen — exactly the ticket's "lines that
/// connect together become one selectable item", with disconnected (or differently-styled)
/// strokes staying their own items. Pure over the scene vector — unit-tested.
pub fn merge_connected_pens(items: &mut Vec<AnnotationItem>, id: AnnotId) -> bool {
    let Some(idx) = items.iter().position(|it| it.id == id) else {
        return false;
    };
    let AnnotKind::Pen { paths, pressure, stroke_w } = &items[idx].kind else {
        return false;
    };
    let (mut group, mut press, mut width) = (paths.clone(), pressure.clone(), *stroke_w);
    let color = items[idx].color;
    let mut merged = false;
    loop {
        // The first OTHER same-looking pen group whose ink touches the (growing) group. The
        // "touching" tolerance rides `max_width` (the widest a pressure-swelled stretch draws),
        // so two strokes whose visible ink meets still merge.
        let hit = items.iter().position(|it| {
            if it.id == id || it.color != color {
                return false;
            }
            match &it.kind {
                AnnotKind::Pen { paths: other, stroke_w: w, .. } if (*w - width).abs() < 1e-3 => {
                    pen_groups_touch(&group, other, crate::pen_stroke::max_width(width) + PEN_JOIN_SLACK)
                }
                _ => false,
            }
        });
        let Some(hit) = hit else { break };
        if let AnnotKind::Pen { paths: other, pressure: other_p, stroke_w: w } = &items[hit].kind {
            // Both arrays grow together (padding a group that carried no signal with empty
            // entries), so `pressure[i]` never stops belonging to `paths[i]`.
            press.resize(group.len(), Vec::new());
            group.extend(other.iter().cloned());
            for i in 0..other.len() {
                press.push(other_p.get(i).cloned().unwrap_or_default());
            }
            width = *w;
        }
        items.remove(hit);
        merged = true;
    }
    if merged {
        // The absorbing item may have shifted left as earlier items were removed.
        if let Some(i) = items.iter().position(|it| it.id == id) {
            items[i].kind = AnnotKind::Pen { paths: group, pressure: press, stroke_w: width };
        }
    }
    merged
}

// ── z-order operations (pure; the scene's z-order IS the vector order) ────────────────

/// Raise `id` one step toward the top (end of the vector). Returns whether it moved.
pub fn raise(items: &mut [AnnotationItem], id: AnnotId) -> bool {
    if let Some(i) = items.iter().position(|it| it.id == id)
        && i + 1 < items.len()
    {
        items.swap(i, i + 1);
        return true;
    }
    false
}

/// Lower `id` one step toward the bottom (start of the vector). Returns whether it moved.
pub fn lower(items: &mut [AnnotationItem], id: AnnotId) -> bool {
    if let Some(i) = items.iter().position(|it| it.id == id)
        && i > 0
    {
        items.swap(i, i - 1);
        return true;
    }
    false
}

/// Move `id` to the top (end). Returns whether it moved.
pub fn to_front(items: &mut Vec<AnnotationItem>, id: AnnotId) -> bool {
    if let Some(i) = items.iter().position(|it| it.id == id) {
        if i + 1 == items.len() {
            return false;
        }
        let it = items.remove(i);
        items.push(it);
        return true;
    }
    false
}

/// Move `id` to the bottom (start). Returns whether it moved.
pub fn to_back(items: &mut Vec<AnnotationItem>, id: AnnotId) -> bool {
    if let Some(i) = items.iter().position(|it| it.id == id) {
        if i == 0 {
            return false;
        }
        let it = items.remove(i);
        items.insert(0, it);
        return true;
    }
    false
}

// ── rasterization + bake ──────────────────────────────────────────────────────────────

fn sk_color(c: AnnotColor) -> resvg::tiny_skia::Color {
    resvg::tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3])
}

/// A rounded-rectangle path (corner radius `r`, clamped to half the smaller side) built
/// from cubic-bezier quarter-circle corners. Falls back to a sharp rect at `r <= 0`.
fn round_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    use resvg::tiny_skia as sk;
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.0 {
        let mut pb = sk::PathBuilder::new();
        pb.push_rect(sk::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let (l, t, rr, b) = (x, y, x + w, y + h);
    let k = r * 0.552_285; // circle-approximating control-point offset (kappa)
    let mut pb = sk::PathBuilder::new();
    pb.move_to(l + r, t);
    pb.line_to(rr - r, t);
    pb.cubic_to(rr - r + k, t, rr, t + r - k, rr, t + r);
    pb.line_to(rr, b - r);
    pb.cubic_to(rr, b - r + k, rr - r + k, b, rr - r, b);
    pb.line_to(l + r, b);
    pb.cubic_to(l + r - k, b, l, b - r + k, l, b - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
    pb.finish()
}

/// Rasterize the whole scene into a `w`×`h` straight-alpha RGBA, scaling SOURCE-pixel
/// geometry by `scale` (raster px per source px — `1.0` for the full-res bake, `< 1` for a
/// downscaled target). Now used ONLY by the bake ([`apply_annotations`]); the live preview
/// draws vectors. `curve` (the shared curviness, 0..1) softens BOTH the box corners
/// and the arrow caps/joins. Returns `None` for an empty target.
/// How much room (RASTER px) the bake gives an OFF-CANVAS caption around the output before
/// cropping back to it (DRAGON-368) — see the Text arm of [`rasterize_scene`]. The artifact it
/// exists to move out of frame lands on the pixmap's own boundary line, so one px would do; eight
/// costs `(w + h) * 8 * 4` bytes once per bake and leaves room for a shallow curve that smears
/// across more than a single scanline.
const TEXT_BAKE_BLEED: u32 = 8;

pub fn rasterize_scene(
    items: &[AnnotationItem],
    w: u32,
    h: u32,
    scale: f32,
    curve_radius: f32,
) -> Option<RgbaImage> {
    use resvg::tiny_skia as sk;
    if w == 0 || h == 0 {
        return None;
    }
    // Sequence-badge ordinals are DERIVED from the scene (never stored), so the bake resolves
    // them exactly like the canvas does — one lookup table for the whole pass.
    let badges = badge_numbers(items);
    // The rounded-corner / round-cap style both shapes share.
    let (cap, join) = if curve_radius > 0.0 {
        (sk::LineCap::Round, sk::LineJoin::Round)
    } else {
        (sk::LineCap::Butt, sk::LineJoin::Miter)
    };
    let mut pixmap = sk::Pixmap::new(w, h)?;
    let ident = sk::Transform::identity();
    for item in items {
        match &item.kind {
            AnnotKind::Box { rect, stroke_w, fill } => {
                let (x, y) = (rect.x * scale, rect.y * scale);
                let (rw, rh) = ((rect.w * scale).max(0.1), (rect.h * scale).max(0.1));
                // ABSOLUTE corner radius: a CONSTANT amount regardless of box size, only
                // shrunk when the box is too small to fit it (round_rect_path clamps to
                // half the smaller side). So every big-enough box shows the same radius.
                let corner = curve_radius * scale;
                let Some(path) = round_rect_path(x, y, rw, rh, corner) else { continue };
                if let Some(f) = fill {
                    let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
                    paint.set_color(sk_color(*f));
                    pixmap.fill_path(&path, &paint, sk::FillRule::Winding, ident, None);
                }
                let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
                paint.set_color(sk_color(item.color));
                let stroke = sk::Stroke {
                    width: (stroke_w * scale).max(0.5),
                    line_join: join,
                    line_cap: cap,
                    ..Default::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, ident, None);
            }
            AnnotKind::Arrow { a, b, stroke_w } => {
                draw_arrow(&mut pixmap, (a.x, a.y), (b.x, b.y), *stroke_w, item.color, scale, cap, join);
            }
            // The SEQUENCE BADGE (DRAGON-340) at FULL capture resolution: the exact same
            // `crate::badge` metrics the canvas draws, multiplied by the raster `scale`
            // instead of by the canvas's zoom — display and bake are one drawing at two
            // resolutions, like the pen's ribbon.
            AnnotKind::Badge { rect, ring_w } => {
                let n = badges.iter().find(|(id, _)| *id == item.id).map_or(1, |(_, n)| *n);
                draw_badge(&mut pixmap, rect, *ring_w, n, item.color, scale);
            }
            // BoxHighlight: its highlight FILL composites through the effect stack (skipped
            // here); its box OUTLINE is a source-over vector, drawn EXACTLY like a fill-less
            // box so the outline stroke matches Box's (DRAGON-333).
            AnnotKind::BoxHighlight { rect, stroke_w } => {
                let (x, y) = (rect.x * scale, rect.y * scale);
                let (rw, rh) = ((rect.w * scale).max(0.1), (rect.h * scale).max(0.1));
                let corner = curve_radius * scale;
                let Some(path) = round_rect_path(x, y, rw, rh, corner) else { continue };
                let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
                paint.set_color(sk_color(item.color));
                let stroke = sk::Stroke {
                    width: (stroke_w * scale).max(0.5),
                    line_join: join,
                    line_cap: cap,
                    ..Default::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, ident, None);
            }
            // Freehand pen (DRAGON-338 + DRAGON-342): a variable-width ribbon, so it bakes as a
            // FILLED outline rather than a stroked polyline — the exact polygons the canvas
            // fills live ([`crate::pen_stroke::stroke_fill_polygons`]), mapped by `scale`
            // instead of by the canvas's zoom. Display and bake therefore differ only in
            // resolution. Every piece of every stroke goes into ONE path filled with the
            // NON-ZERO rule, so a scribble crossing itself unions instead of cancelling into
            // holes (and a partially transparent color composites exactly once).
            AnnotKind::Pen { paths, pressure, stroke_w } => {
                let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
                paint.set_color(sk_color(item.color));
                let mut pb = sk::PathBuilder::new();
                for (i, path) in paths.iter().enumerate() {
                    let pts = pen_xy(path);
                    let press = pen_pressure(pressure, paths, i);
                    let polys = crate::pen_stroke::stroke_fill_polygons(
                        &pts,
                        *stroke_w,
                        press,
                        |p| (p.0 * scale, p.1 * scale),
                        scale,
                    );
                    for poly in polys {
                        let Some(first) = poly.first() else { continue };
                        pb.move_to(first.0, first.1);
                        for q in &poly[1..] {
                            pb.line_to(q.0, q.1);
                        }
                        pb.close();
                    }
                }
                if let Some(ribbon) = pb.finish() {
                    pixmap.fill_path(&ribbon, &paint, sk::FillRule::Winding, ident, None);
                }
            }
            // TEXT (DRAGON-354): the SAME embedded-font renderer the live layer calls, so bake
            // and preview are one drawing at two resolutions — and the SAME layout derivation
            // as the stored geometry ([`text_kind_layout`]; `w / scale` recovers the SOURCE
            // frame width this raster covers), so the wrap can never differ either.
            AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } => {
                let lay = text_kind_layout(
                    text,
                    *size_px,
                    *font,
                    *rect,
                    *constrained,
                    w as f32 / scale.max(f32::EPSILON),
                );
                let origin = (rect.x * scale, rect.y * scale);
                // DRAGON-368 — text may now hang OFF the canvas, and a glyph whose outline
                // crosses the output's edge cannot be drawn straight into the output pixmap:
                // tiny_skia's path clipper closes the clipped outline along the pixmap boundary,
                // which deposits phantom coverage on the boundary ROW/COLUMN. Measured on a
                // caption dragged past the top edge: alpha up to 121/255 in picture row 0 at
                // every place a glyph crossed it, i.e. a faint dotted line along the edge of the
                // exported image, where the live layer (cut by a hard GPU scissor) shows nothing.
                //
                // So a caption that escapes the output is drawn into a BLED pixmap first and
                // blitted back — the clipper's artifact then lands in the bleed and is cropped
                // away, and an axis-aligned pixmap blit has no rasterizer to misbehave. Text that
                // sits wholly inside takes the original call unchanged, so every historical bake
                // stays byte-identical.
                let escapes = text_padded_bounds(std::slice::from_ref(item)).is_some_and(
                    |(x0, y0, x1, y1)| {
                        x0 * scale < 0.0
                            || y0 * scale < 0.0
                            || x1 * scale > w as f32
                            || y1 * scale > h as f32
                    },
                );
                if escapes {
                    let bleed = TEXT_BAKE_BLEED;
                    if let Some(mut bled) = sk::Pixmap::new(w + 2 * bleed, h + 2 * bleed) {
                        super::text_annot::render_into(
                            &mut bled,
                            &lay,
                            *font,
                            *size_px,
                            item.color,
                            *stroke_w,
                            (origin.0 + bleed as f32, origin.1 + bleed as f32),
                            scale,
                        );
                        pixmap.as_mut().draw_pixmap(
                            -(bleed as i32),
                            -(bleed as i32),
                            bled.as_ref(),
                            &sk::PixmapPaint::default(),
                            ident,
                            None,
                        );
                        continue;
                    }
                }
                super::text_annot::render_into(
                    &mut pixmap,
                    &lay,
                    *font,
                    *size_px,
                    item.color,
                    *stroke_w,
                    origin,
                    scale,
                );
            }
            // The region effects (highlight, pixelate, blur) are NOT source-over overlays —
            // they composite through the true-z-order CPU stack ([`apply_one_effect`] /
            // [`apply_effects`]), so this source-over rasterizer skips them. Spotlight
            // (DRAGON-329) has no vector rendering at all — it only punches the dim knockout.
            AnnotKind::Highlight { .. }
            | AnnotKind::Pixelate { .. }
            | AnnotKind::Blur { .. }
            | AnnotKind::Spotlight { .. } => {}
        }
    }
    // Premultiplied → straight alpha (same conversion as the covermark raster).
    let mut rgba = RgbaImage::new(w, h);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = ::image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(rgba)
}

/// A circle as a tiny-skia path, built from four cubic quarter-arcs (the same kappa
/// construction [`round_rect_path`] uses). `None` for a non-positive radius.
fn circle_path(cx: f32, cy: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    use resvg::tiny_skia as sk;
    // NaN-safe: an unordered radius takes this branch and draws nothing.
    if r.is_nan() || r <= 0.0 {
        return None;
    }
    let k = r * 0.552_285;
    let mut pb = sk::PathBuilder::new();
    pb.move_to(cx, cy - r);
    pb.cubic_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
    pb.cubic_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
    pb.cubic_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
    pb.cubic_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
    pb.close();
    pb.finish()
}

/// Draw one SEQUENCE BADGE (DRAGON-340) into `pixmap` at raster `scale` (target px per SOURCE
/// px): the filled disc, the outer ring at the current line weight, and the ordinal `number`
/// in the contrast ink. The MIRROR of the canvas's `draw_badge` — both read
/// [`crate::badge::metrics`] for SOURCE-px figures and apply exactly one uniform factor, so
/// what the editor showed is what the export contains.
fn draw_badge(
    pixmap: &mut resvg::tiny_skia::Pixmap,
    rect: &AnnotRect,
    ring_w: f32,
    number: u32,
    color: AnnotColor,
    scale: f32,
) {
    use resvg::tiny_skia as sk;
    let ident = sk::Transform::identity();
    let side = rect.w.abs().min(rect.h.abs());
    let m = crate::badge::metrics(side, ring_w, crate::badge::digit_count(number));
    if m.disc_r <= 0.0 {
        return;
    }
    let (cx, cy) = ((rect.x + rect.w * 0.5) * scale, (rect.y + rect.h * 0.5) * scale);
    let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
    paint.set_color(sk_color(color));
    // Disc.
    if let Some(disc) = circle_path(cx, cy, m.disc_r * scale) {
        pixmap.fill_path(&disc, &paint, sk::FillRule::Winding, ident, None);
    }
    // Ring — stroked ON the model square's inscribed circle.
    if m.ring_w > 0.0
        && let Some(ring) = circle_path(cx, cy, m.outer_r * scale)
    {
        let stroke = sk::Stroke {
            width: (m.ring_w * scale).max(0.5),
            line_cap: sk::LineCap::Round,
            line_join: sk::LineJoin::Round,
            ..Default::default()
        };
        pixmap.stroke_path(&ring, &paint, &stroke, ident, None);
    }
    // The ordinal, in whichever tone contrasts with the disc.
    let ink = crate::badge::ink_rgb8(color);
    let mut ink_paint = sk::Paint { anti_alias: true, ..Default::default() };
    ink_paint.set_color(sk_color([ink[0], ink[1], ink[2], color[3]]));
    let numeral = sk::Stroke {
        width: (m.digit_stroke * scale).max(0.5),
        line_cap: sk::LineCap::Round,
        line_join: sk::LineJoin::Round,
        ..Default::default()
    };
    let centre = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
    for poly in crate::badge::number_polylines(number, &m, centre) {
        let Some(first) = poly.first() else { continue };
        let mut pb = sk::PathBuilder::new();
        pb.move_to(first.0 * scale, first.1 * scale);
        for q in &poly[1..] {
            pb.line_to(q.0 * scale, q.1 * scale);
        }
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &ink_paint, &numeral, ident, None);
        }
    }
}

/// Draw one arrow into `pixmap` (source coords scaled by `scale`): a shaft to the tip plus
/// an OPEN "V" head — two short barbs diverging from the tip back along the shaft, like a
/// real arrow glyph (NOT a filled triangle). `cap`/`join` (the shared curviness style) keep
/// the tip and barb ends soft.
#[allow(clippy::too_many_arguments)]
fn draw_arrow(
    pixmap: &mut resvg::tiny_skia::Pixmap,
    a: (f32, f32),
    b: (f32, f32),
    stroke_w: f32,
    color: AnnotColor,
    scale: f32,
    cap: resvg::tiny_skia::LineCap,
    join: resvg::tiny_skia::LineJoin,
) {
    use resvg::tiny_skia as sk;
    let ident = sk::Transform::identity();
    let (ax, ay) = (a.0 * scale, a.1 * scale);
    let (bx, by) = (b.0 * scale, b.1 * scale);
    // Arrows always render 2 SOURCE px THICKER than the set stroke width (mirrors the vector
    // display's `ARROW_STROKE_BONUS`) so an arrow reads as bolder than a same-width box.
    let sw = ((stroke_w + 2.0) * scale).max(0.5);
    let dx = bx - ax;
    let dy = by - ay;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
    paint.set_color(sk_color(color));
    let stroke = sk::Stroke { width: sw, line_cap: cap, line_join: join, ..Default::default() };
    // Shaft: tail all the way to the tip.
    let mut pb = sk::PathBuilder::new();
    pb.move_to(ax, ay);
    pb.line_to(bx, by);
    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, &paint, &stroke, ident, None);
    }
    // Open-"V" head: two barbs from the tip, splayed back along the shaft direction.
    // `back` is -unit(shaft); rotate it ±ang for the two barbs. `.min(len * 0.7)` on the
    // floor keeps the clamp bounds ordered (min ≤ max) so a very short arrow can't hit
    // `f32::clamp`'s min > max panic (DRAGON-324, matching the vector display path).
    // Head length = 12.5% of the shaft (GROWS with the line), floored at 40% of the 53px cap so a
    // short arrow still shows a substantial head, capped at 53px (source) so a long line doesn't get
    // a huge head, and never past 70% of the shaft. Independent of stroke width (barbs get THICKER,
    // not longer). In sync with the vector display (`annotation_canvas`, same 0.125 / 0.40·53 / 53).
    let head_cap = (53.0 * scale).min(len * 0.7);
    let head = (len * 0.125).clamp((53.0 * 0.40 * scale).min(head_cap), head_cap);
    let ang = 0.52_f32; // ~30° half-angle
    let (ca, sa) = (ang.cos(), ang.sin());
    let back = (-ux, -uy);
    let lft = (back.0 * ca - back.1 * sa, back.0 * sa + back.1 * ca);
    let rgt = (back.0 * ca + back.1 * sa, -back.0 * sa + back.1 * ca);
    let lp = (bx + lft.0 * head, by + lft.1 * head);
    let rp = (bx + rgt.0 * head, by + rgt.1 * head);
    let mut hb = sk::PathBuilder::new();
    hb.move_to(lp.0, lp.1);
    hb.line_to(bx, by);
    hb.line_to(rp.0, rp.1);
    if let Some(head_path) = hb.finish() {
        pixmap.stroke_path(&head_path, &paint, &stroke, ident, None);
    }
}

/// Composite the SOURCE-OVER vector shapes (box / arrow) of the scene onto `base` at FULL
/// source resolution (position-aware: the raster is already in source coordinates, so it
/// aligns 1:1). Straight-alpha src-over. No-op when empty. The region effects
/// (highlight / pixelate / blur) are NOT drawn here — they composite earlier in the bake via
/// [`apply_effects`]; [`rasterize_scene`] skips them so this pass is the vector shapes only.
pub fn apply_annotations(base: &mut RgbaImage, items: &[AnnotationItem], curve_radius: f32) {
    if items.is_empty() {
        return;
    }
    let (w, h) = base.dimensions();
    let Some(overlay) = rasterize_scene(items, w, h, 1.0, curve_radius) else {
        return;
    };
    for (dst, src) in base.pixels_mut().zip(overlay.pixels()) {
        let a = src.0[3] as u32;
        if a == 0 {
            continue;
        }
        for (d, s) in dst.0.iter_mut().take(3).zip([src.0[0], src.0[1], src.0[2]]) {
            *d = ((s as u32 * a + *d as u32 * (255 - a)) / 255) as u8;
        }
    }
}

/// [`apply_annotations`] onto a canvas whose top-left sits at source-pixel `offset` — the vector
/// overlay drawn into a CROPPED canvas (DRAGON-389).
///
/// Every item is shifted by `-offset` (via [`translated_kind`]) before rasterizing at the CROPPED
/// canvas's own size, so a shape placed over the crop's black extension composites onto the cut
/// canvas instead of being clipped at the old source edge. `offset == (0, 0)` is byte-identical to
/// [`apply_annotations`] (and takes its fast path). Pixels INSIDE the crop are unchanged versus the
/// historical "annotate the full source, then crop" order — the rasterizer's per-pixel coverage is
/// translation-equivariant — so a crop that keeps annotations inside the source bakes identically.
pub fn apply_annotations_at(base: &mut RgbaImage, items: &[AnnotationItem], curve_radius: f32, offset: (f32, f32)) {
    if offset == (0.0, 0.0) {
        return apply_annotations(base, items, curve_radius);
    }
    let shifted: Vec<AnnotationItem> = items
        .iter()
        .map(|it| AnnotationItem {
            id: it.id,
            color: it.color,
            kind: translated_kind(&it.kind, -offset.0, -offset.1),
        })
        .collect();
    apply_annotations(base, &shifted, curve_radius);
}

// ── region effects: multiply highlight + destructive pixelate/blur (DRAGON-326/327/328) ──

/// Rec.709 luma weights, applied to the (sRGB-encoded) low-pass values consistently on
/// display + bake — only the RELATIVE light/dark decision matters, so the encoding cancels.
const HL_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// The background-luminance smoothstep band: below `DARK` the highlight SCREENS (stays
/// visible on dark content), above `LIGHT` it MULTIPLIES (keeps dark text legible on light
/// content), lerping smoothly across a spanning highlight. Mirrored verbatim in the shader.
const HL_DARK_EDGE: f32 = 0.35;
const HL_LIGHT_EDGE: f32 = 0.65;

/// Smoothstep (Hermite), matching WGSL `smoothstep`.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The multiply↔screen blend weight from a low-pass BACKGROUND pixel: `1` = light bg
/// (multiply), `0` = dark bg (screen). Keyed on the BLURRED background luminance (not the
/// pixel's own value), so it never flips on a text stroke.
fn adaptive_weight(bg: [u8; 3]) -> f32 {
    let l = HL_LUMA[0] * (bg[0] as f32 / 255.0)
        + HL_LUMA[1] * (bg[1] as f32 / 255.0)
        + HL_LUMA[2] * (bg[2] as f32 / 255.0);
    smoothstep(HL_DARK_EDGE, HL_LIGHT_EDGE, l)
}

/// The full-strength ADAPTIVE highlight blend (0..1 per channel) of `base` under `color`,
/// selected by the background weight `w`: `mix(screen, multiply, w)` where
/// `multiply = base·color` and `screen = 1 − (1−base)(1−color)`.
fn adaptive_blend_norm(base: [u8; 3], color: [u8; 3], w: f32) -> [f32; 3] {
    std::array::from_fn(|i| {
        let b = base[i] as f32 / 255.0;
        let c = color[i] as f32 / 255.0;
        let multiply = b * c;
        let screen = 1.0 - (1.0 - b) * (1.0 - c);
        screen + (multiply - screen) * w // mix(screen, multiply, w)
    })
}

/// The full ADAPTIVE highlight composite for one pixel (DRAGON-326) — the SINGLE source of the
/// highlight math, used by BOTH the bake and (as its exact intent) the display shader. It
/// picks multiply vs screen from the low-pass BACKGROUND `bg` (`w = smoothstep`), blends the
/// `operand` under `color` (`mix(screen, multiply, w)`), then composites the `backdrop` toward
/// that blend by `alpha` (which already folds in edge coverage): `out = mix(backdrop, blended,
/// alpha)`.
///
/// `operand` is the pixel the color acts on (the pristine base → dark text stays legible);
/// `backdrop` is what the highlight sits over (the possibly-redacted content → no
/// redaction leak in the composited result). With no redaction underneath they're equal, and
/// the display samples the same base + low-pass, so what the user sees is what saves. Works on
/// LIGHT content (dark text legible), DARK content (stays visibly colored), and a highlight
/// spanning BOTH (smooth transition, no flip). Pure — unit-tested against the shader intent.
pub fn adaptive_highlight_px(
    backdrop: [u8; 3],
    operand: [u8; 3],
    color: [u8; 3],
    bg: [u8; 3],
    alpha: u8,
) -> [u8; 3] {
    let a = alpha as f32 / 255.0;
    let w = adaptive_weight(bg);
    let blended = adaptive_blend_norm(operand, color, w);
    std::array::from_fn(|i| {
        let bd = backdrop[i] as f32 / 255.0;
        ((bd + (blended[i] - bd) * a) * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

/// The six rect kinds share the same geometry/interaction — Box Outline, Highlight, Box
/// Highlight, Pixelate, Blur, and Spotlight — so a selected one can CONVERT to another in place
/// when the user picks a different one of those tools. Returns the converted kind (rect
/// preserved; the shared [`AnnotationItem::color`] is untouched by the caller; the outline
/// `stroke_w` carries between Box/BoxHighlight and falls back to `default_stroke` when the source
/// has none), or `None` when either side isn't a rect kind/tool (e.g. Arrow, or the always-1:1
/// SEQUENCE BADGE — see [`rect_family_id`]) or the kind is unchanged.
pub(super) fn converted_rect_kind(
    cur: &AnnotKind,
    tool: Tool,
    default_stroke: f32,
) -> Option<AnnotKind> {
    // Rect-kind ids: 0 Box (outline), 1 Highlight, 2 Box Highlight, 3 Pixelate, 4 Blur, 5 Spotlight.
    // (The always-square SEQUENCE BADGE is not in the family — see [`rect_family_id`].)
    let from = rect_family_id(cur)?;
    let to = rect_family_tool(tool)?;
    if from == to {
        return None; // picking the current kind's own tool — no conversion, no undo entry.
    }
    let rect = match cur {
        AnnotKind::Box { rect, .. }
        | AnnotKind::Highlight { rect }
        | AnnotKind::BoxHighlight { rect, .. }
        | AnnotKind::Pixelate { rect }
        | AnnotKind::Blur { rect }
        | AnnotKind::Spotlight { rect } => *rect,
        _ => return None,
    };
    let stroke = match cur {
        AnnotKind::Box { stroke_w, .. } | AnnotKind::BoxHighlight { stroke_w, .. } => *stroke_w,
        _ => default_stroke,
    };
    Some(match to {
        0 => AnnotKind::Box { rect, stroke_w: stroke, fill: None },
        1 => AnnotKind::Highlight { rect },
        2 => AnnotKind::BoxHighlight { rect, stroke_w: stroke },
        3 => AnnotKind::Pixelate { rect },
        4 => AnnotKind::Blur { rect },
        _ => AnnotKind::Spotlight { rect },
    })
}

/// Downsample `img` to per-`block` MEANS: a `ceil(w/block) × ceil(h/block)` image where each
/// texel is the average of its source block (partial edge blocks average only their real
/// pixels). Block-averaging destroys sub-block detail — the irreversible core of BOTH
/// redactions. The display shader NEAREST-samples this (crisp pixelate) or LINEAR-samples it
/// (soft blur); the bake expands it the same way. Pure — unit-tested.
pub fn block_means(img: &RgbaImage, block: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let block = block.max(1);
    let mw = w.div_ceil(block).max(1);
    let mh = h.div_ceil(block).max(1);
    let mut out = RgbaImage::new(mw, mh);
    for by in 0..mh {
        for bx in 0..mw {
            let (x0, y0) = (bx * block, by * block);
            let (x1, y1) = ((x0 + block).min(w), (y0 + block).min(h));
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = img.get_pixel(xx, yy).0;
                    r += p[0] as u64;
                    g += p[1] as u64;
                    b += p[2] as u64;
                    a += p[3] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.put_pixel(
                bx,
                by,
                ::image::Rgba([(r / n) as u8, (g / n) as u8, (b / n) as u8, (a / n) as u8]),
            );
        }
    }
    out
}

/// The CONTENT-AWARE pixelate cell size (SOURCE px) for `rect` over the PRISTINE full-resolution
/// `src` image — the SINGLE source of truth BOTH the GPU display (`annotation_fx`, via
/// [`content_pixelate_block_px`]) and the CPU bake ([`apply_one_effect_scaled`]) call, so they
/// pick the EXACT same block and stay WYSIWYG. Analyzing the full-res source (not the
/// display-resolution accumulator) is what keeps the two paths in agreement.
pub fn content_pixelate_block(src: &RgbaImage, rect: &AnnotRect) -> u32 {
    content_pixelate_block_px(src.as_raw(), src.width(), src.height(), rect)
}

/// [`content_pixelate_block`] over raw straight-alpha RGBA bytes (`w`×`h`, row-major), so the
/// display side can analyze a `PixelFrame` in place without copying it into an [`RgbaImage`].
///
/// Heuristic (redaction-safe, region-size-INDEPENDENT, STABLE, cheap enough for live drag): sample
/// the WHOLE region on a FIXED fine grid (the stride only grows past `FINE` for a huge region, to
/// bound cost). Sampling EVERYTHING — not sparse patches — is what makes it stable: a 1px change in
/// the selection can't shift a sample window off a text line and flip the cell size. Then measure edge
/// density in ~`TILE_PX` TILES rather than per-row/col lines. A tile spans SEVERAL small-text lines, so
/// it absorbs the blank gaps between them: body text reads as a uniformly DENSE tile, while a big
/// header glyph reads as a SPARSE tile (few large strokes). Pick a HIGH-percentile CONTENT tile
/// (empties dropped) — the DENSEST (busiest) content drives the cell (`PICK_PCT`). Packed text has a
/// consistent density, so this is STABLE under live drag (the MEDIAN jitters — a paragraph's tiles are
/// bimodal, full-text vs line-gap, and the median flips between the two modes on a few px of movement)
/// and yields the tight, crisp cell. A pure paragraph keeps its small cell; a header that's the
/// MAJORITY of the selection still resolves to its coarse cell (lower `PICK_PCT` to grow the cell for a
/// minority header, trading drag stability). `feature_scale = stride / density`; the cell is `GLYPH_FACTOR ×` that so a feature
/// collapses to a couple of cells. Works for ANY content (text, faces, …), region-size-independent.
/// Flat → the [`PIXELATE_BLOCK`] floor; fine content stays small; coarse content grows to the
/// [`PIXELATE_BLOCK_MAX`] ceiling. Snapped to a multiple of 4 (no frame-to-frame jitter), recomputed
/// every render so it tracks while you draw AND move. Pure — unit-tested.
pub fn content_pixelate_block_px(rgba: &[u8], w: u32, h: u32, rect: &AnnotRect) -> u32 {
    const FINE: u32 = 2; // fine sample stride (SOURCE px) — resolves features down to ~4px
    const BUDGET: u32 = 300_000; // max grid samples (the stride only grows past FINE beyond this)
    const EDGE_THRESH: i32 = 24; // |Δluma| (0..255) counted as an edge (~9%)
    const MIN_DENSITY: f32 = 0.02; // busy-percentile below this ⇒ the region reads as flat → floor
    const GLYPH_FACTOR: f32 = 2.0; // cell ≈ 2× the resolved edge spacing (a glyph → a couple cells)
    const TILE_PX: u32 = 32; // analyze density in ~32px tiles — a tile spans several small-text lines
    const PICK_PCT: f32 = 0.8; // which tile (by density, sorted) drives the cell. HIGH → the densest
                               // (busiest) tiles: packed text has a consistent density, so the cell is
                               // STABLE under drag and stays the tight, crisp mosaic. The MEDIAN (0.5)
                               // jitters — a paragraph's tiles are bimodal (full-text vs line-gap), and
                               // the median flips between the two modes on a few px of movement. Lower
                               // this to bias toward coarser cells (a minority header grows the block).
    let floor = PIXELATE_BLOCK;
    let ceil = PIXELATE_BLOCK_MAX;
    if w == 0 || h == 0 || rgba.len() < (w as usize * h as usize * 4) {
        return floor;
    }
    // Clamp the region to the image (integer source px).
    let x0 = (rect.x.max(0.0).floor() as i64).clamp(0, w as i64) as u32;
    let y0 = (rect.y.max(0.0).floor() as i64).clamp(0, h as i64) as u32;
    let x1 = ((rect.x + rect.w).ceil() as i64).clamp(0, w as i64) as u32;
    let y1 = ((rect.y + rect.h).ceil() as i64).clamp(0, h as i64) as u32;
    // LIVE-DRAG STABILITY: snap the analyzed rect to a coarse grid (origin floored, far edge ceiled →
    // the smallest grid-aligned rect covering the selection). Nudging or resizing the selection within
    // one grid cell then feeds byte-identical content in, so the median tile can't drift across a snap
    // step and flip the block frame-to-frame (the whole mosaic re-tiles when the block size changes).
    // The mosaic PHASE is already texture-aligned (stable under move); this stabilizes only the SIZE.
    // Both display and bake call this with the same rect, so they stay in agreement.
    const QUANT: u32 = 16;
    let x0 = (x0 / QUANT) * QUANT;
    let y0 = (y0 / QUANT) * QUANT;
    let x1 = (x1.div_ceil(QUANT) * QUANT).min(w);
    let y1 = (y1.div_ceil(QUANT) * QUANT).min(h);
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return floor; // degenerate / sub-2px region
    }
    let (rw, rh) = (x1 - x0, y1 - y0);
    let luma_at = |x: u32, y: u32| -> i32 {
        let i = ((y as usize * w as usize) + x as usize) * 4;
        // Rec.709-ish integer luma (weights sum to 256).
        (54 * rgba[i] as i32 + 183 * rgba[i + 1] as i32 + 19 * rgba[i + 2] as i32) >> 8
    };
    // Sample the WHOLE region on a FIXED fine grid — sampling everything (not sparse patches) is what
    // makes this STABLE: a 1px change in the selection can't shift a sample window off a text line and
    // flip the block. The stride only grows past FINE if a fine grid would exceed BUDGET samples.
    let area = rw as u64 * rh as u64;
    let stride = ((area as f64 / BUDGET as f64).sqrt().ceil() as u32).max(FINE);
    let nx = (rw / stride).max(2);
    let ny = (rh / stride).max(2);
    let mut grid = vec![0i32; (nx as usize) * (ny as usize)];
    for j in 0..ny {
        let yy = (y0 + j * stride).min(y1 - 1);
        for i in 0..nx {
            let xx = (x0 + i * stride).min(x1 - 1);
            grid[(j * nx + i) as usize] = luma_at(xx, yy);
        }
    }
    let pct = |v: &[f32], q: f32| -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        s[(((s.len() - 1) as f32) * q.clamp(0.0, 1.0)).round() as usize]
    };
    // Edge density per TILE (a tile ≈ TILE_PX source px, so it spans SEVERAL small-text lines). Each
    // tile's density is the busier of its horizontal/vertical stroke frequency. Why tiles and not
    // per-row/col lines: a tile absorbs the blank GAPS between text lines, so body text reads as a
    // uniformly DENSE tile while a big header glyph reads as a SPARSE tile — only genuinely coarse
    // content yields a low-density content tile. That's the signal a header is present, and the
    // line-gap false-positive (which used to blow up plain paragraphs) is gone.
    let t = ((TILE_PX / stride) as usize).max(2); // target tile size in grid cells
    // Tile COUNT (rounded), then partition the grid EVENLY into that many tiles. Even partitioning
    // (vs a fixed size with a leftover) is what keeps this stable under a 1px resize: there's no tiny
    // remainder tile at the edge that — catching mostly a blank line-gap — could read as a spurious
    // coarse (low-density) tile and inflate the cell. Every tile stays ≈ TILE_PX.
    let (gx, gy) = (nx as usize, ny as usize);
    let ntx = ((gx + t / 2) / t).max(1);
    let nty = ((gy + t / 2) / t).max(1);
    let mut tiles: Vec<f32> = Vec::with_capacity(ntx * nty);
    for tj in 0..nty {
        let (cj0, cj1) = (tj * gy / nty, (tj + 1) * gy / nty);
        for ti in 0..ntx {
            let (ci0, ci1) = (ti * gx / ntx, (ti + 1) * gx / ntx);
            let (mut he, mut hp, mut ve, mut vp) = (0u32, 0u32, 0u32, 0u32);
            for cj in cj0..cj1 {
                for ci in ci0..ci1 {
                    let v = grid[cj * nx as usize + ci];
                    if ci + 1 < ci1 {
                        hp += 1;
                        if (grid[cj * nx as usize + ci + 1] - v).abs() > EDGE_THRESH {
                            he += 1;
                        }
                    }
                    if cj + 1 < cj1 {
                        vp += 1;
                        if (grid[(cj + 1) * nx as usize + ci] - v).abs() > EDGE_THRESH {
                            ve += 1;
                        }
                    }
                }
            }
            let hd = if hp > 0 { he as f32 / hp as f32 } else { 0.0 };
            let vd = if vp > 0 { ve as f32 / vp as f32 } else { 0.0 };
            tiles.push(hd.max(vd));
        }
    }
    // CONTENT tiles only (drop the empty ones around/between features), then pick a HIGH-percentile
    // tile density — the DENSEST (busiest) content. Packed text has a consistent density, so this is
    // STABLE under drag (no jitter) and yields the tight, crisp cell. A pure paragraph keeps its small
    // cell; a header that's the MAJORITY of the selection still resolves to its coarse (big) cell.
    // (Lower PICK_PCT to grow the cell for a smaller/minority header — at the cost of drag stability.)
    let content: Vec<f32> = tiles.into_iter().filter(|&d| d >= MIN_DENSITY).collect();
    if content.is_empty() {
        return floor; // flat / clean — nothing to destroy beyond the floor mosaic
    }
    let density = pct(&content, PICK_PCT);
    let feature_scale = stride as f32 / density; // resolved SOURCE px between edges in the densest tile
    let raw = GLYPH_FACTOR * feature_scale;
    let snapped = ((raw / 4.0).round() * 4.0) as i64; // snap to a multiple of 4 (stable across frames)
    snapped.clamp(floor as i64, ceil as i64) as u32
}

/// Stack `passes` single-pass box-ish blurs (block-mean → bilinear `Triangle` upsample) over
/// `img` — three stacked box blurs approximate a Gaussian, so the result reads as a strong SMOOTH
/// blur (one pass alone is too weak). Each pass blurs the PREVIOUS pass's result, compounding the
/// smoothing. `passes` of 0 is treated as 1. The standalone Blur effect uses this; the highlight
/// low-pass deliberately does not (it stays one pass). Pure — unit-tested.
pub fn box_blur_stack(img: &RgbaImage, block: u32, passes: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut work = img.clone();
    for _ in 0..passes.max(1) {
        let m = block_means(&work, block);
        work = ::image::imageops::resize(
            &m,
            w.max(1),
            h.max(1),
            ::image::imageops::FilterType::Triangle,
        );
    }
    work
}

/// Rasterize `rect`'s anti-aliased rounded-rect coverage (SAME `round_rect_path` the box
/// uses) into a full-resolution WHITE premultiplied [`Pixmap`] whose alpha channel IS the
/// coverage. `enumerate_pixels`/`pixels()` are both row-major, so consumers stay aligned.
/// The single geometry source for every effect, so all three round identically to the box.
fn coverage_mask(w: u32, h: u32, rect: &AnnotRect, curve_radius: f32) -> Option<resvg::tiny_skia::Pixmap> {
    use resvg::tiny_skia as sk;
    if w == 0 || h == 0 {
        return None;
    }
    let mut mask = sk::Pixmap::new(w, h)?;
    let (rw, rh) = (rect.w.max(0.1), rect.h.max(0.1));
    let path = round_rect_path(rect.x, rect.y, rw, rh, curve_radius)?;
    let mut paint = sk::Paint { anti_alias: true, ..Default::default() };
    paint.set_color(sk::Color::WHITE);
    mask.fill_path(&path, &paint, sk::FillRule::Winding, sk::Transform::identity(), None);
    Some(mask)
}

/// Straight-alpha `mix(dst, src, cov/255)` of two RGB triples (pure — no side effects).
fn mix_rgb_pure(dst: [u8; 3], src: [u8; 3], cov: u8) -> [u8; 3] {
    let a = cov as u32;
    std::array::from_fn(|i| ((src[i] as u32 * a + dst[i] as u32 * (255 - a)) / 255) as u8)
}

/// Apply ONE region effect at FULL source resolution — the SINGLE compositing core shared by
/// display AND bake, so the two can't diverge (DRAGON-330 true-layer stack). Reads the
/// effect's region FROM `acc` (the content accumulated BELOW it in z-order), computes the
/// effect, and writes the OPAQUE result back INTO `acc` (so a LATER effect samples it — this
/// is how a pixelate above a highlight redacts it: destructive samples everything below) AND,
/// when `overlay` is `Some`, the same opaque pixels into `overlay` (transparent everywhere
/// else). The overlay composited over the base therefore reproduces `acc` exactly, so the
/// display (base + overlay layer) equals the bake (`acc`) BY CONSTRUCTION.
///
/// * pixelate — the block-mean mosaic of the CURRENT `acc` region, whose CELL SIZE is
///   content-aware ([`content_pixelate_block`] over the pristine `analysis` source, so display +
///   bake agree);
/// * blur — [`BLUR_PASSES`] stacked [`BLUR_BLOCK`] box blurs of the CURRENT `acc`
///   ([`box_blur_stack`]) — a strong smooth (≈ Gaussian) blur;
/// * highlight — the adaptive multiply/screen of the CURRENT `acc`, with the low-pass
///   background-luminance ALSO derived from `acc` (not a pristine base) so a highlight over a
///   redaction reads the redacted content — keeping legibility + redaction-safety.
///
/// `analysis` is the PRISTINE full-resolution source (used ONLY to size the content-aware
/// pixelate cell); the effect pixels themselves still come from `acc`.
///
/// A non-effect kind (box / arrow) is a no-op here (those are always-on-top vectors).
pub fn apply_one_effect(
    acc: &mut RgbaImage,
    overlay: Option<&mut RgbaImage>,
    analysis: &RgbaImage,
    item: &AnnotationItem,
    curve_radius: f32,
) {
    // The bake + full-res display path: SOURCE geometry at raster scale 1.0 (byte-identical
    // to the pre-scale core — `x * 1.0` is exact for f32, and the block sizes round back to
    // the raw constants).
    apply_one_effect_scaled(acc, overlay, analysis, item, curve_radius, 1.0);
}

/// [`apply_one_effect`] with an explicit raster `scale` (target px per SOURCE px) for the LIVE
/// (in-drag) reduced-resolution display path — `acc`/`overlay` are already at the reduced
/// dimensions, so the effect's SOURCE-pixel rect, corner curve, AND the pixelate/blur block
/// sizes are all scaled into that space (blocks scale WITH resolution so the mosaic/blur read
/// the same proportionally). At `scale == 1.0` this is exactly the full-resolution core.
pub fn apply_one_effect_scaled(
    acc: &mut RgbaImage,
    mut overlay: Option<&mut RgbaImage>,
    analysis: &RgbaImage,
    item: &AnnotationItem,
    curve_radius: f32,
    scale: f32,
) {
    let (w, h) = acc.dimensions();
    let src_rect = match &item.kind {
        AnnotKind::Highlight { rect }
        | AnnotKind::Pixelate { rect }
        | AnnotKind::Blur { rect }
        // BoxHighlight contributes its highlight FILL here (the outline is a vector, drawn
        // separately by the canvas / rasterize_scene — DRAGON-333).
        | AnnotKind::BoxHighlight { rect, .. } => rect,
        // Spotlight is NOT an effect (it composites nothing) — like box/arrow/pen, no-op here.
        AnnotKind::Box { .. }
        | AnnotKind::Arrow { .. }
        | AnnotKind::Pen { .. }
        // The SEQUENCE BADGE is an always-on-top vector like box/arrow, never an effect.
        | AnnotKind::Badge { .. }
        // TEXT (DRAGON-354) draws through its own raster layer, never the effect stack.
        | AnnotKind::Text { .. }
        | AnnotKind::Spotlight { .. } => return,
    };
    // Scale the geometry into the (possibly reduced) raster space. Blocks scale too, floored
    // at 1 so a heavily-downscaled live frame still averages at least one texel per cell.
    let rect = AnnotRect {
        x: src_rect.x * scale,
        y: src_rect.y * scale,
        w: src_rect.w * scale,
        h: src_rect.h * scale,
    };
    // Pixelate gets SQUARE edges (a mosaic reads as a hard grid; rounded corners look wrong on it);
    // every other effect follows the scene curve. Mirrors the GPU display (own radius 0 for pixelate).
    let curve = if matches!(item.kind, AnnotKind::Pixelate { .. }) { 0.0 } else { curve_radius * scale };
    let blur_block = ((BLUR_BLOCK as f32 * scale).round() as u32).max(1);
    let Some(mask) = coverage_mask(w, h, &rect, curve) else {
        return;
    };
    // The per-pixel effect: given the CURRENT `acc` pixel, its position, and its edge
    // coverage, produce the OPAQUE result color. Each snapshots what it samples from `acc`
    // BEFORE the write loop mutates it (a mosaic is a copy; the highlight reads `acc` pixel
    // by pixel but only within its own region, which the write loop is still traversing —
    // so it reads the pre-write value passed in as `cur`).
    type EffectPx = Box<dyn Fn([u8; 3], u32, u32, u8) -> [u8; 3]>;
    let compute: EffectPx = match &item.kind {
        AnnotKind::Pixelate { .. } => {
            // Content-aware cell size (SOURCE px) from the PRISTINE `analysis` source — the SAME
            // block the GPU display picks — then scaled into the raster space.
            let src_block = content_pixelate_block(analysis, src_rect);
            let pix_block = ((src_block as f32 * scale).round() as u32).max(1);
            let mosaic = block_means(acc, pix_block);
            let (mw, mh) = mosaic.dimensions();
            Box::new(move |cur, x, y, cov| {
                let bx = (x / pix_block).min(mw - 1);
                let by = (y / pix_block).min(mh - 1);
                let m = mosaic.get_pixel(bx, by).0;
                mix_rgb_pure(cur, [m[0], m[1], m[2]], cov)
            })
        }
        AnnotKind::Blur { .. } => {
            // Triple-strength: BLUR_PASSES stacked box blurs (≈ Gaussian) — a strong smooth blur.
            let full = box_blur_stack(acc, blur_block, BLUR_PASSES);
            Box::new(move |cur, x, y, cov| {
                let s = full.get_pixel(x.min(w - 1), y.min(h - 1)).0;
                mix_rgb_pure(cur, [s[0], s[1], s[2]], cov)
            })
        }
        // BoxHighlight's FILL is IDENTICAL to a plain Highlight (the same adaptive core); its
        // outline is drawn separately, not here.
        AnnotKind::Highlight { .. } | AnnotKind::BoxHighlight { .. } => {
            let color = [item.color[0], item.color[1], item.color[2]];
            // The low-pass background-luminance source, derived from the CURRENT `acc`.
            let lowpass = {
                let m = block_means(acc, blur_block);
                ::image::imageops::resize(&m, w.max(1), h.max(1), ::image::imageops::FilterType::Triangle)
            };
            Box::new(move |cur, x, y, cov| {
                let (cx, cy) = (x.min(w - 1), y.min(h - 1));
                let bg = lowpass.get_pixel(cx, cy).0;
                // Fold edge coverage into alpha. `acc` is the accumulated content, so it is
                // BOTH the multiply/screen operand AND the composite backdrop (equal here) —
                // a highlight over a redaction acts on, and shows over, the redacted pixels.
                let eff = ((HIGHLIGHT_ALPHA as u32 * cov as u32) / 255) as u8;
                adaptive_highlight_px(cur, cur, color, [bg[0], bg[1], bg[2]], eff)
            })
        }
        AnnotKind::Box { .. }
        | AnnotKind::Arrow { .. }
        | AnnotKind::Pen { .. }
        | AnnotKind::Badge { .. }
        | AnnotKind::Text { .. }
        | AnnotKind::Spotlight { .. } => {
            unreachable!("handled above")
        }
    };
    // Traverse the region: read `acc`, compute, write the OPAQUE result back into `acc` and
    // (when producing the display layer) into `overlay`.
    for ((x, y, px), cov) in acc.enumerate_pixels_mut().zip(mask.pixels()) {
        let c = cov.alpha();
        if c == 0 {
            continue;
        }
        let out = compute([px.0[0], px.0[1], px.0[2]], x, y, c);
        px.0[0] = out[0];
        px.0[1] = out[1];
        px.0[2] = out[2];
        if let Some(ov) = overlay.as_deref_mut() {
            *ov.get_pixel_mut(x, y) = ::image::Rgba([out[0], out[1], out[2], 255]);
        }
    }
}

/// Composite the scene's region effects (highlight / pixelate / blur) onto `acc` IN PLACE, in
/// true scene z-order, via [`apply_one_effect`] — the BAKE path (overlay = `None`): `acc` IS
/// the composited result. No-op when the scene has no effects.
///
/// Z-order note: the FIRST effect item is the bottom-most of the stack. The global
/// dim/spotlight (DRAGON-329, [`apply_dim`]) sits BELOW even that — it darkens the base BEFORE
/// this walk runs (see [`super::edit::bake_image`]), the hard floor beneath every effect.
pub fn apply_effects(
    acc: &mut RgbaImage,
    analysis: &RgbaImage,
    items: &[AnnotationItem],
    curve_radius: f32,
) {
    for item in items {
        if item.kind.is_effect() {
            apply_one_effect(acc, None, analysis, item, curve_radius);
        }
    }
}

// ── the global dim / spotlight (DRAGON-329) ─────────────────────────────────────────────

/// The rects that KNOCK OUT the global dim (the dim is punched to full brightness inside their
/// UNION): the dedicated [`AnnotKind::Spotlight`] plus the box-shaped kinds that already read as
/// "look here" markup — [`AnnotKind::Box`], [`AnnotKind::Highlight`], and
/// [`AnnotKind::BoxHighlight`]. The destructive redactions (pixelate / blur) deliberately DON'T
/// knock out — they stay dimmed. Pure — unit-tested.
pub fn knockout_rects(items: &[AnnotationItem]) -> Vec<AnnotRect> {
    items
        .iter()
        .filter_map(|it| match &it.kind {
            AnnotKind::Spotlight { rect }
            | AnnotKind::Box { rect, .. }
            | AnnotKind::Highlight { rect }
            | AnnotKind::BoxHighlight { rect, .. } => Some(*rect),
            // Freehand pen is markup like an arrow — it marks a spot, it doesn't frame a
            // region, so it never punches the dim.
            AnnotKind::Arrow { .. }
            | AnnotKind::Pen { .. }
            // A badge MARKS a spot (like an arrow); it doesn't frame a region, so it never
            // punches the dim either.
            | AnnotKind::Badge { .. }
            // TEXT is a caption, not a region marker — it never punches the dim (DRAGON-354).
            | AnnotKind::Text { .. }
            | AnnotKind::Pixelate { .. }
            | AnnotKind::Blur { .. } => None,
        })
        .collect()
}

/// The per-pixel dim ALPHA (0..1) for `dim` (0..1) under a knockout coverage `cov` (0..1): the
/// dim fades to zero where a knockout fully covers, so `dim × (1 − cov)`. The single source of
/// the dim-alpha law the display shader mirrors. Pure — unit-tested.
pub fn dim_alpha(dim: f32, cov: f32) -> f32 {
    dim.clamp(0.0, 1.0) * (1.0 - cov.clamp(0.0, 1.0))
}

/// Darken `base` toward black by the global dim, EXCEPT inside the knockout rects' union (which
/// show through at full brightness) — the CPU bake of the DRAGON-329 dim/spotlight, the exact
/// mirror of the GPU display's bottom-of-stack dim pass. Each pixel is scaled by
/// `1 − dim_alpha(dim, maxKnockoutCoverage)`; the knockout coverage is the MAX over every rect,
/// rasterized with the SAME [`coverage_mask`] rounded-rect the effects use so the edges match
/// the display. A NON-destructive composite: `dim == 0` (no dim) is a byte-identical no-op, so
/// existing scenes are untouched. Runs at the very BOTTOM of the bake — before every effect —
/// so effects composite over the dimmed content (a highlight/spotlight knockout has already
/// removed the dim inside its rect, so the effect acts on bright content there).
pub fn apply_dim(base: &mut RgbaImage, dim: f32, knockouts: &[AnnotRect], curve_radius: f32) {
    let dim = dim.clamp(0.0, 1.0);
    if dim <= 0.0 {
        return; // no dim → byte-identical no-op
    }
    let (w, h) = base.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    // The MAX knockout coverage per pixel (0..255), rasterized like the effects' masks so the
    // display and bake edges agree. Row-major, aligned with `base.pixels_mut()`.
    let mut cover = vec![0u8; (w as usize).saturating_mul(h as usize)];
    for r in knockouts {
        if let Some(mask) = coverage_mask(w, h, r, curve_radius) {
            for (c, m) in cover.iter_mut().zip(mask.pixels()) {
                *c = (*c).max(m.alpha());
            }
        }
    }
    for (px, &cov) in base.pixels_mut().zip(cover.iter()) {
        let a = dim_alpha(dim, cov as f32 / 255.0);
        if a <= 0.0 {
            continue; // full knockout: untouched (bright)
        }
        let k = 1.0 - a;
        for d in px.0.iter_mut().take(3) {
            *d = (*d as f32 * k).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// How opaque a pen group marked by the in-flight eraser sweep draws (DRAGON-338): a quarter
/// (75% transparent, user-tuned from the original half), so "these are going when you let go"
/// reads unmistakably while the strokes stay just legible.
pub const ERASE_PREVIEW_ALPHA: f32 = 0.25;

/// Straight-alpha RGBA bytes → an iced [`Color`](cosmic::iced::Color).
fn to_iced_color(c: AnnotColor) -> cosmic::iced::Color {
    cosmic::iced::Color::from_rgba8(c[0], c[1], c[2], c[3] as f32 / 255.0)
}

/// Convert model items into the widget's hit-test/chrome/DRAW geometry. `curve_radius` (the
/// shared corner curve, SOURCE px) is stamped onto each item so the canvas draws the SAME
/// rounded corners / soft caps the bake rasterizes — the vector display and the raster bake
/// stay visually consistent.
///
/// `erasing` holds the items the in-flight eraser sweep has MARKED (DRAGON-338): they draw at
/// [`ERASE_PREVIEW_ALPHA`] so the user sees exactly what releasing will delete. Purely a
/// display concern — the model is untouched until the sweep commits.
pub fn widget_items(items: &[AnnotationItem], curve_radius: f32, erasing: &[AnnotId]) -> Vec<Item> {
    // Sequence-badge ordinals are DERIVED (never stored), so they are re-resolved on EVERY view
    // build — a delete, an undo or a redo renumbers the tray with no extra plumbing.
    let badges = badge_numbers(items);
    items
        .iter()
        .map(|it| {
            // Marked-for-erase items preview at ERASE_PREVIEW_ALPHA (never baked, never persisted).
            let stroke_color = if erasing.contains(&it.id) {
                let mut c = to_iced_color(it.color);
                c.a *= ERASE_PREVIEW_ALPHA;
                c
            } else {
                to_iced_color(it.color)
            };
            // The region-effect kinds share Box GEOMETRY (a rect, no stroke → 0 chrome offset)
            // but render through shader passes, not this widget — flagged via `fx` so the
            // canvas skips drawing them while still hit-testing + chroming them.
            let rect_item = |rect: &AnnotRect, fx: FxKind| {
                (ItemKind::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h }, 0.0, None, fx)
            };
            let (kind, stroke_w, fill, fx) = match &it.kind {
                AnnotKind::Box { rect, stroke_w, fill } => (
                    ItemKind::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
                    *stroke_w,
                    fill.map(to_iced_color),
                    FxKind::None,
                ),
                AnnotKind::Arrow { a, b, stroke_w } => (
                    ItemKind::Arrow { ax: a.x, ay: a.y, bx: b.x, by: b.y },
                    *stroke_w,
                    None,
                    FxKind::None,
                ),
                // Freehand pen: its polylines go to the canvas verbatim (SOURCE px), which
                // draws them as pressure-profiled ribbons and hit-tests along the strokes
                // themselves. The per-point speed signal rides along so the canvas can build
                // the SAME width profile the bake does.
                AnnotKind::Pen { paths, pressure, stroke_w } => (
                    ItemKind::Path {
                        paths: paths.iter().map(|p| pen_xy(p)).collect(),
                        pressure: (0..paths.len())
                            .map(|i| pen_pressure(pressure, paths, i).to_vec())
                            .collect(),
                    },
                    *stroke_w,
                    None,
                    FxKind::None,
                ),
                AnnotKind::Highlight { rect } => rect_item(rect, FxKind::Highlight),
                AnnotKind::Pixelate { rect } => rect_item(rect, FxKind::Pixelate),
                AnnotKind::Blur { rect } => rect_item(rect, FxKind::Blur),
                // Spotlight (DRAGON-329): rect geometry + zero chrome offset, flagged so the
                // canvas hit-tests + chromes it but draws nothing (a pure knockout region).
                AnnotKind::Spotlight { rect } => rect_item(rect, FxKind::Spotlight),
                // BoxHighlight rides its own fx flag (the highlight FILL renders via the
                // shader) BUT carries a real `stroke_w` + no vector fill, so the canvas still
                // draws its box OUTLINE on top (DRAGON-333).
                AnnotKind::BoxHighlight { rect, stroke_w } => (
                    ItemKind::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
                    *stroke_w,
                    None,
                    FxKind::BoxHighlight,
                ),
                // A SEQUENCE BADGE hands the canvas its square as a plain Rect (so hit-testing,
                // chrome and resize are the ordinary ones) and rides the `badge` render flag
                // below; `stroke_w` carries the RING weight.
                AnnotKind::Badge { rect, ring_w } => (
                    ItemKind::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
                    *ring_w,
                    None,
                    FxKind::None,
                ),
                // TEXT (DRAGON-354): hands the canvas its box as a plain Rect so hit-testing,
                // chrome and resize are the ordinary ones, and rides the `text` flag below so
                // the canvas draws NO outline (the glyphs come from the raster layer).
                AnnotKind::Text { rect, .. } => (
                    ItemKind::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
                    0.0,
                    None,
                    FxKind::None,
                ),
            };
            let badge = badges.iter().find(|(id, _)| *id == it.id).map(|(_, n)| *n);
            let text = matches!(it.kind, AnnotKind::Text { .. });
            Item { id: it.id.0, kind, stroke_w, color: stroke_color, fill, fx, curve_radius, badge, text }
        })
        .collect()
}

/// The grid (SOURCE px) the text layer's REGION is snapped outward to (DRAGON-362). Typing a
/// character usually grows the caption by less than this, so the region — and therefore the
/// raster's pixel dimensions — hold still across a burst of keystrokes. That matters because
/// `LayerStackPipeline::upsert` only updates a layer's texture IN PLACE while its dimensions
/// are unchanged; a region that moved every keystroke would re-create the texture every
/// keystroke, which is exactly the churn `layers.rs` exists to avoid.
const TEXT_REGION_GRID: f32 = 64.0;

/// How far a glyph's INK may escape its layout box, as a fraction of the type size — the slack
/// [`text_padded_bounds`] adds on every side, on top of the outline weight.
///
/// WHY this number (DRAGON-368): it used to be a flat half-em, chosen as "obviously enough". A
/// half-em on all four sides is a large fraction of a big caption's region, and the region's
/// AREA is what a re-render costs, so the guess was being paid for on every event. Measured
/// instead — rendering Latin, accented, emoji and punctuation-heavy samples in both embedded
/// faces, at a hairline and at the heaviest pencil, and comparing the real ink bounds against
/// the stored box — the worst overhang is 0.172 em (Excalifont, accented capitals, heaviest
/// outline; that figure already includes the outline's own half-width). `text_padded_bounds`
/// adds the FULL stroke width separately, so 0.25 em here leaves better than a 2× margin on the
/// worst case measured while cutting the padded area of a large caption by about a fifth.
///
/// The one case no padding covers is a CJK run: our advance ladder under-measures glyphs that
/// fall through to a system face, so the stored box can be ~5 em narrower than the ink. That is
/// a measurement bug (the `text_shape` ladder), not a padding one — a half-em never covered it
/// either — and it is out of DRAGON-368's scope. Guarded by a unit test that renders real
/// samples and asserts the ink stays inside the padded bound.
pub const TEXT_INK_OVERHANG_EM: f32 = 0.25;

/// The PADDED ink bounds of every non-empty text box (SOURCE px, `(x0, y0, x1, y1)`) — the
/// union [`text_layer_region`] then snaps outward. Split out because the DRAGON-367 reuse fast
/// path needs the un-snapped bound on its own: it is what decides whether an existing raster
/// already covers ALL of the ink (see [`placed_text_region`]).
///
/// `None` when nothing would be drawn (no text items, or every one of them blank).
fn text_padded_bounds(items: &[AnnotationItem]) -> Option<(f32, f32, f32, f32)> {
    let mut acc: Option<(f32, f32, f32, f32)> = None; // (x0, y0, x1, y1)
    for item in items {
        let AnnotKind::Text { rect, text, size_px, stroke_w, .. } = &item.kind else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        // The outline (half of it lies OUTSIDE the glyph edge) plus the measured ink overhang
        // for side bearings, accents and descenders — the box is a layout measure, not a
        // guaranteed ink bound, and clipping a caption's tail would be a visible defect.
        let pad = size_px * TEXT_INK_OVERHANG_EM
            + super::text_annot::text_stroke_width(*stroke_w, *size_px);
        let (x0, y0) = (rect.x - pad, rect.y - pad);
        let (x1, y1) = (rect.x + rect.w + pad, rect.y + rect.h + pad);
        acc = Some(match acc {
            None => (x0, y0, x1, y1),
            Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
        });
    }
    acc
}

/// The region of the picture (SOURCE px) the live text layer must cover: the union of every
/// non-empty text box, padded for the glyph OUTLINE and its measured ink overhang
/// ([`TEXT_INK_OVERHANG_EM`]) and snapped outward to [`TEXT_REGION_GRID`]. `None` when there is
/// no text to draw.
///
/// WHY the layer is a REGION and not the whole picture (DRAGON-362): rendering the text layer
/// costs `O(raster area)` — measured at ~5.6 ms per megapixel, essentially all of it the
/// pixmap allocation and the premultiply→straight-alpha pass, with the glyph drawing itself a
/// rounding error — and the GPU upload is another 4 bytes per pixel. A full-frame layer on a
/// 5120×2880 capture therefore cost ~82 ms and ~59 MB on EVERY keystroke, drag tick and
/// selection change. Sizing the raster to the caption instead makes that cost track the text,
/// not the capture.
///
/// WHY it is NO LONGER clipped to the picture (DRAGON-368) — this is the whole drag fix. Clipping
/// meant a raster near an edge did not contain all of its own ink, and the reuse fast path has to
/// refuse such a raster (moving it inward would reveal the tail that was never drawn). Measured
/// on a 5120×2880 capture at fit zoom, that refusal was not an edge case: a caption at 256px took
/// the fast path on 6.7% of a drag's motion events and one at 512px or 768px on **zero** of them,
/// so every event paid a 29–32 ms re-render. That is exactly the "drag still lags at 512px" the
/// owner reported after DRAGON-367. An unclipped region contains its ink BY CONSTRUCTION, so the
/// fast path holds everywhere — including with the box dragged off the canvas, which DRAGON-368
/// also allows ([`clamp_text_rect_on_canvas`]).
///
/// Nothing draws outside the picture as a result: the layer is a `shader::Shader` sized exactly
/// `dw × dh` (the on-screen picture), and iced scissors every shader primitive to its own widget
/// bounds before calling `Primitive::draw` (`iced_wgpu`'s `lib.rs`: `set_viewport(bounds)` +
/// `set_scissor_rect(bounds ∩ physical_bounds)`), so the part of the quad past the picture is
/// discarded by the GPU. That is why this needs no extra clip uniform, and why the region may
/// carry negative coordinates. Pure — unit-tested.
pub fn text_layer_region(items: &[AnnotationItem], frame: (u32, u32)) -> Option<AnnotRect> {
    if frame.0 == 0 || frame.1 == 0 {
        return None;
    }
    let (x0, y0, x1, y1) = text_padded_bounds(items)?;
    // Snap outward to the grid. NOT clipped to the picture — see the doc above.
    let g = TEXT_REGION_GRID;
    let x0 = (x0 / g).floor() * g;
    let y0 = (y0 / g).floor() * g;
    let x1 = (x1 / g).ceil() * g;
    let y1 = (y1 / g).ceil() * g;
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w > 0.0 && h > 0.0) || !(x0.is_finite() && y0.is_finite()) {
        return None;
    }
    Some(AnnotRect { x: x0, y: y0, w, h })
}

/// Rasterize ONLY the TEXT items (DRAGON-354) into a `pw`×`ph` straight-alpha RGBA covering
/// `region` of the picture (SOURCE px, from [`text_layer_region`]) — the live text LAYER,
/// stacked over the base/effects/covermark at that region's place in the canvas. The other
/// annotation kinds draw as canvas vectors (box/arrow/pen/badge) or fx-shader passes
/// (highlight/pixelate/blur), never here. Returns `None` when nothing is drawn (no text, or
/// every box empty). Shares [`super::text_annot::render_into`] with the bake, so the live
/// layer and the exported pixels are the identical drawing at two resolutions — the region is
/// a pure TRANSLATION of the drawing, so live/bake parity is untouched. The premultiply to
/// straight-alpha mirrors [`rasterize_scene`]'s tail.
fn render_text_layer(
    items: &[AnnotationItem],
    frame: (u32, u32),
    region: AnnotRect,
    pw: u32,
    ph: u32,
) -> Option<RgbaImage> {
    use resvg::tiny_skia as sk;
    if pw == 0 || ph == 0 || frame.0 == 0 || region.w <= 0.0 {
        return None;
    }
    // Raster px per SOURCE px, taken from the region the texture actually maps onto — so the
    // glyphs land exactly where the placed quad says they do, whatever rounding the pixel
    // dimensions took.
    let scale = pw as f32 / region.w;
    let mut pixmap = sk::Pixmap::new(pw, ph)?;
    let mut drew = false;
    for item in items {
        if let AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } = &item.kind {
            if text.trim().is_empty() {
                continue;
            }
            // The SAME layout derivation as the stored geometry (`text_kind_layout`), so the
            // live glyphs always wrap exactly where the box says they do.
            let lay =
                text_kind_layout(text, *size_px, *font, *rect, *constrained, frame.0 as f32);
            super::text_annot::render_into(
                &mut pixmap,
                &lay,
                *font,
                *size_px,
                item.color,
                *stroke_w,
                ((rect.x - region.x) * scale, (rect.y - region.y) * scale),
                scale,
            );
            drew = true;
        }
    }
    if !drew {
        return None;
    }
    let mut rgba = RgbaImage::new(pw, ph);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = ::image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(rgba)
}

// ── DRAGON-367/368: re-placing the text raster instead of re-rendering it ────────────
//
// `render_text_layer` is the single most expensive thing the preview's UPDATE path can do:
// every call builds an SVG document, parses it through `usvg::Tree::from_data` (XML + font
// resolution + text shaping) and replays it through resvg into a freshly allocated pixmap,
// then walks that pixmap once more to demultiply. Its cost is `O(region area)` — measured at
// ~5.6 ms per megapixel — and the region grows with BOTH the caption's extent and its padding,
// so it climbs steeply with the type size: on a 5120×2880 capture at fit zoom one event costs
// 3.3 ms at 64px type, 13.7 ms at 256px, 29.2 ms at 512px and 32.2 ms at 768px. A pointer
// delivers events far faster than that, so past ~256px the update path simply cannot keep up
// and the events queue: the caption freezes, then teleports.
//
// Neither a MOVE nor a RESIZE needs any of it, because the layer is PLACED rather than stretched
// (DRAGON-362 gave `Layer` a `dest` rect) and both gestures leave the DRAWING alone — the glyphs,
// the wrap, the face, the outline and the colour are identical; only the similarity transform
// mapping that drawing onto the picture changes. So the fast path re-uses the raster verbatim
// and moves/scales the region it is placed at: one 16-byte uniform write on the GPU instead of a
// full re-raster, and the layer's persistent texture is never even touched
// (`LayerStackPipeline::upsert` re-uploads on a `seq` change; the `Arc<PixelFrame>` is the same
// one, so there is no upload and no churn — the `layers.rs` flicker-free contract).
//
// This is the LIVE-TRANSFORM PROXY every raster editor uses, and it is worth naming the prior
// art because it also fixes what the proxy is allowed to look like. Photoshop's Free Transform
// and Figma both transform the EXISTING pixels during the gesture and re-render at full quality
// once, on commit. The browser compositors do the same thing under `will-change: transform`:
// Chrome's own guidance is that content WITHOUT that hint "is re-rastered when its transform
// scale changes", and that the hint "effectively means 'please apply the transformation quickly'
// without taking the additional time for rasterization" — with a crisp re-raster arriving on the
// frame after the hint is dropped (developer.chrome.com, "Re-rastering composited layers on scale
// change"). A gesture is exactly that window. So:
//
//   * DURING a gesture the raster is re-used and may be SOFT — a caption scaled up is being
//     magnified from the resolution it was rendered at;
//   * the moment the gesture COMMITS (`annot_gesture_end` → `refresh_text_display`) it is
//     re-rendered exactly, at the new size and the display's own device resolution.
//
// The one asymmetry we impose on the browser model is [`TEXT_PROXY_MIN_SCALE`]: shrinking is
// re-rendered once it passes an octave, because a linear-filtered minification with no mip chain
// starts to shimmer, and a smaller raster is CHEAP to re-render. Magnification has no such
// escape hatch and is the expensive direction, so it rides the proxy all the way to commit.
//
// The two functions below are the whole decision, kept pure so both halves are unit-tested:
// [`text_layer_xform`] answers "is the new scene the old scene, rigidly scaled and moved?" and
// [`placed_text_region`] answers "may this raster be re-used at the transformed place?".

/// How far two per-item offsets may disagree (SOURCE px) and still count as ONE rigid
/// translation. Not zero because the deltas are recovered by subtraction from f32 coordinates
/// that each took their own clamp; a thousandth of a source pixel is far below anything the
/// raster can express (the raster is at most ~2 device px per source px) yet comfortably above
/// f32 subtraction noise at 5K coordinates.
const TEXT_SLIDE_EPS: f32 = 1e-3;

/// How far two derived layout metrics may disagree from the common scale factor, RELATIVE, and
/// still count as one uniform scale (DRAGON-368). The metrics are all `em_fraction × size_px`
/// products recomputed at the new size, so they agree to f32 rounding on a genuine scale; a
/// tenth of a percent is orders of magnitude above that noise and far below any re-wrap, which
/// changes a line's measured width by whole glyphs.
const TEXT_SCALE_EPS: f32 = 1e-3;

/// The smallest accumulated shrink a text raster may be re-used at before it is re-rendered
/// instead (DRAGON-368) — one octave.
///
/// Magnification is left unbounded on purpose (see the module note above): it is the expensive
/// direction to re-render, and a soft caption mid-gesture is the accepted behaviour of every
/// editor that does this. Minification is the opposite on both counts — the layer's sampler is
/// `FilterMode::Linear` with NO mip chain, so past about 2× reduction a stroked glyph starts to
/// drop scanlines and shimmer as it moves, and the re-render that fixes it is `O(area)` on the
/// SMALLER region, i.e. cheap. So a shrink past an octave pays for a fresh raster and starts the
/// next octave from there.
const TEXT_PROXY_MIN_SCALE: f32 = 0.5;

/// The similarity transform carrying the drawing the live text raster holds onto the scene as it
/// now stands: `p ↦ scale · p + (dx, dy)`, in SOURCE px (DRAGON-368).
///
/// `scale == 1.0` is DRAGON-367's pure translation — the common case, and still the only thing a
/// MOVE can produce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextXform {
    /// Uniform scale factor (raster's drawing → current drawing).
    pub scale: f32,
    pub dx: f32,
    pub dy: f32,
}

impl TextXform {
    /// Map a picture rect through the transform. The region the raster is placed at is exactly
    /// its old region mapped this way: the raster holds drawing `D` over region `R`, the scene
    /// now holds `T(D)`, and `T` is a similarity — so `T(D)` sits over `T(R)` and the placed
    /// quad is right by construction, whatever resampling the GPU does inside it.
    fn apply(self, r: AnnotRect) -> AnnotRect {
        AnnotRect {
            x: r.x * self.scale + self.dx,
            y: r.y * self.scale + self.dy,
            w: r.w * self.scale,
            h: r.h * self.scale,
        }
    }
}

/// Everything the live text raster is a function of for ONE text item, MINUS the similarity
/// transform placing it: the derived layout (the glyphs and the wrap they fell into), the face,
/// the size, the outline weight and the colour — precisely the arguments
/// [`super::text_annot::render_into`] draws from. Two snapshots that agree on all of it up to
/// ONE common scale draw the SAME ink at two magnifications, so a scene change that only scales
/// and moves the origins is a similarity transform of the pixels already in the raster.
///
/// The LAYOUT is compared, never the wrap inputs: an auto (click-created) box derives its wrap
/// cap from its own width ([`text_auto_cap`]), so a scale changes the cap even when the caption
/// is nowhere near wide enough for it to bind. (A MOVE no longer can — DRAGON-378 took the box's
/// POSITION out of the cap, so a translated caption's layout is identical by construction rather
/// than by comparison.) Comparing the derived layout keeps that common case on the fast path
/// while still catching the case where the gesture genuinely DOES re-wrap the text. Deriving it
/// is cheap — the advance tables in `text_shape` are cached per thread — and it is the same
/// [`text_kind_layout`] seam the render and the bake use, so the comparison can never disagree
/// with what is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TextRenderSig {
    id: AnnotId,
    /// The box origin (SOURCE px) — mapped by the similarity, never compared directly.
    origin: (f32, f32),
    color: AnnotColor,
    font: super::text_annot::TextFont,
    size_px: f32,
    stroke_w: f32,
    layout: super::text_annot::TextLayout,
}

/// The render signature of ONE item — `None` when it isn't a text item at all.
pub(super) fn text_render_sig(item: &AnnotationItem, frame: (u32, u32)) -> Option<TextRenderSig> {
    let AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } = &item.kind else {
        return None;
    };
    Some(TextRenderSig {
        id: item.id,
        origin: (rect.x, rect.y),
        color: item.color,
        font: *font,
        size_px: *size_px,
        stroke_w: *stroke_w,
        layout: text_kind_layout(text, *size_px, *font, *rect, *constrained, frame.0 as f32),
    })
}

/// Snapshot the render signature of every TEXT item, in scene order.
pub(super) fn text_render_sigs(items: &[AnnotationItem], frame: (u32, u32)) -> Vec<TextRenderSig> {
    items.iter().filter_map(|item| text_render_sig(item, frame)).collect()
}

/// Whether `a` is `b × s` to within [`TEXT_SCALE_EPS`], RELATIVE — the comparison every scaled
/// layout metric takes. Zero-vs-zero agrees (an empty line's width is zero at every size).
fn scales_by(a: f32, b: f32, s: f32) -> bool {
    let want = b * s;
    (a - want).abs() <= TEXT_SCALE_EPS * want.abs().max(1.0)
}

/// `Some(xform)` when `after` is `before` SCALED AND MOVED AS ONE: the same text items, in the
/// same order, whose derived layouts differ by one common factor and whose origins differ by the
/// matching offset, and which agree in everything else that reaches the raster. `None` the moment
/// anything else moved — a re-wrap, a recoloured or restyled box, an item added or removed, or
/// members that scaled/slid by different amounts (which is not a similarity of the layer at all,
/// since the layer holds them together in one texture).
///
/// An empty scene yields `None`: there is no raster to re-use, and the caller must fall through
/// to the normal path so the layer is cleared.
pub(super) fn text_layer_xform(
    before: &[TextRenderSig],
    after: &[TextRenderSig],
) -> Option<TextXform> {
    if before.is_empty() || before.len() != after.len() {
        return None;
    }
    // The scale comes from the type size, which is the ONE input every metric is proportional
    // to. A pure move leaves it at exactly 1.0, so DRAGON-367's translation is this with s == 1.
    let s = if after[0].size_px == before[0].size_px {
        1.0
    } else {
        after[0].size_px / before[0].size_px
    };
    if !(s.is_finite() && s > 0.0) {
        return None;
    }
    // …and the offset from the first item's origin, once the scale is taken out.
    let (dx, dy) = (
        after[0].origin.0 - s * before[0].origin.0,
        after[0].origin.1 - s * before[0].origin.1,
    );
    if !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    let xf = TextXform { scale: s, dx, dy };
    for (b, a) in before.iter().zip(after) {
        if b.id != a.id || b.color != a.color || b.font != a.font {
            return None;
        }
        // The pencil width is a SOURCE-px setting the user chose, not a scaled quantity — but
        // the outline it paints is `em_frac(pencil) × size_px`, so it scales with the type
        // automatically. An unchanged pencil is therefore exactly what a similarity needs.
        if b.stroke_w != a.stroke_w || !scales_by(a.size_px, b.size_px, s) {
            return None;
        }
        // Same glyphs, same wrap: the ink is one drawing at two magnifications.
        if b.layout.lines != a.layout.lines {
            return None;
        }
        // …and every derived metric scaled by the same factor. Cheap belt over the line
        // comparison: it catches a face whose metrics are not linear in the size.
        if !scales_by(a.layout.line_h, b.layout.line_h, s)
            || !scales_by(a.layout.ascent, b.layout.ascent, s)
            || !scales_by(a.layout.box_w, b.layout.box_w, s)
            || !scales_by(a.layout.box_h, b.layout.box_h, s)
        {
            return None;
        }
        // Every member must ride the SAME transform — the layer holds them in one texture, so
        // members that separated are not a similarity of it.
        let (wx, wy) = (s * b.origin.0 + dx, s * b.origin.1 + dy);
        // The tolerance grows with the coordinate: at 5K source coordinates the products above
        // carry more than a thousandth of a pixel of f32 rounding on their own.
        let eps = TEXT_SLIDE_EPS * wx.abs().max(wy.abs()).max(1.0);
        if (a.origin.0 - wx).abs() > eps || (a.origin.1 - wy).abs() > eps {
            return None;
        }
    }
    Some(xf)
}

/// Where an existing text raster may be RE-PLACED after the scene moved by `xform` — `None` when
/// it may not be, and the caller must re-render instead.
///
/// `region` is where the raster currently sits and `padded` the post-gesture padded ink bounds
/// ([`text_padded_bounds`]). Two refusals, and both are about EXACTNESS or quality, not caution:
///
/// * **the placed region must still cover all of the ink.** A raster only ever held the ink that
///   fell inside its own region; any ink outside it was never drawn, and re-placing the raster
///   so that missing area becomes visible would show a blank tail. Since DRAGON-368 stopped
///   clipping the region to the picture ([`text_layer_region`]) a freshly rendered raster always
///   contains its whole padded bound, so this holds by construction on every ordinary gesture —
///   it is the guard that a scene change which slipped past [`text_layer_xform`] cannot silently
///   truncate the caption.
/// * **it must not be shrunk past [`TEXT_PROXY_MIN_SCALE`].** See that constant: minification
///   through a mip-less linear sampler shimmers, and re-rendering smaller is cheap.
///
/// Note there is deliberately NO "stays inside the picture" condition (there was until
/// DRAGON-368, and it is what made the fast path unreachable for a large caption — see
/// [`text_layer_region`]). The GPU scissors the layer to the picture, and text is now allowed to
/// hang off the canvas anyway.
pub(super) fn placed_text_region(
    region: AnnotRect,
    xform: TextXform,
    padded: (f32, f32, f32, f32),
) -> Option<AnnotRect> {
    if !(xform.scale.is_finite() && xform.scale >= TEXT_PROXY_MIN_SCALE)
        || !xform.dx.is_finite()
        || !xform.dy.is_finite()
    {
        return None;
    }
    let placed = xform.apply(region);
    if !(placed.w > 0.0 && placed.h > 0.0) {
        return None;
    }
    let (x0, y0, x1, y1) = padded;
    // A hair of slack: `padded` is recomputed from the moved scene while `placed` is the old
    // region carried through the transform, so at 5K coordinates the two disagree in the last
    // f32 place. Refusing on that would drop the fast path for no visible reason.
    let slack = TEXT_SLIDE_EPS * placed.w.max(placed.h).max(1.0);
    if x0 < placed.x - slack
        || y0 < placed.y - slack
        || x1 > placed.x + placed.w + slack
        || y1 > placed.y + placed.h + slack
    {
        return None;
    }
    Some(placed)
}

/// Whether ONE text box's raster already on screen IS what re-rendering it right now would
/// produce (DRAGON-376) — i.e. whether [`App::refresh_text_display`] may re-use it verbatim,
/// texture and all, instead of drawing it again.
///
/// `held` is the (raster scale, signature) of the layer currently displayed for that box, or
/// `None` when it has none (a new box, a blank one that just gained ink, or a render that
/// produced no pixels — a fresh attempt must always be allowed, or a transient failure would
/// stick forever). `want`/`scale` are the signature and resolution a render would use now.
///
/// The signature is the WHOLE raster input ([`text_render_sig`] — the derived layout, origin,
/// face, size, outline weight and colour, which is exactly what
/// [`super::text_annot::render_into`] draws from), so an equal signature at an equal scale means
/// a byte-identical bitmap. Editor chrome — caret index, selection anchor, blink phase — is not
/// in it because it never reaches the renderer: the canvas draws the caret and the selection wash
/// as vectors over the layer. Pure — unit-tested.
fn text_raster_is_current(
    held: Option<(f32, &TextRenderSig)>,
    want: &TextRenderSig,
    scale: f32,
) -> bool {
    held == Some((scale, want))
}

// ── app-side gesture + scene handlers ────────────────────────────────────────────────

impl App {
    /// Push a CUSTOM color onto the last-5 recents queue via [`rotate_recent_color`].
    pub(super) fn push_recent_color(&mut self, c: AnnotColor) {
        rotate_recent_color(&mut self.annot_recent_colors, c);
    }

    /// Begin drawing a new shape of `tool` at image point `(x, y)`.
    pub(super) fn annot_draw_begin(&mut self, id: window::Id, tool: Tool, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let color = p.edit.annot_color.unwrap_or_else(default_annot_color);
        // Presets/preferences are POINTS; the concrete shape geometry is SOURCE px (DRAGON-383).
        // Scale each seed once here, at the boundary where a preset becomes annotation geometry.
        let scale = p.source_scale;
        let stroke_w = points_to_source_px(p.edit.stroke(), scale);
        // Clamp the start point inside the ANNOTATABLE canvas (source ∪ crop; DRAGON-389) — an
        // over-crop extension is drawable too, so this is the union bounds, not the source frame.
        let bounds = p.edit.annot_bounds();
        let (x, y) = (
            x.clamp(bounds.x, bounds.x + bounds.w),
            y.clamp(bounds.y, bounds.y + bounds.h),
        );
        // The ERASER draws nothing (DRAGON-338): the press opens a sweep instead, snapshots the
        // scene for the single undo entry the release will push, and marks whatever it lands on
        // (so a plain CLICK on a stroke already marks it).
        if tool == Tool::Eraser {
            p.edit.sel.clear();
            p.edit.annot_snapshot = Some(p.edit.annotations.clone());
            p.edit.erase_marks.clear();
            p.edit.gesture = Some(AnnotGesture::Erase { last: (x, y) });
            mark_erased(&mut p.edit, (x, y), (x, y));
            return Task::none();
        }
        let id = p.edit.next_annot_id();
        let kind = match tool {
            Tool::Arrow => AnnotKind::Arrow {
                a: AnnotPoint { x, y },
                b: AnnotPoint { x, y },
                stroke_w,
            },
            Tool::Rect => AnnotKind::Box {
                rect: AnnotRect { x, y, w: 0.0, h: 0.0 },
                stroke_w,
                fill: None,
            },
            Tool::Highlight => AnnotKind::Highlight { rect: AnnotRect { x, y, w: 0.0, h: 0.0 } },
            Tool::BoxHighlight => AnnotKind::BoxHighlight {
                rect: AnnotRect { x, y, w: 0.0, h: 0.0 },
                stroke_w,
            },
            Tool::Spotlight => AnnotKind::Spotlight { rect: AnnotRect { x, y, w: 0.0, h: 0.0 } },
            // A badge is PLACED, not drawn (it is a numbered marker, not a region): the press
            // drops a finished square CENTRED on it, at the size the last badge in this editor
            // was placed or resized to. There is no drag-to-size — the `New` arm of
            // `annot_gesture_to` leaves a badge alone, so a click and a click-drag land the
            // same badge in the same place. Its ring rides the shared line weight.
            Tool::Badge => AnnotKind::Badge {
                rect: badge_placement_in_bounds(
                    (x, y),
                    points_to_source_px(p.edit.badge_size(), scale),
                    bounds,
                    // `kind_draw_margin` for a badge: half the ring.
                    stroke_w.max(0.0) / 2.0,
                ),
                ring_w: stroke_w,
            },
            Tool::Pixelate => AnnotKind::Pixelate { rect: AnnotRect { x, y, w: 0.0, h: 0.0 } },
            Tool::Blur => AnnotKind::Blur { rect: AnnotRect { x, y, w: 0.0, h: 0.0 } },
            // One stroke opens as a one-point polyline — already a valid DOT, so a press with
            // no drag at all still inks (DRAGON-342). The drag appends to the RAW trail on
            // `EditState::pen_raw` and re-fits the smoothed curve into this path every sample.
            Tool::Pen => AnnotKind::Pen {
                paths: vec![vec![AnnotPoint { x, y }]],
                pressure: vec![Vec::new()],
                stroke_w,
            },
            // TEXT (DRAGON-354): a press drops an EMPTY auto box at the point (the editor opens
            // on release). A drag re-sizes it into a fixed-width box in `annot_gesture_to`; a
            // bare click leaves it auto. Either way the box hugs the (empty) caret line for now.
            Tool::Text => reflow_text_in_bounds(
                "",
                points_to_source_px(p.edit.text_size(), scale),
                p.edit.annot_text_font,
                AnnotRect { x, y, w: 0.0, h: 0.0 },
                false,
                // DRAGON-358: a new text box captures the active line width, exactly like a box
                // captures it as its stroke — so the width group styles text at creation too.
                stroke_w,
                bounds,
            ),
            // No non-creating tool ever reaches here: the eraser is handled above, the POINTER
            // (DRAGON-341) never emits a `DrawBegin` at all (its empty-canvas drag is a rubber
            // band, not a draw), and the HAND (DRAGON-392) hands every press to the ZoomPan.
            // Defensive.
            Tool::Eraser | Tool::Pointer | Tool::Hand => return Task::none(),
        };
        // A placed badge REMEMBERS its side — the SETTLED one, so a badge the picture clamped
        // down doesn't make every later click try (and fail) at the bigger size again. The
        // document's working copy takes it now; the PERSISTED one is written after the borrow
        // ends (see `remember_badge_size`), so future editors spawn markers at it too.
        let mut remembered_badge = None;
        if let AnnotKind::Badge { rect, .. } = &kind {
            // The settled side is SOURCE px; the remembered/persisted default is POINTS (DRAGON-383).
            let side_pt = source_px_to_points(rect.w, scale);
            p.edit.annot_badge_size = side_pt;
            remembered_badge = Some(side_pt);
        }
        p.edit.annot_snapshot = Some(p.edit.annotations.clone());
        // The freehand pen's RAW trail (DRAGON-342): the model always holds the SMOOTHED curve,
        // so the un-smoothed samples live here for the length of the gesture and nowhere else.
        p.edit.pen_raw = if tool == Tool::Pen { vec![AnnotPoint { x, y }] } else { Vec::new() };
        // A drawn shape becomes the selection so it is immediately editable — EXCEPT freehand
        // ink, which must just land (DRAGON-341): pen selection visuals belong to pointer mode
        // alone, and a dashed bbox snapping around every stroke you draw is pure noise.
        let selects = kind_selects_on_create(&kind);
        p.edit.annotations.push(AnnotationItem { id, color, kind });
        if selects {
            p.edit.sel.set_one(id);
        }
        p.edit.gesture = Some(AnnotGesture::New { press: (x, y), id });
        // Holistic dim rule: with a spotlight now on the canvas, make sure the frame is dimmed so
        // it reads while you draw it (own undo entry; undo removes the spotlight, then the dim).
        p.edit.ensure_dim_for_spotlights();
        if let Some(side) = remembered_badge {
            self.remember_badge_size(side);
        }
        // The GPU effects shader re-renders from the model on the next view build (DRAGON-330) —
        // no async raster to kick; a new effect item shows on the very next frame.
        Task::none()
    }

    /// Arm `tool` as the active annotation tool — the shared body behind BOTH the tray button
    /// (`PreviewMsg::ToolPressed`) and the hotkeys (`PreviewMsg::SelectTool`).
    ///
    /// Leaving POINTER mode DROPS every pen group from the selection (DRAGON-341): pen selection
    /// exists only under the pointer, so the state is pruned rather than the chrome hidden —
    /// otherwise a ghost member would still ride along in a group move or delete.
    pub(super) fn select_annot_tool(&mut self, id: window::Id, tool: Tool) {
        self.set_annot_tool(id, Some(tool));
    }

    /// [`Self::select_annot_tool`]'s body, over an OPTIONAL tool — the ONE funnel every change to
    /// the armed tool goes through, arming and DISARMING alike (DRAGON-392 correction: entering a
    /// crop session disarms the tray, and leaving it re-arms whatever was held). Routing the
    /// disarm through here rather than writing `edit.tool` directly is what keeps the text-edit
    /// settle, the pen-selection drop and the DRAGON-369 slot cursor behaving exactly as they do
    /// for an ordinary tool change.
    ///
    /// `None` is the NEUTRAL state: nothing armed, no conversion, and (like every non-pointer
    /// tool) no pen selection kept.
    pub(super) fn set_annot_tool(&mut self, id: window::Id, tool: Option<Tool>) {
        // Arming ANY tool settles an in-flight text edit first (DRAGON-354): the box you were
        // typing into commits (or, if empty, vanishes) before the new mode takes over. Disarming
        // settles it too — which is also what stops a crop session ever coexisting with a live
        // text edit, an ambiguity Enter would otherwise have to resolve.
        let _ = self.settle_text_edit(id);
        // If a box-family annotation (Box Outline / Highlight / Box Highlight) is selected
        // and the user picks a DIFFERENT one of those three tools, CONVERT the selected
        // item in place (real-time, one undo entry) rather than only arming the tool for
        // the next draw. No-op for every other selection/tool combination — and for a disarm,
        // which picks nothing to convert TO.
        if let Some(tool) = tool {
            self.convert_selected_annotation_kind(id, tool);
        }
        // Only ever SETS the tool — clicking/hotkeying the active tool is a no-op (no
        // re-click-to-neutral). Persist so the next preview opens with it.
        if let Some(p) = self.preview_for_mut(id) {
            p.edit.tool = tool;
            // DRAGON-369: arming a member MOVES its slot's cycle cursor, whatever the route —
            // hotkey, cycle key or tray click. This one line is why the keyboard and the mouse
            // can never disagree about "the slot's current member". A DISARM leaves every cursor
            // where it was, so a crop round-trip can't change what the next M/U press arms.
            if let Some(slot) = tool.and_then(super::chrome::slot_for_tool) {
                p.edit.slot_cursor.insert(slot, tool.expect("slot implies a tool"));
            }
            // Pen groups are selectable ONLY under the pointer, so arming anything else lets
            // them go — the visible selection and the real one never disagree.
            if !tool.is_some_and(Tool::is_pointer) {
                p.edit.drop_pen_selection();
            }
        }
        self.annot_tool = tool;
        self.save_state();
    }

    /// Press a tool SLOT's cycle key (DRAGON-369): arm the slot's current member, or advance to
    /// the next when the slot is already armed. Membership, order and the arm-then-advance rule
    /// all live in `chrome` beside the tray that declares them; this only resolves the target
    /// and arms it through the ordinary [`Self::select_annot_tool`] path, so a cycle press is
    /// indistinguishable from clicking that tray button (undo, persistence, text-edit settling
    /// and the cursor update all included). A slot with no members is a no-op.
    pub(super) fn cycle_tool_slot(&mut self, id: window::Id, slot: crate::shortcuts::Action) {
        let members = super::chrome::slot_tools(slot);
        let (armed, cursor) = match self.preview_for(id) {
            Some(p) => (p.edit.tool, p.edit.slot_cursor.get(&slot).copied()),
            None => return,
        };
        if let Some(tool) = super::chrome::next_slot_tool(&members, armed, cursor) {
            self.select_annot_tool(id, tool);
        }
    }

    /// Spawn a PRE-PLACED item of `tool` in the middle of the picture (DRAGON-339) — what a
    /// DOUBLE-CLICK on the tool's action-tray button does, so an item can be added without
    /// dragging one out. Geometry comes from [`spawn_placement_rect`] (the 200×100 spawn box or
    /// 80% of the image per axis, whichever fits, inset for the stroke — but the REMEMBERED
    /// side for a step marker, sized by the very helper click-to-place uses); appearance from
    /// the SAME current color/stroke a dragged shape would get. The new item lands on TOP of
    /// the z-stack and becomes the selection, as ONE undo entry in the shared history (so it is
    /// undoable and counts toward `EditState::dirty()`'s bake gate exactly like a drawn one).
    ///
    /// Returns `false` (changing nothing) when there is no preview, the tool has no pre-placeable
    /// form ([`spawn_kind`] → `None`, e.g. a freehand tool), or the frame is too small for a
    /// non-degenerate item — the same degeneracy rule a discarded drag uses.
    pub(super) fn spawn_annotation(&mut self, id: window::Id, tool: Tool) -> bool {
        let Some(p) = self.preview_for_mut(id) else {
            return false;
        };
        // Presets/preferences are POINTS; concrete geometry is SOURCE px (DRAGON-383).
        let scale = p.source_scale;
        let stroke_w = points_to_source_px(p.edit.stroke(), scale);
        // The margin is kind-dependent (an arrow's caps overhang more than a box's outline), so
        // measure it on a probe of the kind itself at the nominal size.
        let probe = AnnotRect { x: 0.0, y: 0.0, w: SPAWN_W, h: SPAWN_H };
        let Some(margin) = spawn_kind(tool, probe, stroke_w).as_ref().map(kind_draw_margin) else {
            return false;
        };
        // A step marker is sized by the REMEMBERED side through the click-to-place helper;
        // every other tool takes the shared spawn box (see `spawn_placement_rect`).
        let rect = spawn_placement_in_bounds(tool, p.edit.annot_bounds(), margin, points_to_source_px(p.edit.badge_size(), scale));
        let Some(kind) = spawn_kind(tool, rect, stroke_w) else {
            return false;
        };
        let id = p.edit.next_annot_id();
        let color = p.edit.annot_color.unwrap_or_else(default_annot_color);
        let item = AnnotationItem { id, color, kind };
        if is_degenerate(&item) {
            return false;
        }
        // A badge spawned this way counts as "the last one placed" too, so a later click-place
        // matches what the double-click just dropped (it can differ from the remembered side:
        // a small picture clamps it down). Persisted after the borrow ends, as everywhere.
        let mut remembered_badge = None;
        if let AnnotKind::Badge { rect, .. } = &item.kind {
            // Settled side SOURCE px → remembered/persisted default POINTS (DRAGON-383).
            let side_pt = source_px_to_points(rect.w, scale);
            p.edit.annot_badge_size = side_pt;
            remembered_badge = Some(side_pt);
        }
        let prev = p.edit.annotations.clone();
        p.edit.annotations.push(item);
        p.edit.sel.set_one(id);
        p.edit.annot_menu = None;
        p.edit.push_annotations(prev);
        // Holistic dim rule: a spawned spotlight needs the frame dimmed to read (own undo entry).
        p.edit.ensure_dim_for_spotlights();
        if let Some(side) = remembered_badge {
            self.remember_badge_size(side);
        }
        true
    }

    /// Remember `side` (logical POINTS, DRAGON-383) as the size the NEXT sequence badge is born
    /// at, PERSISTING it so future editors — a new capture process, a later launch, a
    /// DIFFERENT-scale display — spawn markers at the same visual size (the mirror of
    /// [`Self::apply_annot_stroke_w`]'s persist step). The caller has already brought the settled
    /// SOURCE-px side back to points ([`source_px_to_points`]).
    ///
    /// The caller has already written the CURRENT document's working copy
    /// (`EditState::annot_badge_size`); this is the app-wide one. Only the settled, non-degenerate
    /// side is taken, and an unchanged size writes nothing — placing ten identical markers must
    /// not mean ten config writes. With two documents open the working copies may briefly
    /// disagree; last write wins on disk, by design (no cross-document sync).
    pub(super) fn remember_badge_size(&mut self, side: f32) {
        if side <= 0.0 || self.annot_badge_size == side {
            return;
        }
        self.annot_badge_size = side;
        self.save_state();
    }

    /// Recolor the currently-SELECTED colorable annotation(s) to `color`, pushing ONE
    /// [`super::edit::EditOp::Annotations`] undo snapshot — unless a text-edit session is
    /// active, whose settle owns the single snapshot (the change folds into it). No-op (no
    /// snapshot) when nothing is selected, the selection isn't colorable (pixelate/blur), or
    /// the color is unchanged. Iterates the selection so it already extends to multi-select.
    /// The caller sets `annot_color` separately; the view redraws the recolored item
    /// automatically.
    pub(super) fn recolor_selected_annotation(&mut self, id: window::Id, color: AnnotColor) {
        let Some(p) = self.preview_for_mut(id) else {
            return;
        };
        if p.edit.sel.is_empty() {
            return;
        }
        // Change is needed only if a SELECTED, COLORABLE item is actually a different color.
        let needed = p
            .edit
            .annotations
            .iter()
            .any(|it| p.edit.sel.contains(it.id) && it.kind.is_colorable() && it.color != color);
        if !needed {
            return;
        }
        let prev = p.edit.annotations.clone();
        for it in p.edit.annotations.iter_mut() {
            if p.edit.sel.contains(it.id) && it.kind.is_colorable() {
                it.color = color;
            }
        }
        // Same mid-edit gate as the width restyle (and [`Self::apply_text_style`]): during an
        // active text-edit session the SETTLE owns the single undo snapshot, so a recolor folds
        // into it instead of pushing an out-of-order entry of its own.
        if p.edit.text_edit.is_none() {
            p.edit.push_annotations(prev);
        }
    }

    /// Re-stroke the currently-SELECTED box/arrow to `stroke_w` (SOURCE px), pushing ONE
    /// [`super::edit::EditOp::Annotations`] undo snapshot — the width mirror of
    /// [`Self::recolor_selected_annotation`], with the same mid-edit fold: during an active
    /// text-edit session the settle owns the single snapshot. No-op (no snapshot) when nothing
    /// is selected, the selection has no stroke (highlight / pixelate / blur), or the width is
    /// unchanged.
    pub(super) fn restroke_selected_annotation(&mut self, id: window::Id, stroke_w: f32) {
        let Some(p) = self.preview_for_mut(id) else {
            return;
        };
        if p.edit.sel.is_empty() {
            return;
        }
        // Only a SELECTED, STROKED item (box / arrow / pen / badge / text) whose width actually
        // differs needs it. Text carries its width as the glyph OUTLINE weight (DRAGON-358).
        let needed = p.edit.annotations.iter().any(|it| {
            p.edit.sel.contains(it.id)
                && matches!(&it.kind, AnnotKind::Box { stroke_w: w, .. } | AnnotKind::Arrow { stroke_w: w, .. } | AnnotKind::BoxHighlight { stroke_w: w, .. } | AnnotKind::Pen { stroke_w: w, .. } | AnnotKind::Badge { ring_w: w, .. } | AnnotKind::Text { stroke_w: w, .. } if *w != stroke_w)
        });
        if !needed {
            return;
        }
        let prev = p.edit.annotations.clone();
        for it in p.edit.annotations.iter_mut() {
            if p.edit.sel.contains(it.id) {
                match &mut it.kind {
                    AnnotKind::Box { stroke_w: w, .. }
                    | AnnotKind::Arrow { stroke_w: w, .. }
                    // BoxHighlight's OUTLINE stroke re-widths like a box (DRAGON-333).
                    | AnnotKind::BoxHighlight { stroke_w: w, .. }
                    // A pen group re-widths as a whole (DRAGON-338) — 2/4/6px, same presets.
                    | AnnotKind::Pen { stroke_w: w, .. }
                    // A badge's OUTER RING is the line weight, by the ticket's definition
                    // (DRAGON-340), so it re-strokes with everything else.
                    | AnnotKind::Badge { ring_w: w, .. }
                    // Text re-widths its glyph OUTLINE (DRAGON-358) — the width group mirrors the
                    // color flow, so picking a width restyles a selected text box (one undo entry).
                    // No reflow needed: the outline is metrics-neutral, so the box geometry is
                    // unchanged and the raster refresh alone shows the new weight.
                    | AnnotKind::Text { stroke_w: w, .. } => {
                        *w = stroke_w;
                    }
                    // Effects (highlight / pixelate / blur) + spotlight carry no stroke — leave
                    // untouched.
                    AnnotKind::Highlight { .. }
                    | AnnotKind::Pixelate { .. }
                    | AnnotKind::Blur { .. }
                    | AnnotKind::Spotlight { .. } => {}
                }
            }
        }
        // A restyle on a merely-SELECTED item is its own undo entry; during an active text-edit
        // session the SETTLE owns the single snapshot (the pre-edit scene already covers this
        // change), so pushing here would add an out-of-order duplicate — same gate as
        // [`Self::apply_text_style`].
        if p.edit.text_edit.is_none() {
            p.edit.push_annotations(prev);
        }
    }

    /// When a rect annotation (Box Outline / Highlight / Box Highlight / Pixelate / Blur) is
    /// SELECTED and the user picks a DIFFERENT one of those tools, convert the selected item to
    /// that kind IN PLACE (real-time), pushing ONE [`super::edit::EditOp::Annotations`] undo
    /// snapshot. No-op (no snapshot) when nothing is selected, the selection isn't a rect kind,
    /// the tool isn't a rect kind, or the kind is unchanged — so a normal tool pick just arms it.
    pub(super) fn convert_selected_annotation_kind(&mut self, id: window::Id, tool: Tool) {
        let Some(p) = self.preview_for_mut(id) else {
            return;
        };
        let Some(annot) = p.edit.selected() else {
            return;
        };
        // The stroke seeded onto a converted-in arrow is SOURCE px (DRAGON-383).
        let default_stroke = points_to_source_px(p.edit.stroke(), p.source_scale);
        let Some(idx) = p.edit.annotations.iter().position(|it| it.id == annot) else {
            return;
        };
        let Some(new_kind) =
            converted_rect_kind(&p.edit.annotations[idx].kind, tool, default_stroke)
        else {
            return;
        };
        let prev = p.edit.annotations.clone();
        p.edit.annotations[idx].kind = new_kind;
        p.edit.push_annotations(prev);
        // Converting a box INTO a spotlight must also ensure the frame is dimmed (holistic rule).
        p.edit.ensure_dim_for_spotlights();
    }

    /// Set the current annotation stroke width, re-stroke the SELECTED box/arrow to match,
    /// and persist — the shared body behind the width toggle group ([`PreviewMsg::SetAnnotStrokeW`])
    /// and the `L` cycle. Box/arrow redraw as vectors on the next view build, so no raster
    /// refresh is owed (effects carry no stroke).
    pub(super) fn apply_annot_stroke_w(&mut self, id: window::Id, w: f32) {
        // `w` is a preset in POINTS (the flyout / the `L` cycle). The WORKING default and the
        // persisted preference stay POINTS (they match the ladder); only the value re-stroked
        // ONTO the selected shape is scaled to this document's SOURCE px (DRAGON-383).
        let scale = self.preview_for(id).map(|p| p.source_scale).unwrap_or(1.0);
        if let Some(p) = self.preview_for_mut(id) {
            p.edit.annot_stroke_w = w;
        }
        // Picking a width also re-strokes the SELECTED box/arrow immediately (one undo entry).
        self.restroke_selected_annotation(id, points_to_source_px(w, scale));
        // Persist so the next preview opens with this width.
        self.annot_stroke_w = w;
        self.save_state();
    }

    /// Begin manipulating the selection (`grab` from a handle / body).
    ///
    /// A MOVE with more than one item selected (DRAGON-341) opens a group gesture
    /// ([`AnnotGesture::MoveMany`]) that drags every selected item by ONE shared delta, clamped
    /// once on the selection's union bounds so the arrangement never distorts against an image
    /// edge. A RESIZE grab on a multi-selection opens the group SCALE gesture
    /// ([`AnnotGesture::ScaleMany`], DRAGON-388): the handles wear the union's group box, and a
    /// corner/edge drag scales every member in unison. A single selection stays on the historical
    /// one-item [`AnnotGesture::Edit`] path — its resize handles ride the item itself, so the
    /// whole `Grab` machinery is untouched there.
    pub(super) fn annot_grab_begin(&mut self, id: window::Id, grab: Grab, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        if p.edit.sel.len() > 1 && grab != Grab::Move {
            // A RESIZE grab on a multi-selection is a GROUP SCALE (DRAGON-388): the handles wear
            // the union's group box, not the primary, so any corner/edge drag scales the whole
            // set in unison. (`Grab::Move` falls through to the group MOVE below; arrow-endpoint
            // grabs never reach here — the group box shows no endpoint nodes.)
            let originals: Vec<(AnnotId, AnnotKind)> = p
                .edit
                .annotations
                .iter()
                .filter(|it| p.edit.sel.contains(it.id))
                .map(|it| (it.id, it.kind.clone()))
                .collect();
            let Some(bounds) = group_drawn_bounds(originals.iter().map(|(_, k)| k)) else {
                return Task::none();
            };
            p.edit.annot_snapshot = Some(p.edit.annotations.clone());
            p.edit.gesture =
                Some(AnnotGesture::ScaleMany { press: (x, y), originals, bounds, grab });
            return Task::none();
        }
        if grab == Grab::Move && p.edit.sel.len() > 1 {
            let originals: Vec<(AnnotId, AnnotKind)> = p
                .edit
                .annotations
                .iter()
                .filter(|it| p.edit.sel.contains(it.id))
                .map(|it| (it.id, it.kind.clone()))
                .collect();
            let Some(bounds) = group_drawn_bounds(originals.iter().map(|(_, k)| k)) else {
                return Task::none();
            };
            p.edit.annot_snapshot = Some(p.edit.annotations.clone());
            p.edit.gesture = Some(AnnotGesture::MoveMany { press: (x, y), originals, bounds });
            return Task::none();
        }
        let Some(annot) = p.edit.selected() else {
            return Task::none();
        };
        let Some(item) = p.edit.annotations.iter().find(|it| it.id == annot) else {
            return Task::none();
        };
        p.edit.annot_snapshot = Some(p.edit.annotations.clone());
        p.edit.gesture = Some(AnnotGesture::Edit {
            press: (x, y),
            id: annot,
            grab,
            original: item.kind.clone(),
        });
        Task::none()
    }

    /// Live drag update (image point). Updates the model geometry; box/arrow redraw as vector
    /// geometry on the view rebuild, while an effect (highlight/pixelate/blur) being drawn or
    /// resized re-rasters its display layer LIVE (coalesced) so the redaction tracks the drag.
    pub(super) fn annot_gesture_to(
        &mut self,
        id: window::Id,
        x: f32,
        y: f32,
        scale_type: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(gesture) = p.edit.gesture.clone() else {
            return Task::none();
        };
        // Clamp all gesture geometry to the ANNOTATABLE canvas (source ∪ crop; DRAGON-389),
        // zoom-independent, in source px — so a shape can be drawn / moved over the crop extension.
        let canvas = p.edit.annot_bounds();
        // Whether this gesture touches a TEXT box (item 3) — read now, since the annotation's
        // KIND can't change during a drag, and the `match` below moves `gesture`.
        let touches_text = {
            let is_text = |id: AnnotId| {
                p.edit
                    .annotations
                    .iter()
                    .any(|it| it.id == id && matches!(it.kind, AnnotKind::Text { .. }))
            };
            match &gesture {
                AnnotGesture::New { id, .. } | AnnotGesture::Edit { id, .. } => is_text(*id),
                AnnotGesture::MoveMany { originals, .. }
                | AnnotGesture::ScaleMany { originals, .. } => {
                    originals.iter().any(|(id, _)| is_text(*id))
                }
                AnnotGesture::Erase { .. } => false,
            }
        };
        // DRAGON-367/368: the text raster's render signature BEFORE this event, so the tail can
        // ask whether the event only moved/scaled the drawing (which needs no re-render — see
        // [`text_layer_xform`]). Only paid on gestures that touch text; deriving it is a
        // cached-metrics layout pass, orders of magnitude below the resvg render it avoids.
        let text_before =
            touches_text.then(|| text_render_sigs(&p.edit.annotations, p.edit.frame));
        match gesture {
            AnnotGesture::New { press, id } => {
                // The pen's raw trail is a SIBLING field of the item vector — bound up front so
                // the freehand arm below can read/extend it while it holds the item.
                let raw = &mut p.edit.pen_raw;
                if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    // Clamp on the DRAWN extent so a shape drawn to the edge doesn't spill its
                    // outline/cap past it.
                    let m = kind_draw_margin(&item.kind);
                    let clx = |v: f32| v.clamp(canvas.x + m, (canvas.x + canvas.w - m).max(canvas.x + m));
                    let cly = |v: f32| v.clamp(canvas.y + m, (canvas.y + canvas.h - m).max(canvas.y + m));
                    let pr = (clx(press.0), cly(press.1));
                    let cur = (clx(x), cly(y));
                    match &mut item.kind {
                        AnnotKind::Box { rect, .. }
                        | AnnotKind::Highlight { rect }
                        | AnnotKind::BoxHighlight { rect, .. }
                        | AnnotKind::Spotlight { rect }
                        | AnnotKind::Pixelate { rect }
                        | AnnotKind::Blur { rect } => {
                            *rect = AnnotRect::from_points(pr, cur);
                        }
                        AnnotKind::Arrow { b, .. } => {
                            *b = AnnotPoint { x: cur.0, y: cur.1 };
                        }
                        // A badge is PLACED, not drawn: `annot_draw_begin` already dropped the
                        // finished square on the press point, so the drag changes NOTHING —
                        // click and click-drag are the same gesture. (Resizing an existing
                        // badge is the `Edit` gesture below, which keeps it 1:1 via
                        // `square_for_grab`.) Deliberately not a `_` arm: a new rect kind must
                        // still choose its drag behaviour explicitly.
                        AnnotKind::Badge { .. } => {}
                        // TEXT (DRAGON-354): a real drag turns the auto box into a FIXED-width
                        // (constrained) box the text will wrap within — the box you see dragged
                        // out is the wrap frame. The height snaps to the content at
                        // `annot_gesture_end` (empty for now). A bare click never reaches here,
                        // so it stays an auto box.
                        AnnotKind::Text { rect, constrained, .. } => {
                            *rect = AnnotRect::from_points(pr, cur);
                            *constrained = true;
                        }
                        // Freehand: APPEND to the RAW trail, but only once the pointer has
                        // travelled PEN_MIN_STEP — a slow drag must not pile up coincident
                        // vertices, and the gap between kept samples IS the speed proxy the
                        // pseudo-pressure rides. Then RE-FIT the beautified stroke into the
                        // model (DRAGON-342): the smoothing pipeline is causal + linear, so
                        // this is a few microseconds and the settled ink never moves — what
                        // you watch being drawn is exactly what commit keeps.
                        AnnotKind::Pen { paths, pressure, stroke_w } => {
                            let far = raw.last().is_none_or(|l| {
                                (cur.0 - l.x).hypot(cur.1 - l.y) >= PEN_MIN_STEP
                            });
                            if far {
                                raw.push(AnnotPoint { x: cur.0, y: cur.1 });
                                let trail = pen_xy(raw);
                                let fit = crate::pen_stroke::smooth_path(&trail, *stroke_w);
                                let press = crate::pen_stroke::pressure_along(&trail, &fit);
                                // The spline can bulge a hair outside its controls; keep every
                                // stored point inside the picture like the raw samples are.
                                let pts: Vec<AnnotPoint> = fit
                                    .iter()
                                    .map(|p| AnnotPoint {
                                        x: clx(p.0),
                                        y: cly(p.1),
                                    })
                                    .collect();
                                *paths = vec![pts];
                                *pressure = vec![press];
                            }
                        }
                    }
                }
            }
            AnnotGesture::Edit { press, id, grab, original } => {
                if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    item.kind = edited_kind_in_bounds(&original, grab, press, (x, y), canvas, scale_type);
                }
            }
            // A group move (DRAGON-341): ONE delta, clamped ONCE on the union bounds, applied
            // verbatim to every member — so the selection travels as a rigid arrangement.
            AnnotGesture::MoveMany { press, ref originals, bounds } => {
                // Clamp the shared delta so the selection's union stays inside the annotatable
                // canvas — shift the union into bounds-origin space, since the canvas may have a
                // NEGATIVE origin (DRAGON-389); the delta itself is translation-invariant.
                let union0 = AnnotRect { x: bounds.x - canvas.x, y: bounds.y - canvas.y, ..bounds };
                let (dx, dy) =
                    group_move_delta(union0, (canvas.w, canvas.h), (x - press.0, y - press.1));
                for (id, original) in originals {
                    if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == *id) {
                        item.kind = translated_kind(original, dx, dy);
                    }
                }
            }
            // A group SCALE (DRAGON-388): ONE uniform factor about the union's fixed anchor,
            // clamped ONCE so no member collapses, applied to every member — so the selection
            // scales as a rigid similarity, relative layout and overlaps intact.
            AnnotGesture::ScaleMany { press, ref originals, bounds, grab } => {
                let (dx, dy) = (x - press.0, y - press.1);
                let anchor = group_scale_anchor(bounds, grab);
                // Clamp in bounds-origin space (the annotatable canvas may have a NEGATIVE
                // origin, DRAGON-389), mirroring `MoveMany` above: the factor itself is
                // translation-invariant, the containment check is not.
                let union0 = AnnotRect { x: bounds.x - canvas.x, y: bounds.y - canvas.y, ..bounds };
                let anchor0 = (anchor.0 - canvas.x, anchor.1 - canvas.y);
                let k = clamp_group_scale(
                    group_scale_factor(bounds, grab, dx, dy),
                    union0,
                    anchor0,
                    (canvas.w, canvas.h),
                    originals.iter().map(|(_, k)| k),
                );
                for (id, original) in originals {
                    if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == *id) {
                        item.kind = group_scaled_kind_in_bounds(original, anchor, k, canvas);
                    }
                }
            }
            // The eraser MARKS along the segment it just travelled (never only the sampled
            // point — a fast drag would jump clean over a stroke), then advances its anchor.
            AnnotGesture::Erase { last } => {
                let cur = (
                    x.clamp(canvas.x, canvas.x + canvas.w),
                    y.clamp(canvas.y, canvas.y + canvas.h),
                );
                mark_erased(&mut p.edit, last, cur);
                p.edit.gesture = Some(AnnotGesture::Erase { last: cur });
            }
        }
        // A drag that RESHAPED a TEXT box must re-wrap AND re-raster LIVE (DRAGON-354 item 3),
        // not only on release: `edited_kind` already reflowed the model above (the shared
        // `text_kind_layout` seam), so all that remains is refreshing the raster layer from it.
        // Vector shapes + effects still redraw for free on the view rebuild, so only text needs
        // this. (`touches_text` was read BEFORE the match, since the KIND can't change
        // mid-gesture.)
        //
        // DRAGON-367/368 — but neither a MOVE nor a RESIZE reshapes the DRAWING. The glyphs, the
        // wrap, the face, the outline and the colour are identical; only the similarity transform
        // placing them changed, and the layer is PLACED by a `dest` rect (DRAGON-362) rather than
        // stretched. So the raster is re-used verbatim and only the region it is placed at moves
        // and scales: a 16-byte uniform write instead of an SVG parse + resvg render costing
        // 29 ms at 512px type. That is what turned a big caption's gesture into an event-queue
        // backlog (the pointer's events piled up behind the render, then burst) — and it is why
        // the geometry can now be applied on EVERY motion event, with nothing left to throttle.
        if touches_text {
            // SCALING a normal box changes its `size_px` (DRAGON-364 task 4), so the size chip
            // follows the drag live. DISPLAY only — dragging a handle is not the user picking a
            // size, so the persisted default is untouched (see the display-vs-remember comment).
            self.sync_text_style_to_selection(id, TextStyleSource::HandleScale);
            if self.proxy_text_layer(id, text_before.as_deref()).is_some() {
                return Task::none();
            }
            return self.refresh_text_display(id);
        }
        // A live drag mutates the model; the GPU effects shader re-renders from it every frame.
        Task::none()
    }

    /// DRAGON-367/368 — the LIVE-TRANSFORM PROXY: when this motion event only moved and/or
    /// uniformly scaled the text, re-place the existing raster instead of re-rendering it.
    /// `Some(xform)` when it did, and the caller must NOT re-render.
    ///
    /// `before` is the pre-event render signature ([`text_render_sigs`]); `None` means the
    /// gesture never touched text, which is not a proxy. Everything the decision needs is pure
    /// and unit-tested in [`text_layer_xform`] + [`placed_text_region`]; this is only the state
    /// plumbing around them. The transform is composed INCREMENTALLY (each event maps the region
    /// the previous one left), which is exact for a similarity — and every gesture ends in a
    /// fresh exact render, so nothing accumulates across gestures.
    ///
    /// Note the raster's `Arc` is untouched, so the layer's GPU texture is not re-uploaded
    /// either — `LayerStackPipeline::upsert` keys uploads off the frame's `seq`, and only the
    /// `dest` uniform actually changes. That is the `layers.rs` flicker-free contract holding by
    /// construction rather than by luck, and it is what lets a resize run at pointer rate.
    ///
    /// `TextLayerGeom::scale`/`px` deliberately keep describing the RASTER (what it was rendered
    /// at, and how big it is), not the placement — only `region` moves here. That is what
    /// `refresh_text_for_zoom` compares against, and what the commit re-render replaces.
    fn proxy_text_layer(
        &mut self,
        id: window::Id,
        before: Option<&[TextRenderSig]>,
    ) -> Option<TextXform> {
        let before = before?;
        let p = self.preview_for_mut(id)?;
        // No raster on screen yet (a caption that is still empty, or a layer never rendered) —
        // there is nothing to re-place, so the normal path must run and create one.
        if p.edit.text_layers.is_empty() {
            return None;
        }
        let after = text_render_sigs(&p.edit.annotations, p.edit.frame);
        // The layers are now PER BOX (DRAGON-373), so the decision is per box too: each one's
        // raster may be re-placed if that box alone moved/scaled rigidly. A box the gesture did
        // not touch yields the identity and simply stays put, which is what lets a drag of ONE
        // caption keep the fast path in a scene that holds several — the shared layer had to
        // refuse that outright, because members separating is not a similarity of one texture.
        let mut placed: Vec<(AnnotId, AnnotRect)> = Vec::with_capacity(p.edit.text_layers.len());
        let mut moved = TextXform { scale: 1.0, dx: 0.0, dy: 0.0 };
        for layer in &p.edit.text_layers {
            let b = before.iter().find(|s| s.id == layer.id)?;
            let a = after.iter().find(|s| s.id == layer.id)?;
            let xform = text_layer_xform(std::slice::from_ref(b), std::slice::from_ref(a))?;
            let item = p.edit.annotations.iter().find(|it| it.id == layer.id)?;
            let padded = text_padded_bounds(std::slice::from_ref(item))?;
            placed.push((layer.id, placed_text_region(layer.geom.region, xform, padded)?));
            if xform != (TextXform { scale: 1.0, dx: 0.0, dy: 0.0 }) {
                moved = xform;
            }
        }
        for (aid, region) in placed {
            if let Some(layer) = p.edit.text_layers.iter_mut().find(|l| l.id == aid) {
                layer.geom.region = region;
            }
        }
        Some(moved)
    }

    /// Commit the active gesture: discard a degenerate new shape, else push ONE undo entry
    /// (the pre-gesture snapshot); the view redraws the final scene as vectors.
    pub(super) fn annot_gesture_end(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(gesture) = p.edit.gesture.take() else {
            return Task::none();
        };
        let snapshot = p.edit.annot_snapshot.take();
        let raw_trail = std::mem::take(&mut p.edit.pen_raw);
        // This document's backing scale, for the SOURCE-px→POINTS badge remember (DRAGON-383).
        let scale = p.source_scale;
        // Set by the badge-resize arm below; persisted once the `p` borrow ends.
        let mut remembered_badge = None;
        // Set by the TEXT arm below: after the borrow ends, open the editor + render the layer.
        let mut entered_text = false;
        match gesture {
            AnnotGesture::New { id, .. }
                if p
                    .edit
                    .annotations
                    .iter()
                    .any(|it| it.id == id && matches!(it.kind, AnnotKind::Text { .. })) =>
            {
                // TEXT (DRAGON-354): a new box is NEVER discarded here — it OPENS the editor.
                // Reflow it (snap the box to the caret line), stash the pre-edit scene on the
                // editing session so the SETTLE pushes the single undo entry (an empty settle
                // just deletes it), select it, and enter edit mode with the caret at the start.
                let bounds = p.edit.annot_bounds();
                if let Some(it) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    let reflowed = if let AnnotKind::Text {
                        rect, text, size_px, font, constrained, stroke_w,
                    } = &it.kind
                    {
                        Some(reflow_text_in_bounds(text, *size_px, *font, *rect, *constrained, *stroke_w, bounds))
                    } else {
                        None
                    };
                    if let Some(k) = reflowed {
                        it.kind = k;
                    }
                }
                p.edit.sel.set_one(id);
                p.edit.text_edit = Some(super::edit::TextEdit {
                    id,
                    caret: 0,
                    anchor: None,
                    snapshot: snapshot.unwrap_or_default(),
                    is_new: true,
                    blink_on: true,
                    history: Default::default(),
                });
                entered_text = true;
            }
            AnnotGesture::New { id, .. } => {
                // A pen gesture that never really travelled is a TAP: normalize it to the
                // single-point DOT it is (DRAGON-342), so it inks round and firm instead of as
                // a 2px tapered smear. `is_degenerate` then KEEPS it — a deliberate press with
                // the pencil armed is always a mark, while every other tool still discards a
                // stray click.
                if let Some(it) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    normalize_pen_tap(&mut it.kind, raw_trail.first().copied());
                }
                let degenerate = p
                    .edit
                    .annotations
                    .iter()
                    .find(|it| it.id == id)
                    .is_none_or(is_degenerate);
                if degenerate {
                    // Discard: never entered history, so just drop it (no undo entry).
                    p.edit.annotations.retain(|it| it.id != id);
                    p.edit.sel.retain_existing(&p.edit.annotations);
                    // A discarded in-progress effect vanishes on the next view build (GPU shader).
                    return Task::none();
                }
                // CONNECTIVITY (DRAGON-338): a freshly drawn stroke that touches other pen
                // strokes of the same look folds them all into ONE selectable item, so
                // connected scribbles move/delete together while disconnected ones stay
                // separate. Runs BEFORE the undo push, so undo restores the un-merged scene.
                let is_pen = p.edit.annotations.iter().any(|it| it.id == id && it.kind.is_pen());
                if is_pen {
                    merge_connected_pens(&mut p.edit.annotations, id);
                }
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
            // A one-item edit and a whole-selection move commit identically: ONE undo entry
            // holding the pre-gesture scene (DRAGON-341 — a group move is one edit, not N).
            AnnotGesture::Edit { id, .. } => {
                // Resizing a BADGE re-arms the remembered side, so the next badge — placed by
                // click OR pre-placed by double-click, here or in a LATER editor — matches the
                // one you just sized. UNDOING that resize deliberately does NOT un-remember it:
                // the remembered size is a tool preference, not scene state, and rewinding it
                // would make undo mean two different things at once.
                if let Some(AnnotKind::Badge { rect, .. }) =
                    p.edit.annotations.iter().find(|it| it.id == id).map(|it| &it.kind)
                {
                    // Resized side SOURCE px → remembered/persisted default POINTS (DRAGON-383).
                    let side_pt = source_px_to_points(rect.w, scale);
                    p.edit.annot_badge_size = side_pt;
                    remembered_badge = Some(side_pt);
                }
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
            AnnotGesture::MoveMany { .. } | AnnotGesture::ScaleMany { .. } => {
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
            // Releasing the eraser COMMITS: every marked pen group is deleted in ONE undo
            // entry. A sweep that marked nothing leaves no trace (no entry, no redo clear).
            AnnotGesture::Erase { .. } => {
                let marks = std::mem::take(&mut p.edit.erase_marks);
                if marks.is_empty() {
                    return Task::none();
                }
                p.edit.annotations.retain(|it| !marks.contains(&it.id));
                p.edit.sel.clear();
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
        }
        if let Some(side) = remembered_badge {
            self.remember_badge_size(side);
        }
        // A freshly opened text box needs its raster layer rendered (and the blink primed on
        // the next subscription tick). The `p` borrow has ended, so the App-level refresh runs.
        if entered_text {
            let refresh = self.refresh_text_display(id);
            #[cfg(target_os = "macos")]
            let refresh = Task::batch([self.focus_preview_for_text_edit(id), refresh]);
            return refresh;
        }
        // Any committed gesture may have MOVED / RESIZED / ERASED a text box, so re-render the
        // text layer (a cheap no-op when the scene holds no text). Vector shapes + effects
        // redraw on the next view build for free.
        self.refresh_text_display(id)
    }

    /// Re-render the live TEXT raster layers (DRAGON-354) SYNCHRONOUSLY into `edit.text_layers` —
    /// ONE per text annotation (DRAGON-373), each covering just that box's REGION
    /// ([`text_layer_region`]) at the layer's ON-SCREEN
    /// device-pixel resolution ([`super::edit::layer_raster_scale`], capped at the source frame
    /// — the same policy the covermark uses). It runs inline on every keystroke, on zoom, and
    /// after any edit that adds/moves/removes text, so both of those are load-bearing
    /// (DRAGON-362): the resolution is what keeps the glyphs as crisp as the base pixels beside
    /// them, and the region is what keeps the per-keystroke cost proportional to the caption
    /// rather than to the capture. No layers when there is no text (nor for a blank box — it
    /// draws nothing). The edited box reads its LIVE buffer (the item's own text is mutated in
    /// place while editing), so the layer always shows what is being typed.
    ///
    /// # A re-render that would change nothing never happens (DRAGON-376)
    ///
    /// This is the most expensive thing the preview's update path can do, and most of its ~16
    /// call sites reach it on edits that CANNOT change a glyph: a drag-select or a caret click
    /// (chrome, drawn as canvas vectors — [`super::edit::TextEdit`] state reaches no renderer
    /// input at all), re-opening the editor on a box that is already on screen, recolouring or
    /// restroking a selection that holds no text. Drag-select was the sharp end: one full
    /// SVG-build → `usvg` parse → resvg replay → demultiply pass per POINTER EVENT, producing a
    /// byte-identical bitmap, ~29 ms each at 512 px type against a 125 Hz pointer — the event
    /// queue fell ~4× behind realtime and the editor locked up.
    ///
    /// So the gate is here rather than in the callers: each box's RASTER INPUTS are signed
    /// ([`text_render_sig`] — precisely what [`super::text_annot::render_into`] reads) and
    /// compared against the signature of the drawing already on screen, together with the raster
    /// scale ([`text_raster_is_current`]). Nothing changed ⇒ nothing is rendered, and the layer's
    /// `Arc` is re-used so its persistent texture is not even re-uploaded. That makes a wasted
    /// re-render impossible by construction, for the 17th call site as much as for these, instead
    /// of by per-call-site review — the same trick as [`text_layer_xform`]'s layout comparison,
    /// applied per layer rather than to one gesture. Per BOX (DRAGON-373), so typing into one
    /// caption never re-renders the others.
    pub(super) fn refresh_text_display(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // The visual scale needs the App (viewport geometry), so resolve it under an IMMUTABLE
        // borrow before taking the mutable one below.
        let Some(vscale) = self.preview_for(id).map(|p| self.preview_visual_scale(p)) else {
            return Task::none();
        };
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let scale = super::edit::layer_raster_scale(p.view.zoom, vscale);
        let frame = p.edit.frame;
        // The layers that are on screen right now. Each is re-used verbatim — pixels, texture and
        // all — unless THAT box's raster inputs or the raster scale actually moved, so a scene of
        // three captions re-renders only the one being typed into, and a chrome-only event
        // (drag-select, caret click, a recolour that hit no text) re-renders nothing at all.
        let held = std::mem::take(&mut p.edit.text_layers);
        let mut next: Vec<super::edit::TextItemLayer> = Vec::with_capacity(held.len());
        for item in &p.edit.annotations {
            let Some(sig) = text_render_sig(item, frame) else {
                continue; // not a text item
            };
            // A blank box draws nothing, so it owns no layer (and no texture slot).
            let Some(region) = text_layer_region(std::slice::from_ref(item), frame) else {
                continue;
            };
            let prev = held.iter().find(|l| l.id == item.id);
            if text_raster_is_current(prev.map(|l| (l.geom.scale, &l.sig)), &sig, scale) {
                next.push(prev.expect("matched a held layer").clone());
                continue;
            }
            // The region's own pixel dimensions at that scale (never zero, never past the source
            // resolution or the texture limit).
            let (pw, ph) = super::edit::layer_raster_dims(
                (region.w.ceil().max(1.0) as u32, region.h.ceil().max(1.0) as u32),
                scale,
            );
            let Some(img) = render_text_layer(std::slice::from_ref(item), frame, region, pw, ph)
            else {
                continue;
            };
            let (w, h) = img.dimensions();
            next.push(super::edit::TextItemLayer {
                id: item.id,
                frame: crate::app::PixelFrame::new(img.into_raw(), w, h),
                geom: super::edit::TextLayerGeom { scale, region, px: (pw, ph) },
                // Sign what was actually drawn, so the next call can tell "this box moved on"
                // from "only the editor's chrome did" (DRAGON-376).
                sig,
            });
        }
        p.edit.text_layers = next;
        Task::none()
    }

    /// Re-render the text layer for a NEW zoom when the wanted resolution actually changed
    /// (mirrors [`Self::refresh_covermark_for_view`]) — so a magnified caption sharpens toward
    /// the source resolution without a re-render on every idle zoom step. The comparison is on
    /// the quantized raster SCALE (the region is content-derived and zoom-independent), so a
    /// zoom nudge inside one [`super::edit::RASTER_QUANTUM`] step costs nothing.
    pub(super) fn refresh_text_for_zoom(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        let has_text = p
            .edit
            .annotations
            .iter()
            .any(|it| matches!(it.kind, AnnotKind::Text { .. }));
        let want = super::edit::layer_raster_scale(p.view.zoom, self.preview_visual_scale(p));
        // Every layer already at the wanted resolution ⇒ nothing to sharpen. (An empty list with
        // text present means the boxes are all blank, which likewise has nothing to render.)
        if !has_text || p.edit.text_layers.iter().all(|l| l.geom.scale == want) {
            return Task::none();
        }
        self.refresh_text_display(id)
    }

    /// Settle the in-flight text edit (DRAGON-354): an EMPTY box is discarded (with an undo
    /// entry ONLY if it existed before this session), a non-empty CHANGED box pushes ONE undo
    /// entry (its pre-edit scene), and the session ends. No-op when nothing is being edited.
    pub(super) fn settle_text_edit(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(te) = p.edit.text_edit.take() else {
            return Task::none();
        };
        let empty = p
            .edit
            .annotations
            .iter()
            .find(|it| it.id == te.id)
            .map(|it| matches!(&it.kind, AnnotKind::Text { text, .. } if text.trim().is_empty()))
            .unwrap_or(true);
        if empty {
            // Drop the empty box. Emptying a PRE-EXISTING box is a real change (push the undo
            // entry); discarding a just-created one leaves no trace.
            p.edit.annotations.retain(|it| it.id != te.id);
            p.edit.sel.retain_existing(&p.edit.annotations);
            if !te.is_new {
                p.edit.push_annotations(te.snapshot);
            }
        } else if te.snapshot != p.edit.annotations {
            // A changed box: one undo entry holding the pre-edit scene.
            p.edit.push_annotations(te.snapshot);
        }
        self.refresh_text_display(id)
    }

    /// Route ONE key into the active text editor (DRAGON-354): printable input rides iced's
    /// PRODUCED text (so shifted / dead-key / precomposed characters insert correctly),
    /// Backspace/Delete remove (a whole grapheme, or the selection), the arrows + Home/End move
    /// the caret (Shift extends a selection), Cmd/Ctrl+C/X/V do clipboard, Escape or NUMPAD
    /// Enter settles ([`text_edit_exits`], DRAGON-364). A non-clipboard primary-modifier chord
    /// is SWALLOWED. Called from `keyboard.rs` before the keymap sees the press.
    pub(crate) fn text_edit_key(
        &mut self,
        id: window::Id,
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: cosmic::iced::keyboard::Key,
        location: cosmic::iced::keyboard::Location,
        typed: Option<String>,
    ) -> Task<cosmic::Action<Msg>> {
        use cosmic::iced::keyboard::{key::Named, Key};
        use super::text_annot as ta;
        // Escape / numpad Enter END the session. Checked BEFORE the modifier lanes so a stray
        // Shift or AltGr can't turn the exit key into a swallowed chord.
        if text_edit_exits(&key, location) {
            return self.settle_text_edit(id);
        }
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some((te_id, caret, anchor)) = p
            .edit
            .text_edit
            .as_ref()
            .map(|t| (t.id, t.caret, t.anchor))
        else {
            return Task::none();
        };
        let sel = p.edit.text_edit.as_ref().and_then(|t| t.selection());
        let bounds = p.edit.annot_bounds();
        let Some((text, size_px, font, constrained, rect)) = p
            .edit
            .annotations
            .iter()
            .find(|it| it.id == te_id)
            .and_then(|it| match &it.kind {
                AnnotKind::Text { text, size_px, font, constrained, rect, .. } => {
                    Some((text.clone(), *size_px, *font, *constrained, *rect))
                }
                _ => None,
            })
        else {
            return Task::none();
        };
        // The caret-movement keys navigate the SAME layout the box geometry and renderer use.
        let lay = text_kind_layout(&text, size_px, font, rect, constrained, bounds.w);
        // The PRIMARY command modifier (Cmd on macOS, Ctrl elsewhere) marks a shortcut combo;
        // Alt/AltGr must NOT block insertion (it composes real text on many layouts), and Shift
        // never does. So only the primary chord is treated as "a command, not text".
        #[cfg(target_os = "macos")]
        let primary = modifiers.logo();
        #[cfg(not(target_os = "macos"))]
        let primary = modifiers.control();

        // ── Clipboard combos (DRAGON-354 item 13): Cmd/Ctrl+A/C/X/V act on the TEXT while
        // editing (never the image-copy flow); other primary chords are swallowed. The
        // classification is the pure, unit-tested [`text_edit_chord`] — see its doc for why
        // that matters (DRAGON-369). ─────────────────────────────────────────────────────────
        if primary {
            let selected_text = |sel: Option<(usize, usize)>| -> Option<String> {
                let (a, b) = sel?;
                Some(text.chars().skip(a).take(b - a).collect())
            };
            return match text_edit_chord(&key, modifiers.shift()) {
                // In-session text undo/redo (DRAGON-354 item 13): Cmd/Ctrl+Z undoes this
                // session's edits, Shift+Cmd/Ctrl+Z (and Cmd/Ctrl+Y) redoes — WITHOUT touching
                // the shared EditOp stack. Exhausted = a no-op, never a settle-and-pop of the
                // global history mid-edit.
                Some(TextEditChord::Undo) => self.text_edit_history_step(id, false),
                Some(TextEditChord::Redo) => self.text_edit_history_step(id, true),
                // Select all: anchor at 0, caret at the end (a pure selection change).
                Some(TextEditChord::SelectAll) => {
                    let n = ta::char_len(&text);
                    self.apply_text_edit(
                        id, te_id, size_px, font, rect, constrained, bounds, None, n, Some(0), false,
                    )
                }
                Some(TextEditChord::Copy) => {
                    if let Some(t) = selected_text(sel) {
                        crate::share::copy_text(&t);
                    }
                    Task::none()
                }
                Some(TextEditChord::Cut) => {
                    let Some((a, b)) = sel else { return Task::none() };
                    if let Some(t) = selected_text(sel) {
                        crate::share::copy_text(&t);
                    }
                    let (nt, nc) = ta::delete_range(&text, a, b);
                    self.apply_text_edit(id, te_id, size_px, font, rect, constrained, bounds, Some(nt), nc, None, false)
                }
                Some(TextEditChord::Paste) => {
                    let Some(pasted) = crate::share::read_text() else {
                        return Task::none();
                    };
                    // Normalize CRLF/CR so pasted newlines wrap like typed Enter, then CAP
                    // the insertion (a multi-MB clipboard would drive the per-keystroke
                    // reflow + resvg raster into a stall). Truncated at a grapheme
                    // boundary; silent by design (no toast).
                    let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
                    let pasted = ta::cap_graphemes(&pasted, TEXT_PASTE_MAX_CHARS);
                    let (s, e) = sel.unwrap_or((caret, caret));
                    let (nt, nc) = ta::replace_range(&text, s, e, pasted);
                    self.apply_text_edit(id, te_id, size_px, font, rect, constrained, bounds, Some(nt), nc, None, false)
                }
                // Every other primary chord — including a non-character one — is SWALLOWED.
                None => Task::none(),
            };
        }

        let shift = modifiers.shift();
        // `(new_text, new_caret, new_anchor)`: `new_text=None` is a pure caret/selection move.
        let (new_text, new_caret, new_anchor): (Option<String>, usize, Option<usize>) = match &key {
            Key::Named(Named::Backspace) => {
                if let Some((a, b)) = sel {
                    let (t, c) = ta::delete_range(&text, a, b);
                    (Some(t), c, None)
                } else {
                    let (t, c) = ta::backspace(&text, caret);
                    (Some(t), c, None)
                }
            }
            Key::Named(Named::Delete) => {
                if let Some((a, b)) = sel {
                    let (t, c) = ta::delete_range(&text, a, b);
                    (Some(t), c, None)
                } else {
                    let (t, c) = ta::delete_forward(&text, caret);
                    (Some(t), c, None)
                }
            }
            // MAIN Enter inserts a newline — the box is multi-line. The NUMPAD one never reaches
            // here: it settled the session above ([`text_edit_exits`], DRAGON-364).
            Key::Named(Named::Enter) => {
                let (s, e) = sel.unwrap_or((caret, caret));
                let (t, c) = ta::replace_range(&text, s, e, "\n");
                (Some(t), c, None)
            }
            // Space arrives as a Character(" ") on iced, so it rides the Character/text arm below.
            Key::Named(Named::ArrowLeft) => {
                let c = ta::move_left(&text, caret);
                caret_move(caret, c, anchor, sel, shift, true, false)
            }
            Key::Named(Named::ArrowRight) => {
                let c = ta::move_right(&text, caret);
                caret_move(caret, c, anchor, sel, shift, false, false)
            }
            Key::Named(Named::ArrowUp) => {
                let c = ta::move_up(&lay, font, size_px, caret);
                caret_move(caret, c, anchor, sel, shift, true, false)
            }
            Key::Named(Named::ArrowDown) => {
                let c = ta::move_down(&lay, font, size_px, caret);
                caret_move(caret, c, anchor, sel, shift, false, false)
            }
            // Home/End TRAVEL to the line boundary even with a selection (travel = true).
            Key::Named(Named::Home) => {
                let c = ta::line_home(&lay, caret);
                caret_move(caret, c, anchor, sel, shift, true, true)
            }
            Key::Named(Named::End) => {
                let c = ta::line_end(&lay, caret);
                caret_move(caret, c, anchor, sel, shift, false, true)
            }
            // Printable input rides iced's PRODUCED text (DRAGON-354 items 1 + 18a): the shifted /
            // dead-key-composed / precomposed string, not the raw key name (whose base char is
            // unreliable for shifted characters on macOS). Insert only real text; a Character key
            // that produced none (a bare compose press) inserts nothing.
            // Space arrives as Character(" ") on iced, so the Character arm covers it too.
            Key::Character(_) => {
                // `primary` is already false here (a primary chord returned above); the shared
                // pure decision still filters bare compose presses / control text.
                let Some(ins) = ta::insertable_text(false, typed.as_deref()) else {
                    return Task::none();
                };
                let (s, e) = sel.unwrap_or((caret, caret));
                let (t, c) = ta::replace_range(&text, s, e, ins);
                (Some(t), c, None)
            }
            // Everything else (bare modifiers, F-keys, Tab): swallowed, no change — the tool
            // hotkeys stay suspended while a box is being edited.
            _ => return Task::none(),
        };
        // DRAGON-354 item 13: a single, non-whitespace typed character COALESCES into the current
        // undo burst; a word-break space, a multi-char (IME) commit, deletion, Enter, etc. each
        // start their own step (`coalesce = false`). A pure caret move (`new_text == None`) ends
        // the burst inside `apply_text_edit`.
        let coalesce = new_text.is_some()
            && matches!(&key, Key::Character(_))
            && typed.as_deref().is_some_and(|t| {
                let mut chars = t.chars();
                matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_whitespace())
            });
        self.apply_text_edit(id, te_id, size_px, font, rect, constrained, bounds, new_text, new_caret, new_anchor, coalesce)
    }

    /// Insert an OS input-method commit into the edited text box (DRAGON-359): the emoji picker
    /// or a CJK composition delivers its result as one string, which lands at the caret and
    /// replaces any active selection. Same insertion path as typing a character (through
    /// [`Self::apply_text_edit`]) — capped like a paste so a pathological commit can't stall the
    /// per-keystroke reflow. A no-op unless a text box is actually being edited.
    pub(crate) fn text_edit_ime_commit(
        &mut self,
        id: window::Id,
        text: String,
    ) -> Task<cosmic::Action<Msg>> {
        use super::text_annot as ta;
        if text.is_empty() {
            return Task::none();
        }
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some((te_id, caret)) = p.edit.text_edit.as_ref().map(|t| (t.id, t.caret)) else {
            return Task::none();
        };
        let sel = p.edit.text_edit.as_ref().and_then(|t| t.selection());
        let bounds = p.edit.annot_bounds();
        let Some((cur, size_px, font, constrained, rect)) = p
            .edit
            .annotations
            .iter()
            .find(|it| it.id == te_id)
            .and_then(|it| match &it.kind {
                // `stroke_w` (DRAGON-358) is not needed here: `apply_text_edit` reads the live
                // outline weight itself when it reflows.
                AnnotKind::Text { text, size_px, font, constrained, rect, .. } => {
                    Some((text.clone(), *size_px, *font, *constrained, *rect))
                }
                _ => None,
            })
        else {
            return Task::none();
        };
        // Normalize newlines like paste (an IME could deliver them), then CAP the insertion at a
        // grapheme boundary so an outsized commit can't drive the reflow + resvg raster into a
        // stall.
        let ins = text.replace("\r\n", "\n").replace('\r', "\n");
        let ins = ta::cap_graphemes(&ins, TEXT_PASTE_MAX_CHARS);
        let (s, e) = sel.unwrap_or((caret, caret));
        let (nt, nc) = ta::replace_range(&cur, s, e, ins);
        // An IME commit folds like paste (DRAGON-354 item 13 x DRAGON-359): its own undo step,
        // never coalesced into a typing burst.
        self.apply_text_edit(id, te_id, size_px, font, rect, constrained, bounds, Some(nt), nc, None, false)
    }

    /// Commit a computed text-edit result (DRAGON-354): reflow the box when the buffer changed,
    /// clamp + store the caret/selection, prime the blink, and re-render the live layer. Shared
    /// by every branch of [`Self::text_edit_key`] (typing, deletion, clipboard, caret moves).
    #[allow(clippy::too_many_arguments)]
    fn apply_text_edit(
        &mut self,
        id: window::Id,
        te_id: AnnotId,
        size_px: f32,
        font: super::text_annot::TextFont,
        rect: AnnotRect,
        constrained: bool,
        // DRAGON-389: the annotatable canvas (source ∪ crop), so an edited caption reflows against
        // the extended bounds — see [`super::edit::EditState::annot_bounds`].
        bounds: AnnotRect,
        new_text: Option<String>,
        new_caret: usize,
        new_anchor: Option<usize>,
        // DRAGON-354 item 13: this mutation is single-character typing that should COALESCE into
        // the current in-session undo burst. `false` for every non-typing edit (deletion, paste,
        // cut, Enter, word-break space) and for pure caret/selection moves (which end the burst).
        coalesce: bool,
    ) -> Task<cosmic::Action<Msg>> {
        use super::text_annot as ta;
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        // Preserve the box's OUTLINE weight across the edit (DRAGON-358): the width is not one
        // of this helper's style params (it re-styles independently), so read the live value.
        let (cur_text, stroke_w) = p
            .edit
            .annotations
            .iter()
            .find(|it| it.id == te_id)
            .and_then(|it| match &it.kind {
                AnnotKind::Text { text, stroke_w, .. } => Some((text.clone(), *stroke_w)),
                _ => None,
            })
            .unwrap_or_default();
        let final_len = new_text.as_deref().map_or_else(|| ta::char_len(&cur_text), ta::char_len);
        // The caret/selection BEFORE this edit — the state an in-session undo restores to.
        let (old_caret, old_anchor) =
            p.edit.text_edit.as_ref().map(|t| (t.caret, t.anchor)).unwrap_or((0, None));
        // DRAGON-354 item 13: snapshot the PRE-edit state onto the session history for a real
        // buffer change; a pure caret/selection move only ENDS any typing burst (it is not itself
        // undoable — matching every text editor). This history is settled away into the single
        // global `EditOp` when the session ends.
        if let Some(te) = p.edit.text_edit.as_mut() {
            match &new_text {
                Some(nt) if *nt != cur_text => {
                    te.history.record(
                        super::edit::TextSnapshot { text: cur_text.clone(), caret: old_caret, anchor: old_anchor },
                        coalesce,
                    );
                }
                _ => te.history.break_burst(),
            }
        }
        if let Some(nt) = &new_text {
            let reflowed = reflow_text_in_bounds(nt, size_px, font, rect, constrained, stroke_w, bounds);
            if let Some(it) = p.edit.annotations.iter_mut().find(|it| it.id == te_id) {
                it.kind = reflowed;
            }
        }
        if let Some(te) = p.edit.text_edit.as_mut() {
            te.caret = new_caret.min(final_len);
            te.anchor = new_anchor.map(|a| a.min(final_len));
            te.blink_on = true;
        }
        self.refresh_text_display(id)
    }

    /// Apply an in-session undo/redo (DRAGON-354 item 13): pop the session history (or redo)
    /// stack, restore that buffer + caret + selection into the open text box, and re-render.
    /// A NO-OP when the stack is exhausted — the shared `EditOp` history is NEVER touched
    /// mid-edit (it settles to one entry when the session ends). `redo` picks the direction.
    fn text_edit_history_step(&mut self, id: window::Id, redo: bool) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some((te_id, cur_caret, cur_anchor)) =
            p.edit.text_edit.as_ref().map(|t| (t.id, t.caret, t.anchor))
        else {
            return Task::none();
        };
        // `stroke_w` (DRAGON-358) rides the restore too: the outline weight is a style, not part
        // of the text history, so the box keeps its LIVE width across an in-session undo/redo.
        let Some((cur_text, size_px, font, constrained, rect, stroke_w)) = p
            .edit
            .annotations
            .iter()
            .find(|it| it.id == te_id)
            .and_then(|it| match &it.kind {
                AnnotKind::Text { text, size_px, font, constrained, rect, stroke_w } => {
                    Some((text.clone(), *size_px, *font, *constrained, *rect, *stroke_w))
                }
                _ => None,
            })
        else {
            return Task::none();
        };
        let current =
            super::edit::TextSnapshot { text: cur_text, caret: cur_caret, anchor: cur_anchor };
        let Some(te) = p.edit.text_edit.as_mut() else {
            return Task::none();
        };
        let restored = if redo { te.history.redo(current) } else { te.history.undo(current) };
        let Some(snap) = restored else {
            // Exhausted: a no-op. Do NOT fall through to the global undo/redo mid-edit.
            return Task::none();
        };
        let bounds = p.edit.annot_bounds();
        let final_len = super::text_annot::char_len(&snap.text);
        let reflowed = reflow_text_in_bounds(&snap.text, size_px, font, rect, constrained, stroke_w, bounds);
        if let Some(it) = p.edit.annotations.iter_mut().find(|it| it.id == te_id) {
            it.kind = reflowed;
        }
        if let Some(te) = p.edit.text_edit.as_mut() {
            te.caret = snap.caret.min(final_len);
            te.anchor = snap.anchor.map(|a| a.min(final_len));
            te.blink_on = true;
        }
        self.refresh_text_display(id)
    }

    /// Re-open the editor on an existing text box (DRAGON-354) — the double-click / Text-tool
    /// press path. Settles any current edit first, then selects the box and drops the caret at
    /// its end.
    /// macOS (DRAGON-359): when a text edit BEGINS, make our accessory app active + the preview
    /// surface key + the WinitView first responder, so the system emoji & symbols picker
    /// (Ctrl+Cmd+Space) routes to our text box (see
    /// [`crate::platform::mac::window::focus_view_for_text_edit`] for the AppKit reasoning).
    /// Targeted by window IDENTITY through `window::run_with_handle(id, ..)` — DRAGON-336 allows
    /// several simultaneous windowed previews all sharing one title, so a title scan could key
    /// the WRONG document (focus theft, emoji into the wrong box); the handle route reaches
    /// exactly THIS `window::Id`'s native view (the raw handle's `ns_view` IS the WinitView in
    /// our winit fork, the vibrancy nesting included). Scoped to begin-edit only, so capture-time
    /// overlay behavior is unchanged. `Task::none()` when no preview is open.
    #[cfg(target_os = "macos")]
    pub(super) fn focus_preview_for_text_edit(&self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        if self.preview_for(id).is_none() {
            return Task::none();
        }
        window::run_with_handle(id, |handle| {
            use window::raw_window_handle::RawWindowHandle;
            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                // SAFETY: the callback runs synchronously on the winit event loop (the main
                // thread) while the handle borrow of the live window is held — exactly the fn's
                // documented contract.
                unsafe { crate::platform::mac::window::focus_view_for_text_edit(h.ns_view) };
            }
        })
        .discard()
    }

    pub(super) fn edit_existing_text(&mut self, id: window::Id, annot: AnnotId) -> Task<cosmic::Action<Msg>> {
        let _ = self.settle_text_edit(id);
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(text) = p.edit.annotations.iter().find(|it| it.id == annot).and_then(|it| {
            match &it.kind {
                AnnotKind::Text { text, .. } => Some(text.clone()),
                _ => None,
            }
        }) else {
            return Task::none();
        };
        p.edit.sel.set_one(annot);
        p.edit.annot_menu = None;
        // Entering edit mode reflects it in the UI: arm the Text tool (DRAGON-354 item 4) so the
        // tray highlights Text and the pointer becomes the I-beam over the box. Since DRAGON-364
        // the armed Text tool no longer swallows presses on existing boxes, so leaving it armed
        // still allows dragging/resizing this box once the edit settles.
        p.edit.tool = Some(Tool::Text);
        p.edit.text_edit = Some(super::edit::TextEdit {
            id: annot,
            caret: super::text_annot::char_len(&text),
            anchor: None,
            snapshot: p.edit.annotations.clone(),
            is_new: false,
            blink_on: true,
            history: Default::default(),
        });
        // The dropdowns follow the box you just opened (DRAGON-364 task 3) — display only.
        self.sync_text_style_to_selection(id, TextStyleSource::SelectionSync);
        let refresh = self.refresh_text_display(id);
        #[cfg(target_os = "macos")]
        let refresh = Task::batch([self.focus_preview_for_text_edit(id), refresh]);
        refresh
    }

    /// A pointer PRESS inside the actively-edited text box (DRAGON-354 item 12): place the caret
    /// at image point `(x, y)`. `word` (double-click) selects the word; `extend` (Shift) extends
    /// from the current caret; a plain press seeds the anchor so a subsequent drag selects.
    pub(super) fn text_click_at(
        &mut self,
        id: window::Id,
        x: f32,
        y: f32,
        extend: bool,
        word: bool,
        all: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let Some((text, idx)) = self.text_caret_index_at(id, x, y) else {
            return Task::none();
        };
        if let Some(p) = self.preview_for_mut(id)
            && let Some(te) = p.edit.text_edit.as_mut()
        {
            if all {
                // Triple-click (DRAGON-354 item 12): select the WHOLE box — the same target as
                // Cmd/Ctrl+A (anchor at 0, caret at the end). A pure selection change.
                te.anchor = Some(0);
                te.caret = super::text_annot::char_len(&text);
            } else if word {
                let (ws, we) = super::text_annot::word_range_at(&text, idx);
                te.anchor = Some(ws);
                te.caret = we;
            } else if extend {
                if te.anchor.is_none() {
                    te.anchor = Some(te.caret);
                }
                te.caret = idx;
            } else {
                // Seed the anchor at the press so a drag selects; collapsed (anchor == caret)
                // reads as no selection until the caret moves.
                te.anchor = Some(idx);
                te.caret = idx;
            }
            te.blink_on = true;
        }
        self.refresh_text_display(id)
    }

    /// A drag inside the actively-edited text box (DRAGON-354 item 12): extend the selection to
    /// image point `(x, y)` — the caret end moves, the press anchor stays.
    pub(super) fn text_drag_to(&mut self, id: window::Id, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some((_text, idx)) = self.text_caret_index_at(id, x, y) else {
            return Task::none();
        };
        if let Some(p) = self.preview_for_mut(id)
            && let Some(te) = p.edit.text_edit.as_mut()
        {
            if te.anchor.is_none() {
                te.anchor = Some(te.caret);
            }
            te.caret = idx;
            te.blink_on = true;
        }
        self.refresh_text_display(id)
    }

    /// Map image point `(x, y)` to the caret CHAR index in the currently-edited text box, plus a
    /// clone of its text — via the SAME layout the renderer uses. `None` when nothing is being
    /// edited. Shared by the press / drag caret placement.
    fn text_caret_index_at(&self, id: window::Id, x: f32, y: f32) -> Option<(String, usize)> {
        let p = self.preview_for(id)?;
        let te_id = p.edit.text_edit.as_ref()?.id;
        let bounds = p.edit.annot_bounds();
        let (text, size_px, font, constrained, rect) =
            p.edit.annotations.iter().find(|it| it.id == te_id).and_then(|it| match &it.kind {
                AnnotKind::Text { text, size_px, font, constrained, rect, .. } => {
                    Some((text.clone(), *size_px, *font, *constrained, *rect))
                }
                _ => None,
            })?;
        let lay = text_kind_layout(&text, size_px, font, rect, constrained, bounds.w);
        let idx = super::text_annot::caret_at_point(&lay, font, size_px, x - rect.x, y - rect.y);
        Some((text, idx))
    }

    /// The caret-blink tick (DRAGON-354): flip the caret's visible phase while editing.
    pub(super) fn text_caret_blink(&mut self, id: window::Id) {
        if let Some(p) = self.preview_for_mut(id)
            && let Some(te) = p.edit.text_edit.as_mut()
        {
            te.blink_on = !te.blink_on;
        }
    }

    /// Set the text SIZE (SOURCE px) for new boxes and re-size the edited/selected one — the
    /// size dropdown. Re-flows through the same seam every text change uses; when NOT editing
    /// (a plain selection), it is one undo entry.
    pub(super) fn set_text_size(&mut self, id: window::Id, size: f32) -> Task<cosmic::Action<Msg>> {
        self.apply_text_style(id, Some(size), None)
    }

    /// Switch the text FONT for new boxes and re-font the edited/selected one — the font toggle.
    pub(super) fn set_text_font(
        &mut self,
        id: window::Id,
        font: super::text_annot::TextFont,
    ) -> Task<cosmic::Action<Msg>> {
        self.apply_text_style(id, None, Some(font))
    }

    // ── DISPLAY a style vs. REMEMBER a style (DRAGON-364) ────────────────────────────────
    //
    // These two are deliberately SEPARATE operations, and the split is the whole point of the
    // ticket's parenthetical. Text style reaches the user through two different pieces of state:
    //
    //   * `EditState::annot_text_size` / `annot_text_font` — the CURRENT document's working
    //     style. It is what the dropdown chips DISPLAY and what a new box in this document is
    //     born with. Per-document, never persisted, thrown away when the preview closes.
    //   * `App::annot_text_size` / `annot_text_font` — the REMEMBERED default, persisted through
    //     `state/schema.rs` and re-seeded into every future `EditState` on open.
    //
    // Selecting a text box, entering its editor, or SCALING a normal box (DRAGON-364 task 4) all
    // change what the dropdowns should SHOW — they are reports about the element under the
    // cursor, not statements of preference. Only an explicit pick in the dropdown menu is the
    // user saying "this is my size/font from now on", and only that may write the persisted
    // default. Collapsing these two would make merely clicking an old 96px caption silently
    // re-set the default for every future capture.
    //
    // So: [`Self::show_text_style`] never persists, [`Self::remember_text_style`] always does,
    // and the ONE caller that does both is the dropdown handler [`Self::apply_text_style`].

    /// Apply `size`/`font` to the text dropdowns — the ONE seam where "display a value" and
    /// "remember a value" are decided, keyed by WHERE the change came from
    /// ([`TextStyleSource`]). Both halves live here on purpose: a caller cannot forget to
    /// persist, and — the case that matters — cannot accidentally persist. See the block
    /// comment above. `size` is in logical POINTS (DRAGON-383): the working chip value and the
    /// persisted default are both point measures, so a dropdown pick passes its preset directly
    /// and the display-sync path brings the box's source-px size back to points first.
    fn set_text_style(
        &mut self,
        id: window::Id,
        source: TextStyleSource,
        size: Option<f32>,
        font: Option<super::text_annot::TextFont>,
    ) {
        // ALWAYS: what the chips show (and what the next new box in this document takes).
        if let Some(p) = self.preview_for_mut(id) {
            if let Some(s) = size {
                p.edit.annot_text_size = s;
            }
            if let Some(f) = font {
                p.edit.annot_text_font = f;
            }
        }
        // ONLY for an explicit pick: the persisted default for every FUTURE document.
        if !source.writes_default() {
            return;
        }
        if let Some(s) = size {
            self.annot_text_size = s;
        }
        if let Some(f) = font {
            self.annot_text_font = f;
        }
        self.save_state();
    }

    /// Point the font/size dropdowns at the PRIMARY selected (or actively edited) text box, so
    /// the chips always report the element you are working on (DRAGON-364 task 3). With a
    /// multi-selection the primary IS the last-selected item ([`super::edit::EditState::selected`]),
    /// which is exactly the "match the last one selected" rule.
    ///
    /// DISPLAY-only by construction — every caller passes a `source` whose
    /// [`TextStyleSource::writes_default`] is false, so the persisted default can never move
    /// through here. A non-text (or empty) selection leaves the chips alone: they keep showing
    /// what a new box would take, which is still true.
    pub(super) fn sync_text_style_to_selection(
        &mut self,
        id: window::Id,
        source: TextStyleSource,
    ) {
        debug_assert!(
            !source.writes_default(),
            "syncing the chips to an existing element is a REPORT, never a preference write",
        );
        let Some(p) = self.preview_for(id) else {
            return;
        };
        let scale = p.source_scale;
        // A live edit outranks the selection: the box being typed into is the one on screen.
        let target = p.edit.text_edit.as_ref().map(|t| t.id).or_else(|| p.edit.selected());
        if let Some((size, font)) = text_style_for_display(&p.edit.annotations, target) {
            // The box's `size_px` is SOURCE px; the size chips + the remembered default are
            // POINTS (DRAGON-383), so report it in points to match the presets.
            self.set_text_style(id, source, Some(source_px_to_points(size, scale)), Some(font));
        }
    }

    /// Shared body behind the size dropdown + font toggle — the ONE path that is a genuine user
    /// PREFERENCE statement, so it both displays the new style and REMEMBERS it (see the
    /// display-vs-remember block comment above). Then re-flows the box currently being edited
    /// (or, if none, the selected text box) with the new size/font. A change made outside an
    /// edit session is its own undo entry; during an edit the settle pushes the one entry.
    /// Closes the size flyout.
    fn apply_text_style(
        &mut self,
        id: window::Id,
        size: Option<f32>,
        font: Option<super::text_annot::TextFont>,
    ) -> Task<cosmic::Action<Msg>> {
        // The armed tool is DELIBERATELY left alone here (DRAGON-354 item 11c, per the user's
        // correction): changing a text dropdown only updates the setting and restyles the
        // selected/edited text box — it never switches the active tool to Text.
        if self.preview_for(id).is_none() {
            return Task::none();
        }
        // The ONE `DropdownPick`: displays AND persists.
        self.set_text_style(id, TextStyleSource::DropdownPick, size, font);
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        p.edit.flyout = None;
        // The picked `size` is a POINT preset; the box's `size_px` geometry is SOURCE px, so
        // scale the preset before it seeds the box (DRAGON-383). Identity on an unscaled (1x) output.
        let scale = p.source_scale;
        let size_px_seed = size.map(|pt| points_to_source_px(pt, scale));
        let bounds = p.edit.annot_bounds();
        let target = p.edit.text_edit.as_ref().map(|t| t.id).or_else(|| p.edit.selected());
        if let Some(tid) = target {
            let prev = p.edit.annotations.clone();
            let reflowed = p.edit.annotations.iter().find(|it| it.id == tid).and_then(|it| {
                match &it.kind {
                    AnnotKind::Text { rect, text, size_px, font: f, constrained, stroke_w } => Some(reflow_text_in_bounds(
                        text,
                        size_px_seed.unwrap_or(*size_px),
                        font.unwrap_or(*f),
                        *rect,
                        *constrained,
                        *stroke_w,
                        bounds,
                    )),
                    _ => None,
                }
            });
            if let Some(k) = reflowed {
                if let Some(it) = p.edit.annotations.iter_mut().find(|it| it.id == tid) {
                    it.kind = k;
                }
                // A style change on a merely-SELECTED box is its own undo entry; while editing,
                // the settle owns the entry.
                if p.edit.text_edit.is_none() {
                    p.edit.push_annotations(prev);
                }
            }
        }
        self.refresh_text_display(id)
    }

    /// Delete the WHOLE selection (DRAGON-341) — however many items — as ONE undo entry.
    pub(super) fn annot_delete_selected(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        if p.edit.sel.is_empty() {
            return Task::none();
        }
        p.edit.annot_menu = None;
        let prev = p.edit.annotations.clone();
        p.edit.annotations.retain(|it| !p.edit.sel.contains(it.id));
        if prev.len() != p.edit.annotations.len() {
            p.edit.sel.clear();
            // Deleting the text box currently being EDITED ends its session (DRAGON-354): the
            // one undo entry below already restores the box, and a live `text_edit` pointing at
            // a gone id would keep swallowing the keyboard (mirrors the Undo/Redo arms). No
            // settle — settling would push a second, conflicting entry for a removed item.
            if p.edit.text_edit.as_ref().is_some_and(|te| {
                !p.edit.annotations.iter().any(|it| it.id == te.id)
            }) {
                p.edit.text_edit = None;
            }
            p.edit.push_annotations(prev);
            // Deleting a text box drops it from the raster layer (DRAGON-354); effects/vectors
            // redraw on the next view build.
            return self.refresh_text_display(id);
        }
        Task::none()
    }

    /// Select EVERY annotation in the scene (DRAGON-341 — the Ctrl+A action). The armed tool
    /// is NEVER touched (DRAGON-344, user decision): select-all is a selection action, not a
    /// mode switch. Pen groups join the set too — Ctrl+A is as deliberate as a pointer click —
    /// but the usual rule still applies afterwards: arming another tool prunes them
    /// ([`super::edit::EditState::drop_pen_selection`]). Returns whether anything is now
    /// selected — an empty scene changes nothing at all (no persisted state churn).
    pub(super) fn select_all_annotations(&mut self, id: window::Id) -> bool {
        let selected = match self.preview_for_mut(id) {
            Some(p) if !p.edit.annotations.is_empty() => {
                let ids: Vec<AnnotId> = p.edit.annotations.iter().map(|it| it.id).collect();
                p.edit.sel.set_all(ids);
                p.edit.annot_menu = None;
                true
            }
            _ => false,
        };
        if selected {
            // The dropdowns follow the new primary (DRAGON-364 task 3) — display only.
            self.sync_text_style_to_selection(id, TextStyleSource::SelectionSync);
        }
        selected
    }

    /// Apply a POINTER rubber band (DRAGON-341): select every annotation the band
    /// `(x0, y0)`–`(x1, y1)` (image source px, either winding) TOUCHES. `additive` keeps the
    /// existing selection and adds to it; otherwise the band REPLACES it. A band that touches
    /// nothing simply clears (or leaves, when additive) the selection — never an undo entry,
    /// since selecting is not an edit.
    pub(super) fn band_select_annotations(&mut self, id: window::Id,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        additive: bool,
    ) {
        let Some(p) = self.preview_for_mut(id) else {
            return;
        };
        let hits: Vec<AnnotId> = band_hit_ids(&p.edit.annotations, x0, y0, x1, y1);
        if additive {
            p.edit.sel.add_all(hits);
        } else {
            p.edit.sel.set_all(hits);
        }
        p.edit.annot_menu = None;
        // The dropdowns follow the new primary (DRAGON-364 task 3) — display only.
        self.sync_text_style_to_selection(id, TextStyleSource::SelectionSync);
    }

    /// Duplicate the WHOLE selection (DRAGON-356): every selected item is cloned with a new id,
    /// the copies land on TOP of the z-stack (keeping their z-order among themselves), and the
    /// selection swaps to the copies (primary = the copy of the old primary). ONE undo entry.
    /// No-op when nothing is selected.
    ///
    /// The offset is the historical single-item nudge (toward the frame CENTER, from the
    /// PRIMARY's center, scaled a little to the image size), applied as ONE shared delta to every
    /// copy so the arrangement is duplicated RIGIDLY — the relative positions of the members are
    /// preserved exactly. A single-item selection stays byte-equivalent to the pre-DRAGON-356
    /// behavior: the historical [`edited_kind`] Move path (which clamps the lone copy's own drawn
    /// bounds, reflows a text box, etc.). A GROUP clamps the shared delta ONCE against the union
    /// of every member's drawn bounds ([`group_dup_offset`]) and translates every copy verbatim
    /// ([`translated_kind`]) — the same clamp-the-shared-delta discipline as a group MOVE, so the
    /// group can never distort against an image edge.
    pub(super) fn duplicate_selected_annotation(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // A live text edit SETTLES first (DRAGON-354): duplicating moves the selection to the
        // copies, and a session left open on an original would keep the keyboard while the
        // chrome (primary-keyed caret) pointed elsewhere. Settle commits (or discards an
        // empty box) as its own undo entry, then the duplicate proceeds normally. (The hotkey
        // can't fire mid-edit — keys are swallowed — but the context menu can.)
        let _ = self.settle_text_edit(id);
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(primary) = p.edit.sel.primary() else {
            return Task::none();
        };
        // The selected sources in SCENE order (so the copies keep the members' relative z-order),
        // remembering which source is the primary.
        let sources: Vec<AnnotationItem> = p
            .edit
            .annotations
            .iter()
            .filter(|it| p.edit.sel.contains(it.id))
            .cloned()
            .collect();
        let Some(prim_src) = sources.iter().find(|it| it.id == primary).cloned() else {
            return Task::none();
        };
        // DRAGON-389: the duplicate nudge + clamp ride the ANNOTATABLE canvas (source ∪ crop), so
        // copies land on the extension too. Centers/unions shift into bounds-origin space (the
        // canvas may have a NEGATIVE origin); the resulting DELTA is translation-invariant.
        let canvas = p.edit.annot_bounds();
        let prev = p.edit.annotations.clone();
        // Build the copies. SINGLE keeps the historical per-item clamp (byte-equivalent); a GROUP
        // clamps ONE shared delta on the union and translates every copy verbatim (rigid).
        let mut new_ids: Vec<AnnotId> = Vec::with_capacity(sources.len());
        let mut new_primary = primary;
        if sources.len() == 1 {
            let src = &sources[0];
            let c = kind_center(&src.kind);
            let (dx, dy) = single_dup_offset((c.0 - canvas.x, c.1 - canvas.y), (canvas.w, canvas.h));
            // A zero-press Move applies the offset AND clamps the copy's drawn bounds inside the canvas.
            let new_kind = edited_kind_in_bounds(&src.kind, Grab::Move, (0.0, 0.0), (dx, dy), canvas, false);
            let new_id = p.edit.next_annot_id();
            p.edit.annotations.push(AnnotationItem { id: new_id, color: src.color, kind: new_kind });
            new_ids.push(new_id);
            new_primary = new_id;
        } else {
            // The shared, clamped delta: computed from the PRIMARY's center like the single case,
            // then pinned once on the union so the whole arrangement lands inside the canvas.
            let union = group_drawn_bounds(sources.iter().map(|it| &it.kind))
                .expect("a non-empty selection has drawn bounds");
            let c = kind_center(&prim_src.kind);
            let union0 = AnnotRect { x: union.x - canvas.x, y: union.y - canvas.y, ..union };
            let (dx, dy) = group_dup_offset((c.0 - canvas.x, c.1 - canvas.y), union0, (canvas.w, canvas.h));
            for src in &sources {
                let new_kind = translated_kind(&src.kind, dx, dy);
                let new_id = p.edit.next_annot_id();
                p.edit.annotations.push(AnnotationItem { id: new_id, color: src.color, kind: new_kind });
                new_ids.push(new_id);
                if src.id == primary {
                    new_primary = new_id;
                }
            }
        }
        // The selection swaps to the copies, with the copy of the old primary LAST so it becomes
        // the new primary (the one wearing resize handles) — mirroring the single-item rule.
        p.edit.sel.set_all(dup_selection_order(&new_ids, new_primary));
        p.edit.annot_menu = None;
        p.edit.push_annotations(prev);
        // Duplicating a spotlight (e.g. after undo left the frame un-dimmed) re-ensures the dim.
        p.edit.ensure_dim_for_spotlights();
        // Duplicating a text box adds a text item to the raster layer (DRAGON-354).
        self.refresh_text_display(id)
    }

    /// Reorder the selected annotation in the z-stack (one undo entry when it moves).
    pub(super) fn annot_reorder(&mut self, id: window::Id, how: Reorder) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(annot) = p.edit.selected() else {
            return Task::none();
        };
        p.edit.annot_menu = None;
        let prev = p.edit.annotations.clone();
        let changed = match how {
            Reorder::Up => raise(&mut p.edit.annotations, annot),
            Reorder::Down => lower(&mut p.edit.annotations, annot),
            Reorder::Front => to_front(&mut p.edit.annotations, annot),
            Reorder::Back => to_back(&mut p.edit.annotations, annot),
        };
        if changed {
            p.edit.push_annotations(prev);
            // Reordering across effect TYPES changes the true-z-order composite — the GPU shader
            // walks the reordered item list on the next view build; the text layer re-composites
            // in the new scene order too (DRAGON-354).
            return self.refresh_text_display(id);
        }
        Task::none()
    }
}

/// Mark every PEN group the eraser segment `a`–`b` (SOURCE px) touches for deletion, adding to
/// (never replacing) the sweep's running mark set — a sweep only ever grows, so re-crossing a
/// stroke can't un-mark it. Only pen groups erase: the eraser is the pencil's partner, and a
/// sweep must never silently take out a redaction or an arrow it passed over.
fn mark_erased(edit: &mut super::edit::EditState, a: (f32, f32), b: (f32, f32)) {
    for it in &edit.annotations {
        let AnnotKind::Pen { paths, stroke_w, .. } = &it.kind else {
            continue;
        };
        if !edit.erase_marks.contains(&it.id) && pen_hit_by_eraser(paths, *stroke_w, a, b) {
            edit.erase_marks.push(it.id);
        }
    }
}

/// Which way to move a selected annotation in the z-stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reorder {
    Up,
    Down,
    Front,
    Back,
}

/// Whether a new shape is too small to keep (a stray click rather than a real draw).
fn is_degenerate(item: &AnnotationItem) -> bool {
    match &item.kind {
        AnnotKind::Box { rect, .. }
        | AnnotKind::Highlight { rect }
        | AnnotKind::BoxHighlight { rect, .. }
        | AnnotKind::Spotlight { rect }
        | AnnotKind::Pixelate { rect }
        | AnnotKind::Blur { rect }
        // A badge is square, so either axis measures it — same stray-click bar as a box.
        | AnnotKind::Badge { rect, .. } => rect.w < 2.0 || rect.h < 2.0,
        // A TEXT box is NEVER degenerate at creation (DRAGON-354): an empty box is valid — the
        // editor opens on it. Emptiness is resolved at SETTLE (an empty settled box is deleted
        // with no undo entry), never here.
        AnnotKind::Text { .. } => false,
        AnnotKind::Arrow { a, b, .. } => (a.x - b.x).hypot(a.y - b.y) < 3.0,
        // A PEN gesture is NEVER degenerate (DRAGON-342): with the pencil armed, a press is
        // always deliberate ink — a real drag is a stroke and a TAP is a dot (the commit path
        // has already normalized a sub-[`PEN_DOT_MAX`] stroke to its single anchor point). The
        // draw-vs-select ambiguity that made a pen click "probably a misclick" is gone now that
        // selection lives on the pointer tool.
        AnnotKind::Pen { .. } => false,
    }
}

/// The margin (SOURCE px) by which a kind's DRAWN extent overhangs its geometry rect: a box /
/// box-highlight OUTLINE straddles the geometry edge by half its stroke, and an arrow's round
/// caps by half its (bolt-boosted) stroke. Effects with no stroke (highlight / pixelate / blur)
/// draw exactly within the rect, so their margin is 0. Used so a drag/resize keeps the whole
/// DRAWN outline inside the image, not just the geometry. (The `+ 2.0` mirrors the vector
/// display's `ARROW_STROKE_BONUS`.)
/// The geometric center of a kind (SOURCE px): the rect midpoint, or an arrow's midpoint.
fn kind_center(kind: &AnnotKind) -> (f32, f32) {
    match kind {
        AnnotKind::Box { rect, .. }
        | AnnotKind::Highlight { rect }
        | AnnotKind::BoxHighlight { rect, .. }
        | AnnotKind::Pixelate { rect }
        | AnnotKind::Blur { rect }
        | AnnotKind::Badge { rect, .. }
        | AnnotKind::Text { rect, .. }
        | AnnotKind::Spotlight { rect } => (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5),
        AnnotKind::Arrow { a, b, .. } => ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5),
        AnnotKind::Pen { paths, .. } => {
            let r = pen_bounds(paths);
            (r.x + r.w * 0.5, r.y + r.h * 0.5)
        }
    }
}

fn kind_draw_margin(kind: &AnnotKind) -> f32 {
    match kind {
        AnnotKind::Box { stroke_w, .. } | AnnotKind::BoxHighlight { stroke_w, .. } => {
            stroke_w / 2.0
        }
        // The badge's RING is stroked ON the square's inscribed circle, so it overhangs by
        // half its weight exactly like a box outline does.
        AnnotKind::Badge { ring_w, .. } => ring_w / 2.0,
        // A pen's ribbon straddles its centerline by half its WIDEST sample — a heavy
        // (pressure-swelled) stretch, not the nominal preset — so the margin rides `max_width`
        // and no inked pixel can land outside the picture.
        AnnotKind::Pen { stroke_w, .. } => crate::pen_stroke::max_width(*stroke_w) / 2.0,
        AnnotKind::Arrow { stroke_w, .. } => (stroke_w + 2.0) / 2.0,
        // Stroke-less kinds draw exactly within the rect (Spotlight is an invisible knockout).
        // Text's glyphs are laid out inside the box, so it likewise overhangs by nothing.
        AnnotKind::Highlight { .. }
        | AnnotKind::Pixelate { .. }
        | AnnotKind::Blur { .. }
        | AnnotKind::Text { .. }
        | AnnotKind::Spotlight { .. } => 0.0,
    }
}

// ── Multi-selection geometry (DRAGON-341) ───────────────────────────────────────────────

/// Whether a freshly CREATED item of this kind becomes the selection. Every shape does — you
/// draw a box/arrow/redaction to then nudge or resize it. Freehand PEN ink does NOT
/// (DRAGON-341): pen selection visuals belong to pointer mode alone, so a stroke drawn with the
/// pencil must land with no dashed bbox, no handles, and no claim on the primary slot. Pure —
/// unit-tested.
fn kind_selects_on_create(kind: &AnnotKind) -> bool {
    !kind.is_pen()
}

/// A kind's DRAWN bounding rect (SOURCE px): its geometry bbox grown by [`kind_draw_margin`] —
/// the same outer extent every clamp in this module reasons about. Pure — unit-tested.
fn kind_drawn_bounds(kind: &AnnotKind) -> AnnotRect {
    let m = kind_draw_margin(kind);
    let base = match kind {
        AnnotKind::Box { rect, .. }
        | AnnotKind::Highlight { rect }
        | AnnotKind::BoxHighlight { rect, .. }
        | AnnotKind::Spotlight { rect }
        | AnnotKind::Pixelate { rect }
        | AnnotKind::Badge { rect, .. }
        | AnnotKind::Text { rect, .. }
        | AnnotKind::Blur { rect } => *rect,
        AnnotKind::Arrow { a, b, .. } => AnnotRect::from_points((a.x, a.y), (b.x, b.y)),
        AnnotKind::Pen { paths, .. } => pen_bounds(paths),
    };
    AnnotRect { x: base.x - m, y: base.y - m, w: base.w + 2.0 * m, h: base.h + 2.0 * m }
}

/// The UNION of every kind's drawn bounds, or `None` for an empty selection. Pure —
/// unit-tested.
fn group_drawn_bounds<'a>(kinds: impl IntoIterator<Item = &'a AnnotKind>) -> Option<AnnotRect> {
    let mut out: Option<AnnotRect> = None;
    for k in kinds {
        let r = kind_drawn_bounds(k);
        out = Some(match out {
            None => r,
            Some(u) => {
                let (x, y) = (u.x.min(r.x), u.y.min(r.y));
                let (rx, by) = ((u.x + u.w).max(r.x + r.w), (u.y + u.h).max(r.y + r.h));
                AnnotRect { x, y, w: rx - x, h: by - y }
            }
        });
    }
    out
}

/// The drag delta a GROUP move may actually apply: the raw `d` clamped per axis so the
/// selection's union `bounds` stays inside the image `frame`. Clamping ONCE on the union (never
/// per item) is what keeps the arrangement rigid — per-item clamping would squash items together
/// against an edge. A union WIDER than the frame on an axis has no valid range, so that axis
/// passes through unclamped (the group is already overflowing; fighting it would only jump it).
/// Pure — unit-tested.
fn group_move_delta(bounds: AnnotRect, frame: (f32, f32), d: (f32, f32)) -> (f32, f32) {
    let axis = |lo_edge: f32, size: f32, full: f32, delta: f32| {
        let lo = -lo_edge; // shift that puts the near edge exactly at 0
        let hi = full - lo_edge - size; // shift that puts the far edge exactly at `full`
        if lo > hi { delta } else { delta.clamp(lo, hi) }
    };
    (
        axis(bounds.x, bounds.w, frame.0, d.0),
        axis(bounds.y, bounds.h, frame.1, d.1),
    )
}

/// The RAW duplication nudge for an item whose center is `center` in a `frame`-sized image: an
/// equal x/y offset toward the frame CENTER (so the copy is obviously distinct and easy to grab),
/// scaled a little to the image size. This is the historical single-item rule (DRAGON-356 lifted
/// it out of `duplicate_selected_annotation` unchanged); the caller clamps it — the single case
/// through [`edited_kind`]'s own clamp, a group through [`group_dup_offset`]. Pure — unit-tested.
fn single_dup_offset(center: (f32, f32), frame: (f32, f32)) -> (f32, f32) {
    let (fw, fh) = frame;
    let off = (fw.min(fh) * 0.04).clamp(16.0, 64.0);
    let dx = if fw * 0.5 >= center.0 { off } else { -off };
    let dy = if fh * 0.5 >= center.1 { off } else { -off };
    (dx, dy)
}

/// The GROUP duplication delta (DRAGON-356): the same raw nudge as a single item
/// ([`single_dup_offset`], computed from the PRIMARY's `primary_center`), clamped ONCE against the
/// selection's `union` drawn bounds so the whole arrangement lands inside the image without
/// distorting — the identical clamp-the-shared-delta discipline as a group MOVE
/// ([`group_move_delta`]). Applied VERBATIM to every copy, it preserves the members' relative
/// positions exactly. Pure — unit-tested.
fn group_dup_offset(primary_center: (f32, f32), union: AnnotRect, frame: (f32, f32)) -> (f32, f32) {
    group_move_delta(union, frame, single_dup_offset(primary_center, frame))
}

/// The new SELECTION order after a duplicate (DRAGON-356): every `copies` id (in scene order),
/// with the copy of the old primary — `primary_copy` — moved to LAST so it becomes the new
/// primary (the only member wearing resize handles), mirroring the single-item rule. Pure —
/// unit-tested.
fn dup_selection_order(copies: &[AnnotId], primary_copy: AnnotId) -> Vec<AnnotId> {
    let mut out: Vec<AnnotId> = copies.iter().copied().filter(|c| *c != primary_copy).collect();
    out.push(primary_copy);
    out
}

/// `kind` translated by `(dx, dy)` with NO clamping — the group move clamps its shared delta up
/// front ([`group_move_delta`]), so every item must apply it verbatim. Pure — unit-tested.
fn translated_kind(kind: &AnnotKind, dx: f32, dy: f32) -> AnnotKind {
    let shift = |r: &AnnotRect| AnnotRect { x: r.x + dx, y: r.y + dy, w: r.w, h: r.h };
    match kind {
        AnnotKind::Box { rect, stroke_w, fill } => {
            AnnotKind::Box { rect: shift(rect), stroke_w: *stroke_w, fill: *fill }
        }
        AnnotKind::Highlight { rect } => AnnotKind::Highlight { rect: shift(rect) },
        AnnotKind::BoxHighlight { rect, stroke_w } => {
            AnnotKind::BoxHighlight { rect: shift(rect), stroke_w: *stroke_w }
        }
        AnnotKind::Spotlight { rect } => AnnotKind::Spotlight { rect: shift(rect) },
        AnnotKind::Badge { rect, ring_w } => {
            AnnotKind::Badge { rect: shift(rect), ring_w: *ring_w }
        }
        AnnotKind::Pixelate { rect } => AnnotKind::Pixelate { rect: shift(rect) },
        AnnotKind::Blur { rect } => AnnotKind::Blur { rect: shift(rect) },
        // Text moves as a value (DRAGON-354): only the box origin shifts; the string, size,
        // font and wrap mode ride along — so a duplicate/group-offset is a plain clone + shift
        // (no hidden shared state, DRAGON-356).
        AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } => AnnotKind::Text {
            rect: shift(rect),
            text: text.clone(),
            size_px: *size_px,
            font: *font,
            constrained: *constrained,
            stroke_w: *stroke_w,
        },
        AnnotKind::Arrow { a, b, stroke_w } => AnnotKind::Arrow {
            a: AnnotPoint { x: a.x + dx, y: a.y + dy },
            b: AnnotPoint { x: b.x + dx, y: b.y + dy },
            stroke_w: *stroke_w,
        },
        // A pure translation keeps the point count, so the pressure signal rides unchanged.
        AnnotKind::Pen { paths, pressure, stroke_w } => AnnotKind::Pen {
            paths: paths
                .iter()
                .map(|p| p.iter().map(|q| AnnotPoint { x: q.x + dx, y: q.y + dy }).collect())
                .collect(),
            pressure: pressure.clone(),
            stroke_w: *stroke_w,
        },
    }
}

/// The floor a group scale factor (DRAGON-388) is held above so no member ever inverts or
/// collapses to a point — the analog of [`text_scale_factor`]'s own `0.05` guard.
const GROUP_SCALE_FLOOR: f32 = 0.05;

/// The uniform scale factor a GROUP resize-handle drag implies (DRAGON-388): the drag projected
/// onto the dragged handle's direction, normalized by the selection's union `bounds` — exactly
/// the projection [`text_scale_factor`] uses for a text box, so a corner combines both axes and
/// an edge uses its own. ONE scalar keeps the whole selection a SIMILARITY of itself, which is
/// what preserves every per-kind aspect (badge squareness, text aspect-lock) and every overlap.
/// Pure — unit-tested.
fn group_scale_factor(bounds: AnnotRect, grab: Grab, dx: f32, dy: f32) -> f32 {
    text_scale_factor(bounds.w, bounds.h, grab, dx, dy)
}

/// The FIXED point a group scale pivots about (DRAGON-388): the union corner OPPOSITE the dragged
/// handle, so the handle you hold tracks the pointer while the far corner stays put. This mirrors
/// [`anchor_scaled_text_rect`] reduced to a single point — an edge grab pins the opposite corner
/// too, which keeps the pivot a point so a uniform scale about it stays rigid. Pure — unit-tested.
fn group_scale_anchor(bounds: AnnotRect, grab: Grab) -> (f32, f32) {
    use crate::geometry::{Corner, Edge};
    let (l, t, r, b) = bounds.corners();
    match grab {
        Grab::Corner(Corner::Nw) => (r, b),
        Grab::Corner(Corner::Ne) => (l, b),
        Grab::Corner(Corner::Sw) => (r, t),
        Grab::Corner(Corner::Se) => (l, t),
        Grab::Edge(Edge::N) => (l, b),
        Grab::Edge(Edge::S) => (l, t),
        Grab::Edge(Edge::W) => (r, t),
        Grab::Edge(Edge::E) => (l, t),
        // A move never scales; arrow-endpoint grabs never open a group scale.
        Grab::Move | Grab::ArrowA | Grab::ArrowB => (l, t),
    }
}

/// Clamp a group scale factor `k` (DRAGON-388) so the gesture respects the SAME limits a
/// single-item resize does, applied ONCE to the shared factor (never per item, which would break
/// rigidity): no text drops below [`super::text_annot::TEXT_SCALE_MIN_PX`] or exceeds
/// [`super::text_annot::TEXT_SCALE_MAX_PX`], nothing inverts ([`GROUP_SCALE_FLOOR`]), and the
/// scaled union stays inside the image `frame` when GROWING — an axis whose union already
/// overflows passes through there, mirroring [`group_move_delta`]. Pure — unit-tested.
fn clamp_group_scale<'a>(
    k: f32,
    bounds: AnnotRect,
    anchor: (f32, f32),
    frame: (f32, f32),
    originals: impl IntoIterator<Item = &'a AnnotKind>,
) -> f32 {
    let mut lo = GROUP_SCALE_FLOOR;
    let mut hi = f32::INFINITY;
    // A text box may not scale past its own type range; the SHARED factor takes the tightest of
    // them so every caption stays legible and none re-flows relative to the rest.
    for kind in originals {
        if let AnnotKind::Text { size_px, .. } = kind
            && *size_px > 0.0
        {
            lo = lo.max(super::text_annot::TEXT_SCALE_MIN_PX / *size_px);
            hi = hi.min(super::text_annot::TEXT_SCALE_MAX_PX / *size_px);
        }
    }
    // Keep the union inside the frame as it grows: each corner c maps to anchor + k·(c − anchor),
    // required within [0, frame] per axis. A corner already OUTSIDE that band on an axis sets no
    // ceiling there (the arrangement is overflowing; fighting it would jump it — the same
    // pass-through `group_move_delta` takes).
    let (fw, fh) = frame;
    let (l, t, r, b) = bounds.corners();
    let mut cap = |pos: f32, anc: f32, full: f32| {
        let off = pos - anc;
        if off > 0.0 && pos <= full {
            hi = hi.min((full - anc) / off);
        } else if off < 0.0 && pos >= 0.0 {
            hi = hi.min((0.0 - anc) / off); // off < 0, so this quotient is positive
        }
    };
    for (cx, cy) in [(l, t), (r, t), (l, b), (r, b)] {
        cap(cx, anchor.0, fw);
        cap(cy, anchor.1, fh);
    }
    k.clamp(lo, hi.max(lo))
}

/// Map one annotation `kind` through a UNIFORM scale by factor `k` about `anchor` (DRAGON-388) —
/// every point p ↦ anchor + k·(p − anchor). Because the map is a SIMILARITY, each kind keeps its
/// own resize semantics for free: rects scale position AND size together, a badge stays square, a
/// pen group maps affinely through its bounding box ([`scale_pen`]), an arrow's endpoints move,
/// and text scales its `size_px` (through the same [`reflow_text`] seam a single-box scale uses,
/// so live and bake agree). Strokes stay visually consistent (their width is untouched, exactly
/// as a single-item resize leaves `stroke_w`). Pure — unit-tested.
/// [`group_scaled_kind`] run against annotatable `bounds` instead of a `(0,0)`-origin frame
/// (DRAGON-389 × DRAGON-388): geometry and anchor shift into bounds-origin space, the proven
/// kernel runs at the bounds SIZE (its only frame use is the text reflow's wrap/clamp), and the
/// result shifts back. At a `(0,0)`-origin bounds this is byte-identical to the kernel.
fn group_scaled_kind_in_bounds(
    kind: &AnnotKind,
    anchor: (f32, f32),
    k: f32,
    bounds: AnnotRect,
) -> AnnotKind {
    let shifted = translated_kind(kind, -bounds.x, -bounds.y);
    let anchor0 = (anchor.0 - bounds.x, anchor.1 - bounds.y);
    let out = group_scaled_kind(&shifted, anchor0, k, (bounds.w, bounds.h));
    translated_kind(&out, bounds.x, bounds.y)
}

fn group_scaled_kind(kind: &AnnotKind, anchor: (f32, f32), k: f32, frame: (f32, f32)) -> AnnotKind {
    let map = |x: f32, y: f32| (anchor.0 + (x - anchor.0) * k, anchor.1 + (y - anchor.1) * k);
    let scale_rect = |r: &AnnotRect| {
        let (nx, ny) = map(r.x, r.y);
        AnnotRect { x: nx, y: ny, w: r.w * k, h: r.h * k }
    };
    match kind {
        AnnotKind::Box { rect, stroke_w, fill } => {
            AnnotKind::Box { rect: scale_rect(rect), stroke_w: *stroke_w, fill: *fill }
        }
        AnnotKind::Highlight { rect } => AnnotKind::Highlight { rect: scale_rect(rect) },
        AnnotKind::BoxHighlight { rect, stroke_w } => {
            AnnotKind::BoxHighlight { rect: scale_rect(rect), stroke_w: *stroke_w }
        }
        AnnotKind::Spotlight { rect } => AnnotKind::Spotlight { rect: scale_rect(rect) },
        // A uniform scale keeps a square square, so the badge's always-1:1 rule holds with no
        // special-casing; its ring weight is left as-is, like a single-item badge resize.
        AnnotKind::Badge { rect, ring_w } => {
            AnnotKind::Badge { rect: scale_rect(rect), ring_w: *ring_w }
        }
        AnnotKind::Pixelate { rect } => AnnotKind::Pixelate { rect: scale_rect(rect) },
        AnnotKind::Blur { rect } => AnnotKind::Blur { rect: scale_rect(rect) },
        AnnotKind::Arrow { a, b, stroke_w } => {
            let (ax, ay) = map(a.x, a.y);
            let (bx, by) = map(b.x, b.y);
            AnnotKind::Arrow {
                a: AnnotPoint { x: ax, y: ay },
                b: AnnotPoint { x: bx, y: by },
                stroke_w: *stroke_w,
            }
        }
        // A pen group maps affinely through its bounding box, exactly like its own resize —
        // and since `to` is `from` scaled uniformly about the anchor, every point lands on
        // anchor + k·(p − anchor) too, so the whole selection stays one similarity.
        AnnotKind::Pen { paths, pressure, stroke_w } => {
            let from = pen_bounds(paths);
            let to = scale_rect(&from);
            AnnotKind::Pen {
                paths: scale_pen(paths, from, to),
                pressure: pressure.clone(),
                stroke_w: *stroke_w,
            }
        }
        // Text scales its TYPE (DRAGON-364's factor path), not its wrap frame: the size grows by
        // `k` (clamped once by `clamp_group_scale`, so `applied == k` here for a rigid group),
        // the box origin maps by the same `k`, and `reflow_text` re-derives the geometry through
        // the shared layout seam. A uniform factor keeps every line break, so nothing re-flows.
        AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } => {
            let size = clamp_scaled_text_size(size_px * k);
            let applied = if *size_px > 0.0 { size / *size_px } else { 1.0 };
            let (nx, ny) = map(rect.x, rect.y);
            let src = AnnotRect { x: nx, y: ny, w: rect.w * applied, h: rect.h * applied };
            reflow_text(text, size, *font, src, *constrained, *stroke_w, frame)
        }
    }
}

/// Whether two rects overlap (touching edges count).
fn rects_overlap(a: AnnotRect, b: AnnotRect) -> bool {
    a.x <= b.x + b.w && b.x <= a.x + a.w && a.y <= b.y + b.h && b.y <= a.y + a.h
}

/// Whether the segments `p`–`p2` and `q`–`q2` cross (proper or touching). Sign-of-cross-product
/// straddle test, with the collinear case folded in via the zero checks.
fn segments_cross(p: (f32, f32), p2: (f32, f32), q: (f32, f32), q2: (f32, f32)) -> bool {
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let on = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        cross(o, a, b).abs() <= f32::EPSILON
            && b.0 >= o.0.min(a.0)
            && b.0 <= o.0.max(a.0)
            && b.1 >= o.1.min(a.1)
            && b.1 <= o.1.max(a.1)
    };
    let (d1, d2) = (cross(p, p2, q), cross(p, p2, q2));
    let (d3, d4) = (cross(q, q2, p), cross(q, q2, p2));
    if ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) {
        return true;
    }
    on(p, p2, q) || on(p, p2, q2) || on(q, q2, p) || on(q, q2, p2)
}

/// Whether the segment `a`–`b` TOUCHES rect `r`: either endpoint inside, or it crosses an edge.
/// Pure — unit-tested.
fn segment_hits_rect(a: (f32, f32), b: (f32, f32), r: AnnotRect) -> bool {
    let inside = |p: (f32, f32)| {
        p.0 >= r.x && p.0 <= r.x + r.w && p.1 >= r.y && p.1 <= r.y + r.h
    };
    if inside(a) || inside(b) {
        return true;
    }
    let (l, t, rr, bb) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let corners = [(l, t), (rr, t), (rr, bb), (l, bb)];
    (0..4).any(|i| segments_cross(a, b, corners[i], corners[(i + 1) % 4]))
}

/// **THE band rule's entry point**: the ids a rubber band whose corners are `(x0, y0)`–`(x1, y1)`
/// (image SOURCE px, UN-normalized — exactly as the canvas reports them) would take, in scene
/// z-order. Normalizes the corners, then applies [`items_in_band`].
///
/// Both users go through here: the RELEASE commit ([`App::band_select_annotations`]) and
/// DRAGON-397's LIVE sweep preview, which the canvas calls per motion event through an injected
/// closure. One entry point means the boxes that light up under the band and the selection that
/// lands on release cannot drift apart — including the normalization, which a second caller
/// would otherwise have to remember.
pub fn band_hit_ids(
    items: &[AnnotationItem],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
) -> Vec<AnnotId> {
    items_in_band(items, AnnotRect::from_points((x0, y0), (x1, y1)))
}

/// Every annotation the rubber `band` (SOURCE px, normalized) TOUCHES, in scene z-order
/// (DRAGON-341). Rect-geometry kinds test their DRAWN rect against the band; an arrow tests its
/// shaft as a segment; a pen group tests every stroke segment — so a band drawn through the
/// empty middle of a scribble's bounding box does NOT take it, exactly like clicking. Pure —
/// unit-tested.
pub fn items_in_band(items: &[AnnotationItem], band: AnnotRect) -> Vec<AnnotId> {
    items
        .iter()
        .filter(|it| match &it.kind {
            AnnotKind::Arrow { a, b, .. } => segment_hits_rect((a.x, a.y), (b.x, b.y), band),
            AnnotKind::Pen { paths, .. } => paths.iter().any(|path| match path.len() {
                0 => false,
                1 => segment_hits_rect((path[0].x, path[0].y), (path[0].x, path[0].y), band),
                _ => path
                    .windows(2)
                    .any(|w| segment_hits_rect((w[0].x, w[0].y), (w[1].x, w[1].y), band)),
            }),
            other => rects_overlap(kind_drawn_bounds(other), band),
        })
        .map(|it| it.id)
        .collect()
}

/// Move a rect so its WHOLE DRAWN extent (geometry grown by margin `m`) stays inside the image
/// `[0,fw]×[0,fh]` — size preserved, only the position shifts; if the rect+margins are bigger
/// than the image it pins to the inset origin.
fn clamp_rect(r: AnnotRect, fw: f32, fh: f32, m: f32) -> AnnotRect {
    let w = r.w.min((fw - 2.0 * m).max(0.0));
    let h = r.h.min((fh - 2.0 * m).max(0.0));
    AnnotRect {
        x: r.x.clamp(m, (fw - m - w).max(m)),
        y: r.y.clamp(m, (fh - m - h).max(m)),
        w,
        h,
    }
}

/// Apply a box grab to `rect` (relative to the pre-drag geometry), clamping the dragged
/// corner/edge — or the whole box for a Move — so its DRAWN extent (geometry grown by margin
/// `m`) stays inside the image. Shared by Box + Highlight. Pure — unit-tested.
fn edit_rect(rect: &AnnotRect, grab: Grab, dx: f32, dy: f32, fw: f32, fh: f32, m: f32) -> AnnotRect {
    use crate::geometry::{Corner, Edge};
    let (l, t, r, b) = rect.corners();
    let cx = |v: f32| v.clamp(m, (fw - m).max(m));
    let cy = |v: f32| v.clamp(m, (fh - m).max(m));
    match grab {
        Grab::Move => clamp_rect(
            AnnotRect { x: rect.x + dx, y: rect.y + dy, w: rect.w, h: rect.h },
            fw,
            fh,
            m,
        ),
        Grab::Corner(Corner::Nw) => AnnotRect::from_points((cx(l + dx), cy(t + dy)), (r, b)),
        Grab::Corner(Corner::Ne) => AnnotRect::from_points((l, cy(t + dy)), (cx(r + dx), b)),
        Grab::Corner(Corner::Sw) => AnnotRect::from_points((cx(l + dx), t), (r, cy(b + dy))),
        Grab::Corner(Corner::Se) => AnnotRect::from_points((l, t), (cx(r + dx), cy(b + dy))),
        Grab::Edge(Edge::N) => AnnotRect::from_points((l, cy(t + dy)), (r, b)),
        Grab::Edge(Edge::S) => AnnotRect::from_points((l, t), (r, cy(b + dy))),
        Grab::Edge(Edge::W) => AnnotRect::from_points((cx(l + dx), t), (r, b)),
        Grab::Edge(Edge::E) => AnnotRect::from_points((l, t), (cx(r + dx), b)),
        // Arrow grabs never apply to a box (defensive: leave it be).
        Grab::ArrowA | Grab::ArrowB => *rect,
    }
}

/// Apply an edit grab to `original` geometry, dragging from `press` to `cur` (image px),
/// clamped to the image `frame`. RELATIVE (delta-based): the dragged corner/edge/endpoint
/// moves by `(cur - press)`, so an OFFSET handle drags with NO jump. Pure — unit-tested.
///
/// `scale_type` is the DRAGON-370 override, and ONLY a text box reads it: Ctrl held during a
/// handle drag scales a CONSTRAINED (drag-created, "paragraph") box's type instead of reflowing
/// it, which is Photoshop's paragraph-vs-point-text modifier. The canvas samples it per motion
/// EVENT rather than latching it at press, so the user can change their mind mid-drag.
fn edited_kind(
    original: &AnnotKind,
    grab: Grab,
    press: (f32, f32),
    cur: (f32, f32),
    frame: (f32, f32),
    scale_type: bool,
) -> AnnotKind {
    let (dx, dy) = (cur.0 - press.0, cur.1 - press.1);
    let (fw, fh) = frame;
    // Clamp on the DRAWN extent (geometry grown by the kind's outline/cap margin), so no visible
    // stroke/cap spills past the image edge.
    let m = kind_draw_margin(original);
    match original {
        AnnotKind::Box { rect, stroke_w, fill } => AnnotKind::Box {
            rect: edit_rect(rect, grab, dx, dy, fw, fh, m),
            stroke_w: *stroke_w,
            fill: *fill,
        },
        AnnotKind::Highlight { rect } => {
            AnnotKind::Highlight { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
        }
        AnnotKind::BoxHighlight { rect, stroke_w } => AnnotKind::BoxHighlight {
            rect: edit_rect(rect, grab, dx, dy, fw, fh, m),
            stroke_w: *stroke_w,
        },
        AnnotKind::Spotlight { rect } => {
            AnnotKind::Spotlight { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
        }
        // The SEQUENCE BADGE takes the grab exactly like a box and is then FORCED back to 1:1
        // ([`square_for_grab`]) — during the drag, not just on release, so it is never seen as
        // an oval. That is the one place the always-square rule lives for an existing badge.
        AnnotKind::Badge { rect, ring_w } => AnnotKind::Badge {
            rect: square_for_grab(edit_rect(rect, grab, dx, dy, fw, fh, m), grab, fw, fh, m),
            ring_w: *ring_w,
        },
        AnnotKind::Pixelate { rect } => {
            AnnotKind::Pixelate { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
        }
        AnnotKind::Blur { rect } => {
            AnnotKind::Blur { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
        }
        // A TEXT box has TWO resize modes, keyed by how it was CREATED (DRAGON-364). The
        // `constrained` flag already records that: a DRAG lays out a fixed-width "prison" the
        // text wraps inside, a CLICK an auto box that hugs its own content.
        //
        //   * CONSTRAINED (dragged): the handle resizes the BOX. The wrap width follows it and
        //     the text re-flows inside; `size_px` never changes. (DRAGON-354's behaviour.)
        //   * NORMAL (clicked): the box has no independent geometry to stretch — its extent is
        //     DERIVED from the text — so the handle scales the TYPE instead, aspect-locked by
        //     construction ([`text_scale_factor`]), and the box is re-anchored so the handle
        //     opposite the one being dragged stays put ([`anchor_scaled_text_rect`]). It stays
        //     normal, so typing more text still auto-grows it at the new size.
        //
        // That split IS Photoshop's paragraph-text vs point-text distinction, one to one — and
        // DRAGON-370 completes it with Photoshop's OVERRIDE: `scale_type` (Ctrl held) makes a
        // CONSTRAINED box take the scale arm too, wrap width and all. It is meaningless on a
        // normal box, which already scales; the reverse override (Ctrl to set a wrap width on a
        // point box) is deliberately absent, exactly as in Photoshop.
        //
        // A pure MOVE never changes either mode or the size — it only translates.
        AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } => {
            if matches!(grab, Grab::Move) {
                // DRAGON-368: a text MOVE may leave the canvas, keeping only
                // [`TEXT_MIN_ON_CANVAS_PX`] of the box on it — see that constant for why the
                // threshold is on the box and not on the padded region. Every other kind (and a
                // CONSTRAINED box's resize handles, below) still clamps wholly inside: a shape's
                // ink IS its geometry, so an off-canvas box/arrow/redaction would just be a
                // partly-invisible shape, whereas a caption's whole point is where its glyphs
                // sit relative to the picture. (The clamp itself lives in `reflow_text`, the one
                // seam every text placement passes through.)
                let moved = AnnotRect { x: rect.x + dx, y: rect.y + dy, ..*rect };
                reflow_text(text, *size_px, *font, moved, *constrained, *stroke_w, (fw, fh))
            } else if *constrained && !scale_type {
                let r = edit_rect(rect, grab, dx, dy, fw, fh, m);
                reflow_text(text, *size_px, *font, r, *constrained, *stroke_w, (fw, fh))
            } else {
                // The SCALE mode: a normal box always, and a CONSTRAINED one while the
                // DRAGON-370 override is held. The box keeps whatever kind it was — the modifier
                // changes what the handle DOES, not what the text IS, so a paragraph box scaled
                // this way is still a paragraph box the next (unmodified) drag will reflow.
                let size = clamp_scaled_text_size(
                    size_px * text_scale_factor(rect.w, rect.h, grab, dx, dy),
                );
                // The factor actually APPLIED, after the guard bounds have had their say. The
                // box's wrap width is scaled by exactly that, which is what makes this a true
                // similarity of the drawing: identical line breaks, every measure × one factor.
                // That is not only what Photoshop does — it is what keeps a scale drag on
                // DRAGON-368's raster-reuse fast path, and a re-wrap on every motion event of the
                // editor's most expensive gesture is precisely what that ticket removed.
                //
                // For a CONSTRAINED box that width IS the wrap prison (DRAGON-370). For an AUTO
                // one it reaches the layout only as the `rect_w` floor in [`text_auto_cap`], so
                // it changes nothing until the box is already as wide as the picture — and there
                // it is the difference between scaling up cleanly and stalling: the floor would
                // otherwise pin the cap at the PRE-scale width, and a box that may not grow wider
                // can only answer a bigger type with more lines (DRAGON-378).
                let applied = if *size_px > 0.0 { size / *size_px } else { 1.0 };
                let src = AnnotRect { w: rect.w * applied, ..*rect };
                // Reflow at the new size FIRST (that is what decides the box extent), then
                // re-place it against the grab's anchor. Both halves go through the shared
                // `text_kind_layout` seam, so live and bake agree on the scaled geometry.
                let grown = reflow_text(text, size, *font, src, *constrained, *stroke_w, (fw, fh));
                match grown {
                    AnnotKind::Text { rect: gr, .. } => AnnotKind::Text {
                        rect: anchor_scaled_text_rect(*rect, gr.w, gr.h, grab, (fw, fh)),
                        text: text.to_string(),
                        size_px: size,
                        font: *font,
                        constrained: *constrained,
                        stroke_w: *stroke_w,
                    },
                    // `reflow_text` always returns a Text kind; keep the original if it ever
                    // doesn't rather than inventing geometry.
                    other => other,
                }
            }
        }
        // A pen group edits through its BOUNDING BOX: the box takes the grab exactly like a
        // rectangle (same clamping), then the strokes are mapped affinely into the result —
        // so Move translates and a corner/edge drag scales the whole drawing.
        AnnotKind::Pen { paths, pressure, stroke_w } => {
            let from = pen_bounds(paths);
            let to = edit_rect(&from, grab, dx, dy, fw, fh, m);
            // The per-point speed signal rides along UNCHANGED: a resize maps where the ink
            // is, not how hard it was pressed, and the point count is preserved so the
            // parallel arrays stay in step. (The curvature half of the pressure IS recomputed
            // from the new geometry at render, so a squashed loop re-inks coherently.)
            AnnotKind::Pen {
                paths: scale_pen(paths, from, to),
                pressure: pressure.clone(),
                stroke_w: *stroke_w,
            }
        }
        AnnotKind::Arrow { a, b, stroke_w } => {
            // `m` is the round-cap overhang; clamp each endpoint (and shift a Move) so the caps
            // stay inside the image, not just the endpoint positions.
            let clamp = |px: f32, py: f32| AnnotPoint {
                x: px.clamp(m, (fw - m).max(m)),
                y: py.clamp(m, (fh - m).max(m)),
            };
            let (mut na, mut nb) = (*a, *b);
            match grab {
                Grab::Move => {
                    // Translate both, then shift so the whole arrow (caps included) stays inside.
                    let (ax, ay, bx, by) = (a.x + dx, a.y + dy, b.x + dx, b.y + dy);
                    let sx = shift_into(ax.min(bx) - m, ax.max(bx) + m, fw);
                    let sy = shift_into(ay.min(by) - m, ay.max(by) + m, fh);
                    na = AnnotPoint { x: ax + sx, y: ay + sy };
                    nb = AnnotPoint { x: bx + sx, y: by + sy };
                }
                Grab::ArrowA => na = clamp(a.x + dx, a.y + dy),
                Grab::ArrowB => nb = clamp(b.x + dx, b.y + dy),
                // Box grabs never apply to an arrow.
                Grab::Corner(_) | Grab::Edge(_) => {}
            }
            AnnotKind::Arrow { a: na, b: nb, stroke_w: *stroke_w }
        }
    }
}

/// The shift needed to bring the span `[lo, hi]` inside `[0, max]` (0 when already inside;
/// only one side can be out for a span narrower than `max`).
fn shift_into(lo: f32, hi: f32, max: f32) -> f32 {
    if lo < 0.0 {
        -lo
    } else if hi > max {
        max - hi
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Corner, Edge};
    use rstest::rstest;

    #[test]
    fn caret_move_extends_collapses_and_plain_moves() {
        // DRAGON-354 item 12. Shift extends: keep/seed the anchor, move the caret.
        let (t, c, a) = caret_move(5, 3, None, None, true, true, false);
        assert_eq!((t, c, a), (None, 3, Some(5)), "shift seeds anchor at old caret");
        let (_t, c, a) = caret_move(5, 3, Some(9), Some((5, 9)), true, true, false);
        assert_eq!((c, a), (3, Some(9)), "shift keeps the existing anchor");
        // No shift + a selection: an ARROW collapses to the movement-side edge, no travel.
        let (_t, c, a) = caret_move(7, 6, Some(9), Some((7, 9)), false, true, false);
        assert_eq!((c, a), (7, None), "left/up collapses to selection start");
        let (_t, c, a) = caret_move(7, 8, Some(4), Some((4, 7)), false, false, false);
        assert_eq!((c, a), (7, None), "right/down collapses to selection end");
        // No shift, no selection: a plain move.
        let (_t, c, a) = caret_move(5, 4, None, None, false, true, false);
        assert_eq!((c, a), (4, None));
        // HOME/END (travel = true) with a selection: clear it AND travel to the line boundary
        // (index 0 / 12 here), never stop at the selection edge — the reviewed bug.
        let (_t, c, a) = caret_move(7, 0, Some(9), Some((7, 9)), false, true, true);
        assert_eq!((c, a), (0, None), "Home travels to the line start, selection cleared");
        let (_t, c, a) = caret_move(7, 12, Some(4), Some((4, 7)), false, false, true);
        assert_eq!((c, a), (12, None), "End travels to the line end, selection cleared");
        // Shift+Home still extends (travel only changes the no-shift collapse).
        let (_t, c, a) = caret_move(7, 0, Some(9), Some((7, 9)), true, true, true);
        assert_eq!((c, a), (0, Some(9)));
    }

    fn boxed(id: u64, x: f32, y: f32, w: f32, h: f32) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect: AnnotRect { x, y, w, h }, stroke_w: 4.0, fill: None },
        }
    }

    /// OUTLINE PARITY (DRAGON-358): the active line width thickens text through the ONE renderer
    /// (`text_annot::render_into`) both the live layer (`render_text_layer`) and the bake
    /// (`rasterize_scene`) call, so a stroked caption is pixel-identical on both — and a wider
    /// pencil demonstrably paints MORE ink than a hairline on BOTH paths (the width reaches the
    /// raster, not just the model). Mirrors the wrap-parity test's live-vs-bake pattern.
    #[test]
    fn text_outline_width_is_honored_identically_live_and_baked() {
        use super::super::text_annot::TextFont;
        let (w, h) = (200u32, 60u32);
        let frame = (w as f32, h as f32);
        // A hairline (no outline) vs a heavy pencil, same caption / size / origin.
        let item = |pencil_w: f32| AnnotationItem {
            id: AnnotId(1),
            color: [0, 0, 0, 255],
            kind: reflow_text(
                "Weighty",
                24.0,
                TextFont::Clean,
                AnnotRect { x: 4.0, y: 4.0, w: 0.0, h: 0.0 },
                false,
                pencil_w,
                frame,
            ),
        };
        let ink = |img: &RgbaImage| img.pixels().filter(|p| p.0[3] > 0).count();
        // The bake (rasterize_scene) and the live layer (render_text_layer) draw the SAME stroked
        // text at scale 1.0 → byte-identical rasters (the parity contract).
        // DRAGON-362: the live layer now rasters a REGION; comparing against the bake means
        // asking for the WHOLE frame as the region, at scale 1 — which is exactly the bake's
        // canvas, so the parity contract is unchanged.
        let whole = AnnotRect { x: 0.0, y: 0.0, w: frame.0, h: frame.1 };
        let bold = vec![item(6.0)];
        let live = render_text_layer(&bold, (w, h), whole, w, h).expect("live raster");
        let bake = rasterize_scene(&bold, w, h, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("bake raster");
        assert_eq!(live.as_raw(), bake.as_raw(), "live and bake agree pixel-for-pixel");
        // And the width is genuinely honored: a wider pencil inks MORE than a hairline, on BOTH.
        let hair = vec![item(1.0)];
        let live_hair = render_text_layer(&hair, (w, h), whole, w, h).expect("live hairline");
        let bake_hair = rasterize_scene(&hair, w, h, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("bake hairline");
        assert!(ink(&live) > ink(&live_hair), "the wider pencil thickens the live text");
        assert!(ink(&bake) > ink(&bake_hair), "and thickens the baked text too");
    }

    /// WRAP PARITY (DRAGON-354 review fix): an AUTO (click-placed) box's STORED geometry and
    /// every render/caret path derive their layout through the ONE seam
    /// ([`text_kind_layout`]), so a long caption wraps identically in the box, the live raster,
    /// the bake and the caret math. The old render paths passed the constant `AUTO_WRAP_FALLBACK`
    /// instead of the frame-derived cap — the caption then STORED a wrapped multi-line box but
    /// RENDERED one long clipped line; this pins the fix (the fallback layout is asserted to be
    /// genuinely different).
    ///
    /// It also pins DRAGON-378's half of that seam: the cap is the PICTURE's width, so the very
    /// same caption wraps the very same way wherever it is placed. Before, the wrap width was the
    /// room from the box to the picture's right edge, and the identical string wrapped differently
    /// at x=250 than at x=10 — an invisible layout choice made by where you happened to click.
    #[test]
    fn auto_box_render_layout_matches_the_stored_reflow_wrap() {
        use super::super::text_annot::{self, TextFont};
        let frame = (400.0_f32, 300.0_f32);
        let caption = "a long caption that certainly exceeds the room left of the edge";
        let placed = |x: f32| {
            reflow_text(
                caption,
                24.0,
                TextFont::Clean,
                AnnotRect { x, y: 10.0, w: 0.0, h: 0.0 },
                false,
                4.0,
                frame,
            )
        };
        // Placed at x=250 on a 400px picture — under the old rule, ~146px of room.
        let kind = placed(250.0);
        let AnnotKind::Text { rect, text, size_px, font, constrained, .. } = &kind else {
            panic!("reflow_text yields a Text kind");
        };
        assert!(!constrained, "a click-placed box stays auto");
        // The layout every render/caret path derives is EXACTLY the stored box's geometry.
        let lay = text_kind_layout(text, *size_px, *font, *rect, *constrained, frame.0);
        assert!((lay.box_w - rect.w).abs() < 0.01, "rendered width == stored width");
        assert!((lay.box_h - rect.h).abs() < 0.01, "rendered height == stored height");
        assert!(lay.lines.len() > 1, "a caption wider than the picture must wrap");
        // No rendered line may outrun the PICTURE — the cap, and the only bound left now that
        // glyphs are allowed past its edges (DRAGON-368).
        for line in &lay.lines {
            assert!(
                text_annot::measure(*font, *size_px, &line.text) <= frame.0 + 0.01,
                "line {:?} outruns the picture width",
                line.text
            );
        }
        // DRAGON-378: the SAME caption at the other end of the picture is the same drawing,
        // translated — same lines, same box extent.
        let far_left = placed(10.0);
        let AnnotKind::Text { rect: lr, .. } = &far_left else { panic!("text") };
        assert!(
            (lr.w - rect.w).abs() < 0.01 && (lr.h - rect.h).abs() < 0.01,
            "where the caption was clicked changed how it wrapped: {rect:?} vs {lr:?}",
        );
        // And the OLD behavior really was different — the constant fallback cap would not
        // have wrapped at all, which is exactly the divergence this test pins closed.
        let old = text_annot::layout(
            text,
            *font,
            *size_px,
            None,
            text_annot::AUTO_WRAP_FALLBACK,
        );
        assert_eq!(old.lines.len(), 1, "the constant cap would render one clipped line");
    }

    #[test]
    fn rect_kind_conversion_preserves_rect_carries_stroke_and_no_ops_off_family() {
        let r = AnnotRect { x: 5.0, y: 6.0, w: 30.0, h: 20.0 };
        let boxk = AnnotKind::Box { rect: r, stroke_w: 7.0, fill: None };
        let hl = AnnotKind::Highlight { rect: r };
        let bh = AnnotKind::BoxHighlight { rect: r, stroke_w: 7.0 };

        // Box -> Highlight: rect preserved, no stroke on the result.
        assert_eq!(converted_rect_kind(&boxk, Tool::Highlight, 3.0), Some(AnnotKind::Highlight { rect: r }));
        // Box -> Box Highlight: the box stroke CARRIES.
        assert_eq!(
            converted_rect_kind(&boxk, Tool::BoxHighlight, 3.0),
            Some(AnnotKind::BoxHighlight { rect: r, stroke_w: 7.0 })
        );
        // Highlight -> Box: no source stroke, so the fallback (current) width is used.
        assert_eq!(
            converted_rect_kind(&hl, Tool::Rect, 3.0),
            Some(AnnotKind::Box { rect: r, stroke_w: 3.0, fill: None })
        );
        // Box Highlight -> Box: the outline stroke carries.
        assert_eq!(
            converted_rect_kind(&bh, Tool::Rect, 3.0),
            Some(AnnotKind::Box { rect: r, stroke_w: 7.0, fill: None })
        );
        // Rect redactions join the family: Box <-> Pixelate <-> Blur all convert, rect preserved.
        assert_eq!(converted_rect_kind(&boxk, Tool::Pixelate, 3.0), Some(AnnotKind::Pixelate { rect: r }));
        assert_eq!(converted_rect_kind(&boxk, Tool::Blur, 3.0), Some(AnnotKind::Blur { rect: r }));
        assert_eq!(
            converted_rect_kind(&AnnotKind::Pixelate { rect: r }, Tool::Blur, 3.0),
            Some(AnnotKind::Blur { rect: r })
        );
        assert_eq!(
            converted_rect_kind(&AnnotKind::Blur { rect: r }, Tool::Rect, 3.0),
            Some(AnnotKind::Box { rect: r, stroke_w: 3.0, fill: None })
        );
        // Same kind's own tool → no conversion (no undo entry).
        assert_eq!(converted_rect_kind(&boxk, Tool::Rect, 3.0), None);
        assert_eq!(converted_rect_kind(&hl, Tool::Highlight, 3.0), None);
        assert_eq!(converted_rect_kind(&AnnotKind::Blur { rect: r }, Tool::Blur, 3.0), None);
        // Spotlight joins the rect family: convert to and from it, rect preserved.
        assert_eq!(converted_rect_kind(&boxk, Tool::Spotlight, 3.0), Some(AnnotKind::Spotlight { rect: r }));
        assert_eq!(
            converted_rect_kind(&AnnotKind::Spotlight { rect: r }, Tool::Rect, 3.0),
            Some(AnnotKind::Box { rect: r, stroke_w: 3.0, fill: None })
        );
        // Arrow source OR target is NOT a rect kind → no conversion.
        assert_eq!(converted_rect_kind(&boxk, Tool::Arrow, 3.0), None);
    }

    #[test]
    fn kind_center_is_the_rect_or_arrow_midpoint() {
        let r = AnnotRect { x: 10.0, y: 20.0, w: 40.0, h: 60.0 };
        assert_eq!(kind_center(&AnnotKind::Box { rect: r, stroke_w: 2.0, fill: None }), (30.0, 50.0));
        assert_eq!(kind_center(&AnnotKind::Pixelate { rect: r }), (30.0, 50.0));
        assert_eq!(kind_center(&AnnotKind::Spotlight { rect: r }), (30.0, 50.0));
        let arrow = AnnotKind::Arrow {
            a: AnnotPoint { x: 0.0, y: 0.0 },
            b: AnnotPoint { x: 100.0, y: 40.0 },
            stroke_w: 2.0,
        };
        assert_eq!(kind_center(&arrow), (50.0, 20.0));
    }

    fn scene(ids: &[u64]) -> Vec<AnnotationItem> {
        ids.iter().map(|&i| boxed(i, 0.0, 0.0, 10.0, 10.0)).collect()
    }
    fn order(items: &[AnnotationItem]) -> Vec<u64> {
        items.iter().map(|it| it.id.0).collect()
    }

    #[test]
    fn stroke_width_cycle_wraps_the_seven_presets() {
        // DRAGON-357 item 9: the `L` hotkey advances 1 → 2 → 4 → 6 → 8 → 10 → 12 → 1.
        assert_eq!(cycle_stroke_width(1.0), 2.0);
        assert_eq!(cycle_stroke_width(2.0), 4.0);
        assert_eq!(cycle_stroke_width(4.0), 6.0);
        assert_eq!(cycle_stroke_width(6.0), 8.0);
        assert_eq!(cycle_stroke_width(8.0), 10.0);
        assert_eq!(cycle_stroke_width(10.0), 12.0);
        assert_eq!(cycle_stroke_width(12.0), 1.0); // wraps
        // The default (4px) cycles to 6px.
        assert_eq!(cycle_stroke_width(DEFAULT_ANNOT_STROKE), 6.0);
        // A near-but-inexact width snaps to its nearest preset first, then advances.
        assert_eq!(cycle_stroke_width(3.9), 6.0); // nearest 4 → 6
        assert_eq!(cycle_stroke_width(100.0), 1.0); // nearest 12 → wraps to 1
    }

    #[test]
    fn stroke_width_nearest_index_picks_closest_preset() {
        assert_eq!(stroke_width_nearest_index(1.0), 0);
        assert_eq!(stroke_width_nearest_index(2.0), 1);
        assert_eq!(stroke_width_nearest_index(4.0), 2);
        assert_eq!(stroke_width_nearest_index(6.0), 3);
        assert_eq!(stroke_width_nearest_index(8.0), 4);
        assert_eq!(stroke_width_nearest_index(10.0), 5);
        assert_eq!(stroke_width_nearest_index(12.0), 6);
        // Off-preset values map to the closest segment (so exactly one reads active).
        assert_eq!(stroke_width_nearest_index(2.9), 1); // |2.9-2|<|2.9-4|
        assert_eq!(stroke_width_nearest_index(5.0), 2); // tie 4 vs 6 → lower index
        assert_eq!(stroke_width_nearest_index(0.0), 0); // 0 (unset) → 1px, nearest
        assert_eq!(stroke_width_nearest_index(100.0), 6); // huge → thickest
    }

    /// DRAGON-447: the line-width guarantee, pinned rather than assumed. Every platform
    /// resolves a REAL per-output scale into `PreviewState::source_scale` (COSMIC buffer
    /// scale / macOS backing scale / Windows `GetDpiForMonitor`), so the points↔source-px
    /// ladder is live everywhere — not a macOS-only path with a Linux no-op. A preset must
    /// survive the round trip at every step of the scale ladder, or a stroke width /
    /// badge size / text size drifts each time it is re-seeded on a scaled display.
    #[test]
    fn the_point_ladder_round_trips_at_every_scale_on_every_platform() {
        const PRESETS: [f32; 7] =
            [1.0, 4.0, 8.0, 16.0, 32.0, DEFAULT_ANNOT_STROKE, DEFAULT_BADGE_SIZE];
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            for pt in PRESETS {
                let px = points_to_source_px(pt, scale);
                // The preset really is `scale`× bigger in source pixels — that is what keeps
                // a "4pt" stroke the same VISUAL width on a 1x and a 3x capture.
                assert!((px - pt * scale).abs() < 1e-4, "{pt}pt @ {scale}x -> {px}px");
                // ...and comes back to the same points, so a resize-settled default re-seeds
                // correctly on a document grabbed from a DIFFERENT-scale display.
                let back = source_px_to_points(px, scale);
                assert!((back - pt).abs() < 1e-4, "round trip {pt}pt @ {scale}x -> {back}");
            }
        }
        // The whole persisted stroke ladder keeps its identity in POINTS at every scale:
        // the flyout highlight is chosen off the point value, never the scaled pixels.
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            for (i, w) in STROKE_WIDTHS.iter().enumerate() {
                let px = points_to_source_px(*w, scale);
                assert_eq!(stroke_width_nearest_index(source_px_to_points(px, scale)), i);
            }
        }
    }

    #[test]
    fn points_to_source_px_is_identity_on_an_unscaled_output() {
        // DRAGON-383: on any UNSCALED (1x) output — on every platform — scale is 1.0, so
        // EVERY seeding path that routes through this stays byte-identical: the no-op
        // safety property that makes the conversion safe to thread everywhere.
        for pt in [1.0, 4.0, 8.0, 32.0, 75.0, DEFAULT_ANNOT_STROKE, DEFAULT_BADGE_SIZE] {
            assert_eq!(points_to_source_px(pt, 1.0), pt);
            assert_eq!(source_px_to_points(pt, 1.0), pt);
        }
        // A non-positive scale degrades to 1.0 (defensive) rather than zeroing the dimension.
        assert_eq!(points_to_source_px(4.0, 0.0), 4.0);
        assert_eq!(points_to_source_px(4.0, -2.0), 4.0);
        assert_eq!(source_px_to_points(4.0, 0.0), 4.0);
    }

    #[test]
    fn points_to_source_px_scales_on_retina_and_round_trips() {
        // A "4pt" preset spans 8 SOURCE px on a 2x capture (so it reads the same visual size as
        // 4px does on a 1x capture), and 3x triples it.
        assert_eq!(points_to_source_px(4.0, 2.0), 8.0);
        assert_eq!(points_to_source_px(12.0, 2.0), 24.0);
        assert_eq!(points_to_source_px(32.0, 3.0), 96.0);
        // The badge/text round trip: a resized source-px side comes back to the SAME points, so
        // the remembered/persisted default re-seeds correctly on a different-scale document.
        for (px, scale) in [(150.0, 2.0), (96.0, 3.0), (75.0, 1.0), (37.0, 2.0)] {
            let pt = source_px_to_points(px, scale);
            assert!((points_to_source_px(pt, scale) - px).abs() < 1e-4, "round trip {px}@{scale}");
        }
        // The ladder still matches in POINTS regardless of the document scale: a 2x document's
        // working stroke is stored in points, so the flyout highlight is chosen off the raw
        // preset value (8pt → index 4), never the scaled 16px.
        assert_eq!(stroke_width_nearest_index(source_px_to_points(16.0, 2.0)), 4);
        assert_eq!(stroke_width_nearest_index(8.0), 4);
    }

    #[test]
    fn new_shape_seeds_the_selected_stroke_width() {
        // A drawn box/arrow must take its stroke_w from the SELECTED width (EditState::stroke),
        // not the hard-coded default — the width control is the single source of truth.
        let mut e = super::super::edit::EditState { annot_stroke_w: 8.0, ..Default::default() };
        let stroke_w = e.stroke();
        assert_eq!(stroke_w, 8.0);
        let id = e.next_annot_id();
        e.annotations.push(AnnotationItem {
            id,
            color: [1, 2, 3, 255],
            kind: AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 }, stroke_w, fill: None },
        });
        let AnnotKind::Box { stroke_w: got, .. } = &e.annotations[0].kind else {
            panic!("expected a box");
        };
        assert_eq!(*got, 8.0, "new box must seed the selected 8px width");
        // With the field unset (0.0), the getter falls back to the 5px default.
        let e2 = super::super::edit::EditState::default();
        assert_eq!(e2.stroke(), DEFAULT_ANNOT_STROKE);
    }

    #[test]
    fn raise_and_lower_swap_neighbours_and_clamp() {
        let mut s = scene(&[1, 2, 3]);
        assert!(raise(&mut s, AnnotId(1)));
        assert_eq!(order(&s), [2, 1, 3]);
        assert!(lower(&mut s, AnnotId(1)));
        assert_eq!(order(&s), [1, 2, 3]);
        // Top can't rise; bottom can't fall.
        assert!(!raise(&mut s, AnnotId(3)));
        assert!(!lower(&mut s, AnnotId(1)));
        assert_eq!(order(&s), [1, 2, 3]);
    }

    #[test]
    fn to_front_and_back_move_to_the_ends() {
        let mut s = scene(&[1, 2, 3]);
        assert!(to_front(&mut s, AnnotId(1)));
        assert_eq!(order(&s), [2, 3, 1]);
        assert!(to_back(&mut s, AnnotId(1)));
        assert_eq!(order(&s), [1, 2, 3]);
        // Already at an end → no move.
        assert!(!to_front(&mut s, AnnotId(3)));
        assert!(!to_back(&mut s, AnnotId(1)));
    }

    #[test]
    fn complement_rotates_hue_180_and_keeps_gray() {
        // Primary/secondary complements: red↔cyan, blue↔yellow, green↔magenta.
        assert_eq!(complement([255, 0, 0, 255]), [0, 255, 255, 255]);
        assert_eq!(complement([0, 0, 255, 255]), [255, 255, 0, 255]);
        assert_eq!(complement([0, 255, 0, 255]), [255, 0, 255, 255]);
        // Grays have no hue → unchanged (black, mid-gray, white).
        assert_eq!(complement([0, 0, 0, 255]), [0, 0, 0, 255]);
        assert_eq!(complement([128, 128, 128, 255]), [128, 128, 128, 255]);
        assert_eq!(complement([255, 255, 255, 255]), [255, 255, 255, 255]);
        // Alpha is preserved.
        assert_eq!(complement([255, 0, 0, 200])[3], 200);
        // Double complement returns (near) the original hue.
        assert_eq!(complement(complement([255, 0, 0, 255])), [255, 0, 0, 255]);
    }

    #[test]
    fn companion_color_is_the_complement_and_total() {
        // The companion IS the complement — the same relationship the whole palette speaks.
        for c in [
            [255, 0, 0, 255],
            [17, 200, 43, 255],
            [0, 0, 255, 200],
            [128, 128, 128, 255], // gray → itself
            [0, 0, 0, 255],
        ] {
            assert_eq!(companion_color(c), complement(c), "{c:?}");
        }
    }

    #[test]
    fn companion_swap_double_press_returns_to_start_exactly() {
        // Totality + exact double-swap-return across a spread of colors, incl. grays and alpha.
        for start in [
            [255, 0, 0, 255],
            [17, 200, 43, 255],
            [200, 90, 10, 128],
            [3, 5, 250, 255],
            [128, 128, 128, 255], // gray: its own companion, still a clean toggle
            [255, 255, 255, 255],
            [0, 0, 0, 200],
        ] {
            // First X: swap to the companion, remembering where we came from.
            let (a, back) = companion_swap(start, None);
            assert_eq!(a, companion_color(start), "{start:?} first swap");
            // Second X: return to the EXACT starting color (memory beats rounding).
            let (b, back2) = companion_swap(a, Some(back));
            assert_eq!(b, start, "{start:?} double swap returns to start");
            // Third X: swaps forward again to the companion (the toggle keeps cycling).
            let (c, _) = companion_swap(b, Some(back2));
            assert_eq!(c, companion_color(start), "{start:?} third swap goes forward");
        }
    }

    #[test]
    fn companion_swap_forgets_stale_partner_after_a_manual_pick() {
        // Swap once, then a NON-swap color pick lands (the caller clears `color_swap_back`):
        // the next X must operate on the NEW color, not chase the stale remembered partner.
        let start = [255, 0, 0, 255];
        let (_swapped, _back) = companion_swap(start, None);
        let picked = [10, 220, 60, 255]; // an unrelated color the user chose from the flyout
        let (t, back) = companion_swap(picked, None);
        assert_eq!(t, companion_color(picked));
        assert_eq!(back, picked);
    }

    #[test]
    fn from_points_normalizes_either_drag_direction() {
        let a = AnnotRect::from_points((100.0, 100.0), (40.0, 60.0));
        assert_eq!((a.x, a.y, a.w, a.h), (40.0, 60.0, 60.0, 40.0));
        let b = AnnotRect::from_points((40.0, 60.0), (100.0, 100.0));
        assert_eq!((b.x, b.y, b.w, b.h), (40.0, 60.0, 60.0, 40.0));
    }

    #[test]
    fn edited_move_translates_a_box() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 }, stroke_w: 4.0, fill: None };
        let k = edited_kind(&orig, Grab::Move, (0.0, 0.0), (5.0, -7.0), (10000.0, 10000.0), false);
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (15.0, 13.0, 30.0, 40.0));
    }

    #[test]
    fn edited_corner_resizes_from_the_opposite_corner() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, stroke_w: 4.0, fill: None };
        // Drag the SE corner to (150, 130): the NW corner (0,0) stays put.
        let k = edited_kind(&orig, Grab::Corner(Corner::Se), (100.0, 100.0), (150.0, 130.0), (10000.0, 10000.0), false);
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.0, 0.0, 150.0, 130.0));
    }

    #[test]
    fn edited_corner_is_relative_so_an_offset_handle_press_has_no_jump() {
        // The resize handle is drawn ~4px OUTSIDE the object, so the press lands off the
        // corner. A RELATIVE edit moves the corner by the drag DELTA (not to the cursor's
        // absolute position), so grabbing the offset handle and dragging 10px enlarges by
        // exactly 10px with no snap.
        let orig = AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, stroke_w: 4.0, fill: None };
        // Press 4px past the SE corner (the outward handle), drag +10,+10.
        let k = edited_kind(&orig, Grab::Corner(Corner::Se), (104.0, 104.0), (114.0, 114.0), (10000.0, 10000.0), false);
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.0, 0.0, 110.0, 110.0), "no 4px jump");
    }

    #[test]
    fn edited_edge_moves_only_that_side() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, stroke_w: 4.0, fill: None };
        let k = edited_kind(&orig, Grab::Edge(Edge::E), (100.0, 50.0), (140.0, 50.0), (10000.0, 10000.0), false);
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.0, 0.0, 140.0, 100.0));
    }

    #[test]
    fn edited_arrow_endpoints_and_move() {
        let orig = AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 0.0 }, b: AnnotPoint { x: 100.0, y: 0.0 }, stroke_w: 6.0 };
        // Drag endpoint B to (120, 30): A unchanged.
        let k = edited_kind(&orig, Grab::ArrowB, (100.0, 0.0), (120.0, 30.0), (10000.0, 10000.0), false);
        let AnnotKind::Arrow { a, b, .. } = k else { panic!() };
        assert_eq!((a.x, a.y, b.x, b.y), (0.0, 0.0, 120.0, 30.0));
        // Move translates both.
        let k = edited_kind(&orig, Grab::Move, (0.0, 0.0), (10.0, 5.0), (10000.0, 10000.0), false);
        let AnnotKind::Arrow { a, b, .. } = k else { panic!() };
        assert_eq!((a.x, a.y, b.x, b.y), (10.0, 5.0, 110.0, 5.0));
    }

    #[test]
    fn degenerate_shapes_are_rejected() {
        assert!(is_degenerate(&boxed(1, 0.0, 0.0, 1.0, 50.0)), "1px-wide box is degenerate");
        assert!(!is_degenerate(&boxed(1, 0.0, 0.0, 20.0, 20.0)));
        let tiny_arrow = AnnotationItem {
            id: AnnotId(2),
            color: [0; 4],
            kind: AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 0.0 }, b: AnnotPoint { x: 1.0, y: 1.0 }, stroke_w: 4.0 },
        };
        assert!(is_degenerate(&tiny_arrow), "a near-zero-length arrow is degenerate");
    }

    #[test]
    fn rasterize_scene_draws_something_and_apply_composites() {
        let items = vec![
            boxed(1, 10.0, 10.0, 80.0, 60.0),
            AnnotationItem {
                id: AnnotId(2),
                color: [0, 128, 255, 255],
                kind: AnnotKind::Arrow { a: AnnotPoint { x: 5.0, y: 5.0 }, b: AnnotPoint { x: 90.0, y: 70.0 }, stroke_w: 6.0 },
            },
        ];
        let raster = rasterize_scene(&items, 100, 80, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("rasterizes");
        assert_eq!(raster.dimensions(), (100, 80));
        assert!(raster.pixels().any(|p| p.0[3] > 0), "scene rendered fully transparent");
        // Composite over an opaque base changes it.
        let mut base = RgbaImage::from_pixel(100, 80, ::image::Rgba([200, 200, 200, 255]));
        let before = base.clone();
        apply_annotations(&mut base, &items, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_ne!(base, before, "annotations left the base unchanged");
        // Empty scene is a no-op.
        let mut base2 = RgbaImage::from_pixel(10, 10, ::image::Rgba([0, 0, 0, 255]));
        let before2 = base2.clone();
        apply_annotations(&mut base2, &[], DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(base2, before2);
    }

    #[test]
    fn rasterize_handles_zero_and_near_zero_length_arrows_without_panicking() {
        // DRAGON-324 regression: the bake's `draw_arrow` sized its head with
        // `clamp(6*scale, len*0.7)`, which PANICS (min > max) on a very short arrow. A
        // zero-length arrow (draw-begin state, both endpoints equal) and a 1px arrow must
        // both rasterize without panicking — the head just shrinks to the cap.
        let arrows = vec![
            AnnotationItem {
                id: AnnotId(1),
                color: [255, 0, 0, 255],
                kind: AnnotKind::Arrow {
                    a: AnnotPoint { x: 40.0, y: 40.0 },
                    b: AnnotPoint { x: 40.0, y: 40.0 }, // zero length
                    stroke_w: 6.0,
                },
            },
            AnnotationItem {
                id: AnnotId(2),
                color: [0, 0, 255, 255],
                kind: AnnotKind::Arrow {
                    a: AnnotPoint { x: 10.0, y: 10.0 },
                    b: AnnotPoint { x: 11.0, y: 10.0 }, // 1px
                    stroke_w: 6.0,
                },
            },
        ];
        let raster =
            rasterize_scene(&arrows, 100, 80, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("rasterizes");
        assert_eq!(raster.dimensions(), (100, 80));
        // Full-res bake path (scale 1.0) must also not panic.
        let mut base = RgbaImage::from_pixel(100, 80, ::image::Rgba([200, 200, 200, 255]));
        apply_annotations(&mut base, &arrows, DEFAULT_ANNOT_CURVE_RADIUS);
    }

    #[test]
    fn edits_clamp_to_the_image_bounds() {
        let frame = (200.0, 100.0);
        let orig = AnnotKind::Box {
            rect: AnnotRect { x: 150.0, y: 50.0, w: 40.0, h: 40.0 },
            stroke_w: 4.0,
            fill: None,
        };
        // MOVE far past the edges: the WHOLE box + its stroke/2 margin (stroke 4 → 2) pins inside,
        // so the geometry stops 2px short of the raw edge (x ≤ 158, y ≤ 58).
        let AnnotKind::Box { rect, .. } =
            edited_kind(&orig, Grab::Move, (0.0, 0.0), (500.0, 500.0), frame, false)
        else {
            panic!()
        };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (158.0, 58.0, 40.0, 40.0));
        // RESIZE the SE corner past the edge: the corner clamps to the image edge LESS the margin,
        // so the drawn stroke edge lands exactly on the image edge.
        let AnnotKind::Box { rect, .. } = edited_kind(
            &orig,
            Grab::Corner(crate::geometry::Corner::Se),
            (190.0, 90.0),
            (500.0, 500.0),
            frame,
            false,
        ) else {
            panic!()
        };
        assert_eq!((rect.x + rect.w, rect.y + rect.h), (198.0, 98.0));
        // DRAW clamps its two points (the New path uses from_points on clamped points).
        let pr = (10.0_f32.clamp(0.0, frame.0), 10.0_f32.clamp(0.0, frame.1));
        let cur = (999.0_f32.clamp(0.0, frame.0), 999.0_f32.clamp(0.0, frame.1));
        let r = AnnotRect::from_points(pr, cur);
        assert_eq!((r.x, r.y, r.x + r.w, r.y + r.h), (10.0, 10.0, 200.0, 100.0));
        // A Highlight edits via the SAME clamped rect path.
        let hl = AnnotKind::Highlight { rect: AnnotRect { x: 150.0, y: 50.0, w: 40.0, h: 40.0 } };
        let AnnotKind::Highlight { rect } =
            edited_kind(&hl, Grab::Move, (0.0, 0.0), (500.0, 500.0), frame, false)
        else {
            panic!()
        };
        assert_eq!((rect.x, rect.y), (160.0, 60.0), "highlight clamps like a box");
    }

    #[test]
    fn widget_items_carries_appearance_for_vector_draw() {
        // DRAGON-324: the widget draws shapes as vectors, so `widget_items` must ferry each
        // shape's appearance (color / fill / highlight / shared curve) alongside its geometry.
        let items = vec![
            AnnotationItem {
                id: AnnotId(1),
                color: [10, 20, 30, 255],
                kind: AnnotKind::Box {
                    rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 },
                    stroke_w: 4.0,
                    fill: None,
                },
            },
            AnnotationItem {
                id: AnnotId(2),
                color: [200, 100, 50, 255],
                kind: AnnotKind::Highlight { rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 } },
            },
        ];
        let w = widget_items(&items, 7.0, &[]);
        // The shared curve radius rides onto every item.
        assert_eq!(w[0].curve_radius, 7.0);
        assert_eq!(w[1].curve_radius, 7.0);
        // A box: vector-drawn (fx None), outline stroke at full alpha, no fill.
        assert_eq!(w[0].fx, FxKind::None);
        assert!(w[0].fill.is_none());
        assert!((w[0].color.a - 1.0).abs() < 1e-3, "box stroke is opaque");
        assert!(w[0].stroke_w > 0.0);
        // A highlight: flagged for the multiply shader pass, zero stroke (chrome offset).
        assert_eq!(w[1].fx, FxKind::Highlight);
        assert_eq!(w[1].stroke_w, 0.0);
        // Pixelate + blur carry their own fx flags (rect geometry, no stroke).
        let redactions = widget_items(
            &[
                AnnotationItem {
                    id: AnnotId(3),
                    color: [0; 4],
                    kind: AnnotKind::Pixelate { rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 } },
                },
                AnnotationItem {
                    id: AnnotId(4),
                    color: [0; 4],
                    kind: AnnotKind::Blur { rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 } },
                },
            ],
            7.0,
            &[],
        );
        assert_eq!(redactions[0].fx, FxKind::Pixelate);
        assert_eq!(redactions[1].fx, FxKind::Blur);
    }

    #[test]
    fn highlight_adaptive_display_matches_bake() {
        // WYSIWYG (DRAGON-326 adaptive): the display shader picks multiply vs screen from the
        // low-pass BACKGROUND luminance (`w = smoothstep(0.35, 0.65, luma(bg))`), blends
        // `mix(screen, multiply, w)`, then composites `mix(base, blended, alpha)`. This encodes
        // that formula INDEPENDENTLY and asserts `adaptive_highlight_px` agrees across samples,
        // so display + bake can't drift.
        let luma = |c: [u8; 3]| {
            0.2126 * (c[0] as f32 / 255.0)
                + 0.7152 * (c[1] as f32 / 255.0)
                + 0.0722 * (c[2] as f32 / 255.0)
        };
        let ss = |x: f32| {
            let t = ((x - 0.35) / (0.65 - 0.35)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        for base in [[0u8, 0, 0], [255, 255, 255], [128, 64, 200], [10, 240, 33]] {
            for color in [[255u8, 255, 0], [0, 128, 255], [200, 50, 50]] {
                for bg in [[0u8, 0, 0], [128, 128, 128], [255, 255, 255], [30, 40, 200]] {
                    for alpha in [0u8, 64, HIGHLIGHT_ALPHA, 255] {
                        let a = alpha as f32 / 255.0;
                        let w = ss(luma(bg));
                        let display: [u8; 3] = std::array::from_fn(|i| {
                            let b = base[i] as f32 / 255.0;
                            let c = color[i] as f32 / 255.0;
                            let mult = b * c;
                            let screen = 1.0 - (1.0 - b) * (1.0 - c);
                            let blended = screen + (mult - screen) * w;
                            ((b + (blended - b) * a) * 255.0).round().clamp(0.0, 255.0) as u8
                        });
                        // Non-overlap WYSIWYG: backdrop == operand == base.
                        assert_eq!(
                            display,
                            adaptive_highlight_px(base, base, color, bg, alpha),
                            "base={base:?} color={color:?} bg={bg:?} a={alpha}"
                        );
                    }
                }
            }
        }
        // alpha 0 is a no-op on the backdrop.
        assert_eq!(
            adaptive_highlight_px([200, 100, 50], [1, 2, 3], [10, 20, 30], [255, 255, 255], 0),
            [200, 100, 50]
        );
        // Backdrop ≠ operand (highlight over a redaction, at the real translucent alpha so the
        // backdrop shows through): the COLOR acts on the operand but the result composites over
        // the backdrop — so a redacted backdrop is never replaced by the pristine operand.
        let over_redacted = adaptive_highlight_px(
            [90, 90, 90], [10, 20, 30], [255, 255, 0], [200, 200, 200], HIGHLIGHT_ALPHA,
        );
        let over_plain = adaptive_highlight_px(
            [10, 20, 30], [10, 20, 30], [255, 255, 0], [200, 200, 200], HIGHLIGHT_ALPHA,
        );
        assert_ne!(over_redacted, over_plain, "the backdrop, not the operand, is what shows");
        // DARK background → SCREEN: a colored highlight over black stays visibly colored.
        let dark = adaptive_highlight_px([0, 0, 0], [0, 0, 0], [200, 60, 220], [0, 0, 0], 255);
        assert!(dark[0] > 150 && dark[2] > 150, "screen on dark keeps the color visible: {dark:?}");
        // LIGHT background → MULTIPLY: dark text (low operand) on a light panel stays legible.
        let light_text =
            adaptive_highlight_px([20, 20, 20], [20, 20, 20], [255, 255, 0], [245, 245, 245], 255);
        assert!(light_text[2] < 40, "multiply on light keeps dark text dark: {light_text:?}");
    }

    #[test]
    fn block_means_destroys_sub_block_detail() {
        // A 4×4 image split into 2×2 blocks of a single block size 2: each block collapses to
        // its mean, so a checkerboard within a block averages to grey (detail unrecoverable).
        let mut img = RgbaImage::new(2, 2);
        img.put_pixel(0, 0, ::image::Rgba([0, 0, 0, 255]));
        img.put_pixel(1, 0, ::image::Rgba([200, 200, 200, 255]));
        img.put_pixel(0, 1, ::image::Rgba([200, 200, 200, 255]));
        img.put_pixel(1, 1, ::image::Rgba([0, 0, 0, 255]));
        let m = block_means(&img, 2);
        assert_eq!(m.dimensions(), (1, 1));
        assert_eq!(m.get_pixel(0, 0).0, [100, 100, 100, 255], "block mean of the checker");
        // A finer block (1) is an identity downsample — dims and pixels preserved.
        let id = block_means(&img, 1);
        assert_eq!(id.dimensions(), (2, 2));
        assert_eq!(id.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }

    fn gen_img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
        let mut im = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                im.put_pixel(x, y, ::image::Rgba(f(x, y)));
            }
        }
        im
    }

    #[test]
    fn content_pixelate_block_scales_with_feature_size_and_floors_flat() {
        let full =
            |im: &RgbaImage| AnnotRect { x: 0.0, y: 0.0, w: im.width() as f32, h: im.height() as f32 };
        // Flat region → the floor (nothing to destroy, cheapest on the GPU).
        let flat = gen_img(128, 128, |_, _| [130, 130, 130, 255]);
        assert_eq!(content_pixelate_block(&flat, &full(&flat)), PIXELATE_BLOCK, "flat → floor");
        // Fine, high-frequency checker (2px cells) → a SMALL block (at the floor).
        let fine = gen_img(128, 128, |x, y| {
            if ((x / 2) + (y / 2)) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }
        });
        let fine_b = content_pixelate_block(&fine, &full(&fine));
        // Coarse checker (20px cells — deliberately NOT a multiple of the 32px tile, so tiles straddle
        // cells instead of landing inside a uniform one) → a LARGER block than the fine one.
        let coarse = gen_img(256, 256, |x, y| {
            if ((x / 20) + (y / 20)) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }
        });
        let coarse_b = content_pixelate_block(&coarse, &full(&coarse));
        assert!(coarse_b > fine_b, "coarser content → larger cell (fine={fine_b}, coarse={coarse_b})");
        // Always within [floor, ceiling], snapped to a multiple of 4.
        for b in [fine_b, coarse_b] {
            assert!((PIXELATE_BLOCK..=PIXELATE_BLOCK_MAX).contains(&b), "block {b} in range");
            assert_eq!(b % 4, 0, "block {b} snapped to a multiple of 4");
        }
        // REGION-SIZE INDEPENDENCE (the bug fix): the SAME content yields the SAME cell whether the
        // selection is small or large — a single line and a whole paragraph of the same-size text no
        // longer disagree. This is EXACT for uniform (text-like) content, the case that matters:
        let line = AnnotRect { x: 0.0, y: 0.0, w: 48.0, h: 16.0 };
        assert_eq!(
            content_pixelate_block(&fine, &line),
            content_pixelate_block(&fine, &full(&fine)),
            "fine content: small region == large region",
        );
        // Coarse content stays COARSE at any selection size (its exact magnitude can shift when features
        // sit near the tile scale and tile differently — a synthetic-checker artifact, not text).
        for r in [AnnotRect { x: 0.0, y: 0.0, w: 128.0, h: 128.0 }, full(&coarse)] {
            assert!(
                content_pixelate_block(&coarse, &r) > fine_b,
                "coarse content → coarse cell at any selection size",
            );
        }
        // STABILITY (the 1px bug): a 1px change in the selection HEIGHT must NOT flip the block. Use a
        // TEXT-LIKE image — 12px bands of fine vertical stripes ("lines") separated by 12px gaps — so
        // the busy rows and blank rows interleave exactly like real text, then sweep the height and
        // require the block never to jump by more than one snap step.
        let banded = gen_img(256, 200, |x, y| {
            if (y / 12) % 2 == 0 {
                if (x / 2) % 2 == 0 { [20, 20, 20, 255] } else { [235, 235, 235, 255] }
            } else {
                [245, 245, 245, 255]
            }
        });
        let mut prev: Option<u32> = None;
        for hgt in 24..80 {
            let b = content_pixelate_block(
                &banded,
                &AnnotRect { x: 0.0, y: 0.0, w: 256.0, h: hgt as f32 },
            );
            if let Some(pb) = prev {
                assert!(
                    (b as i32 - pb as i32).abs() <= 4,
                    "a 1px height change flipped the block: {pb} → {b} at h={hgt}",
                );
            }
            prev = Some(b);
        }
        // ...and the SAME under a 1px WIDTH change (left/right resize). Mirror image — 12px bands of
        // fine HORIZONTAL stripes ("columns") separated by 12px gaps, feature axis = cols — swept over
        // width. Same guarantee as the height sweep: dragging an edge one pixel can't flip the block.
        let vbanded = gen_img(200, 256, |x, y| {
            if (x / 12) % 2 == 0 {
                if (y / 2) % 2 == 0 { [20, 20, 20, 255] } else { [235, 235, 235, 255] }
            } else {
                [245, 245, 245, 255]
            }
        });
        let mut prev: Option<u32> = None;
        for wid in 24..80 {
            let b = content_pixelate_block(
                &vbanded,
                &AnnotRect { x: 0.0, y: 0.0, w: wid as f32, h: 256.0 },
            );
            if let Some(pb) = prev {
                assert!(
                    (b as i32 - pb as i32).abs() <= 4,
                    "a 1px width change flipped the block: {pb} → {b} at w={wid}",
                );
            }
            prev = Some(b);
        }
        // MOVE stability (the reported jitter): sliding the SAME-size selection a few px right/down
        // must NOT flip the block. The analyzed rect quantizes, so sub-cell moves feed byte-identical
        // content — the block can't drift across a snap step and re-tile the whole mosaic frame-to-frame.
        let mut prev: Option<u32> = None;
        for off in 0..24 {
            let b = content_pixelate_block(
                &banded,
                &AnnotRect { x: off as f32, y: off as f32, w: 180.0, h: 90.0 },
            );
            if let Some(pb) = prev {
                assert!(
                    (b as i32 - pb as i32).abs() <= 4,
                    "a small move flipped the block: {pb} → {b} at off={off}",
                );
            }
            prev = Some(b);
        }
        // A multi-line "paragraph" of small text (the `banded` fine stripe-bands separated by blank
        // gaps) must NOT inflate from those line gaps — the TILE analysis absorbs them (a tile spans
        // several bands). This is the exact regression that per-row density caused. Its cell stays near
        // the fine end, nowhere near the coarse block.
        let para_b = content_pixelate_block(&banded, &full(&banded));
        assert!(
            para_b < coarse_b,
            "a plain paragraph must NOT inflate to a coarse cell (para={para_b}, coarse={coarse_b})",
        );
        // MIXED content → the DENSEST content drives the cell. A selection that's half big strokes
        // (top) and half fine detail (bottom): the fine (busiest) tiles win, so the cell is small
        // (nowhere near the all-coarse block). This is what keeps a paragraph's cell stable and crisp
        // even when a bit of coarse content shares the region. (Lower PICK_PCT to let coarse win.)
        let mixed = gen_img(128, 128, |x, y| {
            let on = if y < 64 {
                (x % 24) < 4 // big, low-frequency strokes (a header)
            } else {
                ((x / 2) + (y / 2)) % 2 == 0 // fine, dense body-text-like detail
            };
            if on { [0, 0, 0, 255] } else { [255, 255, 255, 255] }
        });
        let mixed_b = content_pixelate_block(&mixed, &full(&mixed));
        assert!(
            mixed_b < coarse_b,
            "the densest content drives the cell small, below the all-coarse block \
             (fine={fine_b}, coarse={coarse_b}, mixed={mixed_b})",
        );
        // Determinism (this is what keeps display == bake): same source region → same block.
        assert_eq!(coarse_b, content_pixelate_block(&coarse, &full(&coarse)));
        // A degenerate (sub-2px) rect → floor.
        assert_eq!(
            content_pixelate_block(&coarse, &AnnotRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
            PIXELATE_BLOCK
        );
    }

    #[test]
    fn box_blur_stack_is_stronger_and_smoother_than_one_pass() {
        // A checker whose cells (16px) are larger than the blur block (4px), so one pass leaves
        // structure that further passes keep smoothing.
        let im = gen_img(64, 64, |x, y| {
            let c = if ((x / 16) + (y / 16)) % 2 == 0 { 20 } else { 235 };
            [c, c, c, 255]
        });
        let one = box_blur_stack(&im, 4, 1);
        let three = box_blur_stack(&im, 4, BLUR_PASSES);
        assert_ne!(one, three, "3× differs from 1×");
        // Total variation of the luma along rows — lower means smoother.
        let tv = |img: &RgbaImage| -> u64 {
            let mut s = 0u64;
            for y in 0..img.height() {
                for x in 1..img.width() {
                    let a = img.get_pixel(x, y).0[0] as i64;
                    let b = img.get_pixel(x - 1, y).0[0] as i64;
                    s += (a - b).unsigned_abs();
                }
            }
            s
        };
        assert!(tv(&three) < tv(&one), "3× is smoother than 1× (tv3={} tv1={})", tv(&three), tv(&one));
        assert!(tv(&one) < tv(&im), "even one pass smooths the checker");
        // 0 passes is treated as 1 (never a no-op).
        assert_eq!(box_blur_stack(&im, 4, 0), one, "0 passes == 1 pass");
    }

    // ── the true-z-order CPU effect BAKE stack (DRAGON-330) ──────────────────────────
    // The DISPLAY of these effects is now a real-time GPU shader (`widgets::annotation_fx`),
    // headless-unverifiable, so these tests pin the CPU BAKE (`apply_effects`) — the exact save
    // path — only. A `effect` helper builds an item with an explicit id/kind/color.
    fn effect(id: u64, kind: AnnotKind, color: AnnotColor) -> AnnotationItem {
        AnnotationItem { id: AnnotId(id), color, kind }
    }

    #[test]
    fn effects_bake_mutates_only_their_region() {
        // A flat mid-grey base; a pixelate + a blur + a highlight, each in a distinct corner,
        // walked through the ONE z-order core. Non-overlapping, so each sees the pristine base
        // in its region (byte-identical to the pre-DRAGON-330 grouped bake).
        let mut base = RgbaImage::from_pixel(64, 64, ::image::Rgba([120, 120, 120, 255]));
        base.put_pixel(4, 4, ::image::Rgba([255, 0, 0, 255]));
        let items = vec![
            effect(1, AnnotKind::Pixelate { rect: AnnotRect { x: 0.0, y: 0.0, w: 20.0, h: 20.0 } }, [0; 4]),
            effect(2, AnnotKind::Blur { rect: AnnotRect { x: 40.0, y: 0.0, w: 20.0, h: 20.0 } }, [0; 4]),
            effect(3, AnnotKind::Highlight { rect: AnnotRect { x: 0.0, y: 40.0, w: 20.0, h: 20.0 } }, [255, 0, 0, 255]),
        ];
        let before = base.clone();
        apply_effects(&mut base, &before, &items, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_ne!(base.get_pixel(4, 4), before.get_pixel(4, 4), "pixelate averaged the speck");
        let hl = base.get_pixel(4, 50).0;
        assert!(hl[0] > 120, "red highlight lifts red channel: {hl:?}");
        assert!(hl[1] < 120 && hl[2] < 120, "red highlight lowers green/blue: {hl:?}");
        assert_eq!(base.get_pixel(30, 30), before.get_pixel(30, 30), "gap left alone");
    }

    #[test]
    fn z_order_matters_pixelate_over_highlight_differs_from_highlight_over_pixelate() {
        // Base: flat grey. A RED highlight over the TOP HALF, a pixelate over the WHOLE region
        // (8×8 < PIXELATE_BLOCK, so one block). Order is everything:
        //   * pixelate ON TOP  → the whole region collapses to one block mean; the highlight's
        //     top/bottom boundary is REDACTED away (uniform).
        //   * highlight ON TOP → the pixelate flattens first, then the highlight paints the top
        //     half — the boundary is VISIBLE (top ≠ bottom).
        let base = RgbaImage::from_pixel(8, 8, ::image::Rgba([100, 100, 100, 255]));
        let hl = effect(1, AnnotKind::Highlight { rect: AnnotRect { x: 0.0, y: 0.0, w: 8.0, h: 4.0 } }, [255, 0, 0, 255]);
        let px = effect(2, AnnotKind::Pixelate { rect: AnnotRect { x: 0.0, y: 0.0, w: 8.0, h: 8.0 } }, [0; 4]);

        let mut a = base.clone(); // highlight below, pixelate on top
        apply_effects(&mut a, &base, &[hl.clone(), px.clone()], 0.0);
        let mut b = base.clone(); // pixelate below, highlight on top
        apply_effects(&mut b, &base, &[px, hl], 0.0);

        assert_ne!(a, b, "swapping z-order changes the composite");
        // Pixelate on top → the highlight boundary is gone (uniform block).
        assert_eq!(a.get_pixel(0, 0), a.get_pixel(0, 7), "pixelate-on-top redacts to one block");
        // Highlight on top → the boundary shows (tinted top over the flat redaction).
        assert_ne!(b.get_pixel(0, 0), b.get_pixel(0, 7), "highlight-on-top paints over the flat block");
    }

    #[test]
    fn destructive_samples_everything_below_it() {
        // A pixelate ON TOP of a highlight samples the ALREADY-HIGHLIGHTED accumulator, so its
        // block mean folds in (and thereby redacts) the highlight — the result differs from a
        // pixelate over the plain base, and is uniform (the highlight shape is destroyed).
        let base = RgbaImage::from_pixel(8, 8, ::image::Rgba([100, 100, 100, 255]));
        let hl = effect(1, AnnotKind::Highlight { rect: AnnotRect { x: 0.0, y: 0.0, w: 8.0, h: 4.0 } }, [255, 0, 0, 255]);
        let px = effect(2, AnnotKind::Pixelate { rect: AnnotRect { x: 0.0, y: 0.0, w: 8.0, h: 8.0 } }, [0; 4]);

        let mut with_hl = base.clone();
        apply_effects(&mut with_hl, &base, &[hl, px.clone()], 0.0);
        let mut only_px = base.clone();
        apply_effects(&mut only_px, &base, &[px], 0.0);

        // The highlight below was SEEN by the pixelate (its red averaged in): redder result.
        assert!(
            with_hl.get_pixel(0, 0).0[0] > only_px.get_pixel(0, 0).0[0],
            "the redaction folded in the highlight below it: {:?} vs {:?}",
            with_hl.get_pixel(0, 0).0,
            only_px.get_pixel(0, 0).0
        );
        // The highlight's own top/bottom boundary is gone — redacted into one block.
        assert_eq!(with_hl.get_pixel(0, 0), with_hl.get_pixel(0, 7), "highlight redacted to uniform");
    }

    #[test]
    fn apply_one_effect_scaled_at_one_equals_the_bake_core() {
        // The scaled core at 1.0 is the SAME accumulator mutation the bake runs — pinning the
        // byte-identity that keeps `apply_effects` (bake) untouched.
        let base = RgbaImage::from_pixel(40, 40, ::image::Rgba([100, 110, 120, 255]));
        let item = effect(
            1,
            AnnotKind::Pixelate { rect: AnnotRect { x: 2.0, y: 2.0, w: 30.0, h: 30.0 } },
            [0; 4],
        );
        let mut a = base.clone();
        apply_one_effect(&mut a, None, &base, &item, DEFAULT_ANNOT_CURVE_RADIUS);
        let mut b = base.clone();
        apply_one_effect_scaled(&mut b, None, &base, &item, DEFAULT_ANNOT_CURVE_RADIUS, 1.0);
        assert_eq!(a, b, "the scaled core at 1.0 is the full-res core");
    }

    #[test]
    fn no_effects_leaves_the_bake_unchanged() {
        // Box/arrow are not effects: the bake's effect stack leaves the base untouched (the
        // vectors composite separately via `apply_annotations`).
        let base = RgbaImage::from_pixel(16, 16, ::image::Rgba([70, 70, 70, 255]));
        let vectors = vec![
            effect(1, AnnotKind::Box { rect: AnnotRect { x: 1.0, y: 1.0, w: 8.0, h: 8.0 }, stroke_w: 4.0, fill: None }, [255, 0, 0, 255]),
            effect(2, AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 0.0 }, b: AnnotPoint { x: 9.0, y: 9.0 }, stroke_w: 4.0 }, [0, 0, 255, 255]),
        ];
        let mut baked = base.clone();
        apply_effects(&mut baked, &base, &vectors, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(baked, base, "vector-only scene leaves the effect accumulator untouched");
        // An empty scene is likewise a no-op.
        let mut empty = base.clone();
        apply_effects(&mut empty, &base, &[], DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(empty, base, "empty scene leaves the effect accumulator untouched");
    }

    #[test]
    fn only_box_arrow_highlight_are_colorable() {
        // A color change recolors these; pixelate/blur have no color and are skipped.
        let r = AnnotRect { x: 0.0, y: 0.0, w: 4.0, h: 4.0 };
        assert!(AnnotKind::Box { rect: r, stroke_w: 4.0, fill: None }.is_colorable());
        assert!(
            AnnotKind::Arrow {
                a: AnnotPoint { x: 0.0, y: 0.0 },
                b: AnnotPoint { x: 1.0, y: 1.0 },
                stroke_w: 4.0
            }
            .is_colorable()
        );
        assert!(AnnotKind::Highlight { rect: r }.is_colorable());
        assert!(!AnnotKind::Pixelate { rect: r }.is_colorable());
        assert!(!AnnotKind::Blur { rect: r }.is_colorable());
    }

    #[test]
    fn box_highlight_is_effect_bearing_and_yields_an_outline_vector() {
        // DRAGON-333: BoxHighlight is BOTH an effect (highlight fill) AND a vector (box outline).
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let bh = AnnotKind::BoxHighlight { rect, stroke_w: 5.0 };
        // The FILL feeds the effect pipeline, and the shared color applies (fill + outline tint).
        assert!(bh.is_effect(), "box-highlight's fill composites through the effect stack");
        assert!(bh.is_colorable(), "box-highlight uses the shared annotation color");
        // The OUTLINE is a real vector item: rect geometry, the box stroke width, NO vector fill
        // (the highlight IS the fill), flagged BoxHighlight so the canvas draws the outline yet
        // skips the fill.
        let items = vec![AnnotationItem {
            id: AnnotId(1),
            color: [10, 20, 30, 255],
            kind: AnnotKind::BoxHighlight { rect, stroke_w: 5.0 },
        }];
        let w = widget_items(&items, 8.0, &[]);
        assert_eq!(w[0].fx, FxKind::BoxHighlight);
        assert_eq!(w[0].stroke_w, 5.0, "the outline carries the box stroke width");
        assert!(w[0].fill.is_none(), "the highlight is the fill, not a vector fill");
        assert!(matches!(w[0].kind, ItemKind::Rect { .. }), "box-highlight hit-tests as a rect");
        assert_eq!(w[0].curve_radius, 8.0, "the shared curve rides onto the item");
    }

    #[test]
    fn box_highlight_bakes_a_highlight_fill_plus_an_outline() {
        // WYSIWYG: the FILL is byte-identical to a plain Highlight of the same rect + color (so
        // the GPU display shader — which feeds BoxHighlight as a Highlight FxItem — matches the
        // bake), and the OUTLINE rasterizes as a box stroke (which a plain highlight never does).
        let base = RgbaImage::from_pixel(40, 40, ::image::Rgba([120, 120, 120, 255]));
        let rect = AnnotRect { x: 5.0, y: 5.0, w: 30.0, h: 30.0 };
        let color = [255, 0, 0, 255];
        let mut bh = base.clone();
        apply_effects(
            &mut bh,
            &base,
            &[effect(1, AnnotKind::BoxHighlight { rect, stroke_w: 5.0 }, color)],
            DEFAULT_ANNOT_CURVE_RADIUS,
        );
        let mut hl = base.clone();
        apply_effects(
            &mut hl,
            &base,
            &[effect(1, AnnotKind::Highlight { rect }, color)],
            DEFAULT_ANNOT_CURVE_RADIUS,
        );
        assert_eq!(bh, hl, "box-highlight's fill is byte-identical to a plain highlight");
        assert_ne!(bh, base, "the highlight fill mutated the region");
        // The vector OUTLINE: rasterize_scene strokes the box (a plain highlight rasterizes empty).
        let outline = rasterize_scene(
            &[effect(1, AnnotKind::BoxHighlight { rect, stroke_w: 5.0 }, color)],
            40,
            40,
            1.0,
            DEFAULT_ANNOT_CURVE_RADIUS,
        )
        .expect("rasterizes");
        assert!(outline.pixels().any(|p| p.0[3] > 0), "box-highlight rasterizes an outline stroke");
        let plain = rasterize_scene(
            &[effect(1, AnnotKind::Highlight { rect }, color)],
            40,
            40,
            1.0,
            DEFAULT_ANNOT_CURVE_RADIUS,
        )
        .expect("rasterizes");
        assert!(plain.pixels().all(|p| p.0[3] == 0), "a plain highlight has no vector outline");
    }

    #[test]
    fn new_box_highlight_seeds_color_and_width() {
        // The Tool::BoxHighlight draw path seeds the selected line WIDTH onto the outline and the
        // shared COLOR onto the item (which drives BOTH the fill tint and the outline) — the same
        // single-source-of-truth the box/arrow tools use.
        let mut e = super::super::edit::EditState { annot_stroke_w: 8.0, ..Default::default() };
        let stroke_w = e.stroke();
        let color = [7, 8, 9, 255];
        let id = e.next_annot_id();
        e.annotations.push(AnnotationItem {
            id,
            color,
            kind: AnnotKind::BoxHighlight { rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 }, stroke_w },
        });
        let AnnotKind::BoxHighlight { stroke_w: got, .. } = &e.annotations[0].kind else {
            panic!("expected a box-highlight");
        };
        assert_eq!(*got, 8.0, "new box-highlight seeds the selected 8px width");
        assert_eq!(e.annotations[0].color, color, "the shared color rides on the item");
        // The width flows through to the outline vector item.
        let w = widget_items(&e.annotations, DEFAULT_ANNOT_CURVE_RADIUS, &[]);
        assert_eq!(w[0].stroke_w, 8.0);
    }

    #[test]
    fn rasterize_scene_skips_region_effect_kinds() {
        // The region effects are NOT source-over overlays — a highlight/pixelate/blur-only
        // scene rasterizes fully transparent (they composite through the CPU effect stack).
        let items = vec![
            AnnotationItem {
                id: AnnotId(1),
                color: [255, 0, 0, 255],
                kind: AnnotKind::Highlight { rect: AnnotRect { x: 5.0, y: 5.0, w: 40.0, h: 40.0 } },
            },
            AnnotationItem {
                id: AnnotId(2),
                color: [0; 4],
                kind: AnnotKind::Pixelate { rect: AnnotRect { x: 5.0, y: 5.0, w: 40.0, h: 40.0 } },
            },
            AnnotationItem {
                id: AnnotId(3),
                color: [0; 4],
                kind: AnnotKind::Blur { rect: AnnotRect { x: 5.0, y: 5.0, w: 40.0, h: 40.0 } },
            },
        ];
        let raster = rasterize_scene(&items, 60, 60, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("ok");
        assert!(raster.pixels().all(|p| p.0[3] == 0), "region effects are not rasterized here");
    }

    // ── the global dim / spotlight (DRAGON-329) ──────────────────────────────────────
    #[test]
    fn spotlight_is_not_effect_colorable_or_rasterized() {
        // Spotlight is a PURE knockout region: no effect composite, no color, no vector draw.
        let r = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        assert!(!AnnotKind::Spotlight { rect: r }.is_effect(), "spotlight composites nothing");
        assert!(!AnnotKind::Spotlight { rect: r }.is_colorable(), "spotlight has no color");
        // It rasterizes to NOTHING (like a highlight/pixelate — no source-over vector).
        let items = vec![effect(1, AnnotKind::Spotlight { rect: r }, [0; 4])];
        let raster = rasterize_scene(&items, 20, 20, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("ok");
        assert!(raster.pixels().all(|p| p.0[3] == 0), "spotlight draws no vector geometry");
        // The effect bake leaves the base untouched (spotlight is not an effect).
        let base = RgbaImage::from_pixel(20, 20, ::image::Rgba([120, 120, 120, 255]));
        let mut baked = base.clone();
        apply_effects(&mut baked, &base, &items, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(baked, base, "spotlight is invisible to the effect stack");
    }

    #[test]
    fn knockout_rects_are_the_right_union() {
        // The knockout union = spotlight + box + highlight + box-highlight; pixelate/blur/arrow
        // never knock out (they stay dimmed).
        let r = |n: f32| AnnotRect { x: n, y: 0.0, w: 5.0, h: 5.0 };
        let items = vec![
            effect(1, AnnotKind::Spotlight { rect: r(1.0) }, [0; 4]),
            effect(2, AnnotKind::Box { rect: r(2.0), stroke_w: 4.0, fill: None }, [1; 4]),
            effect(3, AnnotKind::Highlight { rect: r(3.0) }, [1; 4]),
            effect(4, AnnotKind::BoxHighlight { rect: r(4.0), stroke_w: 4.0 }, [1; 4]),
            effect(5, AnnotKind::Pixelate { rect: r(5.0) }, [0; 4]),
            effect(6, AnnotKind::Blur { rect: r(6.0) }, [0; 4]),
            effect(7, AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 0.0 }, b: AnnotPoint { x: 9.0, y: 9.0 }, stroke_w: 4.0 }, [1; 4]),
        ];
        let ko = knockout_rects(&items);
        assert_eq!(ko.len(), 4, "spotlight + box + highlight + box-highlight only");
        let xs: Vec<f32> = ko.iter().map(|r| r.x).collect();
        assert_eq!(xs, vec![1.0, 2.0, 3.0, 4.0], "pixelate/blur/arrow excluded");
    }

    #[test]
    fn dim_alpha_law_and_knockout_fade() {
        // dim_alpha = dim × (1 − cov): full dim with no knockout, zero inside a full knockout.
        assert_eq!(dim_alpha(0.6, 0.0), 0.6);
        assert!((dim_alpha(0.6, 1.0)).abs() < 1e-6, "full knockout removes the dim");
        assert!((dim_alpha(0.6, 0.5) - 0.3).abs() < 1e-6, "half coverage halves the dim");
        // Clamped both ways.
        assert_eq!(dim_alpha(2.0, -1.0), 1.0);
        assert_eq!(dim_alpha(-1.0, 0.0), 0.0);
    }

    #[test]
    fn apply_dim_darkens_except_knockouts_and_is_a_noop_at_zero() {
        let base = RgbaImage::from_pixel(60, 60, ::image::Rgba([200, 200, 200, 255]));
        // A spotlight knockout in the top-left corner; the rest dims.
        let ko = vec![AnnotRect { x: 0.0, y: 0.0, w: 20.0, h: 20.0 }];

        // dim == 0 (and even a spotlight present): byte-identical no-op.
        let mut none = base.clone();
        apply_dim(&mut none, 0.0, &ko, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(none, base, "dim==0 is byte-identical (no darkening anywhere)");

        // dim > 0: a knockout pixel stays bright, a far pixel darkens.
        let mut dimmed = base.clone();
        apply_dim(&mut dimmed, 0.5, &ko, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(dimmed.get_pixel(5, 5).0, [200, 200, 200, 255], "inside the knockout: bright");
        let far = dimmed.get_pixel(50, 50).0;
        assert!(far[0] < 200 && far[1] < 200 && far[2] < 200, "outside the knockout: dimmed {far:?}");
        // 0.5 dim over 200 → 100 (× (1−0.5)).
        assert_eq!(far, [100, 100, 100, 255], "dim halves the far pixel");
        // Alpha is never touched (non-destructive darkening).
        assert!(dimmed.pixels().all(|p| p.0[3] == 255));

        // No knockouts: the whole frame dims uniformly.
        let mut all = base.clone();
        apply_dim(&mut all, 0.5, &[], DEFAULT_ANNOT_CURVE_RADIUS);
        assert!(all.pixels().all(|p| p.0 == [100, 100, 100, 255]), "uniform dim with no knockouts");
    }

    #[test]
    fn bake_dim_matches_apply_dim_then_effects_ordering() {
        // The dim sits BELOW the effects: a pixelate over a DIMMED region reads dimmed content
        // (the mosaic is dimmed), while a spotlight knockout under a highlight keeps it bright.
        // Here: a flat base, dim 0.5, a pixelate with NO knockout → its block mean is dimmed.
        let base = RgbaImage::from_pixel(16, 16, ::image::Rgba([200, 200, 200, 255]));
        let px = effect(1, AnnotKind::Pixelate { rect: AnnotRect { x: 0.0, y: 0.0, w: 16.0, h: 16.0 } }, [0; 4]);
        let mut dimmed = base.clone();
        apply_dim(&mut dimmed, 0.5, &knockout_rects(std::slice::from_ref(&px)), 0.0);
        apply_effects(&mut dimmed, &base, std::slice::from_ref(&px), 0.0);
        // Pixelate is not a knockout, so it stays dimmed: the mosaic of a flat 100-grey is 100.
        assert!(dimmed.get_pixel(8, 8).0[0] <= 101, "pixelate over a dimmed region stays dim");
    }

    // ── pre-placed items: double-click a tray tool (DRAGON-339) ──────────────────────

    #[rstest]
    // A roomy image: the nominal 200×100, centred.
    #[case((1920, 1080), 0.0, (860.0, 490.0, 200.0, 100.0))]
    // Exactly big enough for the nominal size on both axes (250×125 → 80% = 200×100).
    #[case((250, 125), 0.0, (25.0, 12.5, 200.0, 100.0))]
    // A SMALL image: 80% wins on both axes, still centred (10% inset each side).
    #[case((100, 50), 0.0, (10.0, 5.0, 80.0, 40.0))]
    // Mixed: wide but short — width takes the nominal 200, height the 80% (independent axes).
    #[case((1000, 60), 0.0, (400.0, 6.0, 200.0, 48.0))]
    // A tiny image where the STROKE margin binds tighter than 80%: 10 − 2·2 = 6 of room.
    #[case((10, 10), 2.0, (2.0, 2.0, 6.0, 6.0))]
    // Degenerate frames never produce NaN/negative geometry.
    #[case((0, 0), 4.0, (0.0, 0.0, 0.0, 0.0))]
    fn placement_rect_fits_and_centers(
        #[case] frame: (u32, u32),
        #[case] margin: f32,
        #[case] want: (f32, f32, f32, f32),
    ) {
        let r = default_placement_rect(frame, margin);
        assert_eq!((r.x, r.y, r.w, r.h), want, "frame {frame:?} margin {margin}");
        // Invariants that must hold for EVERY frame: non-negative, inside the image, centred.
        assert!(r.w >= 0.0 && r.h >= 0.0);
        let (fw, fh) = (frame.0 as f32, frame.1 as f32);
        assert!(r.x >= 0.0 && r.x + r.w <= fw + f32::EPSILON, "inside horizontally");
        assert!(r.y >= 0.0 && r.y + r.h <= fh + f32::EPSILON, "inside vertically");
        assert!(((r.x + r.w * 0.5) - fw * 0.5).abs() < 1e-3, "centred horizontally");
        assert!(((r.y + r.h * 0.5) - fh * 0.5).abs() < 1e-3, "centred vertically");
    }

    #[test]
    fn spawn_kind_maps_every_tool_onto_the_placement_rect() {
        let r = AnnotRect { x: 10.0, y: 20.0, w: 200.0, h: 100.0 };
        assert_eq!(spawn_kind(Tool::Rect, r, 4.0), Some(AnnotKind::Box { rect: r, stroke_w: 4.0, fill: None }));
        assert_eq!(spawn_kind(Tool::Highlight, r, 4.0), Some(AnnotKind::Highlight { rect: r }));
        assert_eq!(
            spawn_kind(Tool::BoxHighlight, r, 4.0),
            Some(AnnotKind::BoxHighlight { rect: r, stroke_w: 4.0 })
        );
        assert_eq!(spawn_kind(Tool::Spotlight, r, 4.0), Some(AnnotKind::Spotlight { rect: r }));
        assert_eq!(spawn_kind(Tool::Pixelate, r, 4.0), Some(AnnotKind::Pixelate { rect: r }));
        assert_eq!(spawn_kind(Tool::Blur, r, 4.0), Some(AnnotKind::Blur { rect: r }));
        // The arrow spans the rect corner-to-corner, so it reads as an arrow (not a dot).
        assert_eq!(
            spawn_kind(Tool::Arrow, r, 4.0),
            Some(AnnotKind::Arrow {
                a: AnnotPoint { x: 10.0, y: 20.0 },
                b: AnnotPoint { x: 210.0, y: 120.0 },
                stroke_w: 4.0,
            })
        );
        // The NON-creating tools spawn NOTHING — double-clicking their tray button must only
        // pick the tool. The pencil/eraser have been that way since DRAGON-338; the POINTER
        // joins them in DRAGON-341 (it is pure selection).
        assert_eq!(spawn_kind(Tool::Pen, r, 4.0), None);
        assert_eq!(spawn_kind(Tool::Eraser, r, 4.0), None);
        assert_eq!(spawn_kind(Tool::Pointer, r, 4.0), None, "the pointer places nothing");
        // The badge is the one kind that does NOT take the placement rect verbatim: it squares
        // down to the rect's shorter axis, centred (DRAGON-340). See
        // `a_pre_placed_badge_is_a_centred_square`.
        assert_eq!(
            spawn_kind(Tool::Badge, r, 4.0),
            Some(AnnotKind::Badge {
                rect: AnnotRect { x: 60.0, y: 20.0, w: 100.0, h: 100.0 },
                ring_w: 4.0,
            })
        );
    }

    // ── Multi-selection geometry (DRAGON-341) ────────────────────────────────────────

    #[test]
    fn a_committed_pen_stroke_never_selects_itself_but_every_shape_does() {
        // DRAGON-341 follow-up: drawing with the pencil must leave NO selection chrome on the
        // ink — pen selection belongs to pointer mode alone. Every other kind keeps the
        // historical draw-then-selected behavior (you draw a box to then nudge/resize it).
        let r = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        for kind in [
            AnnotKind::Box { rect: r, stroke_w: 4.0, fill: None },
            AnnotKind::Highlight { rect: r },
            AnnotKind::BoxHighlight { rect: r, stroke_w: 4.0 },
            AnnotKind::Spotlight { rect: r },
            AnnotKind::Pixelate { rect: r },
            AnnotKind::Blur { rect: r },
            AnnotKind::Arrow {
                a: AnnotPoint { x: 0.0, y: 0.0 },
                b: AnnotPoint { x: 9.0, y: 9.0 },
                stroke_w: 4.0,
            },
            AnnotKind::Badge { rect: r, ring_w: 4.0 },
        ] {
            assert!(kind_selects_on_create(&kind), "{kind:?} selects on create");
        }
        let pen = AnnotKind::Pen {
            paths: vec![vec![AnnotPoint { x: 1.0, y: 1.0 }, AnnotPoint { x: 8.0, y: 8.0 }]],
            pressure: Vec::new(),
            stroke_w: 4.0,
        };
        assert!(!kind_selects_on_create(&pen), "freehand ink just lands — never selected");
    }

    #[test]
    fn group_move_delta_clamps_once_on_the_union_so_the_arrangement_stays_rigid() {
        // Union bounds (20,20)-(120,70) in a 200×100 frame: the group may travel −20 left,
        // +80 right, −20 up, +30 down.
        let b = AnnotRect { x: 20.0, y: 20.0, w: 100.0, h: 50.0 };
        let frame = (200.0, 100.0);
        assert_eq!(group_move_delta(b, frame, (10.0, 5.0)), (10.0, 5.0), "inside the range: verbatim");
        assert_eq!(group_move_delta(b, frame, (-500.0, -500.0)), (-20.0, -20.0), "pinned at the near edges");
        assert_eq!(group_move_delta(b, frame, (500.0, 500.0)), (80.0, 30.0), "pinned at the far edges");
        // Axes clamp INDEPENDENTLY — a group pinned horizontally still slides vertically.
        assert_eq!(group_move_delta(b, frame, (500.0, -5.0)), (80.0, -5.0));
        // A union WIDER than the frame has no valid range on that axis, so the delta passes
        // through rather than snapping the group somewhere it never was.
        let wide = AnnotRect { x: -10.0, y: 20.0, w: 400.0, h: 50.0 };
        assert_eq!(group_move_delta(wide, frame, (7.0, 500.0)), (7.0, 30.0));
    }

    #[test]
    fn a_group_move_translates_every_kind_by_the_same_delta() {
        // Every kind translates verbatim (the shared delta is already clamped), so the
        // arrangement is rigid — relative offsets are preserved exactly.
        let r = AnnotRect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 };
        let moved = translated_kind(&AnnotKind::Box { rect: r, stroke_w: 4.0, fill: None }, 5.0, -3.0);
        assert_eq!(
            moved,
            AnnotKind::Box {
                rect: AnnotRect { x: 15.0, y: 17.0, w: 30.0, h: 40.0 },
                stroke_w: 4.0,
                fill: None
            }
        );
        let arrow = AnnotKind::Arrow {
            a: AnnotPoint { x: 0.0, y: 0.0 },
            b: AnnotPoint { x: 10.0, y: 10.0 },
            stroke_w: 2.0,
        };
        assert_eq!(
            translated_kind(&arrow, -4.0, 6.0),
            AnnotKind::Arrow {
                a: AnnotPoint { x: -4.0, y: 6.0 },
                b: AnnotPoint { x: 6.0, y: 16.0 },
                stroke_w: 2.0,
            }
        );
        let pen = AnnotKind::Pen {
            paths: vec![vec![AnnotPoint { x: 1.0, y: 1.0 }, AnnotPoint { x: 3.0, y: 5.0 }]],
            pressure: Vec::new(),
            stroke_w: 4.0,
        };
        let AnnotKind::Pen { paths, .. } = translated_kind(&pen, 2.0, 2.0) else {
            panic!("a translated pen is still a pen");
        };
        assert_eq!(paths[0][0], AnnotPoint { x: 3.0, y: 3.0 });
        assert_eq!(paths[0][1], AnnotPoint { x: 5.0, y: 7.0 });
        // A zero delta is the identity for every kind (a click that never dragged).
        for k in [AnnotKind::Highlight { rect: r }, arrow.clone(), pen.clone()] {
            assert_eq!(translated_kind(&k, 0.0, 0.0), k);
        }
    }

    // ── Duplicate (DRAGON-356) ───────────────────────────────────────────────────────

    #[test]
    fn single_dup_offset_nudges_toward_the_frame_center_scaled_to_the_image() {
        // The historical single-item rule (lifted verbatim into `single_dup_offset`): an equal
        // x/y offset TOWARD the frame center, `frame.min * 0.04` clamped to [16, 64].
        // A 1000×1000 image → 40px; an item in the top-left nudges DOWN-RIGHT (+,+).
        assert_eq!(single_dup_offset((100.0, 100.0), (1000.0, 1000.0)), (40.0, 40.0));
        // An item past the center on both axes nudges back UP-LEFT (−,−).
        assert_eq!(single_dup_offset((900.0, 900.0), (1000.0, 1000.0)), (-40.0, -40.0));
        // Mixed: right of center but above it → (−x, +y).
        assert_eq!(single_dup_offset((900.0, 100.0), (1000.0, 1000.0)), (-40.0, 40.0));
        // The magnitude clamps: a tiny image floors at 16, a huge one caps at 64.
        assert_eq!(single_dup_offset((10.0, 10.0), (100.0, 100.0)), (16.0, 16.0));
        assert_eq!(single_dup_offset((10.0, 10.0), (5000.0, 5000.0)), (64.0, 64.0));
        // On the exact center line the `>=` tie resolves to the POSITIVE direction, matching the
        // pre-DRAGON-356 `if fw*0.5 >= cx` branch byte-for-byte.
        assert_eq!(single_dup_offset((500.0, 500.0), (1000.0, 1000.0)), (40.0, 40.0));
    }

    #[test]
    fn group_dup_offset_clamps_the_shared_delta_on_the_union() {
        // The primary sits top-left so the raw nudge is (+16, +16) in this 200×200 image
        // (200*0.04 = 8, floored to 16). A union already near the far edge can only travel part
        // of the way before it would push the arrangement out of the picture.
        let frame = (200.0, 200.0);
        // Union with room to move: the full (+16,+16) applies.
        let roomy = AnnotRect { x: 20.0, y: 20.0, w: 40.0, h: 40.0 };
        assert_eq!(group_dup_offset((30.0, 30.0), roomy, frame), (16.0, 16.0));
        // Union hard against the far edge (right edge at x=195): only +5 of the +16 fits, so the
        // shared delta is pinned — the WHOLE group stops at the edge, never distorts past it.
        let tight = AnnotRect { x: 100.0, y: 20.0, w: 95.0, h: 40.0 };
        assert_eq!(group_dup_offset((30.0, 30.0), tight, frame), (5.0, 16.0));
    }

    #[test]
    fn a_group_duplicate_preserves_member_relative_positions_exactly() {
        // Three members; the shared clamped delta is applied verbatim to every copy, so the
        // vectors BETWEEN members are identical before and after — the arrangement is rigid.
        let a = AnnotKind::Box { rect: AnnotRect { x: 20.0, y: 20.0, w: 30.0, h: 30.0 }, stroke_w: 4.0, fill: None };
        let b = AnnotKind::Highlight { rect: AnnotRect { x: 90.0, y: 60.0, w: 20.0, h: 20.0 } };
        let c = AnnotKind::Arrow { a: AnnotPoint { x: 40.0, y: 100.0 }, b: AnnotPoint { x: 70.0, y: 130.0 }, stroke_w: 2.0 };
        let members = [&a, &b, &c];
        let union = group_drawn_bounds(members).expect("non-empty");
        // Primary = `a` (top-left), image large enough that the nudge applies unclamped.
        let (dx, dy) = group_dup_offset(kind_center(&a), union, (1000.0, 1000.0));
        assert_eq!((dx, dy), (40.0, 40.0), "roomy image: the raw nudge applies verbatim");
        let copies: Vec<AnnotKind> = members.iter().map(|k| translated_kind(k, dx, dy)).collect();
        // The relative vector between each pair of member CENTERS is unchanged by the shared move.
        let rel = |x: &AnnotKind, y: &AnnotKind| {
            let (cx0, cy0) = kind_center(x);
            let (cx1, cy1) = kind_center(y);
            (cx1 - cx0, cy1 - cy0)
        };
        assert_eq!(rel(&a, &b), rel(&copies[0], &copies[1]), "A→B vector preserved");
        assert_eq!(rel(&a, &c), rel(&copies[0], &copies[2]), "A→C vector preserved");
        assert_eq!(rel(&b, &c), rel(&copies[1], &copies[2]), "B→C vector preserved");
        // And every copy is offset by EXACTLY the shared delta from its source.
        for (src, cp) in members.iter().zip(&copies) {
            let (sx, sy) = kind_center(src);
            let (dx2, dy2) = kind_center(cp);
            assert_eq!((dx2 - sx, dy2 - sy), (dx, dy), "each copy moves by the shared delta");
        }
    }

    #[test]
    fn single_dup_offset_matches_the_historical_edited_kind_move() {
        // Single-item equivalence: `single_dup_offset` + `edited_kind(Move, false)` reproduces the
        // pre-DRAGON-356 duplicate EXACTLY — the offset formula and the per-item clamp are both
        // the original code, just relocated.
        let src = AnnotKind::Box { rect: AnnotRect { x: 100.0, y: 100.0, w: 50.0, h: 50.0 }, stroke_w: 4.0, fill: None };
        let frame: (f32, f32) = (1000.0, 1000.0);
        // The OLD inline computation, verbatim.
        let off = (frame.0.min(frame.1) * 0.04).clamp(16.0, 64.0);
        let (cx, cy) = kind_center(&src);
        let old_dx = if frame.0 * 0.5 >= cx { off } else { -off };
        let old_dy = if frame.1 * 0.5 >= cy { off } else { -off };
        let old_copy = edited_kind(&src, Grab::Move, (0.0, 0.0), (old_dx, old_dy), frame, false);
        // The NEW path.
        let (dx, dy) = single_dup_offset(kind_center(&src), frame);
        let new_copy = edited_kind(&src, Grab::Move, (0.0, 0.0), (dx, dy), frame, false);
        assert_eq!((dx, dy), (old_dx, old_dy));
        assert_eq!(new_copy, old_copy);
    }

    #[test]
    fn dup_selection_order_puts_the_primary_copy_last() {
        // The copies land in scene order; the copy of the old primary must be LAST so it becomes
        // the new primary (handle-wearing), whatever position the primary held among them.
        let ids: Vec<AnnotId> = [10, 11, 12, 13].into_iter().map(AnnotId).collect();
        // Primary copy in the middle → moved to the end, the rest keep their order.
        assert_eq!(
            dup_selection_order(&ids, AnnotId(12)),
            vec![AnnotId(10), AnnotId(11), AnnotId(13), AnnotId(12)],
        );
        // Already last → unchanged.
        assert_eq!(
            dup_selection_order(&ids, AnnotId(13)),
            vec![AnnotId(10), AnnotId(11), AnnotId(12), AnnotId(13)],
        );
        // A single copy → itself, and it is the primary.
        assert_eq!(dup_selection_order(&[AnnotId(7)], AnnotId(7)), vec![AnnotId(7)]);
    }

    #[test]
    fn group_bounds_union_covers_every_member_including_its_stroke() {
        // A box's drawn extent includes half its stroke, so the union grows past the geometry.
        let a = AnnotKind::Box {
            rect: AnnotRect { x: 20.0, y: 20.0, w: 20.0, h: 20.0 },
            stroke_w: 8.0,
            fill: None,
        };
        let b = AnnotKind::Highlight { rect: AnnotRect { x: 100.0, y: 60.0, w: 10.0, h: 10.0 } };
        let u = group_drawn_bounds([&a, &b]).expect("a non-empty selection has bounds");
        assert_eq!((u.x, u.y), (16.0, 16.0), "the box's stroke overhang is included");
        assert_eq!((u.x + u.w, u.y + u.h), (110.0, 70.0), "the far member sets the far edge");
        // An EMPTY selection has no bounds at all (the group gesture refuses to open).
        assert_eq!(group_drawn_bounds(std::iter::empty()), None);
    }

    #[test]
    fn band_select_takes_what_it_touches_and_nothing_it_only_encloses_emptily() {
        let band = AnnotRect { x: 50.0, y: 50.0, w: 40.0, h: 40.0 };
        let mk = |id: u64, kind: AnnotKind| AnnotationItem { id: AnnotId(id), color: [1, 2, 3, 4], kind };
        let items = vec![
            // 1: a box OVERLAPPING the band's corner.
            mk(1, AnnotKind::Box { rect: AnnotRect { x: 80.0, y: 80.0, w: 30.0, h: 30.0 }, stroke_w: 4.0, fill: None }),
            // 2: a box far away.
            mk(2, AnnotKind::Box { rect: AnnotRect { x: 300.0, y: 300.0, w: 10.0, h: 10.0 }, stroke_w: 4.0, fill: None }),
            // 3: an arrow whose SHAFT crosses the band though both ends sit outside it.
            mk(3, AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 70.0 }, b: AnnotPoint { x: 200.0, y: 70.0 }, stroke_w: 4.0 }),
            // 4: an "L" pen whose bbox SPANS the band but whose ink never enters it.
            mk(4, AnnotKind::Pen {
                paths: vec![
                    vec![AnnotPoint { x: 10.0, y: 10.0 }, AnnotPoint { x: 200.0, y: 10.0 }],
                    vec![AnnotPoint { x: 10.0, y: 10.0 }, AnnotPoint { x: 10.0, y: 200.0 }],
                ],
                pressure: Vec::new(),
                stroke_w: 4.0,
            }),
            // 5: a pen stroke that DOES run through the band.
            mk(5, AnnotKind::Pen {
                paths: vec![vec![AnnotPoint { x: 40.0, y: 60.0 }, AnnotPoint { x: 100.0, y: 60.0 }]],
                pressure: Vec::new(),
                stroke_w: 4.0,
            }),
        ];
        let hit = items_in_band(&items, band);
        assert_eq!(hit, vec![AnnotId(1), AnnotId(3), AnnotId(5)], "touch = selected, in z-order");
        // A band over nothing selects nothing (it becomes a plain deselect).
        assert!(items_in_band(&items, AnnotRect { x: 500.0, y: 500.0, w: 5.0, h: 5.0 }).is_empty());
        // A band ENCLOSING an item takes it too (touch includes contain).
        let all = items_in_band(&items, AnnotRect { x: -10.0, y: -10.0, w: 1000.0, h: 1000.0 });
        assert_eq!(all.len(), items.len(), "an all-covering band takes everything");
    }

    #[test]
    fn segment_hits_rect_covers_crossing_containment_and_misses() {
        let r = AnnotRect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert!(segment_hits_rect((0.0, 20.0), (100.0, 20.0), r), "crosses straight through");
        assert!(segment_hits_rect((15.0, 15.0), (18.0, 18.0), r), "wholly inside");
        assert!(segment_hits_rect((0.0, 0.0), (15.0, 15.0), r), "one endpoint inside");
        assert!(segment_hits_rect((10.0, 0.0), (10.0, 100.0), r), "runs along an edge");
        assert!(!segment_hits_rect((0.0, 0.0), (5.0, 5.0), r), "clear of the rect");
        assert!(!segment_hits_rect((0.0, 40.0), (100.0, 40.0), r), "passes below it");
        // A degenerate segment (a one-point pen dab) is a point-in-rect test.
        assert!(segment_hits_rect((20.0, 20.0), (20.0, 20.0), r));
        assert!(!segment_hits_rect((50.0, 50.0), (50.0, 50.0), r));
    }

    #[test]
    fn spawned_items_are_never_degenerate_on_a_normal_frame() {
        // Every tool's spawn on an ordinary capture survives the degeneracy gate a discarded
        // drag would trip — i.e. double-click always yields a real, grabbable item.
        for tool in [
            Tool::Arrow,
            Tool::Badge,
            Tool::Rect,
            Tool::Highlight,
            Tool::BoxHighlight,
            Tool::Spotlight,
            Tool::Pixelate,
            Tool::Blur,
        ] {
            let probe = AnnotRect { x: 0.0, y: 0.0, w: SPAWN_W, h: SPAWN_H };
            let m = kind_draw_margin(&spawn_kind(tool, probe, DEFAULT_ANNOT_STROKE).unwrap());
            let rect = default_placement_rect((1280, 720), m);
            let kind = spawn_kind(tool, rect, DEFAULT_ANNOT_STROKE).unwrap();
            let item = AnnotationItem { id: AnnotId(1), color: [255, 0, 0, 255], kind };
            assert!(!is_degenerate(&item), "{tool:?} spawns a real item");
        }
    }

    // ── freehand pencil + eraser (DRAGON-338) ────────────────────────────────────────

    /// A pen item from a list of polylines given as raw point pairs.
    fn pen(id: u64, color: AnnotColor, stroke_w: f32, paths: &[&[(f32, f32)]]) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(id),
            color,
            kind: AnnotKind::Pen {
                paths: paths
                    .iter()
                    .map(|p| p.iter().map(|&(x, y)| AnnotPoint { x, y }).collect())
                    .collect(),
                // No stored speed signal: the profile reads neutral pressure and takes its
                // character from the geometry's curvature alone (the "plain stroke" path).
                pressure: Vec::new(),
                stroke_w,
            },
        }
    }

    fn pen_paths(item: &AnnotationItem) -> &[Vec<AnnotPoint>] {
        match &item.kind {
            AnnotKind::Pen { paths, .. } => paths,
            _ => panic!("expected a pen"),
        }
    }

    #[test]
    fn tool_double_click_needs_the_same_tool_inside_the_window() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut c = ToolClicks::default();
        // A single press is never a double-click.
        assert!(!c.press(Tool::Rect, t0));
        // The same tool, promptly → double.
        assert!(c.press(Tool::Rect, t0 + Duration::from_millis(150)));
        // The pair is CONSUMED: an immediate third press starts over rather than re-firing.
        assert!(!c.press(Tool::Rect, t0 + Duration::from_millis(200)));
        assert!(c.press(Tool::Rect, t0 + Duration::from_millis(300)));

        // A DIFFERENT tool never completes the pair (it opens its own).
        let mut c = ToolClicks::default();
        assert!(!c.press(Tool::Rect, t0));
        assert!(!c.press(Tool::Arrow, t0 + Duration::from_millis(50)));
        assert!(c.press(Tool::Arrow, t0 + Duration::from_millis(100)));

        // Too slow → just two plain picks; the later press still opens a fresh pair.
        let mut c = ToolClicks::default();
        assert!(!c.press(Tool::Blur, t0));
        assert!(!c.press(Tool::Blur, t0 + TOOL_DOUBLE_CLICK + Duration::from_millis(1)));
        assert!(c.press(Tool::Blur, t0 + TOOL_DOUBLE_CLICK + Duration::from_millis(2)));
    }

    #[test]
    fn seg_seg_dist_is_zero_when_crossing_and_the_gap_otherwise() {
        // Crossing segments (an X) touch — distance 0.
        assert_eq!(seg_seg_dist((0.0, 0.0), (10.0, 10.0), (0.0, 10.0), (10.0, 0.0)), 0.0);
        // Parallel 3px apart: the perpendicular gap.
        assert!((seg_seg_dist((0.0, 0.0), (10.0, 0.0), (0.0, 3.0), (10.0, 3.0)) - 3.0).abs() < 1e-4);
        // End-to-end but offset: nearest at the endpoints.
        assert!((seg_seg_dist((0.0, 0.0), (5.0, 0.0), (9.0, 0.0), (20.0, 0.0)) - 4.0).abs() < 1e-4);
        // A degenerate (point) segment measures point-to-segment.
        assert!((seg_seg_dist((5.0, 5.0), (5.0, 5.0), (0.0, 0.0), (10.0, 0.0)) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn pen_groups_touch_only_when_their_ink_meets() {
        let a: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 0.0, y: 0.0 }, AnnotPoint { x: 20.0, y: 0.0 }]];
        // A stroke CROSSING it touches at any tolerance.
        let crossing: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 10.0, y: -5.0 }, AnnotPoint { x: 10.0, y: 5.0 }]];
        assert!(pen_groups_touch(&a, &crossing, 0.0));
        // A stroke 5px away touches at tol 6 but not at tol 4 — the tolerance IS the ink width.
        let near: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 0.0, y: 5.0 }, AnnotPoint { x: 20.0, y: 5.0 }]];
        assert!(pen_groups_touch(&a, &near, 6.0));
        assert!(!pen_groups_touch(&a, &near, 4.0));
        // A stroke far away never touches.
        let far: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 0.0, y: 400.0 }, AnnotPoint { x: 20.0, y: 400.0 }]];
        assert!(!pen_groups_touch(&a, &far, 20.0));
        // Symmetric.
        assert!(pen_groups_touch(&crossing, &a, 0.0));
    }

    #[test]
    fn connected_strokes_merge_into_one_item_disconnected_stay_separate() {
        // The ticket's core rule: strokes that connect become ONE selectable item; strokes
        // that don't stay their own. The merge runs on the NEWLY drawn stroke's id.
        let red = [255, 0, 0, 255];
        let mut items = vec![
            pen(1, red, 4.0, &[&[(0.0, 0.0), (20.0, 0.0)]]),          // a horizontal line
            pen(2, red, 4.0, &[&[(0.0, 200.0), (20.0, 200.0)]]),      // far away, untouched
            pen(3, red, 4.0, &[&[(10.0, -8.0), (10.0, 8.0)]]),        // crosses #1 → merges
        ];
        assert!(merge_connected_pens(&mut items, AnnotId(3)));
        assert_eq!(items.len(), 2, "the crossing stroke absorbed the one it touches");
        let merged = items.iter().find(|it| it.id == AnnotId(3)).expect("the drawn stroke survives");
        assert_eq!(pen_paths(merged).len(), 2, "one item now holds BOTH polylines");
        assert!(items.iter().any(|it| it.id == AnnotId(2)), "the disconnected stroke is untouched");
        // Re-running is a no-op (nothing left to connect).
        assert!(!merge_connected_pens(&mut items, AnnotId(3)));
    }

    #[test]
    fn merging_is_transitive_through_the_growing_group() {
        // A stroke that bridges two separate groups pulls in BOTH — and absorbing the first
        // grows the group's reach, so a third only reachable through it merges too.
        let c = [0, 255, 0, 255];
        let mut items = vec![
            pen(1, c, 4.0, &[&[(0.0, 0.0), (10.0, 0.0)]]),
            pen(2, c, 4.0, &[&[(20.0, 0.0), (30.0, 0.0)]]),
            // Touches #1's right end and #2's left end.
            pen(3, c, 4.0, &[&[(10.0, 0.0), (20.0, 0.0)]]),
        ];
        assert!(merge_connected_pens(&mut items, AnnotId(3)));
        assert_eq!(items.len(), 1, "all three strokes are one item");
        assert_eq!(pen_paths(&items[0]).len(), 3);
    }

    #[test]
    fn merging_never_repaints_differently_styled_strokes() {
        // A group carries ONE color + width, so merging across either would silently restyle
        // the user's earlier strokes: touching-but-different strokes stay separate items.
        let mut items = vec![
            pen(1, [255, 0, 0, 255], 4.0, &[&[(0.0, 0.0), (20.0, 0.0)]]),
            pen(2, [0, 0, 255, 255], 4.0, &[&[(10.0, -8.0), (10.0, 8.0)]]), // crosses, other color
        ];
        assert!(!merge_connected_pens(&mut items, AnnotId(2)));
        assert_eq!(items.len(), 2, "a different COLOR never merges");
        let mut widths = vec![
            pen(1, [255, 0, 0, 255], 2.0, &[&[(0.0, 0.0), (20.0, 0.0)]]),
            pen(2, [255, 0, 0, 255], 6.0, &[&[(10.0, -8.0), (10.0, 8.0)]]),
        ];
        assert!(!merge_connected_pens(&mut widths, AnnotId(2)));
        assert_eq!(widths.len(), 2, "a different WIDTH never merges");
        // A non-pen id is simply not a merge candidate.
        let mut mixed = vec![boxed(9, 0.0, 0.0, 10.0, 10.0)];
        assert!(!merge_connected_pens(&mut mixed, AnnotId(9)));
    }

    #[test]
    fn eraser_marks_the_strokes_its_sweep_crosses() {
        let paths: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 0.0, y: 0.0 }, AnnotPoint { x: 100.0, y: 0.0 }]];
        // A sweep straight across it hits.
        assert!(pen_hit_by_eraser(&paths, 4.0, (50.0, -20.0), (50.0, 20.0)));
        // A plain CLICK (zero-length sweep) ON the stroke hits — click-to-erase works.
        assert!(pen_hit_by_eraser(&paths, 4.0, (50.0, 0.0), (50.0, 0.0)));
        // Just outside the ink + slack does NOT (4/2 + 6 = 8px reach).
        assert!(pen_hit_by_eraser(&paths, 4.0, (50.0, 7.5), (50.0, 7.5)));
        assert!(!pen_hit_by_eraser(&paths, 4.0, (50.0, 40.0), (50.0, 40.0)));
        // A FAST drag that steps clean over the stroke still hits — the SEGMENT is tested, not
        // the sampled endpoints (both of which are far from the ink).
        assert!(pen_hit_by_eraser(&paths, 4.0, (50.0, -60.0), (50.0, 60.0)));
        // A sweep that misses the stroke entirely marks nothing.
        assert!(!pen_hit_by_eraser(&paths, 4.0, (0.0, 50.0), (100.0, 50.0)));
    }

    #[test]
    fn erase_preview_halves_the_alpha_of_marked_items_only() {
        // The pending-deletion preview is DISPLAY-only: marked groups draw at ERASE_PREVIEW_ALPHA and
        // the model is untouched (the sweep commits on release).
        let items = vec![
            pen(1, [255, 0, 0, 255], 4.0, &[&[(0.0, 0.0), (10.0, 0.0)]]),
            pen(2, [255, 0, 0, 255], 4.0, &[&[(0.0, 20.0), (10.0, 20.0)]]),
        ];
        let w = widget_items(&items, 8.0, &[AnnotId(2)]);
        assert!((w[0].color.a - 1.0).abs() < 1e-6, "unmarked stays fully opaque");
        assert!((w[1].color.a - ERASE_PREVIEW_ALPHA).abs() < 1e-6, "marked previews at the erase alpha");
        // Both are still real pen items in the model.
        assert!(items.iter().all(|it| it.kind.is_pen()));
    }

    #[test]
    fn pen_geometry_bounds_length_and_degeneracy() {
        let paths: Vec<Vec<AnnotPoint>> = vec![
            vec![AnnotPoint { x: 10.0, y: 5.0 }, AnnotPoint { x: 20.0, y: 5.0 }],
            vec![AnnotPoint { x: 12.0, y: 25.0 }, AnnotPoint { x: 12.0, y: 30.0 }],
        ];
        let b = pen_bounds(&paths);
        assert_eq!((b.x, b.y, b.w, b.h), (10.0, 5.0, 10.0, 25.0));
        assert!((pen_length(&paths) - 15.0).abs() < 1e-4);
        // An empty group is a zero rect (never a NaN/infinite one).
        let empty = pen_bounds(&[]);
        assert_eq!((empty.x, empty.y, empty.w, empty.h), (0.0, 0.0, 0.0, 0.0));
        // A pen gesture is NEVER discarded (DRAGON-342): a real stroke is a stroke, a press
        // that barely moved is a TAP, and a single point is a dot. Every other kind still
        // discards a stray click (checked in the box/arrow degeneracy tests).
        assert!(!is_degenerate(&pen(1, [0; 4], 4.0, &[&[(0.0, 0.0), (1.0, 0.0)]])));
        assert!(!is_degenerate(&pen(1, [0; 4], 4.0, &[&[(0.0, 0.0), (40.0, 0.0)]])));
        assert!(!is_degenerate(&pen(1, [0; 4], 4.0, &[&[(0.0, 0.0)]])));
        // The pen's drawn margin is half its WIDEST (pressure-swelled) sample, so no inked
        // pixel of a heavy stretch can land outside the picture.
        assert_eq!(
            kind_draw_margin(&pen(1, [0; 4], 6.0, &[&[(0.0, 0.0)]]).kind),
            crate::pen_stroke::max_width(6.0) / 2.0
        );
        // Its center is the bbox midpoint.
        assert_eq!(
            kind_center(&pen(1, [0; 4], 4.0, &[&[(0.0, 0.0), (10.0, 20.0)]]).kind),
            (5.0, 10.0)
        );
    }

    #[test]
    fn scale_pen_maps_the_group_into_the_new_bounding_box() {
        let paths: Vec<Vec<AnnotPoint>> = vec![vec![
            AnnotPoint { x: 0.0, y: 0.0 },
            AnnotPoint { x: 5.0, y: 10.0 },
            AnnotPoint { x: 10.0, y: 20.0 },
        ]];
        let from = pen_bounds(&paths);
        // Pure translation (a Move): same size, shifted.
        let moved = scale_pen(&paths, from, AnnotRect { x: 100.0, y: 50.0, w: 10.0, h: 20.0 });
        assert_eq!(moved[0][0], AnnotPoint { x: 100.0, y: 50.0 });
        assert_eq!(moved[0][2], AnnotPoint { x: 110.0, y: 70.0 });
        // A 2× resize scales every point about the box origin.
        let big = scale_pen(&paths, from, AnnotRect { x: 0.0, y: 0.0, w: 20.0, h: 40.0 });
        assert_eq!(big[0][1], AnnotPoint { x: 10.0, y: 20.0 });
        // A zero-extent axis (a perfectly straight horizontal stroke) translates instead of
        // dividing by zero.
        let flat: Vec<Vec<AnnotPoint>> =
            vec![vec![AnnotPoint { x: 0.0, y: 7.0 }, AnnotPoint { x: 10.0, y: 7.0 }]];
        let fb = pen_bounds(&flat);
        let out = scale_pen(&flat, fb, AnnotRect { x: 0.0, y: 20.0, w: 10.0, h: 0.0 });
        assert!(out[0].iter().all(|p| (p.y - 20.0).abs() < 1e-4 && p.y.is_finite()));
    }

    // ── DRAGON-388: multi-select group scale ─────────────────────────────────────────
    fn rect_of(kind: &AnnotKind) -> AnnotRect {
        match kind {
            AnnotKind::Box { rect, .. }
            | AnnotKind::Highlight { rect }
            | AnnotKind::BoxHighlight { rect, .. }
            | AnnotKind::Spotlight { rect }
            | AnnotKind::Badge { rect, .. }
            | AnnotKind::Pixelate { rect }
            | AnnotKind::Blur { rect }
            | AnnotKind::Text { rect, .. } => *rect,
            AnnotKind::Pen { paths, .. } => pen_bounds(paths),
            AnnotKind::Arrow { a, b, .. } => AnnotRect::from_points((a.x, a.y), (b.x, b.y)),
        }
    }

    #[test]
    fn group_scale_anchor_pins_the_corner_opposite_the_handle() {
        use crate::geometry::{Corner, Edge};
        // A 100×50 union at (20,30): (l,t,r,b) = (20,30,120,80).
        let b = AnnotRect { x: 20.0, y: 30.0, w: 100.0, h: 50.0 };
        assert_eq!(group_scale_anchor(b, Grab::Corner(Corner::Se)), (20.0, 30.0), "SE drag pins NW");
        assert_eq!(group_scale_anchor(b, Grab::Corner(Corner::Nw)), (120.0, 80.0), "NW drag pins SE");
        assert_eq!(group_scale_anchor(b, Grab::Corner(Corner::Ne)), (20.0, 80.0), "NE drag pins SW");
        assert_eq!(group_scale_anchor(b, Grab::Corner(Corner::Sw)), (120.0, 30.0), "SW drag pins NE");
        // Edge grabs pin the OPPOSITE corner, keeping the pivot a point.
        assert_eq!(group_scale_anchor(b, Grab::Edge(Edge::S)), (20.0, 30.0));
        assert_eq!(group_scale_anchor(b, Grab::Edge(Edge::N)), (20.0, 80.0));
        assert_eq!(group_scale_anchor(b, Grab::Edge(Edge::E)), (20.0, 30.0));
        assert_eq!(group_scale_anchor(b, Grab::Edge(Edge::W)), (120.0, 30.0));
    }

    #[test]
    fn group_scale_factor_is_identity_for_a_zero_drag() {
        use crate::geometry::Corner;
        let b = AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        assert!((group_scale_factor(b, Grab::Corner(Corner::Se), 0.0, 0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn group_scaled_kind_preserves_relative_layout_and_overlap() {
        // Two OVERLAPPING boxes, scaled 2× about the shared NW anchor (0,0). Every position and
        // size scales together, so relative offsets double and the overlap survives exactly.
        let a = AnnotKind::Box {
            rect: AnnotRect { x: 10.0, y: 10.0, w: 30.0, h: 30.0 },
            stroke_w: 4.0,
            fill: None,
        };
        let b = AnnotKind::Box {
            rect: AnnotRect { x: 20.0, y: 20.0, w: 30.0, h: 30.0 },
            stroke_w: 4.0,
            fill: None,
        };
        assert!(rects_overlap(rect_of(&a), rect_of(&b)), "precondition: they overlap");
        let (anchor, k, frame) = ((0.0, 0.0), 2.0, (1000.0, 1000.0));
        let a2 = group_scaled_kind(&a, anchor, k, frame);
        let b2 = group_scaled_kind(&b, anchor, k, frame);
        assert_eq!(rect_of(&a2), AnnotRect { x: 20.0, y: 20.0, w: 60.0, h: 60.0 });
        assert_eq!(rect_of(&b2), AnnotRect { x: 40.0, y: 40.0, w: 60.0, h: 60.0 });
        // Relative offset between the two boxes scaled by exactly k.
        assert_eq!(rect_of(&b2).x - rect_of(&a2).x, (20.0 - 10.0) * k);
        assert!(rects_overlap(rect_of(&a2), rect_of(&b2)), "overlap is preserved");
        // The stroke is left visually consistent (unchanged), like a single-item resize.
        let AnnotKind::Box { stroke_w, .. } = a2 else { panic!("stays a box") };
        assert_eq!(stroke_w, 4.0);
    }

    #[test]
    fn group_scale_is_an_involution_within_tolerance() {
        let frame = (1000.0, 1000.0);
        let anchor = (5.0, 7.0);
        let kinds = [
            AnnotKind::Box {
                rect: AnnotRect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 },
                stroke_w: 4.0,
                fill: Some([1, 2, 3, 4]),
            },
            AnnotKind::Arrow {
                a: AnnotPoint { x: 12.0, y: 18.0 },
                b: AnnotPoint { x: 44.0, y: 60.0 },
                stroke_w: 6.0,
            },
            AnnotKind::Badge { rect: AnnotRect { x: 30.0, y: 30.0, w: 24.0, h: 24.0 }, ring_w: 3.0 },
            pen(1, [255, 0, 0, 255], 4.0, &[&[(10.0, 10.0), (25.0, 40.0), (30.0, 12.0)]]).kind,
        ];
        for kind in kinds {
            let up = group_scaled_kind(&kind, anchor, 1.7, frame);
            let back = group_scaled_kind(&up, anchor, 1.0 / 1.7, frame);
            let (r0, r1) = (rect_of(&kind), rect_of(&back));
            assert!(
                (r0.x - r1.x).abs() < 1e-3
                    && (r0.y - r1.y).abs() < 1e-3
                    && (r0.w - r1.w).abs() < 1e-3
                    && (r0.h - r1.h).abs() < 1e-3,
                "scale by k then 1/k returns the original: {r0:?} vs {r1:?}"
            );
        }
        // A no-drag group scale (k = 1) is the identity map.
        let one = AnnotKind::Box {
            rect: AnnotRect { x: 1.0, y: 1.0, w: 2.0, h: 3.0 },
            stroke_w: 1.0,
            fill: None,
        };
        assert_eq!(group_scaled_kind(&one, anchor, 1.0, frame), one, "k=1 is a no-op");
    }

    #[test]
    fn group_scaled_kind_keeps_a_badge_square_under_a_non_unit_factor() {
        let badge = AnnotKind::Badge {
            rect: AnnotRect { x: 40.0, y: 40.0, w: 20.0, h: 20.0 },
            ring_w: 2.0,
        };
        let out = group_scaled_kind(&badge, (0.0, 0.0), 1.5, (1000.0, 1000.0));
        let r = rect_of(&out);
        assert!((r.w - r.h).abs() < 1e-4, "badge stays 1:1: {r:?}");
        assert_eq!((r.w, r.h), (30.0, 30.0));
        let AnnotKind::Badge { ring_w, .. } = out else { panic!("stays a badge") };
        assert_eq!(ring_w, 2.0, "ring weight left alone, like a single badge resize");
    }

    #[test]
    fn group_scaled_kind_scales_text_type_by_the_factor() {
        let text = AnnotKind::Text {
            rect: AnnotRect { x: 100.0, y: 100.0, w: 200.0, h: 60.0 },
            text: "hi".into(),
            size_px: 40.0,
            font: super::super::text_annot::TextFont::Clean,
            constrained: true,
            stroke_w: 4.0,
        };
        let out = group_scaled_kind(&text, (0.0, 0.0), 2.0, (10_000.0, 10_000.0));
        let AnnotKind::Text { size_px, .. } = out else { panic!("stays text") };
        assert_eq!(size_px, 80.0, "type scales by the group factor");
    }

    #[test]
    fn clamp_group_scale_floors_so_no_member_collapses_or_inverts() {
        let bounds = AnnotRect { x: 100.0, y: 100.0, w: 50.0, h: 50.0 };
        let anchor = (100.0, 100.0);
        let frame = (10_000.0, 10_000.0);
        // No text in the set: only the hard inversion floor bites for a huge shrink.
        let empty: [AnnotKind; 0] = [];
        let k = clamp_group_scale(-3.0, bounds, anchor, frame, empty.iter());
        assert_eq!(k, GROUP_SCALE_FLOOR, "a collapsing/negative factor floors at the guard");
        // A tiny text box pins the FLOOR higher so it never drops below its own min size.
        let tiny = [AnnotKind::Text {
            rect: AnnotRect { x: 100.0, y: 100.0, w: 20.0, h: 20.0 },
            text: "x".into(),
            size_px: 4.0,
            font: super::super::text_annot::TextFont::Clean,
            constrained: false,
            stroke_w: 2.0,
        }];
        let lo = super::super::text_annot::TEXT_SCALE_MIN_PX / 4.0;
        assert!(
            (clamp_group_scale(0.01, bounds, anchor, frame, tiny.iter()) - lo).abs() < 1e-6,
            "text min-size sets the floor for the shared factor"
        );
    }

    #[test]
    fn clamp_group_scale_keeps_the_growing_union_inside_the_frame() {
        // Union (0,0)-(100,100), NW pinned so growth pushes SE toward the frame's far edge at 200.
        let bounds = AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let anchor = (0.0, 0.0); // NW anchor (SE handle dragged)
        let frame = (200.0, 200.0);
        let empty: [AnnotKind; 0] = [];
        // A 2× ask lands exactly at the edge (SE 100→200); anything larger is capped to 2×.
        assert!((clamp_group_scale(2.0, bounds, anchor, frame, empty.iter()) - 2.0).abs() < 1e-4);
        assert!((clamp_group_scale(9.0, bounds, anchor, frame, empty.iter()) - 2.0).abs() < 1e-4);
        // A union already OVERFLOWING the frame on BOTH axes passes growth through (no valid
        // ceiling on either), like the group MOVE — fighting it would jump the arrangement. (A
        // uniform factor is shared, so an axis that still FITS would legitimately cap growth.)
        let over = AnnotRect { x: -50.0, y: -50.0, w: 400.0, h: 400.0 };
        assert!(clamp_group_scale(3.0, over, (-50.0, -50.0), frame, empty.iter()) >= 2.9);
    }

    #[test]
    fn pen_moves_and_resizes_through_its_bounding_box() {
        // The grab model: Move translates the whole drawing; a corner drag scales it — both
        // clamped inside the image like every other kind.
        let item = pen(1, [255, 0, 0, 255], 4.0, &[&[(10.0, 10.0), (30.0, 30.0)]]);
        let moved = edited_kind(&item.kind, Grab::Move, (0.0, 0.0), (5.0, 5.0), (200.0, 200.0), false);
        let AnnotKind::Pen { paths, stroke_w, .. } = &moved else { panic!("stays a pen") };
        assert_eq!(*stroke_w, 4.0, "a move never changes the width");
        assert_eq!(paths[0][0], AnnotPoint { x: 15.0, y: 15.0 });
        assert_eq!(paths[0][1], AnnotPoint { x: 35.0, y: 35.0 });
        // Dragging the SE corner out by +20,+20 doubles the drawing's extent.
        let grown = edited_kind(
            &item.kind,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (20.0, 20.0),
            (200.0, 200.0),
            false,
        );
        let AnnotKind::Pen { paths, .. } = &grown else { panic!("stays a pen") };
        assert_eq!(paths[0][0], AnnotPoint { x: 10.0, y: 10.0 }, "the anchored corner holds");
        assert_eq!(paths[0][1], AnnotPoint { x: 50.0, y: 50.0 }, "the dragged corner follows");
    }

    #[test]
    fn pen_is_a_colorable_vector_never_an_effect_or_a_knockout() {
        let p = pen(1, [255, 0, 0, 255], 4.0, &[&[(0.0, 0.0), (10.0, 10.0)]]);
        assert!(p.kind.is_colorable(), "a pen takes the shared annotation color");
        assert!(!p.kind.is_effect(), "a pen is a source-over vector, not a region effect");
        assert!(p.kind.is_pen());
        assert!(knockout_rects(std::slice::from_ref(&p)).is_empty(), "a pen never dims-knocks out");
        // A pen is not part of the rect-conversion family in either direction.
        assert_eq!(converted_rect_kind(&p.kind, Tool::Rect, 4.0), None);
        let boxk = AnnotKind::Box {
            rect: AnnotRect { x: 0.0, y: 0.0, w: 5.0, h: 5.0 },
            stroke_w: 4.0,
            fill: None,
        };
        assert_eq!(converted_rect_kind(&boxk, Tool::Pen, 4.0), None);
        assert_eq!(converted_rect_kind(&boxk, Tool::Eraser, 4.0), None);
    }

    // ── pen beautification: smoothing / pseudo-pressure / the tap dot (DRAGON-342) ──────

    /// One "drag" through the app's own live path: raw samples in, the model's stored
    /// (beautified) stroke out — exactly what `annot_gesture_to` writes on every sample.
    fn drag_fit(raw: &[(f32, f32)], stroke_w: f32) -> (Vec<AnnotPoint>, Vec<f32>) {
        let fit = crate::pen_stroke::smooth_path(raw, stroke_w);
        let press = crate::pen_stroke::pressure_along(raw, &fit);
        (fit.iter().map(|p| AnnotPoint { x: p.0, y: p.1 }).collect(), press)
    }

    #[test]
    fn a_pencil_tap_commits_a_dot_instead_of_being_discarded() {
        // The tap rule: under PEN_DOT_MAX of travel the gesture collapses to its anchor point
        // and is KEPT (the canvas emits DrawBegin+GestureEnd for a no-drag pencil press).
        let mut tap = pen(1, [0; 4], 4.0, &[&[(10.0, 10.0)]]).kind;
        assert!(normalize_pen_tap(&mut tap, None), "a lone point IS a tap");
        let AnnotKind::Pen { paths, .. } = &tap else { panic!("stays a pen") };
        assert_eq!(paths, &vec![vec![AnnotPoint { x: 10.0, y: 10.0 }]]);
        // A micro-drag (a press that jittered) becomes the same dot, anchored where it began.
        let mut jitter = pen(1, [0; 4], 4.0, &[&[(10.0, 10.0), (11.0, 10.5), (11.5, 10.0)]]).kind;
        assert!(normalize_pen_tap(&mut jitter, None), "under 3px of ink is a tap");
        let AnnotKind::Pen { paths, pressure, .. } = &jitter else { panic!("stays a pen") };
        assert_eq!(paths, &vec![vec![AnnotPoint { x: 10.0, y: 10.0 }]]);
        assert_eq!(pressure, &vec![Vec::<f32>::new()], "a dot carries no speed signal");
        // A real stroke is left alone…
        let mut real = pen(1, [0; 4], 4.0, &[&[(0.0, 0.0), (40.0, 0.0)]]).kind;
        assert!(!normalize_pen_tap(&mut real, None));
        // …and no other kind is ever touched.
        let mut arrow = AnnotKind::Arrow {
            a: AnnotPoint { x: 0.0, y: 0.0 },
            b: AnnotPoint { x: 0.5, y: 0.0 },
            stroke_w: 4.0,
        };
        assert!(!normalize_pen_tap(&mut arrow, None));
        // The committed dot survives the degeneracy gate (every other kind's stray click does
        // not) and bakes as a firm round press a touch wider than the preset.
        let dot = pen(1, [255, 0, 0, 255], 6.0, &[&[(20.0, 20.0)]]);
        assert!(!is_degenerate(&dot));
        let img = rasterize_scene(&[dot], 40, 40, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("raster");
        assert!(img.get_pixel(20, 20).0[3] > 0, "the dot inks at its anchor");
        assert!(img.get_pixel(23, 20).0[3] > 0, "…and pools past the 6px preset's own radius");
        assert!(img.get_pixel(20, 30).0[3] == 0, "but stays a dot");
    }

    #[test]
    fn the_live_fit_is_what_commit_keeps_and_the_settled_ink_never_moves() {
        // A jittery drag, sampled the way the pointer path does. The model holds the BEAUTIFIED
        // curve at every step, so the commit changes nothing — and appending samples must not
        // re-shape the ink already behind the cursor.
        let raw: Vec<(f32, f32)> = (0..90)
            .map(|i| {
                let t = i as f32 * 0.06;
                (30.0 + t * 30.0, 50.0 + (t * 1.4).sin() * 20.0 + if i % 2 == 0 { 0.4 } else { -0.4 })
            })
            .collect();
        let (mid, _) = drag_fit(&raw[..60], 4.0);
        let (end, press) = drag_fit(&raw, 4.0);
        assert_eq!(press.len(), end.len(), "one pressure sample per stored point");
        let settled = mid.iter().zip(&end).take_while(|(a, b)| a == b).count();
        assert!(settled > mid.len() / 2, "most of the drawn ink had already settled");
        // Committing re-runs nothing: the stored stroke IS the last live fit.
        assert_eq!(drag_fit(&raw, 4.0).0, end);
        // The stored curve is smooth (no jitter) and starts/ends exactly where the hand did.
        assert_eq!((end[0].x, end[0].y), raw[0]);
        assert_eq!((end[end.len() - 1].x, end[end.len() - 1].y), raw[raw.len() - 1]);
    }

    #[test]
    fn the_speed_signal_rides_the_group_through_merge_and_resize() {
        // The parallel pressure array must never stop belonging to its path: a resize keeps it,
        // a merge concatenates it, and a group that carries none reads as neutral.
        let (pts, press) = drag_fit(&[(10.0, 10.0), (12.0, 10.0), (14.0, 10.0), (40.0, 10.0)], 4.0);
        let inked = AnnotationItem {
            id: AnnotId(1),
            color: [1, 2, 3, 255],
            kind: AnnotKind::Pen { paths: vec![pts.clone()], pressure: vec![press], stroke_w: 4.0 },
        };
        // Resize: the points move, the per-point signal comes along unchanged and in step.
        let grown = edited_kind(
            &inked.kind,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (20.0, 0.0),
            (200.0, 200.0),
            false,
        );
        let AnnotKind::Pen { paths, pressure, .. } = &grown else { panic!("stays a pen") };
        assert_eq!(pressure[0].len(), paths[0].len(), "still one value per point");
        assert!(!pen_pressure(pressure, paths, 0).is_empty(), "…so it is still READ");
        // Merge: a touching same-look stroke that carries NO signal still lines up (its slot is
        // an empty vector, which reads as neutral pressure — never a mis-indexed neighbour).
        let plain = pen(2, [1, 2, 3, 255], 4.0, &[&[(40.0, 10.0), (60.0, 10.0)]]);
        let mut items = vec![inked, plain];
        assert!(merge_connected_pens(&mut items, AnnotId(1)));
        assert_eq!(items.len(), 1);
        let AnnotKind::Pen { paths, pressure, .. } = &items[0].kind else { panic!("a pen") };
        assert_eq!(paths.len(), 2);
        assert_eq!(pressure.len(), paths.len(), "one pressure slot per stroke");
        assert!(!pen_pressure(pressure, paths, 0).is_empty(), "the inked stroke keeps its signal");
        assert!(pen_pressure(pressure, paths, 1).is_empty(), "the plain one reads as neutral");
        // A stale/short signal is ignored rather than mis-read.
        assert!(pen_pressure(&[vec![0.5]], paths, 0).is_empty());
    }

    #[test]
    fn the_bake_tapers_its_tips_and_covers_its_centerline() {
        // Bake parity sanity: the rasterized ribbon covers the centerline it was traced along,
        // and the tapered TIP is measurably thinner than the body.
        let items = vec![pen(1, [255, 0, 0, 255], 8.0, &[&[(10.0, 40.0), (70.0, 40.0)]])];
        let img = rasterize_scene(&items, 80, 80, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("raster");
        for x in [12u32, 25, 40, 55, 68] {
            assert!(img.get_pixel(x, 40).0[3] > 0, "the centerline is uncovered at {x}");
        }
        // Vertical extent at the middle vs 1px in from the start tip.
        let thickness = |x: u32| (0..80).filter(|y| img.get_pixel(x, *y).0[3] > 0).count();
        let mid = thickness(40);
        let tip = thickness(11);
        assert!(mid >= 7, "the body inks at about the 8px preset: {mid}");
        assert!(tip < mid, "the tip is pinched: {tip} vs {mid}");
        assert!(tip > 0, "…but never vanishes");
        // Nothing inks outside the ribbon's width ceiling.
        assert_eq!(img.get_pixel(40, 40 - 7).0[3], 0, "no ink past max_width/2");
    }

    #[test]
    fn pen_rasterizes_its_strokes_for_the_bake() {
        // The bake path: the same vector geometry the canvas draws, filled at full resolution.
        let items = vec![pen(1, [255, 0, 0, 255], 4.0, &[&[(2.0, 20.0), (38.0, 20.0)]])];
        let img = rasterize_scene(&items, 40, 40, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("raster");
        assert!(img.get_pixel(20, 20).0[3] > 0, "the stroke is drawn where it was traced");
        assert!(img.get_pixel(20, 2).0[3] == 0, "and nowhere else");
        // A one-point stroke bakes as a dot rather than vanishing.
        let dot = vec![pen(2, [255, 0, 0, 255], 6.0, &[&[(20.0, 20.0)]])];
        let img = rasterize_scene(&dot, 40, 40, 1.0, DEFAULT_ANNOT_CURVE_RADIUS).expect("raster");
        assert!(img.get_pixel(20, 20).0[3] > 0, "a single-point stroke is a dot");
        // Baking composites it onto the base (a pen scene is a real edit).
        let mut base = RgbaImage::from_pixel(40, 40, ::image::Rgba([0, 0, 0, 255]));
        let before = base.clone();
        apply_annotations(&mut base, &items, DEFAULT_ANNOT_CURVE_RADIUS);
        assert_ne!(base, before, "the pen strokes bake into the picture");
    }

    #[test]
    fn pen_carries_its_polylines_to_the_canvas_at_the_selected_width() {
        // The width control (2/4/6px presets) is the single source of truth for a new stroke,
        // and the polylines reach the widget verbatim in SOURCE px.
        let mut e = super::super::edit::EditState { annot_stroke_w: 6.0, ..Default::default() };
        assert_eq!(e.stroke(), 6.0);
        assert!(STROKE_WIDTHS.contains(&e.stroke()), "6px is one of the three presets");
        let id = e.next_annot_id();
        e.annotations.push(pen(id.0, [1, 2, 3, 255], e.stroke(), &[&[(1.0, 2.0), (3.0, 4.0)]]));
        let w = widget_items(&e.annotations, DEFAULT_ANNOT_CURVE_RADIUS, &[]);
        assert_eq!(w[0].stroke_w, 6.0);
        assert_eq!(w[0].fx, FxKind::None, "a pen draws as a vector, never through a shader pass");
        let ItemKind::Path { paths, .. } = &w[0].kind else { panic!("pens hit-test as paths") };
        assert_eq!(paths, &vec![vec![(1.0, 2.0), (3.0, 4.0)]]);
    }

    // ── custom-color recents queue (DRAGON-348) ──────────────────────────────────────

    #[test]
    fn a_new_custom_color_leads_the_strip_and_the_oldest_is_replaced_at_cap() {
        let col = |n: u8| [n, n, n, 255];
        let mut recents: Vec<AnnotColor> = Vec::new();
        // Fills newest-first: each pick lands at the FRONT of the strip.
        for n in 1..=5 {
            rotate_recent_color(&mut recents, col(n));
        }
        assert_eq!(recents, (1..=5).rev().map(col).collect::<Vec<_>>());
        // At cap, a sixth pick replaces the OLDEST (1, the last entry) — always.
        rotate_recent_color(&mut recents, col(6));
        assert_eq!(recents, [col(6), col(5), col(4), col(3), col(2)]);
        // Re-picking an existing color moves it to the front — no duplicate, nothing lost.
        rotate_recent_color(&mut recents, col(3));
        assert_eq!(recents, [col(3), col(6), col(5), col(4), col(2)]);
        // The dedup is RGB-only: a same-RGB pick with different alpha replaces the entry.
        rotate_recent_color(&mut recents, [3, 3, 3, 128]);
        assert_eq!(recents.len(), 5);
        assert_eq!(recents[0], [3, 3, 3, 128]);
    }

    #[test]
    fn the_flyout_lists_recents_newest_first_before_the_custom_opener() {
        let recents = [[10, 0, 0, 255], [0, 20, 0, 255]]; // as stored: newest at index 0
        let entries = palette_entries(&recents);
        let n = entries.len();
        assert_eq!(entries[n - 1], PaletteEntry::Custom, "the '+' closes the flyout");
        // The strip renders in stored order — newest first, oldest adjacent to the '+'.
        assert_eq!(entries[n - 3], PaletteEntry::Color(recents[0]));
        assert_eq!(entries[n - 2], PaletteEntry::Color(recents[1]));
    }

    // ── the sequence badge (DRAGON-340) ──────────────────────────────────────────────

    fn badge(id: u64, x: f32, y: f32, s: f32) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Badge { rect: AnnotRect { x, y, w: s, h: s }, ring_w: 4.0 },
        }
    }

    fn numbers(items: &[AnnotationItem]) -> Vec<(u64, u32)> {
        badge_numbers(items).into_iter().map(|(id, n)| (id.0, n)).collect()
    }

    /// Placing badges numbers them 1, 2, 3… in scene order, and NON-badge items in between
    /// never consume a number.
    #[test]
    fn badges_number_from_one_in_scene_order_and_skip_other_items() {
        let scene = vec![
            badge(1, 0.0, 0.0, 40.0),
            boxed(2, 10.0, 10.0, 30.0, 30.0), // an ordinary box between them…
            badge(3, 60.0, 0.0, 40.0),
            badge(4, 120.0, 0.0, 40.0),
        ];
        assert_eq!(numbers(&scene), [(1, 1), (3, 2), (4, 3)]);
        // An all-non-badge scene has no numbering at all.
        assert!(badge_numbers(&[boxed(9, 0.0, 0.0, 5.0, 5.0)]).is_empty());
    }

    /// Deleting badge #2 renumbers the rest so the set stays a contiguous 1..N — the ticket's
    /// "if i delete 2, then 3 becomes 2". Deleting the FIRST one shifts everything down too.
    #[test]
    fn deleting_a_badge_renumbers_the_rest_contiguously() {
        let scene = vec![
            badge(1, 0.0, 0.0, 40.0),
            badge(2, 60.0, 0.0, 40.0),
            badge(3, 120.0, 0.0, 40.0),
            badge(4, 180.0, 0.0, 40.0),
        ];
        assert_eq!(numbers(&scene), [(1, 1), (2, 2), (3, 3), (4, 4)]);
        // Delete the MIDDLE one (#2): 3 becomes 2, 4 becomes 3.
        let mut without_middle = scene.clone();
        without_middle.retain(|it| it.id != AnnotId(2));
        assert_eq!(numbers(&without_middle), [(1, 1), (3, 2), (4, 3)]);
        // Delete the FIRST one: everything shifts down by one.
        let mut without_first = scene.clone();
        without_first.retain(|it| it.id != AnnotId(1));
        assert_eq!(numbers(&without_first), [(2, 1), (3, 2), (4, 3)]);
        // Whatever is deleted, the set is always exactly 1..N with no gaps.
        for drop in 1..=4u64 {
            let mut s = scene.clone();
            s.retain(|it| it.id != AnnotId(drop));
            let got: Vec<u32> = numbers(&s).into_iter().map(|(_, n)| n).collect();
            assert_eq!(got, (1..=3).collect::<Vec<_>>(), "dropping {drop} left a gap");
        }
    }

    /// UNDO restores correct numbering for free, because the numbers are derived from the item
    /// VECTOR that undo restores — the whole reason nothing is stored per badge. Modelled here
    /// exactly as the shared history does it: snapshot the scene, mutate, restore the snapshot.
    #[test]
    fn undo_restores_badge_numbering_because_it_is_derived() {
        let scene = vec![
            badge(1, 0.0, 0.0, 40.0),
            badge(2, 60.0, 0.0, 40.0),
            badge(3, 120.0, 0.0, 40.0),
        ];
        let snapshot = scene.clone(); // what `push_annotations` keeps
        let mut live = scene.clone();
        live.retain(|it| it.id != AnnotId(2));
        assert_eq!(numbers(&live), [(1, 1), (3, 2)]);
        // Undo = restore the pre-edit vector.
        let live = snapshot.clone();
        assert_eq!(numbers(&live), [(1, 1), (2, 2), (3, 3)]);
        // Redo = re-apply, and the numbering follows again.
        let mut live = live;
        live.retain(|it| it.id != AnnotId(2));
        assert_eq!(numbers(&live), [(1, 1), (3, 2)]);
    }

    /// A pre-placed badge (double-click the tray button) is squared down to the shorter axis of
    /// the shared 200x100 placement rect, and stays centred in it.
    #[test]
    fn a_pre_placed_badge_is_a_centred_square() {
        let rect = AnnotRect { x: 100.0, y: 50.0, w: 200.0, h: 100.0 };
        let sq = centered_square(rect);
        assert_eq!((sq.w, sq.h), (100.0, 100.0));
        assert_eq!((sq.x, sq.y), (150.0, 50.0));
        // Through the real spawn path.
        let Some(AnnotKind::Badge { rect: r, ring_w }) = spawn_kind(Tool::Badge, rect, 6.0) else {
            panic!("the badge tool pre-places");
        };
        assert_eq!(r.w, r.h);
        assert_eq!(ring_w, 6.0, "the ring takes the current line weight");
    }

    /// EVERY corner and edge grab leaves the badge 1:1 — the "always square, during and after
    /// any resize" rule. The anchor (the handle the user is NOT holding) stays put.
    #[rstest]
    #[case(Grab::Corner(Corner::Nw))]
    #[case(Grab::Corner(Corner::Ne))]
    #[case(Grab::Corner(Corner::Sw))]
    #[case(Grab::Corner(Corner::Se))]
    #[case(Grab::Edge(Edge::N))]
    #[case(Grab::Edge(Edge::S))]
    #[case(Grab::Edge(Edge::W))]
    #[case(Grab::Edge(Edge::E))]
    #[case(Grab::Move)]
    fn a_badge_stays_square_through_every_grab(#[case] grab: Grab) {
        let start = AnnotKind::Badge {
            rect: AnnotRect { x: 200.0, y: 200.0, w: 100.0, h: 100.0 },
            ring_w: 4.0,
        };
        let frame = (1000.0, 800.0);
        // A deliberately LOPSIDED drag: 70 across, 20 down — the aspect a free rect would take.
        for cur in [(370.0, 320.0), (130.0, 180.0), (500.0, 210.0), (210.0, 500.0)] {
            let out = edited_kind(&start, grab, (300.0, 300.0), cur, frame, false);
            let AnnotKind::Badge { rect, .. } = out else { panic!("still a badge") };
            assert!(
                (rect.w - rect.h).abs() < 1e-3,
                "{grab:?} at {cur:?} broke 1:1 ({} x {})",
                rect.w,
                rect.h
            );
            assert!(rect.w >= 0.0 && rect.h >= 0.0);
        }
    }

    /// The corner grab anchors the OPPOSITE corner, exactly like a box does, and takes the
    /// LARGER of the two dragged extents as the side.
    #[test]
    fn a_corner_grab_anchors_the_opposite_corner() {
        use crate::geometry::Corner;
        let r = AnnotRect { x: 100.0, y: 100.0, w: 40.0, h: 90.0 };
        // Dragging NW: the SE corner (140, 190) is fixed, the side is the larger extent (90).
        let out = square_for_grab(r, Grab::Corner(Corner::Nw), 1000.0, 1000.0, 0.0);
        assert_eq!((out.w, out.h), (90.0, 90.0));
        assert_eq!((out.x + out.w, out.y + out.h), (140.0, 190.0));
        // Dragging SE: the NW corner is fixed.
        let out = square_for_grab(r, Grab::Corner(Corner::Se), 1000.0, 1000.0, 0.0);
        assert_eq!((out.x, out.y), (100.0, 100.0));
        assert_eq!((out.w, out.h), (90.0, 90.0));
    }

    /// An EDGE grab sizes on its own axis and re-centres on the other, so dragging the top edge
    /// doesn't also slide the badge sideways.
    #[test]
    fn an_edge_grab_sizes_on_its_axis_and_recentres_on_the_other() {
        use crate::geometry::Edge;
        let r = AnnotRect { x: 100.0, y: 100.0, w: 40.0, h: 90.0 };
        let out = square_for_grab(r, Grab::Edge(Edge::N), 1000.0, 1000.0, 0.0);
        assert_eq!((out.w, out.h), (90.0, 90.0));
        assert_eq!(out.y + out.h, 190.0, "the south edge is the anchor");
        assert!((out.x + out.w / 2.0 - 120.0).abs() < 1e-3, "the badge stayed centred in x");
    }

    /// Clamping a badge to the image keeps it SQUARE — the axis-independent `clamp_rect` would
    /// have turned it into a rectangle against an edge.
    #[test]
    fn clamping_a_badge_to_the_image_keeps_it_square() {
        // A badge taller than the (short) frame shrinks on BOTH axes, not just the tight one.
        let r = AnnotRect { x: -50.0, y: -50.0, w: 400.0, h: 400.0 };
        let out = clamp_square(r, 300.0, 120.0, 2.0);
        assert_eq!(out.w, out.h);
        assert_eq!(out.w, 116.0, "the side takes the TIGHTER axis, inset by the margin");
        assert!(out.x >= 2.0 && out.y >= 2.0);
        assert!(out.x + out.w <= 298.0 && out.y + out.h <= 118.0);
        // A drag past the right edge slides back in at full size, still square.
        let out = clamp_square(AnnotRect { x: 280.0, y: 10.0, w: 60.0, h: 60.0 }, 300.0, 200.0, 0.0);
        assert_eq!((out.w, out.h), (60.0, 60.0));
        assert_eq!(out.x, 240.0);
    }

    // ── click-to-place (the badge is placed, never dragged out) ──────────────────────────

    /// A click in open picture places the badge CENTRED on it at exactly the wanted side.
    #[test]
    fn a_click_placed_badge_is_centred_on_the_click() {
        let r = badge_placement_rect((400.0, 300.0), DEFAULT_BADGE_SIZE, (1920, 1080), 2.0);
        assert_eq!((r.w, r.h), (DEFAULT_BADGE_SIZE, DEFAULT_BADGE_SIZE));
        assert!((r.x + r.w / 2.0 - 400.0).abs() < 1e-3);
        assert!((r.y + r.h / 2.0 - 300.0).abs() < 1e-3);
    }

    /// A click near an EDGE keeps the full size and slides inside the picture (the drawn
    /// margin — half the ring — reserved), exactly like any other clamped item.
    #[test]
    fn a_click_placed_badge_slides_inside_the_picture() {
        let m = 3.0;
        let r = badge_placement_rect((5.0, 5.0), DEFAULT_BADGE_SIZE, (1920, 1080), m);
        assert_eq!((r.w, r.h), (DEFAULT_BADGE_SIZE, DEFAULT_BADGE_SIZE), "size is not sacrificed");
        assert_eq!((r.x, r.y), (m, m), "pushed clear of the top-left, margin reserved");
        // ...and the same at the far corner.
        let r = badge_placement_rect((1919.0, 1079.0), DEFAULT_BADGE_SIZE, (1920, 1080), m);
        assert!((r.x + r.w - (1920.0 - m)).abs() < 1e-3);
        assert!((r.y + r.h - (1080.0 - m)).abs() < 1e-3);
    }

    /// A picture too small for the wanted side shrinks the badge on BOTH axes together (the
    /// 1:1 invariant), by the SAME rule the double-click pre-placement uses: capped at
    /// `SPAWN_MAX_FRAC` of the tighter axis and at the room the margin leaves.
    #[test]
    fn a_click_placed_badge_shrinks_squarely_on_a_small_picture() {
        let r = badge_placement_rect((30.0, 20.0), DEFAULT_BADGE_SIZE, (200, 60), 2.0);
        assert_eq!(r.w, r.h, "still square");
        // The short axis allows min(0.8 * 60, 60 - 4) = 48.
        assert_eq!(r.w, 48.0);
        assert!(r.x >= 2.0 && r.y >= 2.0);
        assert!(r.x + r.w <= 198.0 && r.y + r.h <= 58.0);
        // A degenerate frame yields a zero square, which the caller discards like a bad drag.
        let z = badge_placement_rect((0.0, 0.0), DEFAULT_BADGE_SIZE, (0, 0), 2.0);
        assert_eq!((z.w, z.h), (0.0, 0.0));
    }

    /// Click-to-place and the double-click pre-placement share ONE size rule: the ordinary
    /// tools ride [`placement_extent`] per axis, and a BADGE double-click goes through the very
    /// helper a click uses ([`badge_placement_rect`]) at the REMEMBERED side — it must never
    /// fall back to the generic 200×100 spawn box, which used to hand it a fixed 100px square
    /// no matter what size the user had settled on.
    #[test]
    fn click_placement_and_double_click_share_the_size_rule() {
        for frame in [(1920u32, 1080u32), (300, 300), (200, 60), (90, 400)] {
            let m = 2.0;
            let pre = centered_square(default_placement_rect(frame, m));
            // The pre-placement asks SPAWN_W across and SPAWN_H down, then squares to the
            // smaller; a click asking for the SAME per-axis wants must land the same side.
            let click_w = placement_extent(frame.0 as f32, SPAWN_W, m);
            let click_h = placement_extent(frame.1 as f32, SPAWN_H, m);
            assert!((pre.w - click_w.min(click_h)).abs() < 1e-3, "{frame:?}");
            // A badge double-click IS a click-place, centred: same helper, same clamp, same
            // wanted side — for the default AND for any remembered one.
            let (fw, fh) = (frame.0 as f32, frame.1 as f32);
            for want in [DEFAULT_BADGE_SIZE, 40.0, 160.0] {
                let dbl = spawn_placement_rect(Tool::Badge, frame, m, want);
                assert_eq!(
                    dbl,
                    badge_placement_rect((fw * 0.5, fh * 0.5), want, frame, m),
                    "{frame:?} @ {want}"
                );
                // `spawn_kind`'s squaring leaves it alone (it is already 1:1), so what the
                // double-click actually drops is exactly that rect.
                assert_eq!(centered_square(dbl), dbl, "{frame:?} @ {want}");
                assert_eq!(dbl.w, dbl.h, "{frame:?} @ {want}");
            }
            // ...and on a picture big enough to hold them, the remembered side is what lands —
            // 40 and 160 must NOT both come out as the spawn box's shorter axis.
            if frame == (1920, 1080) {
                assert_eq!(spawn_placement_rect(Tool::Badge, frame, m, 40.0).w, 40.0);
                assert_eq!(spawn_placement_rect(Tool::Badge, frame, m, 160.0).w, 160.0);
                assert_eq!(
                    spawn_placement_rect(Tool::Badge, frame, m, DEFAULT_BADGE_SIZE).w,
                    DEFAULT_BADGE_SIZE
                );
            }
            // Every OTHER tool still takes the shared spawn box, untouched by the badge rule.
            for tool in [Tool::Rect, Tool::Arrow, Tool::Blur] {
                assert_eq!(
                    spawn_placement_rect(tool, frame, m, 40.0),
                    default_placement_rect(frame, m),
                    "{tool:?} on {frame:?}"
                );
            }
        }
    }

    /// The remembered-size fallback chain: an editor with nothing remembered (fresh install —
    /// the persisted seed is `0.0`) places at the default; once a badge has been placed or
    /// resized (or a persisted side seeded the document), that side wins.
    #[test]
    fn the_badge_size_falls_back_to_the_default_until_one_is_remembered() {
        let mut e = super::super::edit::EditState::default();
        assert_eq!(e.annot_badge_size, 0.0, "nothing remembered on a fresh editor");
        assert_eq!(e.badge_size(), DEFAULT_BADGE_SIZE);
        assert_eq!(DEFAULT_BADGE_SIZE, 75.0, "the shipped default side");
        // The seed a preview open copies out of the PERSISTED setting behaves the same way as
        // one placed in this editor — it is the same field.
        e.annot_badge_size = 64.0;
        assert_eq!(e.badge_size(), 64.0);
        // A nonsense value can't make a badge vanish — it reads as "unset".
        e.annot_badge_size = -1.0;
        assert_eq!(e.badge_size(), DEFAULT_BADGE_SIZE);
    }

    /// The badge's DRAWN extent is its square grown by half the ring — the same overhang rule a
    /// box outline gets, which is what keeps a badge dragged to the edge fully on the picture.
    #[test]
    fn a_badge_reserves_half_its_ring_as_drawn_margin() {
        let k = AnnotKind::Badge {
            rect: AnnotRect { x: 10.0, y: 20.0, w: 60.0, h: 60.0 },
            ring_w: 6.0,
        };
        assert_eq!(kind_draw_margin(&k), 3.0);
        let b = kind_drawn_bounds(&k);
        assert_eq!((b.x, b.y, b.w, b.h), (7.0, 17.0, 66.0, 66.0));
        // It marks a spot rather than framing a region, so it never punches the global dim.
        assert!(knockout_rects(&[badge(1, 0.0, 0.0, 40.0)]).is_empty());
    }

    /// A badge is colourable (its disc/ring AND, through the contrast rule, its numeral ink),
    /// and re-strokes with the shared line weight — the ring IS the line weight.
    #[test]
    fn a_badge_is_colourable_and_its_ring_is_the_line_weight() {
        let k = AnnotKind::Badge {
            rect: AnnotRect { x: 0.0, y: 0.0, w: 40.0, h: 40.0 },
            ring_w: 2.0,
        };
        assert!(k.is_colorable());
        assert!(k.is_badge());
        assert!(!k.is_effect() && !k.is_pen());
    }

    /// The always-1:1 badge is NOT part of the rect-conversion family: picking the badge tool
    /// with a box selected must not reshape the box, and vice versa.
    #[test]
    fn a_badge_never_converts_to_or_from_the_rect_family() {
        let boxk = AnnotKind::Box {
            rect: AnnotRect { x: 0.0, y: 0.0, w: 80.0, h: 20.0 },
            stroke_w: 4.0,
            fill: None,
        };
        assert_eq!(converted_rect_kind(&boxk, Tool::Badge, 4.0), None);
        let badgek = AnnotKind::Badge {
            rect: AnnotRect { x: 0.0, y: 0.0, w: 40.0, h: 40.0 },
            ring_w: 4.0,
        };
        for tool in [Tool::Rect, Tool::Highlight, Tool::Blur, Tool::Spotlight] {
            assert_eq!(converted_rect_kind(&badgek, tool, 4.0), None, "{tool:?}");
        }
    }

    /// The canvas gets the badge as a plain square Rect carrying its DERIVED ordinal and the
    /// ring weight — so hit-testing/chrome/resize are the ordinary ones and the number can
    /// never be stale.
    #[test]
    fn widget_items_stamp_the_derived_ordinal_on_each_badge() {
        let scene = vec![
            badge(1, 0.0, 0.0, 40.0),
            boxed(2, 0.0, 0.0, 10.0, 10.0),
            badge(3, 60.0, 0.0, 40.0),
        ];
        let w = widget_items(&scene, 8.0, &[]);
        assert_eq!(w[0].badge, Some(1));
        assert_eq!(w[1].badge, None, "an ordinary box is not a badge");
        assert_eq!(w[2].badge, Some(2));
        assert_eq!(w[0].stroke_w, 4.0, "the ring weight rides `stroke_w`");
        assert_eq!(w[0].fx, FxKind::None, "a badge is a vector, never a shader pass");
        assert!(matches!(w[0].kind, ItemKind::Rect { w: 40.0, h: 40.0, .. }));
    }

    /// A badge draws at FULL capture resolution: the bake rasterizes real, opaque pixels inside
    /// the badge and leaves everything outside it untouched.
    #[test]
    fn a_badge_bakes_at_full_resolution() {
        let scene = vec![badge(1, 20.0, 20.0, 80.0)];
        let img = rasterize_scene(&scene, 200, 200, 1.0, 8.0).expect("rasterized");
        // The disc centre is opaque (the badge colour).
        let centre = img.get_pixel(60, 60).0;
        assert_eq!(centre[3], 255, "the disc is filled");
        // Well outside the badge nothing was drawn.
        assert_eq!(img.get_pixel(180, 180).0[3], 0);
        // The ring lands on the square's inscribed circle (the leftmost point of the badge).
        let on_ring = img.get_pixel(20, 60).0;
        assert!(on_ring[3] > 0, "the ring is drawn on the inscribed circle");
    }

    /// A caption on a big capture, for the region/cost tests below.
    fn caption(frame: (u32, u32), x: f32, y: f32, size: f32) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(1),
            color: [255, 255, 255, 255],
            kind: reflow_text(
                "The quick brown fox jumps over the lazy dog",
                size,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x, y, w: 0.0, h: 0.0 },
                false,
                2.0,
                (frame.0 as f32, frame.1 as f32),
            ),
        }
    }

    /// DRAGON-362: the live text layer covers the CAPTION, not the capture — the whole reason
    /// a 5K editor stopped being usable was that every keystroke rendered (and uploaded) a
    /// full-frame raster. The region must contain the text box with room for the outline, and
    /// be a small fraction of a large frame.
    #[test]
    fn text_layer_region_hugs_the_caption_on_a_large_frame() {
        let frame = (5120u32, 2880u32);
        let item = caption(frame, 200.0, 300.0, 64.0);
        let r = text_layer_region(std::slice::from_ref(&item), frame).expect("a region");
        let AnnotKind::Text { rect, .. } = &item.kind else { panic!("text") };
        // Contains the box, with padding on every side (glyph overhang + outline).
        assert!(r.x <= rect.x && r.y <= rect.y, "region {r:?} clips the box origin {rect:?}");
        assert!(r.x + r.w >= rect.x + rect.w, "region {r:?} clips the box width");
        assert!(r.y + r.h >= rect.y + rect.h, "region {r:?} clips the box height");
        // And it is a small slice of the picture — this ratio IS the per-keystroke speedup.
        let frac = (r.w * r.h) / (frame.0 as f32 * frame.1 as f32);
        assert!(frac < 0.05, "region covers {:.1}% of the frame", frac * 100.0);
    }

    /// The region is snapped to [`TEXT_REGION_GRID`] so a burst of keystrokes keeps the SAME
    /// raster dimensions — which is what lets the layer's GPU texture update in place instead
    /// of being re-created (the `layers.rs` flicker-free contract).
    #[test]
    fn text_layer_region_is_grid_stable_across_small_edits() {
        let frame = (5120u32, 2880u32);
        let short = AnnotationItem {
            kind: reflow_text(
                "Hello",
                64.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x: 200.0, y: 300.0, w: 0.0, h: 0.0 },
                false,
                2.0,
                (frame.0 as f32, frame.1 as f32),
            ),
            ..caption(frame, 200.0, 300.0, 64.0)
        };
        let longer = AnnotationItem {
            kind: reflow_text(
                "Hell",
                64.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x: 200.0, y: 300.0, w: 0.0, h: 0.0 },
                false,
                2.0,
                (frame.0 as f32, frame.1 as f32),
            ),
            ..caption(frame, 200.0, 300.0, 64.0)
        };
        let a = text_layer_region(std::slice::from_ref(&short), frame).expect("region");
        let b = text_layer_region(std::slice::from_ref(&longer), frame).expect("region");
        assert_eq!((a.x, a.y), (b.x, b.y), "the origin holds still while typing");
        // Both snapped to the grid.
        for r in [a, b] {
            for v in [r.x, r.y, r.w, r.h] {
                assert!(
                    (v % TEXT_REGION_GRID).abs() < 1e-3 || v == 0.0,
                    "{v} is not on the {TEXT_REGION_GRID}px grid"
                );
            }
        }
    }

    /// Several captions spread across the picture union into one region; no text at all (or
    /// only empty boxes) yields no layer.
    #[test]
    fn text_layer_region_unions_and_handles_the_empty_cases() {
        let frame = (5120u32, 2880u32);
        let a = caption(frame, 200.0, 300.0, 64.0);
        let mut b = caption(frame, 3000.0, 2000.0, 64.0);
        b.id = AnnotId(2);
        let r = text_layer_region(&[a.clone(), b], frame).expect("a region");
        assert!(r.x <= 200.0 && r.y <= 300.0);
        assert!(r.x + r.w >= 3000.0 && r.y + r.h >= 2000.0);
        // NOT clipped to the picture any more (DRAGON-368) — it contains the padded ink instead,
        // which is what lets the raster be re-placed rather than re-rendered. See
        // [`text_layer_region`] for why nothing draws outside the picture as a result.
        let (x0, y0, x1, y1) = text_padded_bounds(&[a.clone(), AnnotationItem {
            id: AnnotId(2),
            ..caption(frame, 3000.0, 2000.0, 64.0)
        }])
        .expect("padded bounds");
        assert!(r.x <= x0 && r.y <= y0 && r.x + r.w >= x1 && r.y + r.h >= y1);
        // No text / whitespace-only text / a degenerate frame → no layer at all.
        assert!(text_layer_region(&[], frame).is_none());
        assert!(text_layer_region(&[boxed(9, 0.0, 0.0, 10.0, 10.0)], frame).is_none());
        let blank = AnnotationItem {
            kind: reflow_text(
                "   ",
                64.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x: 10.0, y: 10.0, w: 0.0, h: 0.0 },
                false,
                2.0,
                (frame.0 as f32, frame.1 as f32),
            ),
            ..a.clone()
        };
        assert!(text_layer_region(&[blank], frame).is_none());
        assert!(text_layer_region(&[a], (0, 0)).is_none());
    }

    /// The region is a pure TRANSLATION of the drawing: glyphs rendered into a region raster
    /// land at the same picture position as in a full-frame raster (live/bake parity — the
    /// bake still composites the whole frame, so the two must agree on WHERE the ink is).
    #[test]
    fn region_raster_places_glyphs_at_the_same_picture_position_as_a_full_frame_raster() {
        let frame = (1024u32, 512u32);
        let item = caption(frame, 200.0, 180.0, 32.0);
        let region = text_layer_region(std::slice::from_ref(&item), frame).expect("region");
        // Full-frame reference: the region is the whole picture, rastered 1:1.
        let full = AnnotRect { x: 0.0, y: 0.0, w: frame.0 as f32, h: frame.1 as f32 };
        let a = render_text_layer(std::slice::from_ref(&item), frame, full, frame.0, frame.1)
            .expect("full raster");
        // The sub-region raster at the SAME 1:1 scale.
        let (rw, rh) = (region.w as u32, region.h as u32);
        let b = render_text_layer(std::slice::from_ref(&item), frame, region, rw, rh)
            .expect("region raster");
        assert_eq!(b.dimensions(), (rw, rh));
        // Every pixel of the region raster equals the full raster's pixel at the same picture
        // coordinate — the region only moves the canvas, never the ink.
        let (ox, oy) = (region.x as u32, region.y as u32);
        let mut ink = 0u32;
        for y in 0..rh {
            for x in 0..rw {
                let want = a.get_pixel(ox + x, oy + y).0;
                let got = b.get_pixel(x, y).0;
                // Tolerance of one antialiasing step: the two rasters are the same drawing at
                // the same scale but with different pixmap ORIGINS, and resvg's edge coverage
                // can round a boundary pixel's alpha by 1/255. Ink placement is exact; only the
                // faintest edge sample may differ.
                for ch in 0..4 {
                    assert!(
                        want[ch].abs_diff(got[ch]) <= 2,
                        "mismatch at region ({x},{y}) = picture ({},{}): {want:?} vs {got:?}",
                        ox + x,
                        oy + y,
                    );
                }
                ink += u32::from(got[3] > 0);
            }
        }
        assert!(ink > 100, "the region raster drew almost nothing ({ink} px) — vacuous test");
        // And the full raster has NO ink outside the region (so nothing was lost by cropping).
        for y in 0..frame.1 {
            for x in 0..frame.0 {
                let inside = x >= ox && x < ox + rw && y >= oy && y < oy + rh;
                if !inside {
                    assert_eq!(a.get_pixel(x, y).0[3], 0, "ink at ({x},{y}) is outside the region");
                }
            }
        }
    }

    /// The measured cost driver (DRAGON-362): rendering the text layer is `O(raster area)` —
    /// the glyph pass is a rounding error next to the pixmap allocation and the
    /// premultiply→straight-alpha conversion. This pins the CONSEQUENCE rather than a wall
    /// time (which would be a flaky assertion): the raster the editor actually builds for a
    /// caption on a 5K capture is orders of magnitude smaller than the frame it used to build.
    #[test]
    fn text_layer_raster_area_is_bounded_by_the_caption_not_the_capture() {
        let frame = (5120u32, 2880u32);
        let item = caption(frame, 200.0, 300.0, 64.0);
        let region = text_layer_region(std::slice::from_ref(&item), frame).expect("region");
        // Fit zoom on a 2560-wide screen: visual_scale ≈ 0.45.
        let scale = super::super::edit::layer_raster_scale(1.0, 2311.0 / 5120.0);
        let (pw, ph) = super::super::edit::layer_raster_dims(
            (region.w as u32, region.h as u32),
            scale,
        );
        let px = pw as u64 * ph as u64;
        let old_full_frame_px = 5120u64 * 2880;
        assert!(
            px * 50 < old_full_frame_px,
            "raster {pw}x{ph} = {px}px is not decisively smaller than the old {old_full_frame_px}px"
        );
        // ...and still at least the caption's on-screen size, so it is not soft either.
        assert!(pw as f32 >= region.w * scale - 1.0);
    }

    // ── DRAGON-367/368: a move OR a resize must not re-raster ────────────────────────────

    /// The mechanism behind the owner's "scale the text up and it lags, scale it down and it is
    /// fine": the text layer's region grows with the caption's own extent AND its padding, so the
    /// raster's AREA grows super-linearly with the type size. A re-render per motion event is
    /// therefore cheap for a small caption and expensive for a large one — which is exactly the
    /// asymmetry reported, and exactly why the fix has to be to skip the render, not to shrink it.
    #[test]
    fn the_text_raster_area_grows_super_linearly_with_the_type_size() {
        let frame = (5120u32, 2880u32);
        let area = |size: f32| {
            let item = caption(frame, 200.0, 300.0, size);
            let r = text_layer_region(std::slice::from_ref(&item), frame).expect("region");
            (r.w * r.h) as f64
        };
        let (small, large) = (area(16.0), area(64.0));
        assert!(
            large > small * 4.0,
            "a 4x type size only grew the raster {:.1}x — the reported asymmetry has no mechanism",
            large / small,
        );
    }

    /// DRAGON-368: the padded region genuinely CONTAINS the ink it claims to. The half-em slack
    /// was replaced by the measured [`TEXT_INK_OVERHANG_EM`], so this is the guard that the
    /// smaller number is still honest — it renders real samples (accents that overshoot the
    /// ascent, descenders, emoji, punctuation) in BOTH faces at a hairline and at the heaviest
    /// pencil, and asserts no inked pixel escapes the padded bound.
    ///
    /// A CJK run is deliberately absent: our advance ladder under-measures glyphs that fall
    /// through to a system face, so its stored box can be ~5 em narrower than its ink. That is a
    /// `text_shape` measurement bug, not a padding one (a half-em never covered it either) and it
    /// is out of DRAGON-368's scope — but it is why this test names what it covers.
    #[test]
    fn the_padded_region_actually_contains_the_ink() {
        use super::super::text_annot::TextFont;
        let frame = (2000u32, 700u32);
        let samples = [
            "The quick brown fox jumps over the lazy dog",
            "ÀÉÎÕÜ ĝĵŷ ÅÆØ Ïÿ Ǻ ẞ ﬁﬂ",
            "gjpqy_,;| WAV// {}[]()",
            "1234567890 @#$%^&*",
            "hi 😀🎉 there",
        ];
        for font in [TextFont::Hand, TextFont::Clean] {
            for pencil in [1.0f32, 6.0, 12.0] {
                for text in samples {
                    let item = AnnotationItem {
                        id: AnnotId(1),
                        color: [255, 255, 255, 255],
                        kind: reflow_text(
                            text,
                            64.0,
                            font,
                            AnnotRect { x: 300.0, y: 300.0, w: 0.0, h: 0.0 },
                            false,
                            pencil,
                            (frame.0 as f32, frame.1 as f32),
                        ),
                    };
                    let items = std::slice::from_ref(&item);
                    let (x0, y0, x1, y1) = text_padded_bounds(items).expect("padded bounds");
                    // Render into the WHOLE frame so ink outside the padded bound is visible
                    // rather than being cropped away by the region itself.
                    let whole =
                        AnnotRect { x: 0.0, y: 0.0, w: frame.0 as f32, h: frame.1 as f32 };
                    let img = render_text_layer(items, frame, whole, frame.0, frame.1)
                        .expect("a raster");
                    let mut ink = 0u32;
                    for (x, y, px) in img.enumerate_pixels() {
                        if px.0[3] == 0 {
                            continue;
                        }
                        ink += 1;
                        let (fx, fy) = (x as f32, y as f32);
                        assert!(
                            fx + 1.0 > x0 && fx < x1 && fy + 1.0 > y0 && fy < y1,
                            "{font:?}/pencil {pencil}: ink at ({x},{y}) escapes the padded bound \
                             ({x0:.1},{y0:.1})-({x1:.1},{y1:.1}) for {text:?}",
                        );
                    }
                    assert!(ink > 100, "{text:?} drew almost nothing ({ink} px) — vacuous");
                }
            }
        }
    }

    /// DRAGON-368: the region is NO LONGER clipped to the picture, which is what makes the reuse
    /// fast path reachable at all for a large caption (a clipped raster does not contain its own
    /// ink, so it can never be re-placed) — and what lets a caption be dragged off the canvas.
    /// Nothing draws outside the picture as a result: the GPU scissors the layer to the shader
    /// widget, which IS the picture. See [`text_layer_region`].
    #[test]
    fn the_region_may_hang_off_the_picture_and_always_contains_its_ink() {
        let frame = (1024u32, 512u32);
        // A caption pushed hard against the top-left, and one against the bottom-right.
        for (x, y) in [(-40.0f32, -30.0f32), (900.0, 470.0)] {
            let item = caption(frame, x, y, 48.0);
            let items = std::slice::from_ref(&item);
            let r = text_layer_region(items, frame).expect("a region");
            let (x0, y0, x1, y1) = text_padded_bounds(items).expect("padded bounds");
            // THE invariant the fast path rests on: the region contains the whole padded ink,
            // whatever the picture's edges do.
            assert!(
                r.x <= x0 && r.y <= y0 && r.x + r.w >= x1 && r.y + r.h >= y1,
                "region {r:?} does not contain the padded ink ({x0},{y0})-({x1},{y1})",
            );
        }
        // …and it really does leave the picture rather than being silently clamped.
        let out = caption(frame, -40.0, -30.0, 48.0);
        let r = text_layer_region(std::slice::from_ref(&out), frame).expect("a region");
        assert!(r.x < 0.0 && r.y < 0.0, "region {r:?} was clamped back onto the picture");
    }

    /// The predicate the fast path rests on: MOVING a caption changes nothing the raster is a
    /// function of, so it reports the offset (at scale 1) and the pixels are re-used.
    #[test]
    fn a_pure_move_is_recognised_as_a_rigid_translation() {
        let frame = (5120u32, 2880u32);
        let before = vec![caption(frame, 200.0, 300.0, 64.0)];
        let moved = vec![AnnotationItem {
            kind: translated_kind(&before[0].kind, 137.0, -42.0),
            ..before[0].clone()
        }];
        let d = text_layer_xform(
            &text_render_sigs(&before, frame),
            &text_render_sigs(&moved, frame),
        );
        assert_eq!(d, Some(TextXform { scale: 1.0, dx: 137.0, dy: -42.0 }));
        // A zero move (pointer jitter inside one source pixel) is still a translation — it must
        // not fall back to a re-render either.
        assert_eq!(
            text_layer_xform(
                &text_render_sigs(&before, frame),
                &text_render_sigs(&before, frame)
            ),
            Some(TextXform { scale: 1.0, dx: 0.0, dy: 0.0 }),
        );
    }

    /// DRAGON-368's own predicate: a continuous handle SCALE is a similarity of the same drawing,
    /// so it too re-uses the raster. This is the test the whole ticket turns on — without it,
    /// removing the size ladder would have made every resize event re-render.
    #[test]
    fn a_uniform_scale_is_recognised_and_reports_its_factor() {
        use crate::geometry::Corner;
        let frame = (5120u32, 2880u32);
        let before = vec![caption(frame, 400.0, 500.0, 64.0)];
        // The real gesture, not a hand-built scene: pull the SE corner outward.
        let after: Vec<AnnotationItem> = before
            .iter()
            .map(|it| AnnotationItem {
                kind: edited_kind(
                    &it.kind,
                    Grab::Corner(Corner::Se),
                    (0.0, 0.0),
                    (60.0, 60.0),
                    (frame.0 as f32, frame.1 as f32),
                    false,
                ),
                ..it.clone()
            })
            .collect();
        let AnnotKind::Text { size_px: was, .. } = &before[0].kind else { panic!("text") };
        let AnnotKind::Text { size_px: now, .. } = &after[0].kind else { panic!("text") };
        assert!(now > was, "the drag must have grown the type for this test to mean anything");
        assert_ne!(now, was, "…and it must not have snapped back to the old size");
        let xf = text_layer_xform(
            &text_render_sigs(&before, frame),
            &text_render_sigs(&after, frame),
        )
        .expect("a scale is a similarity of the same drawing");
        assert!(
            (xf.scale - now / was).abs() < 1e-4,
            "reported scale {} is not the type-size ratio {}",
            xf.scale,
            now / was,
        );
        // An SE drag pins the NW corner. The transform scales about the picture's ORIGIN, not
        // about the anchor, so its offset is NOT zero — what must hold is that the anchor point
        // maps to itself, which is the geometric statement of "the corner you are not holding
        // stayed put".
        let AnnotKind::Text { rect, .. } = &before[0].kind else { panic!("text") };
        let anchor = xf.apply(AnnotRect { x: rect.x, y: rect.y, w: 0.0, h: 0.0 });
        assert!(
            (anchor.x - rect.x).abs() < 1e-2 && (anchor.y - rect.y).abs() < 1e-2,
            "an SE scale moved the pinned NW corner: {rect:?} → ({}, {})",
            anchor.x,
            anchor.y,
        );
    }

    /// …and the other half of the contract: anything that changes the DRAWING is refused, so a
    /// re-wrap, a restyle or an edit still re-renders. This is the test that stops the fast path
    /// from quietly freezing the caption's appearance.
    #[test]
    fn anything_that_changes_the_drawing_refuses_the_fast_path() {
        let frame = (5120u32, 2880u32);
        let fw = (frame.0 as f32, frame.1 as f32);
        let base = vec![caption(frame, 200.0, 300.0, 64.0)];
        let sigs = |items: &[AnnotationItem]| text_render_sigs(items, frame);
        let AnnotKind::Text { text, rect, font, stroke_w, .. } = base[0].kind.clone() else {
            panic!("text")
        };
        let with = |kind: AnnotKind| vec![AnnotationItem { kind, ..base[0].clone() }];

        // A CONSTRAINED box resized: the same glyphs re-wrapped inside a narrower frame. The
        // drawing genuinely changed, so no similarity can express it.
        let narrow = AnnotRect { w: 240.0, ..rect };
        let wrapped = with(reflow_text(&text, 64.0, font, narrow, true, stroke_w, fw));
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&wrapped)), None, "a re-wrap re-renders");

        // Edited text, a different face, a different outline weight, a different colour.
        let typed = with(reflow_text("Something else entirely", 64.0, font, rect, false, stroke_w, fw));
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&typed)), None, "an edit re-renders");
        let other_face = with(reflow_text(
            &text,
            64.0,
            super::super::text_annot::TextFont::Hand,
            rect,
            false,
            stroke_w,
            fw,
        ));
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&other_face)), None, "a face re-renders");
        let outlined = with(reflow_text(&text, 64.0, font, rect, false, 12.0, fw));
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&outlined)), None, "an outline re-renders");
        let recoloured =
            vec![AnnotationItem { color: [255, 0, 0, 255], ..base[0].clone() }];
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&recoloured)), None, "a colour re-renders");

        // An item added or removed, and an empty scene, all fall through to the normal path.
        let mut two = base.clone();
        two.push(AnnotationItem { id: AnnotId(2), ..caption(frame, 900.0, 900.0, 64.0) });
        assert_eq!(text_layer_xform(&sigs(&base), &sigs(&two)), None, "a new caption re-renders");
        assert_eq!(text_layer_xform(&sigs(&two), &sigs(&base)), None, "a deletion re-renders");
        assert_eq!(text_layer_xform(&[], &sigs(&base)), None, "an empty before has no raster");
    }

    // ── DRAGON-373: one raster PER text box ──────────────────────────────────────────────

    /// Splitting the layer per box is what lets the canvas draw each caption at its own depth,
    /// and it is CHEAPER too — measured, not assumed. Two captions in opposite corners of a 5K
    /// capture shared one raster spanning their UNION, which is most of the picture; per box each
    /// raster tracks only its own ink.
    #[test]
    fn per_box_rasters_are_far_smaller_than_the_union_they_replaced() {
        let frame = (5120u32, 2880u32);
        let both = vec![
            caption(frame, 200.0, 200.0, 96.0),
            AnnotationItem { id: AnnotId(2), ..caption(frame, 2600.0, 2400.0, 96.0) },
        ];
        let area = |r: AnnotRect| r.w * r.h;
        let union = area(text_layer_region(&both, frame).expect("two captions have a region"));
        let per_box: f32 = both
            .iter()
            .map(|it| area(text_layer_region(std::slice::from_ref(it), frame).expect("ink")))
            .sum();
        assert!(
            per_box * 4.0 < union,
            "per-box rasters ({per_box} px²) should be a small fraction of the shared union \
             ({union} px²)"
        );
    }

    /// …and they must draw the SAME PICTURE. Two per-box rasters composited at their own regions
    /// have to land pixel-for-pixel where the one shared raster put them — that is what keeps the
    /// live view identical to `rasterize_scene`'s single in-order pass, which is the parity this
    /// whole ticket is about restoring. (The regions are snapped to the same 64 px grid, so the
    /// glyphs fall on the same sub-pixel phase in either raster and the comparison is exact.)
    #[test]
    fn per_box_rasters_composite_to_the_same_picture_as_one_shared_raster() {
        let frame = (2560u32, 1440u32);
        let items = vec![
            caption(frame, 200.0, 200.0, 48.0),
            AnnotationItem { id: AnnotId(2), ..caption(frame, 300.0, 900.0, 48.0) },
        ];
        // `region` may hang off the picture (DRAGON-368), so composite into a canvas that is
        // offset enough to hold every region whole; both paths use the same one.
        let pad = 256i32;
        let (cw, ch) = (frame.0 + 2 * pad as u32, frame.1 + 2 * pad as u32);
        let over = |dst: &mut RgbaImage, src: &RgbaImage, at: (i32, i32)| {
            for (sx, sy, px) in src.enumerate_pixels() {
                let (x, y) = (at.0 + sx as i32 + pad, at.1 + sy as i32 + pad);
                if x < 0 || y < 0 || x >= cw as i32 || y >= ch as i32 {
                    continue;
                }
                let a = px.0[3] as u32;
                if a == 0 {
                    continue;
                }
                let d = dst.get_pixel_mut(x as u32, y as u32);
                for (i, s) in px.0.iter().take(3).enumerate() {
                    d.0[i] = ((*s as u32 * a + d.0[i] as u32 * (255 - a)) / 255) as u8;
                }
                d.0[3] = (a + d.0[3] as u32 * (255 - a) / 255).min(255) as u8;
            }
        };
        let render = |slice: &[AnnotationItem]| {
            let r = text_layer_region(slice, frame).expect("ink");
            let (pw, ph) = (r.w as u32, r.h as u32);
            (render_text_layer(slice, frame, r, pw, ph).expect("draws"), (r.x as i32, r.y as i32))
        };

        let mut shared_canvas = RgbaImage::new(cw, ch);
        let (shared, at) = render(&items);
        over(&mut shared_canvas, &shared, at);

        let mut split_canvas = RgbaImage::new(cw, ch);
        for item in &items {
            let (raster, at) = render(std::slice::from_ref(item));
            over(&mut split_canvas, &raster, at);
        }
        assert_eq!(
            shared_canvas.as_raw(),
            split_canvas.as_raw(),
            "per-box rasters must composite to the same picture as the shared one"
        );
    }

    // ── DRAGON-376: a re-render that would change nothing must not happen ────────────────

    /// The claim the whole fix rests on, checked against real PIXELS rather than inferred from a
    /// signature: the editor's caret/selection/blink state cannot reach the raster, so a
    /// drag-select re-render produces a byte-identical bitmap. Here the "drag" is a live
    /// [`super::edit::TextEdit`] whose caret and anchor sweep across the caption exactly as
    /// `text_drag_to` moves them — the item list, which is all the renderer sees, never changes.
    #[test]
    fn a_drag_select_cannot_change_a_single_pixel_of_the_raster() {
        let frame = (2560u32, 1440u32);
        let items = vec![caption(frame, 200.0, 300.0, 96.0)];
        let region = text_layer_region(&items, frame).expect("a caption has a region");
        let (pw, ph) =
            (region.w.ceil().max(1.0) as u32, region.h.ceil().max(1.0) as u32);
        let first = render_text_layer(&items, frame, region, pw, ph).expect("the caption draws");

        let AnnotKind::Text { text, .. } = &items[0].kind else { panic!("text") };
        let len = super::super::text_annot::char_len(text);
        let mut te = super::edit::TextEdit {
            id: AnnotId(1),
            caret: 0,
            anchor: None,
            snapshot: items.clone(),
            is_new: false,
            blink_on: true,
            history: Default::default(),
        };
        // 20 motion events of a drag-select, i.e. what used to cost a full re-raster each.
        for step in 0..20usize {
            if te.anchor.is_none() {
                te.anchor = Some(te.caret);
            }
            te.caret = (step * len / 20).min(len);
            te.blink_on = !te.blink_on;
            let again =
                render_text_layer(&items, frame, region, pw, ph).expect("the caption draws");
            assert_eq!(
                first.as_raw(),
                again.as_raw(),
                "caret {} produced different pixels — the raster DOES depend on edit state",
                te.caret
            );
        }
        // …and the same thing said in the currency of the gate: the raster-input signature is
        // untouched by any of it, so `refresh_text_display` short-circuits every one of those
        // events. `region`/`px` follow from the signature, so the scale is the only other input.
        let sig = text_render_sig(&items[0], frame).expect("a text item signs");
        assert!(
            text_raster_is_current(Some((0.5, &sig)), &sig, 0.5),
            "a chrome-only change must not re-render"
        );
    }

    /// The gate's other half: everything that DOES reach the renderer re-renders, and so does a
    /// zoom that changes the raster resolution. This is what stops the guard from freezing the
    /// caption's appearance — the same contract `anything_that_changes_the_drawing_refuses_the_
    /// fast_path` holds the DRAGON-368 proxy to.
    #[test]
    fn anything_that_reaches_the_renderer_still_re_renders() {
        let frame = (2560u32, 1440u32);
        let fw = (frame.0 as f32, frame.1 as f32);
        let base = [caption(frame, 200.0, 300.0, 64.0)];
        let sig = |item: &AnnotationItem| text_render_sig(item, frame).expect("a text item signs");
        let held = sig(&base[0]);
        let AnnotKind::Text { text, rect, font, stroke_w, .. } = base[0].kind.clone() else {
            panic!("text")
        };
        let with = |kind: AnnotKind| AnnotationItem { kind, ..base[0].clone() };
        let stale =
            |item: &AnnotationItem| !text_raster_is_current(Some((0.5, &held)), &sig(item), 0.5);

        assert!(stale(&with(reflow_text("Typed something", 64.0, font, rect, false, stroke_w, fw))));
        assert!(stale(&with(reflow_text(&text, 96.0, font, rect, false, stroke_w, fw))), "size");
        assert!(
            stale(&with(reflow_text(
                &text,
                64.0,
                super::super::text_annot::TextFont::Hand,
                rect,
                false,
                stroke_w,
                fw
            ))),
            "face"
        );
        assert!(stale(&with(reflow_text(&text, 64.0, font, rect, false, 12.0, fw))), "outline");
        assert!(stale(&with(translated_kind(&base[0].kind, 40.0, 0.0))), "a move");
        assert!(
            stale(&AnnotationItem { color: [255, 0, 0, 255], ..base[0].clone() }),
            "a recolour"
        );
        // A zoom step that moves the raster resolution re-renders even with an identical scene.
        assert!(
            !text_raster_is_current(Some((0.5, &held)), &held, 0.75),
            "a new raster scale must re-render"
        );
        // No raster yet (a new box, one that just gained ink, or a render that made no pixels).
        assert!(!text_raster_is_current(None, &held, 0.5), "no raster to re-use");
    }

    /// A GROUP gesture only re-uses the raster when the members travel TOGETHER: the layer holds
    /// them in one texture, so members moving (or scaling) by different amounts is not a
    /// similarity of it.
    #[test]
    fn a_group_move_must_be_rigid_for_the_whole_layer() {
        let frame = (5120u32, 2880u32);
        let a = caption(frame, 200.0, 300.0, 48.0);
        let b = AnnotationItem { id: AnnotId(2), ..caption(frame, 1500.0, 900.0, 48.0) };
        let before = vec![a.clone(), b.clone()];
        let rigid = vec![
            AnnotationItem { kind: translated_kind(&a.kind, 60.0, 10.0), ..a.clone() },
            AnnotationItem { kind: translated_kind(&b.kind, 60.0, 10.0), ..b.clone() },
        ];
        assert_eq!(
            text_layer_xform(&text_render_sigs(&before, frame), &text_render_sigs(&rigid, frame)),
            Some(TextXform { scale: 1.0, dx: 60.0, dy: 10.0 }),
        );
        let skewed = vec![
            AnnotationItem { kind: translated_kind(&a.kind, 60.0, 10.0), ..a.clone() },
            AnnotationItem { kind: translated_kind(&b.kind, 60.0, 40.0), ..b },
        ];
        assert_eq!(
            text_layer_xform(&text_render_sigs(&before, frame), &text_render_sigs(&skewed, frame)),
            None,
            "members that separated are not one rigid translation",
        );
        // One member scaled while the other only moved is likewise not one similarity.
        let one_scaled = vec![
            AnnotationItem { kind: translated_kind(&a.kind, 60.0, 10.0), ..a.clone() },
            AnnotationItem { id: AnnotId(2), ..caption(frame, 1560.0, 910.0, 96.0) },
        ];
        assert_eq!(
            text_layer_xform(
                &text_render_sigs(&before, frame),
                &text_render_sigs(&one_scaled, frame)
            ),
            None,
            "one member rescaled is not a similarity of the whole layer",
        );
    }

    /// Where the transformed raster may be RE-PLACED. The coverage refusal is correctness — a
    /// raster that never held the whole padded ink would reveal a missing tail — and the shrink
    /// refusal is quality (see [`TEXT_PROXY_MIN_SCALE`]). There is deliberately NO
    /// inside-the-picture condition: that is what DRAGON-368 removed.
    #[test]
    fn a_raster_is_re_placed_while_it_covers_the_ink_and_is_not_over_shrunk() {
        let region = AnnotRect { x: 128.0, y: 128.0, w: 256.0, h: 128.0 };
        let mv = |dx, dy| TextXform { scale: 1.0, dx, dy };
        // The ink sits comfortably inside the raster: a modest slide is exact.
        assert_eq!(
            placed_text_region(region, mv(64.0, 32.0), (224.0, 192.0, 364.0, 232.0)),
            Some(AnnotRect { x: 192.0, y: 160.0, w: 256.0, h: 128.0 }),
        );
        // A zero move is a legal (and exact) placement.
        let padded = (160.0, 160.0, 300.0, 200.0);
        assert_eq!(placed_text_region(region, mv(0.0, 0.0), padded), Some(region));
        // A SCALE maps the region the same way the drawing was mapped.
        let grown = placed_text_region(
            region,
            TextXform { scale: 2.0, dx: 0.0, dy: 0.0 },
            (320.0, 320.0, 600.0, 400.0),
        )
        .expect("a scale-up is placeable");
        assert_eq!(grown, AnnotRect { x: 256.0, y: 256.0, w: 512.0, h: 256.0 });
        // Ink that pokes outside the placed region means the raster never held all of it.
        assert_eq!(
            placed_text_region(region, mv(0.0, 0.0), (100.0, 160.0, 300.0, 200.0)),
            None,
            "ink left of the raster was never drawn into it",
        );
        assert_eq!(
            placed_text_region(region, mv(0.0, 0.0), (160.0, 160.0, 300.0, 400.0)),
            None,
            "ink below the raster was never drawn into it",
        );
        // DRAGON-368: leaving the PICTURE is explicitly fine — the GPU scissors the layer, and
        // text is allowed to hang off the canvas. These used to be refusals.
        assert!(placed_text_region(region, mv(-200.0, 0.0), (-72.0, 160.0, 68.0, 200.0)).is_some());
        assert!(placed_text_region(region, mv(0.0, 400.0), (160.0, 560.0, 300.0, 600.0)).is_some());
        // A shrink past an octave re-renders instead (quality, see TEXT_PROXY_MIN_SCALE); one
        // exactly AT the octave is still re-used.
        let half = TextXform { scale: TEXT_PROXY_MIN_SCALE, dx: 0.0, dy: 0.0 };
        assert!(placed_text_region(region, half, (80.0, 80.0, 150.0, 100.0)).is_some());
        let quarter = TextXform { scale: 0.25, dx: 0.0, dy: 0.0 };
        assert_eq!(placed_text_region(region, quarter, (40.0, 40.0, 75.0, 50.0)), None);
        // Degenerate inputs never produce a placement.
        assert_eq!(placed_text_region(region, mv(f32::NAN, 0.0), padded), None);
        assert_eq!(
            placed_text_region(region, TextXform { scale: f32::NAN, dx: 0.0, dy: 0.0 }, padded),
            None,
        );
    }

    /// The end-to-end property the fix exists for: a caption dragged across the picture yields
    /// the SAME pixels whether the raster is re-rendered at the new place or simply re-placed
    /// there. If this ever fails, the fast path is showing something the slow path would not.
    #[test]
    fn re_placing_the_raster_draws_what_re_rendering_would_have_drawn() {
        let frame = (1024u32, 512u32);
        let before = vec![caption(frame, 180.0, 160.0, 24.0)];
        let region = text_layer_region(&before, frame).expect("region");
        let (pw, ph) = (region.w as u32, region.h as u32);
        let raster = render_text_layer(&before, frame, region, pw, ph).expect("raster");

        let (dx, dy) = (96.0f32, 64.0f32);
        let after = vec![AnnotationItem {
            kind: translated_kind(&before[0].kind, dx, dy),
            ..before[0].clone()
        }];
        // The fast path must accept this move and say where the raster goes...
        let xf = text_layer_xform(
            &text_render_sigs(&before, frame),
            &text_render_sigs(&after, frame),
        )
        .expect("a move is a similarity");
        assert_eq!(xf, TextXform { scale: 1.0, dx, dy });
        let padded = text_padded_bounds(&after).expect("bounds");
        let slid = placed_text_region(region, xf, padded).expect("a placement");
        // ...and rendering the moved caption into that very region must reproduce the raster.
        let fresh = render_text_layer(&after, frame, slid, pw, ph).expect("raster");
        assert_eq!(fresh.dimensions(), raster.dimensions());
        let mut ink = 0u32;
        for (a, b) in raster.pixels().zip(fresh.pixels()) {
            for ch in 0..4 {
                assert_eq!(a.0[ch], b.0[ch], "the re-placed raster differs from a re-render");
            }
            ink += u32::from(a.0[3] > 0);
        }
        assert!(ink > 100, "the caption drew almost nothing ({ink} px) — vacuous test");
    }

    /// DRAGON-368 — the proxy's SCALED counterpart of the test above, and the honest statement
    /// of what a proxy is: re-placing the raster under a uniform scale puts the ink in the same
    /// PLACE a re-render would, only at the resolution it was rendered at.
    ///
    /// So this compares GEOMETRY, not pixels: the ink's bounding box in picture coordinates,
    /// which is what "the caption is where you dragged it" means. Sharpness is deliberately not
    /// asserted — that is the trade the proxy makes, and `annot_gesture_end` is what settles it.
    #[test]
    fn a_scaled_proxy_puts_the_ink_where_a_re_render_would() {
        use crate::geometry::Corner;
        let frame = (1400u32, 900u32);
        let fw = (frame.0 as f32, frame.1 as f32);
        let before = vec![caption(frame, 120.0, 200.0, 32.0)];
        let region = text_layer_region(&before, frame).expect("region");
        let after: Vec<AnnotationItem> = before
            .iter()
            .map(|it| AnnotationItem {
                kind: edited_kind(&it.kind, Grab::Corner(Corner::Se), (0.0, 0.0), (40.0, 40.0), fw, false),
                ..it.clone()
            })
            .collect();
        let xf = text_layer_xform(
            &text_render_sigs(&before, frame),
            &text_render_sigs(&after, frame),
        )
        .expect("a similarity");
        assert!(xf.scale > 1.0, "the drag must have grown the type");
        let padded = text_padded_bounds(&after).expect("bounds");
        let placed = placed_text_region(region, xf, padded).expect("a placement");

        // Where the ink sits in the OLD raster, mapped through the proxy's own placement...
        let (pw, ph) = (region.w as u32, region.h as u32);
        let old = render_text_layer(&before, frame, region, pw, ph).expect("raster");
        let ink_box = |img: &RgbaImage| {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            for (x, y, p) in img.enumerate_pixels() {
                if p.0[3] > 0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            assert!(x1 > 0, "nothing drawn — vacuous test");
            (x0 as f32, y0 as f32, x1 as f32, y1 as f32)
        };
        let (ox0, oy0, ox1, oy1) = ink_box(&old);
        // The old raster is 1:1 with SOURCE px here, so raster px k IS picture px region.x + k;
        // the proxy stretches that whole rect onto `placed`.
        let map = |v: f32, from_lo: f32, from_len: f32, to_lo: f32, to_len: f32| {
            to_lo + (v + from_lo - from_lo) / from_len * to_len
        };
        let proxied = (
            map(ox0, 0.0, region.w, placed.x, placed.w),
            map(oy0, 0.0, region.h, placed.y, placed.h),
            map(ox1, 0.0, region.w, placed.x, placed.w),
            map(oy1, 0.0, region.h, placed.y, placed.h),
        );
        // ...must match where a genuine re-render puts it, to within a couple of source px of
        // antialiasing + the region's 64px grid rounding riding the scale.
        let fresh_region = text_layer_region(&after, frame).expect("region");
        let (fw2, fh2) = (fresh_region.w as u32, fresh_region.h as u32);
        let fresh = render_text_layer(&after, frame, fresh_region, fw2, fh2).expect("raster");
        let (fx0, fy0, fx1, fy1) = ink_box(&fresh);
        let truth = (
            fresh_region.x + fx0,
            fresh_region.y + fy0,
            fresh_region.x + fx1,
            fresh_region.y + fy1,
        );
        let tol = 3.0 + xf.scale * 2.0;
        for (got, want, name) in [
            (proxied.0, truth.0, "left"),
            (proxied.1, truth.1, "top"),
            (proxied.2, truth.2, "right"),
            (proxied.3, truth.3, "bottom"),
        ] {
            assert!(
                (got - want).abs() <= tol,
                "the proxy puts the ink's {name} edge at {got:.1}, a re-render at {want:.1}",
            );
        }
    }

    // ── DRAGON-368: text may be dragged OFF the canvas ───────────────────────────────────

    /// The clamp itself: a text box keeps at least [`TEXT_MIN_ON_CANVAS_PX`] of ITSELF on the
    /// picture, per axis, and is otherwise free — which is what lets a caption's ink be aligned
    /// flush to (or past) an edge. The same number is what stays grabbable, since the canvas
    /// hit-tests this rect.
    #[test]
    fn a_text_box_may_leave_the_canvas_but_never_entirely() {
        let (fw, fh) = (1000.0f32, 800.0f32);
        let r = |x: f32, y: f32| AnnotRect { x, y, w: 300.0, h: 120.0 };
        // Well inside: untouched.
        let inside = clamp_text_rect_on_canvas(r(100.0, 100.0), fw, fh);
        assert_eq!((inside.x, inside.y), (100.0, 100.0));
        // Pushed off each edge: stopped exactly at the 5px overlap, not at the picture's edge.
        let left = clamp_text_rect_on_canvas(r(-9999.0, 100.0), fw, fh);
        assert_eq!(left.x, TEXT_MIN_ON_CANVAS_PX - 300.0, "5px of box must remain on the right");
        let right = clamp_text_rect_on_canvas(r(9999.0, 100.0), fw, fh);
        assert_eq!(right.x, fw - TEXT_MIN_ON_CANVAS_PX);
        let up = clamp_text_rect_on_canvas(r(100.0, -9999.0), fw, fh);
        assert_eq!(up.y, TEXT_MIN_ON_CANVAS_PX - 120.0);
        let down = clamp_text_rect_on_canvas(r(100.0, 9999.0), fw, fh);
        assert_eq!(down.y, fh - TEXT_MIN_ON_CANVAS_PX);
        // The overlap really is ≥ 5px on every one of those.
        for c in [left, right, up, down] {
            let ox = (c.x + c.w).min(fw) - c.x.max(0.0);
            let oy = (c.y + c.h).min(fh) - c.y.max(0.0);
            assert!(ox >= TEXT_MIN_ON_CANVAS_PX - 1e-3, "only {ox}px of box left horizontally");
            assert!(oy >= TEXT_MIN_ON_CANVAS_PX - 1e-3, "only {oy}px of box left vertically");
        }
        // A box NARROWER than the threshold is held by its whole width rather than being
        // unclampable (the `min` in the rule).
        let hair = clamp_text_rect_on_canvas(
            AnnotRect { x: 9999.0, y: 9999.0, w: 2.0, h: 1.0 },
            fw,
            fh,
        );
        assert_eq!((hair.x, hair.y), (fw - 2.0, fh - 1.0));
        // A degenerate frame leaves the rect alone rather than inventing a position.
        assert_eq!(clamp_text_rect_on_canvas(r(50.0, 60.0), 0.0, 0.0), r(50.0, 60.0));
    }

    /// The rule reaching the actual gesture — and the boundary of it: TEXT moves off canvas,
    /// every other kind still clamps wholly inside (a shape's ink IS its geometry, so a
    /// half-off box would just be a broken box), and a text box's RESIZE handles keep their
    /// old inside-the-picture clamp while its SCALE follows the move rule.
    #[test]
    fn only_text_drags_off_the_canvas() {
        let frame = (1000.0f32, 800.0f32);
        let far = 5000.0f32;
        // TEXT: a Move ends up hanging off the right edge, with 5px still on.
        let text = text_kind(false, 32.0, AnnotRect { x: 400.0, y: 300.0, w: 0.0, h: 0.0 }, frame);
        let moved = edited_kind(&text, Grab::Move, (0.0, 0.0), (far, far), frame, false);
        let AnnotKind::Text { rect, .. } = &moved else { panic!("text") };
        assert!(rect.x + rect.w > frame.0, "the caption did not leave the canvas at all");
        assert!(
            (rect.x - (frame.0 - TEXT_MIN_ON_CANVAS_PX)).abs() < 1e-3,
            "a moved caption stopped somewhere other than the 5px rule: {rect:?}",
        );
        // A BOX with the same drag is still held wholly inside.
        let bx = AnnotKind::Box {
            rect: AnnotRect { x: 400.0, y: 300.0, w: 100.0, h: 80.0 },
            stroke_w: 4.0,
            fill: None,
        };
        let AnnotKind::Box { rect: br, .. } =
            edited_kind(&bx, Grab::Move, (0.0, 0.0), (far, far), frame, false)
        else {
            panic!("box")
        };
        assert!(br.x + br.w <= frame.0 + 1e-3 && br.y + br.h <= frame.1 + 1e-3);
        // A CONSTRAINED text box's resize handle keeps the old clamp: it is dragging the WRAP
        // FRAME, not placing the ink, so nothing is gained by letting it leave.
        let boxed_text =
            text_kind(true, 32.0, AnnotRect { x: 400.0, y: 300.0, w: 200.0, h: 90.0 }, frame);
        let AnnotKind::Text { rect: cr, .. } = edited_kind(
            &boxed_text,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (far, far),
            frame,
            false,
        ) else {
            panic!("text")
        };
        assert!(cr.x + cr.w <= frame.0 + 1e-3, "a constrained resize left the picture: {cr:?}");
    }

    /// The wrap math has to survive coordinates a clamped box could never produce (DRAGON-368):
    /// a caption dragged clean off either edge. Neither may yield nonsense — and a MOVE must
    /// never re-wrap an existing caption, because that is both visually wrong and what would
    /// knock the drag off the raster-reuse fast path. DRAGON-378 is what makes that hold in BOTH
    /// directions: the cap no longer knows where the box IS, so no translation can reach it.
    #[test]
    fn the_auto_wrap_cap_survives_off_canvas_origins_and_never_re_wraps_a_move() {
        let frame = (1200u32, 700u32);
        let fw = frame.0 as f32;
        // The PICTURE's width is the cap — one number, whatever the box's width or position…
        assert_eq!(text_auto_cap(0.0, 32.0, fw), fw);
        assert_eq!(text_auto_cap(400.0, 32.0, fw), fw);
        // …floored at one glyph, so a picture narrower than the type can never divide the
        // caption into a zero-width (infinite) column…
        assert_eq!(text_auto_cap(0.0, 900.0, 10.0), 900.0);
        // …and floored at the box's OWN width (DRAGON-368), so a box already wider than the
        // picture — only a scale can make one — is never collapsed back into it.
        assert_eq!(text_auto_cap(fw + 800.0, 32.0, fw), fw + 800.0);
        // An absurd frame still lands inside resvg's coordinate sanity.
        assert_eq!(text_auto_cap(0.0, 32.0, 1e9), super::super::text_annot::AUTO_WRAP_FALLBACK);

        // End to end, BOTH directions: a wide caption dragged clean off either edge keeps its
        // exact layout, and stays on the raster-reuse fast path. Dragging RIGHT used to shrink
        // the cap (the `rect_w` floor was what held it); dragging LEFT used to GROW it, and could
        // silently un-wrap the caption mid-drag — the half DRAGON-368's floor could not cover.
        for dx in [5000.0f32, -5000.0] {
            let before = caption(frame, 500.0, 200.0, 32.0);
            let AnnotKind::Text { rect: r0, text: t0, .. } = before.kind.clone() else { panic!() };
            assert!(r0.w > 400.0, "the caption must be wide enough for the cap to threaten it");
            let after =
                edited_kind(&before.kind, Grab::Move, (0.0, 0.0), (dx, 0.0), (fw, 700.0), false);
            let AnnotKind::Text { rect: r1, text: t1, .. } = after.clone() else { panic!() };
            assert_eq!(t0, t1);
            assert!(
                (r1.w - r0.w).abs() < 1e-3 && (r1.h - r0.h).abs() < 1e-3,
                "the move ({dx:+}) re-wrapped the caption: {r0:?} → {r1:?}",
            );
            assert!(
                text_layer_xform(
                    &text_render_sigs(std::slice::from_ref(&before), frame),
                    &text_render_sigs(&[AnnotationItem { kind: after, ..before.clone() }], frame),
                )
                .is_some(),
                "a drag of {dx:+} fell off the raster-reuse fast path",
            );
        }
    }

    /// BAKE PARITY for an off-canvas caption (DRAGON-368): the bake composites at the SOURCE
    /// frame, so ink outside the picture is clipped by the output pixmap — which must be exactly
    /// the pixels the live layer shows, since the GPU scissors the layer to the same picture.
    /// This pins that `render_into` with an origin outside the pixmap CLIPS rather than
    /// misbehaving, and that the surviving pixels agree with the live raster.
    #[test]
    fn an_off_canvas_caption_bakes_the_same_pixels_the_live_layer_shows() {
        let frame = (600u32, 300u32);
        // Hanging off the left AND the top, with only a corner of the box on the picture.
        let item = caption(frame, -120.0, -40.0, 40.0);
        let items = std::slice::from_ref(&item);
        let bake = rasterize_scene(items, frame.0, frame.1, 1.0, DEFAULT_ANNOT_CURVE_RADIUS)
            .expect("the bake must not fail on an off-canvas caption");
        assert_eq!(bake.dimensions(), frame, "the bake is always the source frame");
        let region = text_layer_region(items, frame).expect("region");
        assert!(region.x < 0.0 && region.y < 0.0, "the region must genuinely hang off");
        let (rw, rh) = (region.w as u32, region.h as u32);
        let live = render_text_layer(items, frame, region, rw, rh).expect("live raster");
        // Every PICTURE pixel: what the bake drew is what the live layer holds at that same
        // picture coordinate (the layer is 1:1 with source px here). Ink that fell outside the
        // picture is simply absent from the bake — clipped, not wrapped or shifted.
        //
        // This is byte-for-byte, and it is what pins the BLED bake pixmap (see the Text arm of
        // [`rasterize_scene`]). Rendering an off-canvas caption straight into the output made
        // tiny_skia's path clipper deposit phantom coverage on the boundary line — measured at
        // alpha up to 121/255 across picture ROW 0, a faint dotted line along the edge of the
        // exported image that the GPU-scissored live layer never showed. With the bleed the two
        // agree to within one antialiasing step — the same tolerance
        // `region_raster_places_glyphs_at_the_same_picture_position_as_a_full_frame_raster`
        // takes, and for the same reason: two pixmaps with different ORIGINS can round a
        // boundary sample's coverage by a hair. Ink PLACEMENT is exact.
        let (ox, oy) = (region.x, region.y);
        let mut ink = 0u32;
        for y in 0..frame.1 {
            for x in 0..frame.0 {
                let want = bake.get_pixel(x, y).0;
                let (lx, ly) = (x as f32 - ox, y as f32 - oy);
                let got = if lx >= 0.0 && ly >= 0.0 && (lx as u32) < rw && (ly as u32) < rh {
                    live.get_pixel(lx as u32, ly as u32).0
                } else {
                    [0, 0, 0, 0]
                };
                // ALPHA is the comparison: these are straight-alpha pixels, so a FULLY
                // transparent sample's RGB is whatever the renderer happened to leave behind and
                // carries no information. Colour is compared only where there is ink to colour.
                assert!(
                    want[3].abs_diff(got[3]) <= 2,
                    "picture ({x},{y}): bake {want:?} vs live {got:?} — the coverage differs",
                );
                if want[3] > 32 && got[3] > 32 {
                    assert_eq!(
                        want[..3],
                        got[..3],
                        "picture ({x},{y}): bake and live disagree on the glyph COLOUR",
                    );
                }
                ink += u32::from(want[3] > 0);
            }
        }
        assert!(ink > 100, "the off-canvas caption drew almost nothing ({ink} px) — vacuous test");
        // And it really is only PART of the caption: a fully on-canvas copy inks strictly more.
        let inboard = vec![caption(frame, 10.0, 10.0, 40.0)];
        let full = rasterize_scene(&inboard, frame.0, frame.1, 1.0, DEFAULT_ANNOT_CURVE_RADIUS)
            .expect("bake");
        let count = |img: &RgbaImage| img.pixels().filter(|p| p.0[3] > 0).count();
        assert!(count(&full) > count(&bake), "nothing was actually clipped — vacuous test");
    }

    /// DRAGON-368 — what one MOTION EVENT costs the update path, at several type sizes, for BOTH
    /// gestures. This is the pairing the ticket turns on: the RE-RENDER column is what an event
    /// used to pay (and it climbs steeply with the type size, which is the owner's "512px lags"),
    /// the PROXY column is what it pays now, and the HIT columns say how often the proxy is
    /// actually reached over a realistic gesture — the number that was 0% at 512px before this
    /// ticket, which is why DRAGON-367 alone did not fix the drag. `#[ignore]`d: it is a
    /// benchmark, not an assertion.
    #[test]
    #[ignore]
    fn bench_text_gesture_paths() {
        use crate::geometry::Corner;
        use std::time::Instant;
        let frame = (5120u32, 2880u32);
        // Fit zoom on the owner's 2560-wide screen: the picture is shown ~2311 px wide.
        let vscale = 2311.0 / 5120.0;
        let fwh = (frame.0 as f32, frame.1 as f32);
        println!(
            "{:>6}  {:>13}  {:>13}  {:>9}  {:>9}  {:>8}  {:>10}",
            "type", "region(src)", "raster(px)", "render", "proxy", "drag hit", "resize hit"
        );
        for size in [64.0f32, 128.0, 256.0, 512.0, 768.0] {
            let start = vec![caption(frame, 900.0, 700.0, size)];
            let Some(region) = text_layer_region(&start, frame) else { continue };
            let scale = super::super::edit::layer_raster_scale(1.0, vscale);
            let (pw, ph) = super::super::edit::layer_raster_dims(
                (region.w.ceil().max(1.0) as u32, region.h.ceil().max(1.0) as u32),
                scale,
            );
            // One re-render: the cost every event pays whenever the proxy is refused.
            let _ = render_text_layer(&start, frame, region, pw, ph);
            let n = 10;
            let t = Instant::now();
            for _ in 0..n {
                std::hint::black_box(render_text_layer(&start, frame, region, pw, ph));
            }
            let render_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

            // The proxy decision itself — the whole of what an event now costs the update path.
            let moved = vec![AnnotationItem {
                kind: translated_kind(&start[0].kind, 3.0, 2.0),
                ..start[0].clone()
            }];
            let n = 2000;
            let t = Instant::now();
            for _ in 0..n {
                let b = text_render_sigs(&start, frame);
                let a = text_render_sigs(&moved, frame);
                let xf = text_layer_xform(&b, &a).expect("a move");
                let p = text_padded_bounds(&moved).expect("bounds");
                std::hint::black_box(placed_text_region(region, xf, p));
            }
            let proxy_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

            // A realistic gesture, replayed event by event: each step asks the proxy for a
            // placement and, when refused, pays a re-render (which also re-bases the region).
            let sweep = |steps: u32, step_kind: &dyn Fn(&[AnnotationItem], u32) -> Vec<AnnotationItem>| {
                let (mut items, mut geom) = (start.clone(), region);
                let (mut hits, mut evts) = (0u32, 0u32);
                for k in 1..=steps {
                    let next = step_kind(&items, k);
                    evts += 1;
                    let placed = text_layer_xform(
                        &text_render_sigs(&items, frame),
                        &text_render_sigs(&next, frame),
                    )
                    .and_then(|xf| {
                        text_padded_bounds(&next).and_then(|p| placed_text_region(geom, xf, p))
                    });
                    match placed {
                        Some(r) => {
                            hits += 1;
                            geom = r;
                        }
                        None => geom = text_layer_region(&next, frame).unwrap_or(geom),
                    }
                    items = next;
                }
                100.0 * hits as f64 / evts as f64
            };
            // The caption walked right + down across the picture in 8px steps.
            let drag_hit = sweep(240, &|items, k| {
                let (dx, dy) = if k % 2 == 0 { (8.0, 3.0) } else { (8.0, -1.0) };
                items
                    .iter()
                    .map(|it| AnnotationItem {
                        kind: translated_kind(&it.kind, dx, dy),
                        ..it.clone()
                    })
                    .collect()
            });
            // The SE corner pulled outward in 6px steps — with the ladder gone, a fresh
            // `size_px` on every single event.
            let resize_hit = sweep(120, &|_, k| {
                let drag = k as f32 * 6.0;
                start
                    .iter()
                    .map(|it| AnnotationItem {
                        kind: edited_kind(
                            &it.kind,
                            Grab::Corner(Corner::Se),
                            (0.0, 0.0),
                            (drag, drag),
                            fwh,
                            false,
                        ),
                        ..it.clone()
                    })
                    .collect()
            });
            println!(
                "{size:>6.0}  {:>6.0}x{:<6.0}  {pw:>6}x{ph:<6}  {render_ms:>7.2}ms  \
                 {proxy_ms:>7.4}ms  {drag_hit:>7.1}%  {resize_hit:>9.1}%",
                region.w, region.h,
            );
        }
    }

    /// TEMP instrumentation (DRAGON-362) — the wall-clock numbers quoted in the ticket.
    /// `#[ignore]`d: it is a benchmark, not an assertion.
    #[test]
    #[ignore]
    fn bench_render_text_layer() {
        let frame = (5120u32, 2880u32);
        let item = caption(frame, 200.0, 300.0, 64.0);
        let full = AnnotRect { x: 0.0, y: 0.0, w: frame.0 as f32, h: frame.1 as f32 };
        let region = text_layer_region(std::slice::from_ref(&item), frame).expect("region");
        let cases: [(&str, AnnotRect, u32, u32); 5] = [
            ("OLD full-frame @ the 1024 box", full, 1024, 576),
            ("OLD full-frame @ screen size", full, 2560, 1440),
            ("OLD full-frame @ source", full, 5120, 2880),
            ("NEW region @ screen size", region, (region.w * 0.5) as u32, (region.h * 0.5) as u32),
            ("NEW region @ source", region, region.w as u32, region.h as u32),
        ];
        // Q: does the per-motion cost scale with the SOURCE FRAME independently of the output
        // pixmap? (The layout + SVG are built in frame coordinates.) Answer: no — with the
        // output pinned, a 1080p and a 5K frame cost the same. The cost is OUTPUT AREA only.
        for f in [(1920u32, 1080u32), (5120, 2880)] {
            let it = caption(f, 200.0, 300.0, 64.0);
            let rect = AnnotRect { x: 0.0, y: 0.0, w: f.0 as f32, h: f.1 as f32 };
            let _ = render_text_layer(std::slice::from_ref(&it), f, rect, 1024, 576);
            const N: u32 = 20;
            let t0 = std::time::Instant::now();
            for _ in 0..N {
                assert!(render_text_layer(std::slice::from_ref(&it), f, rect, 1024, 576).is_some());
            }
            eprintln!("BENCH frame {:?} @ fixed 1024x576 output = {:8.3?}/render", f, t0.elapsed() / N);
        }
        for (label, rect, pw, ph) in cases {
            let _ = render_text_layer(std::slice::from_ref(&item), frame, rect, pw, ph);
            const N: u32 = 20;
            let t = std::time::Instant::now();
            for _ in 0..N {
                assert!(render_text_layer(std::slice::from_ref(&item), frame, rect, pw, ph).is_some());
            }
            eprintln!(
                "BENCH {label:32} {pw:5}x{ph:<5} = {:8.3?}/render, {:5.1} MB upload",
                t.elapsed() / N,
                (pw as f64 * ph as f64 * 4.0) / 1e6,
            );
        }
    }

    // ── DRAGON-364: numpad Enter exits, the two text-box resize modes, dropdown sync ──────

    #[test]
    fn only_the_numpad_enter_exits_a_live_text_edit() {
        use cosmic::iced::keyboard::{key::Named, Key, Location};
        // The whole point of plumbing `Location`: the two Enters are the SAME logical key, so
        // the location is the only thing that can separate them.
        let enter = Key::Named(Named::Enter);
        assert!(text_edit_exits(&enter, Location::Numpad), "numpad Enter settles the edit");
        assert!(
            !text_edit_exits(&enter, Location::Standard),
            "MAIN Enter must keep inserting a newline — the box is multi-line",
        );
        // Escape stays an exit from every location (it has no keypad twin, but be explicit).
        for loc in [Location::Standard, Location::Left, Location::Right, Location::Numpad] {
            assert!(text_edit_exits(&Key::Named(Named::Escape), loc), "Escape always settles");
        }
        // Nothing else exits — including keys that DO have a numpad twin, so the predicate is
        // keyed on the key AND the location, never on "any numpad press".
        for k in [
            Key::Named(Named::Tab),
            Key::Named(Named::Backspace),
            Key::Named(Named::Delete),
            Key::Named(Named::ArrowDown),
            Key::Named(Named::Home),
            Key::Character("5".into()),
            Key::Character(" ".into()),
        ] {
            for loc in [Location::Standard, Location::Numpad] {
                assert!(!text_edit_exits(&k, loc), "{k:?} at {loc:?} must not end the session");
            }
        }
    }

    // ── DRAGON-369: what a live text edit does with the DESTRUCTIVE preview chords ────────

    /// **The safety pin.** A live text edit swallows every PRIMARY chord it does not claim, and
    /// the claimed set is exactly Z / Y / A / C / X / V. `keyboard.rs`'s `preview_modal_key`
    /// routes into the text editor BEFORE the Preview keymap is consulted, so this inventory is
    /// the whole answer to "what happens if the user hits an editor chord mid-typing".
    ///
    /// The two that matter, both bound in the Preview context by DRAGON-369:
    /// * `Ctrl+D` (deselect all annotations) — NOT claimed, so it does nothing at all while
    ///   typing and can never deselect out from under an active edit;
    /// * `Ctrl+Shift+X` (delete the capture file, irreversibly) — lands on the text CUT arm, so
    ///   the worst it can do is cut the text selection. The capture is untouchable while a box
    ///   is being edited.
    #[test]
    fn a_live_text_edit_swallows_the_destructive_preview_chords() {
        use cosmic::iced::keyboard::{key::Named, Key};
        // Ctrl+D: the deselect chord is not claimed, with or without Shift → swallowed.
        assert_eq!(text_edit_chord(&Key::Character("d".into()), false), None);
        assert_eq!(text_edit_chord(&Key::Character("d".into()), true), None);
        // Ctrl+Shift+X: the DELETE-the-capture chord means CUT-the-text here — never the file.
        assert_eq!(
            text_edit_chord(&Key::Character("x".into()), true),
            Some(TextEditChord::Cut),
        );
        assert_eq!(
            text_edit_chord(&Key::Character("X".into()), true),
            Some(TextEditChord::Cut),
        );
        // The rest of the claimed set, unchanged (DRAGON-354 item 13) — Shift only ever
        // switches undo to redo.
        for (k, shift, want) in [
            ("z", false, TextEditChord::Undo),
            ("z", true, TextEditChord::Redo),
            ("y", false, TextEditChord::Redo),
            ("a", false, TextEditChord::SelectAll),
            ("c", false, TextEditChord::Copy),
            ("x", false, TextEditChord::Cut),
            ("v", false, TextEditChord::Paste),
        ] {
            assert_eq!(text_edit_chord(&Key::Character(k.into()), shift), Some(want), "{k}");
        }
        // Every OTHER preview hotkey letter is swallowed too, so no tool arms itself mid-word.
        for k in ["b", "e", "h", "i", "l", "m", "p", "s", "t", "u", "w", "g", "n", "q"] {
            assert_eq!(text_edit_chord(&Key::Character(k.into()), false), None, "{k}");
            assert_eq!(text_edit_chord(&Key::Character(k.into()), true), None, "{k}+shift");
        }
        // A non-character primary chord (Delete, Escape, an F-key) is swallowed as well.
        for k in [Named::Delete, Named::Backspace, Named::Escape, Named::F5, Named::Enter] {
            assert_eq!(text_edit_chord(&Key::Named(k), false), None, "{k:?}");
        }
    }

    /// A CLICK-created ("normal", auto) caption and a DRAG-created ("constrained") one, both
    /// laid out through the shared reflow seam so the geometry is the real thing.
    fn text_kind(constrained: bool, size: f32, rect: AnnotRect, frame: (f32, f32)) -> AnnotKind {
        reflow_text(
            "hello there world",
            size,
            super::super::text_annot::TextFont::Clean,
            rect,
            constrained,
            2.0,
            frame,
        )
    }

    #[test]
    fn resizing_a_constrained_box_rewraps_it_and_never_scales_the_glyphs() {
        let frame = (1000.0, 800.0);
        let orig = text_kind(true, 32.0, AnnotRect { x: 100.0, y: 100.0, w: 300.0, h: 0.0 }, frame);
        let AnnotKind::Text { rect: r0, size_px: s0, .. } = &orig else { panic!("text") };
        let (r0, s0) = (*r0, *s0);
        // Drag the EAST edge inward: the prison narrows, the text re-wraps inside it, and the
        // type size is untouched — that is the constrained contract.
        let out = edited_kind(
            &orig,
            Grab::Edge(crate::geometry::Edge::E),
            (0.0, 0.0),
            (-120.0, 0.0),
            frame,
            false,
        );
        let AnnotKind::Text { rect, size_px, constrained, .. } = &out else { panic!("text") };
        assert!(*constrained, "a dragged box stays a wrap prison");
        assert_eq!(*size_px, s0, "the glyphs must NOT scale — only the box changed");
        assert!(rect.w < r0.w, "the wrap width followed the handle ({} -> {})", r0.w, rect.w);
        // Narrower wrap ⇒ more lines ⇒ a taller box: the height snaps to the wrapped content.
        assert!(rect.h >= r0.h, "re-wrapping into a narrower prison grows the height");
    }

    // ── DRAGON-370: Ctrl overrides a paragraph box's handle into a type scaler ───────────

    /// The override itself. DRAGON-364's constrained/normal split IS Photoshop's
    /// paragraph/point-text distinction; the piece that was missing is Photoshop's modifier —
    /// Ctrl on a PARAGRAPH box's handle scales the type instead of reflowing the box. The box
    /// keeps its kind, so the very next unmodified drag reflows again.
    #[test]
    fn ctrl_makes_a_constrained_box_scale_its_type_instead_of_reflowing() {
        use crate::geometry::Corner;
        let frame = (2000.0, 1600.0);
        let orig = text_kind(true, 32.0, AnnotRect { x: 200.0, y: 200.0, w: 400.0, h: 0.0 }, frame);
        let AnnotKind::Text { rect: r0, size_px: s0, .. } = &orig else { panic!("text") };
        let (r0, s0) = (*r0, *s0);
        let drag = |scale_type: bool| {
            edited_kind(&orig, Grab::Corner(Corner::Se), (0.0, 0.0), (120.0, 120.0), frame, scale_type)
        };
        // WITHOUT the modifier: DRAGON-364's contract, untouched.
        let AnnotKind::Text { size_px: plain, constrained: c0, .. } = drag(false) else {
            panic!("text")
        };
        assert_eq!(plain, s0, "an unmodified paragraph drag must never scale the glyphs");
        assert!(c0, "…and it stays a paragraph box");
        // WITH it: the type scales, and the box is still a paragraph box.
        let out = drag(true);
        let AnnotKind::Text { rect, size_px, constrained, .. } = &out else { panic!("text") };
        assert!(*size_px > s0, "ctrl-drag must scale the type ({s0} -> {size_px})");
        assert!(*constrained, "the modifier changes what the HANDLE does, not what the text IS");
        // The wrap width scaled by the same factor — that is what keeps the line breaks and
        // makes the change a similarity rather than a re-wrap.
        let applied = size_px / s0;
        assert!(
            (rect.w / r0.w - applied).abs() < 0.05,
            "the wrap width ({} -> {}) did not follow the type ({applied:.3}x)",
            r0.w,
            rect.w,
        );
        // A NORMAL box is unaffected by the modifier: it already scales, and Photoshop offers no
        // reverse override (point text has no wrap width to set).
        let normal = text_kind(false, 24.0, AnnotRect { x: 200.0, y: 300.0, w: 0.0, h: 0.0 }, frame);
        let with = edited_kind(&normal, Grab::Corner(Corner::Se), (0.0, 0.0), (60.0, 60.0), frame, true);
        let without =
            edited_kind(&normal, Grab::Corner(Corner::Se), (0.0, 0.0), (60.0, 60.0), frame, false);
        assert_eq!(with, without, "ctrl must mean nothing on a point box");
    }

    /// The half that makes it usable, and the reason it had to wait for DRAGON-368: a Ctrl-drag
    /// scales the wrap width by the SAME factor as the type, so the line breaks hold and the
    /// whole change is a similarity — which is exactly what the raster-reuse proxy accepts. A
    /// re-wrap on every motion event of the editor's most expensive gesture is what this avoids.
    #[test]
    fn a_ctrl_scale_stays_a_similarity_so_the_raster_is_re_used() {
        use crate::geometry::Corner;
        let frame = (4000u32, 3000u32);
        let fwh = (frame.0 as f32, frame.1 as f32);
        let para = AnnotationItem {
            id: AnnotId(1),
            color: [255, 255, 255, 255],
            kind: reflow_text(
                "The quick brown fox jumps over the lazy dog and keeps going for a while",
                96.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x: 300.0, y: 400.0, w: 1200.0, h: 0.0 },
                true,
                2.0,
                fwh,
            ),
        };
        let AnnotKind::Text { text, .. } = &para.kind else { panic!("text") };
        assert!(text.len() > 40, "the caption must be long enough to actually wrap");
        let start = vec![para];
        let (mut items, mut geom) = (start.clone(), text_layer_region(&start, frame).expect("r"));
        for step in 1..=40 {
            let next: Vec<AnnotationItem> = start
                .iter()
                .map(|it| AnnotationItem {
                    kind: edited_kind(
                        &it.kind,
                        Grab::Corner(Corner::Se),
                        (0.0, 0.0),
                        (step as f32 * 8.0, step as f32 * 8.0),
                        fwh,
                        true,
                    ),
                    ..it.clone()
                })
                .collect();
            let xf = text_layer_xform(
                &text_render_sigs(&items, frame),
                &text_render_sigs(&next, frame),
            )
            .unwrap_or_else(|| panic!("ctrl-scale step {step} was not recognised as a similarity"));
            assert!(xf.scale > 1.0, "step {step} did not grow the type");
            let padded = text_padded_bounds(&next).expect("bounds");
            geom = placed_text_region(geom, xf, padded)
                .unwrap_or_else(|| panic!("ctrl-scale step {step} could not re-place the raster"));
            items = next;
        }
        assert!(geom.w > 0.0 && geom.h > 0.0);
    }

    #[test]
    fn resizing_a_normal_box_scales_the_type_aspect_locked_and_stays_normal() {
        let frame = (2000.0, 1600.0);
        let orig = text_kind(false, 24.0, AnnotRect { x: 200.0, y: 300.0, w: 0.0, h: 0.0 }, frame);
        let AnnotKind::Text { rect: r0, size_px: s0, .. } = &orig else { panic!("text") };
        let (r0, s0) = (*r0, *s0);
        // Drag the SE corner outward along the box diagonal: the TYPE grows.
        let out = edited_kind(
            &orig,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (r0.w * 0.5, r0.h * 0.5),
            frame,
            false,
        );
        let AnnotKind::Text { rect, size_px, constrained, text, .. } = &out else { panic!("text") };
        assert!(!*constrained, "a click-created box stays NORMAL — a scale is not a wrap change");
        assert!(*size_px > s0, "the font size grew ({s0} -> {size_px})");
        assert_eq!(text, "hello there world", "a resize never edits the string");
        // Aspect-ratio LOCKED: the box extent comes from one uniformly scaled number, so its
        // proportions survive the drag (a stretch would move this ratio).
        let (a0, a1) = (r0.w / r0.h, rect.w / rect.h);
        assert!((a0 - a1).abs() / a0 < 0.02, "aspect held: {a0} vs {a1}");
        // …and the box tracks the size, not the pointer delta.
        assert!((rect.w / r0.w - size_px / s0).abs() < 0.05, "extent scales with the type size");
        // Dragging the same corner INWARD shrinks it — the factor is continuous through 1.0.
        let smaller = edited_kind(
            &orig,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (-r0.w * 0.25, -r0.h * 0.25),
            frame,
            false,
        );
        let AnnotKind::Text { size_px: s2, .. } = &smaller else { panic!("text") };
        assert!(*s2 < s0, "dragging inward shrinks the type ({s0} -> {s2})");
    }

    #[test]
    fn a_scaled_normal_box_still_auto_grows_when_more_text_is_typed() {
        // The interaction the ticket asks about: scaling must not secretly constrain the box,
        // or the caption would start wrapping at its scaled width instead of growing.
        let frame = (2000.0, 1600.0);
        let orig = text_kind(false, 24.0, AnnotRect { x: 100.0, y: 100.0, w: 0.0, h: 0.0 }, frame);
        let scaled = edited_kind(
            &orig,
            Grab::Corner(crate::geometry::Corner::Se),
            (0.0, 0.0),
            (200.0, 200.0),
            frame,
            false,
        );
        let AnnotKind::Text { rect, size_px, constrained, font, stroke_w, .. } = &scaled else {
            panic!("text")
        };
        assert!(!*constrained);
        // Re-flow the SAME box with a longer string, exactly as a keystroke does.
        let longer = reflow_text(
            "hello there world and then some considerably more words",
            *size_px,
            *font,
            *rect,
            *constrained,
            *stroke_w,
            frame,
        );
        let AnnotKind::Text { rect: r2, .. } = &longer else { panic!("text") };
        assert!(r2.w > rect.w, "an auto box still grows to its widest line ({} -> {})", rect.w, r2.w);
    }

    // ── DRAGON-378: expanding a caption LEFTWARD works exactly like expanding it right ────

    /// THE bug, as the owner met it: "if my text is close to the left edge and i expand it to the
    /// right, it keeps expanding correctly. if my text is close to the right edge and i expand it
    /// to the left, it keeps wrapping and wont expand correctly, even though it has a lot of room
    /// to go on the left still."
    ///
    /// The two gestures are mirror images, so their results must be too. They were not: the wrap
    /// cap was the room from the box's LEFT edge to the picture's RIGHT edge, which a leftward
    /// expansion cannot grow ([`reflow_text`] keeps the ORIGIN, and the scale re-laid the text out
    /// against the PRE-drag one). Measured before the fix, on this exact setup: the leftward drag
    /// scaled the type 32 → 107px but widened the box by 18px and shattered "hello there world"
    /// into `["hello", "there", "worl", "d"]`, while the mirrored rightward drag grew it
    /// 254 → 854px on one line.
    ///
    /// Pinned for BOTH box kinds, because they reach the cap differently — a click-created (auto)
    /// box scales its type and only touches the cap through the `rect_w` floor, a drag-created
    /// (constrained) one reflows inside `rect.w` and is capped only by the picture clamp.
    #[test]
    fn an_auto_caption_expands_leftward_at_the_right_edge_exactly_as_it_expands_right() {
        use crate::geometry::Edge;
        let frame = (1920.0f32, 1080.0f32);
        let lines = |k: &AnnotKind| {
            let AnnotKind::Text { rect, text, size_px, font, constrained, .. } = k else {
                panic!("text")
            };
            let lay = text_kind_layout(text, *size_px, *font, *rect, *constrained, frame.0);
            // The stored geometry IS the derived layout — the fixed-point property the render,
            // the bake and the caret all ride on.
            assert!(
                (lay.box_w - rect.w).abs() < 0.01 && (lay.box_h - rect.h).abs() < 0.01,
                "stored box {rect:?} disagrees with the derived layout ({}, {})",
                lay.box_w,
                lay.box_h,
            );
            lay.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        };
        for constrained in [false, true] {
            // Near the RIGHT edge, dragged 600px LEFT by its west handle…
            let right = text_kind(
                constrained,
                32.0,
                AnnotRect { x: 1650.0, y: 400.0, w: 250.0, h: 0.0 },
                frame,
            );
            let AnnotKind::Text { rect: r0, size_px: s0, .. } = &right else { panic!("text") };
            let (r0, s0) = (*r0, *s0);
            let grown =
                edited_kind(&right, Grab::Edge(Edge::W), (0.0, 0.0), (-600.0, 0.0), frame, false);
            let AnnotKind::Text { rect: r1, size_px: s1, .. } = &grown else { panic!("text") };
            // The room on the left was there, so the box took it: the west edge travelled with
            // the pointer and the east edge (the drag's anchor) stayed put.
            assert!(
                r1.x <= r0.x - 550.0,
                "[constrained={constrained}] the box barely moved left: {r0:?} → {r1:?}",
            );
            assert!(
                ((r1.x + r1.w) - (r0.x + r0.w)).abs() < 1.0,
                "[constrained={constrained}] the anchored east edge slid: {r0:?} → {r1:?}",
            );
            // …and the text followed the KIND's contract on the way. An auto box scales its type,
            // so its line breaks must survive untouched (a similarity, not a re-flow); a
            // constrained box IS a wrap prison, so widening it re-flows — but only ever into
            // FEWER lines, never the narrowing column the bug produced.
            if constrained {
                assert!(
                    lines(&grown).len() <= lines(&right).len(),
                    "[constrained] widening the prison added lines: {:?} → {:?}",
                    lines(&right),
                    lines(&grown),
                );
            } else {
                assert_eq!(lines(&right), lines(&grown), "[auto] the scale re-wrapped the caption");
            }

            // Near the LEFT edge, dragged 600px RIGHT by its east handle: the mirror image.
            let left =
                text_kind(constrained, 32.0, AnnotRect { x: 20.0, y: 400.0, w: 250.0, h: 0.0 }, frame);
            let mirrored =
                edited_kind(&left, Grab::Edge(Edge::E), (0.0, 0.0), (600.0, 0.0), frame, false);
            let AnnotKind::Text { rect: r2, size_px: s2, .. } = &mirrored else { panic!("text") };
            assert_eq!(lines(&grown), lines(&mirrored), "[constrained={constrained}] wrap differs");
            assert!(
                (r1.w - r2.w).abs() < 0.01 && (r1.h - r2.h).abs() < 0.01,
                "[constrained={constrained}] mirrored drags gave different boxes: {r1:?} vs {r2:?}",
            );
            assert!((s1 - s2).abs() < 0.01, "[constrained={constrained}] type sizes differ");
            // The auto box scales its type (DRAGON-364); the constrained one never does. Both
            // widen by 600px, which is the whole point.
            assert_eq!(*s1 > s0, !constrained, "[constrained={constrained}] wrong resize mode");
            assert!(
                (r1.w - (r0.w + 600.0)).abs() < 1.0,
                "[constrained={constrained}] the box did not take the 600px: {r0:?} → {r1:?}",
            );
        }
    }

    /// The other end of the same rule: an auto caption ALREADY as wide as the picture must still
    /// scale up, growing OFF the canvas (DRAGON-368) rather than re-wrapping into more lines.
    /// The `rect_w` floor is what would otherwise stall it — it pins the cap at the PRE-scale
    /// width, and a box that may not grow wider can only answer a bigger type with more lines —
    /// so the scale arm scales that floor by the factor it applied, exactly as DRAGON-370 does
    /// for a constrained box's wrap prison. Same reason, too: it keeps the gesture a similarity,
    /// which is what the raster-reuse proxy accepts.
    #[test]
    fn scaling_a_picture_wide_auto_caption_grows_it_off_canvas_instead_of_re_wrapping() {
        use crate::geometry::Edge;
        let frame = (1200u32, 700u32);
        let fwh = (frame.0 as f32, frame.1 as f32);
        // A caption sized so its single line already spans nearly the whole picture.
        let item = caption(frame, 20.0, 200.0, 55.0);
        let AnnotKind::Text { rect: r0, .. } = &item.kind else { panic!("text") };
        assert!(r0.w > fwh.0 * 0.8, "the caption must start near the picture's width: {r0:?}");
        let before = text_kind_layout_of(&item.kind, fwh.0);
        let scaled = AnnotationItem {
            kind: edited_kind(&item.kind, Grab::Edge(Edge::E), (0.0, 0.0), (600.0, 0.0), fwh, false),
            ..item.clone()
        };
        let AnnotKind::Text { rect: r1, size_px, .. } = &scaled.kind else { panic!("text") };
        let after = text_kind_layout_of(&scaled.kind, fwh.0);
        assert!(*size_px > 55.0, "the type must have grown");
        assert_eq!(before.lines, after.lines, "the scale re-wrapped a picture-wide caption");
        assert!(r1.w > fwh.0, "the caption must have grown PAST the picture: {r1:?}");
        // …and that is exactly what keeps it on the raster-reuse fast path.
        assert!(
            text_layer_xform(
                &text_render_sigs(std::slice::from_ref(&item), frame),
                &text_render_sigs(std::slice::from_ref(&scaled), frame),
            )
            .is_some(),
            "scaling a picture-wide caption fell off the raster-reuse fast path",
        );
    }

    /// The derived layout of a text kind, at the shared seam — test sugar.
    fn text_kind_layout_of(kind: &AnnotKind, fw: f32) -> super::super::text_annot::TextLayout {
        let AnnotKind::Text { rect, text, size_px, font, constrained, .. } = kind else {
            panic!("text")
        };
        text_kind_layout(text, *size_px, *font, *rect, *constrained, fw)
    }

    // ── DRAGON-379: expanding a caption past the media must not flicker ──────────────────

    /// THE bug: "now when i expand a text area beyond the bounds of the media, it quickly
    /// alternates between word wrapping and just allowing text to go beyond the frame."
    ///
    /// Cause, measured: past the picture width the auto cap is the box's OWN width scaled by the
    /// drag factor ([`text_auto_cap`]'s floor, fed by the scale arm), and the box's own width IS
    /// the widest laid-out line — so the wrap width and the line being measured against it are
    /// ONE number computed two ways (`Σ em_adv × s·k` vs `(Σ em_adv × s)·k`). They agree to a
    /// rounding step, and a rounding step flips the decision: over this very sweep the caption
    /// alternated between one line and two **23 times in 121 steps**. The slack in the wrap's fit
    /// test ([`super::super::text_annot::WRAP_FIT_SLACK_REL`]) is what settles it.
    ///
    /// This sweep — not the fixed-point test below — is the regression net for that bug: the
    /// derivation was already a fixed point at every INDIVIDUAL step, so iterating one step to
    /// convergence saw nothing wrong. What flickered was the DRAG PARAMETER moving through the
    /// tie, which only a sweep can see.
    #[test]
    fn expanding_a_caption_past_the_media_never_alternates_between_wrapped_and_unwrapped() {
        use crate::geometry::Edge;
        let frame = (1200u32, 700u32);
        let fwh = (frame.0 as f32, frame.1 as f32);
        let text = "The quick brown fox jumps over the lazy dog";
        // Every gesture that can push a caption past the picture's width: an auto box grown from
        // either side (DRAGON-378), and a paragraph box Ctrl-scaled (DRAGON-370) — the prison is
        // scaled by the same factor there, so it rides the same comparison.
        for (tag, grab, sign, x, constrained, ctrl) in [
            ("auto, east handle", Grab::Edge(Edge::E), 1.0f32, 20.0f32, false, false),
            ("auto, west handle", Grab::Edge(Edge::W), -1.0, 1000.0, false, false),
            ("paragraph, ctrl-scaled", Grab::Edge(Edge::E), 1.0, 20.0, true, true),
        ] {
            let seed = reflow_text(
                text,
                55.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x, y: 100.0, w: 1150.0, h: 0.0 },
                constrained,
                2.0,
                fwh,
            );
            let mut prev: Option<(usize, f32)> = None;
            let mut widest = 0.0f32;
            for step in 0..=120 {
                let dx = sign * step as f32 * 10.0;
                let k = edited_kind(&seed, grab, (0.0, 0.0), (dx, 0.0), fwh, ctrl);
                let AnnotKind::Text { rect, text, size_px, font, constrained, .. } = &k else {
                    panic!("text")
                };
                let lay = text_kind_layout(text, *size_px, *font, *rect, *constrained, fwh.0);
                widest = widest.max(rect.w);
                if let Some((lines, w)) = prev {
                    // The whole gesture is ONE similarity, so the wrap must never change at all —
                    // and certainly never change BACK, which is what the eye reads as flicker.
                    assert_eq!(
                        lines,
                        lay.lines.len(),
                        "{tag}: the wrap changed at step {step} ({lines} → {} lines)",
                        lay.lines.len(),
                    );
                    // …and the box grows monotonically with the drag, never snapping narrower.
                    assert!(
                        rect.w >= w - 0.01,
                        "{tag}: the box snapped narrower at step {step} ({w} → {})",
                        rect.w,
                    );
                }
                prev = Some((lay.lines.len(), rect.w));
            }
            assert!(
                widest > fwh.0,
                "{tag}: the sweep never took the caption past the picture width ({widest})",
            );
        }
    }

    /// The other half of the guarantee, and the one the ticket's shape demands: the auto cap
    /// FLOORS at the box's own width, and for a click-created box that width is an OUTPUT of the
    /// layout — so past the picture width the cap is the previous layout fed back in as an input.
    /// That loop must reach a fixed point rather than cycle, or a caption would re-flow on every
    /// render for as long as it is on screen.
    ///
    /// It settles on the FIRST re-derivation, in every regime: a plain caption, one whose
    /// trailing spaces make its box wider than any of its lines' text (`box_w` can exceed the cap
    /// that produced it — the one way the loop's input can grow), a caption full of runs of
    /// spaces, and unbreakable words longer than the picture.
    #[test]
    fn the_auto_wrap_cap_settles_to_a_fixed_point_past_the_picture_width() {
        use crate::geometry::Edge;
        let frame = (1200u32, 700u32);
        let fwh = (frame.0 as f32, frame.1 as f32);
        for (tag, text) in [
            ("plain", "The quick brown fox jumps over the lazy dog"),
            ("trailing spaces", "The quick brown fox jumps over the lazy dog    "),
            ("interior runs", "The quick brown fox    jumps    over the lazy dog   "),
            ("unbreakable words", "Supercalifragilisticexpialidocious antidisestablishmentarianism"),
        ] {
            let seed = reflow_text(
                text,
                55.0,
                super::super::text_annot::TextFont::Clean,
                AnnotRect { x: 20.0, y: 100.0, w: 0.0, h: 0.0 },
                false,
                2.0,
                fwh,
            );
            // Scaled well past the picture width, which is the only regime where the floor —
            // and therefore the feedback — binds at all.
            let mut kind =
                edited_kind(&seed, Grab::Edge(Edge::E), (0.0, 0.0), (1400.0, 0.0), fwh, false);
            let state = |k: &AnnotKind| {
                let AnnotKind::Text { rect, text, size_px, font, constrained, .. } = k else {
                    panic!("text")
                };
                let lay = text_kind_layout(text, *size_px, *font, *rect, *constrained, fwh.0);
                (lay.lines, rect.w, rect.h)
            };
            let settled = state(&kind);
            assert!(settled.1 > fwh.0, "{tag}: the caption is not past the picture width");
            // Re-derive repeatedly, exactly as a render / a keystroke / a re-open does. Ten
            // rounds: a two-state cycle would show up on the very first one, a longer one here.
            for round in 1..=10 {
                let AnnotKind::Text { rect, text, size_px, font, constrained, stroke_w } = &kind
                else {
                    panic!("text")
                };
                kind = reflow_text(text, *size_px, *font, *rect, *constrained, *stroke_w, fwh);
                assert_eq!(
                    state(&kind),
                    settled,
                    "{tag}: the layout moved on re-derivation {round} — the cap is cycling",
                );
            }
        }
    }

    #[test]
    fn a_move_changes_neither_text_box_mode_nor_its_size() {
        let frame = (1000.0, 800.0);
        for constrained in [false, true] {
            let orig =
                text_kind(constrained, 32.0, AnnotRect { x: 100.0, y: 100.0, w: 250.0, h: 0.0 }, frame);
            let AnnotKind::Text { rect: r0, size_px: s0, .. } = &orig else { panic!("text") };
            let (r0, s0) = (*r0, *s0);
            let out = edited_kind(&orig, Grab::Move, (0.0, 0.0), (40.0, 30.0), frame, false);
            let AnnotKind::Text { rect, size_px, constrained: c, .. } = &out else { panic!("text") };
            assert_eq!(*c, constrained, "a move never flips the wrap mode");
            assert_eq!(*size_px, s0, "a move never scales the type");
            assert!((rect.x - (r0.x + 40.0)).abs() < 0.01 && (rect.y - (r0.y + 30.0)).abs() < 0.01);
            assert!((rect.w - r0.w).abs() < 0.01 && (rect.h - r0.h).abs() < 0.01, "extent held");
        }
    }

    #[test]
    fn the_text_scale_factor_is_directional_continuous_and_never_inverts() {
        use crate::geometry::{Corner, Edge};
        let (w, h) = (200.0f32, 100.0f32);
        // No drag ⇒ no scale, for every grab.
        for g in [
            Grab::Corner(Corner::Nw),
            Grab::Corner(Corner::Se),
            Grab::Edge(Edge::N),
            Grab::Edge(Edge::E),
        ] {
            assert!((text_scale_factor(w, h, g, 0.0, 0.0) - 1.0).abs() < 1e-6, "{g:?} idle");
        }
        // A Move (and the arrow grabs) never scale at all.
        for g in [Grab::Move, Grab::ArrowA, Grab::ArrowB] {
            assert_eq!(text_scale_factor(w, h, g, 99.0, 99.0), 1.0);
        }
        // Each corner grows when dragged OUTWARD and shrinks when dragged inward.
        for (g, out) in [
            (Grab::Corner(Corner::Se), (1.0f32, 1.0f32)),
            (Grab::Corner(Corner::Nw), (-1.0, -1.0)),
            (Grab::Corner(Corner::Ne), (1.0, -1.0)),
            (Grab::Corner(Corner::Sw), (-1.0, 1.0)),
        ] {
            let grow = text_scale_factor(w, h, g, out.0 * 20.0, out.1 * 20.0);
            let shrink = text_scale_factor(w, h, g, out.0 * -20.0, out.1 * -20.0);
            assert!(grow > 1.0, "{g:?} outward grows (got {grow})");
            assert!(shrink < 1.0, "{g:?} inward shrinks (got {shrink})");
        }
        // A corner responds to a PURELY horizontal drag as well as a purely vertical one —
        // the diagonal projection is exactly what avoids a per-axis dead zone.
        assert!(text_scale_factor(w, h, Grab::Corner(Corner::Se), 30.0, 0.0) > 1.0);
        assert!(text_scale_factor(w, h, Grab::Corner(Corner::Se), 0.0, 30.0) > 1.0);
        // Edges act on their OWN axis only.
        assert!(text_scale_factor(w, h, Grab::Edge(Edge::E), 40.0, 0.0) > 1.0);
        assert_eq!(text_scale_factor(w, h, Grab::Edge(Edge::E), 0.0, 40.0), 1.0);
        assert!(text_scale_factor(w, h, Grab::Edge(Edge::S), 0.0, 40.0) > 1.0);
        assert!(text_scale_factor(w, h, Grab::Edge(Edge::N), 0.0, 40.0) < 1.0);
        // A huge inward drag can never flip the type negative (or to zero).
        let f = text_scale_factor(w, h, Grab::Corner(Corner::Se), -100000.0, -100000.0);
        assert!(f > 0.0 && f.is_finite(), "factor stays positive, got {f}");
        // A degenerate extent falls back instead of dividing by ~0.
        assert!(text_scale_factor(0.0, 0.0, Grab::Corner(Corner::Se), 5.0, 5.0).is_finite());
    }

    #[test]
    fn a_scaled_normal_box_is_anchored_at_the_handle_you_are_not_holding() {
        use crate::geometry::{Corner, Edge};
        let frame = (1000.0, 1000.0);
        let orig = AnnotRect { x: 100.0, y: 200.0, w: 200.0, h: 100.0 };
        let (l, t, r, b) = (100.0, 200.0, 300.0, 300.0);
        // Growing from each corner pins the OPPOSITE one.
        let se = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Corner(Corner::Se), frame);
        assert_eq!((se.x, se.y), (l, t), "an SE drag pins NW");
        let nw = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Corner(Corner::Nw), frame);
        assert!((nw.x + nw.w - r).abs() < 1e-4 && (nw.y + nw.h - b).abs() < 1e-4, "NW pins SE");
        let ne = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Corner(Corner::Ne), frame);
        assert!((ne.x - l).abs() < 1e-4 && (ne.y + ne.h - b).abs() < 1e-4, "NE pins SW");
        let sw = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Corner(Corner::Sw), frame);
        assert!((sw.x + sw.w - r).abs() < 1e-4 && (sw.y - t).abs() < 1e-4, "SW pins NE");
        // An edge pins the opposite edge and leaves the OTHER axis' origin alone (no drift).
        let n = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Edge(Edge::N), frame);
        assert!((n.x - l).abs() < 1e-4, "an N drag must not slide the box sideways");
        assert!((n.y + n.h - b).abs() < 1e-4, "…and pins the south edge");
        let e = anchor_scaled_text_rect(orig, 300.0, 150.0, Grab::Edge(Edge::E), frame);
        assert_eq!((e.x, e.y), (l, t), "an E drag pins the west edge and the top");
        // The result is always clamped inside the picture.
        let huge = anchor_scaled_text_rect(orig, 4000.0, 4000.0, Grab::Corner(Corner::Se), frame);
        assert!(huge.x >= 0.0 && huge.y >= 0.0, "a scaled box never leaves the frame");
    }

    /// DRAGON-368 REPLACES DRAGON-367's `a_scale_steps_past_both_ends_…`, which pinned the size
    /// LADDER. The owner's rules, in order: DRAGON-367 — "we shouldn't limit how much we can
    /// scale, it should exceed beyond the min/max of the options in dropdown"; DRAGON-368 — "we
    /// need to remove snapping from the sizer". So a drag reaches past both ends of the listed
    /// presets, lands on ANY size in between, and only the guard bounds hold.
    #[test]
    fn a_scale_slides_continuously_past_both_ends_and_stops_only_at_the_guard_bounds() {
        use super::super::text_annot::{TEXT_SCALE_MAX_PX, TEXT_SCALE_MIN_PX, TEXT_SIZES};
        use crate::geometry::Corner;
        let frame = (4000.0, 4000.0);
        let scaled = |px: f32, drag: f32| {
            let orig = text_kind(false, px, AnnotRect { x: 10.0, y: 10.0, w: 0.0, h: 0.0 }, frame);
            let out = edited_kind(&orig, Grab::Corner(Corner::Se), (0.0, 0.0), (drag, drag), frame, false);
            let AnnotKind::Text { size_px, .. } = out else { panic!("text") };
            size_px
        };
        // Past BOTH ends of the listed range — the behaviour DRAGON-364 forbade.
        let (lo, hi) = (TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]);
        assert!(scaled(hi, 100_000.0) > hi, "a drag must reach above the largest preset");
        assert!(scaled(lo, -100_000.0) < lo, "a drag must reach below the smallest preset");
        // …but never past the guard bounds, however violent the drag.
        for (px, drag) in [(12.0f32, -1.0e9f32), (128.0, 1.0e9), (32.0, 0.0)] {
            let got = scaled(px, drag);
            assert!(
                (TEXT_SCALE_MIN_PX..=TEXT_SCALE_MAX_PX).contains(&got),
                "a scaled size escaped the guard bounds: {got}",
            );
        }
        // NO SNAPPING: the size moves with EVERY step of the drag, and lands off-preset. A
        // ladder would have produced long runs of one value and only ever listed sizes.
        let mut seen = Vec::new();
        for step in 0..40 {
            let got = scaled(32.0, step as f32 * 2.0);
            if let Some(prev) = seen.last() {
                assert!(got > *prev, "the size stalled between {prev} and {got} — that is a snap");
            }
            seen.push(got);
        }
        assert!(
            seen.iter().any(|s| super::super::text_annot::text_size_preset_index(*s).is_none()),
            "a continuous drag must be able to sit between the dropdown's presets",
        );
    }

    /// The DRAGON-364 × DRAGON-368 interaction that replaces DRAGON-367's
    /// `a_resize_within_one_ladder_rung_…`. Snapping used to make most resize events a no-op,
    /// which was an accidental performance win; with it gone, EVERY event changes the drawing's
    /// size and the raster must follow. This pins that the live-transform proxy is what carries
    /// it — the whole reason removing the snap did not reintroduce the stall.
    #[test]
    fn every_continuous_resize_event_moves_the_size_and_still_re_uses_the_raster() {
        use crate::geometry::Corner;
        let frame = (4000u32, 4000u32);
        let fwh = (frame.0 as f32, frame.1 as f32);
        let start = vec![caption(frame, 200.0, 200.0, 96.0)];
        let at = |drag: f32| -> Vec<AnnotationItem> {
            start
                .iter()
                .map(|it| AnnotationItem {
                    kind: edited_kind(
                        &it.kind,
                        Grab::Corner(Corner::Se),
                        (0.0, 0.0),
                        (drag, drag),
                        fwh,
                        false,
                    ),
                    ..it.clone()
                })
                .collect()
        };
        // Two adjacent motion events genuinely differ now — no rung to hide behind.
        let (a, b) = (at(1.0), at(2.0));
        let AnnotKind::Text { size_px: sa, .. } = &a[0].kind else { panic!("text") };
        let AnnotKind::Text { size_px: sb, .. } = &b[0].kind else { panic!("text") };
        assert_ne!(sa, sb, "with the ladder gone, every event must move the size");
        // …and the proxy takes every one of them: replay a whole resize and require a placement
        // at each step. Before DRAGON-368 this was a re-render per event.
        let (mut items, mut geom) = (start.clone(), text_layer_region(&start, frame).expect("r"));
        for step in 1..=60 {
            let next = at(step as f32 * 4.0);
            let xf = text_layer_xform(
                &text_render_sigs(&items, frame),
                &text_render_sigs(&next, frame),
            )
            .unwrap_or_else(|| panic!("resize step {step} was not recognised as a similarity"));
            let padded = text_padded_bounds(&next).expect("bounds");
            geom = placed_text_region(geom, xf, padded)
                .unwrap_or_else(|| panic!("resize step {step} could not re-place the raster"));
            items = next;
        }
        assert!(geom.w > 0.0 && geom.h > 0.0);
    }

    #[test]
    fn only_an_explicit_dropdown_pick_writes_the_remembered_text_default() {
        // The DRAGON-364 rule, in the one place it lives. Selecting an element, or dragging a
        // handle that scales it, are REPORTS about that element — the chips follow, the
        // persisted default for future captures does not move. Only picking in the menu does.
        assert!(
            TextStyleSource::DropdownPick.writes_default(),
            "picking a size/font IS the user stating a preference",
        );
        assert!(
            !TextStyleSource::SelectionSync.writes_default(),
            "clicking an existing 96px caption must not re-set the default for every capture",
        );
        assert!(
            !TextStyleSource::HandleScale.writes_default(),
            "dragging a handle scales the element; it is not picking a size",
        );
    }

    #[test]
    fn the_dropdowns_report_the_last_selected_text_element() {
        use super::super::text_annot::TextFont;
        let frame = (1000.0, 1000.0);
        let mk = |id: u64, size: f32, font: TextFont| AnnotationItem {
            id: AnnotId(id),
            color: [255, 255, 255, 255],
            kind: reflow_text(
                "caption",
                size,
                font,
                AnnotRect { x: 10.0, y: 10.0, w: 0.0, h: 0.0 },
                false,
                2.0,
                frame,
            ),
        };
        let shape = AnnotationItem {
            id: AnnotId(9),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box {
                rect: AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
                stroke_w: 4.0,
                fill: None,
            },
        };
        let items = vec![mk(1, 16.0, TextFont::Clean), mk(2, 96.0, TextFont::Hand), shape];
        // The PRIMARY (= last-selected) item is what the caller passes, so "match the last one
        // selected" falls straight out.
        assert_eq!(
            text_style_for_display(&items, Some(AnnotId(2))),
            Some((96.0, TextFont::Hand)),
        );
        assert_eq!(
            text_style_for_display(&items, Some(AnnotId(1))),
            Some((16.0, TextFont::Clean)),
        );
        // Nothing selected, a NON-text primary, or a stale id: the chips keep showing what a
        // new box would take rather than inventing a value.
        assert_eq!(text_style_for_display(&items, None), None);
        assert_eq!(text_style_for_display(&items, Some(AnnotId(9))), None, "a box has no font");
        assert_eq!(text_style_for_display(&items, Some(AnnotId(404))), None, "a deleted id");
    }

    // ── DRAGON-389: annotatable bounds (source ∪ crop) + the over-crop bake ───────────────────

    fn box_kind(x: f32, y: f32, w: f32, h: f32) -> AnnotKind {
        AnnotKind::Box { rect: AnnotRect { x, y, w, h }, stroke_w: 4.0, fill: None }
    }

    #[test]
    fn dragon389_union_rect_covers_both_and_handles_negative_origin() {
        // A crop that extends LEFT/UP past the source: the union has a NEGATIVE origin and spans
        // both rects.
        let src = AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let crop = AnnotRect { x: -50.0, y: -30.0, w: 100.0, h: 100.0 };
        let u = union_rect(src, crop);
        assert_eq!((u.x, u.y), (-50.0, -30.0));
        assert_eq!((u.x + u.w, u.y + u.h), (100.0, 100.0));
        // A crop wholly INSIDE the source leaves the union as the whole source — out-of-crop
        // annotations stay annotatable (DRAGON-385).
        let inside = AnnotRect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert_eq!(union_rect(src, inside), src);
    }

    #[test]
    fn dragon389_annot_bounds_is_source_union_committed_crop() {
        use super::super::crop::CropRect;
        let mut e = super::super::edit::EditState { frame: (200, 100), ..Default::default() };
        // No crop → the source frame.
        assert_eq!(e.annot_bounds(), AnnotRect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 });
        // An over-crop extending past every side → the union.
        e.crop = Some(CropRect { x: -20.0, y: -10.0, w: 260.0, h: 140.0 });
        let b = e.annot_bounds();
        assert_eq!((b.x, b.y), (-20.0, -10.0));
        assert_eq!((b.x + b.w, b.y + b.h), (240.0, 130.0));
    }

    #[test]
    fn dragon389_move_reaches_the_over_crop_extension() {
        // Source 100×100, crop extends 50px LEFT and UP → bounds (-50,-50,150,150).
        let bounds = AnnotRect { x: -50.0, y: -50.0, w: 150.0, h: 150.0 };
        let start = box_kind(0.0, 0.0, 20.0, 20.0);
        // Drag it up-left by 40 — onto the extension.
        let moved = edited_kind_in_bounds(&start, Grab::Move, (0.0, 0.0), (-40.0, -40.0), bounds, false);
        let AnnotKind::Box { rect, .. } = moved else { panic!("box stays a box") };
        assert!(rect.x < 0.0 && rect.y < 0.0, "moved onto the extension, not pinned at the source edge: {rect:?}");
        assert!((rect.x + 40.0).abs() < 0.01 && (rect.y + 40.0).abs() < 0.01, "moved by the full delta: {rect:?}");
        // The SAME drag against the bare source frame pins at the inset edge — the bug this fixes.
        let pinned = edited_kind(&start, Grab::Move, (0.0, 0.0), (-40.0, -40.0), (100.0, 100.0), false);
        let AnnotKind::Box { rect: pr, .. } = pinned else { panic!() };
        assert!(pr.x >= 0.0, "source-frame clamp pins at the edge: {pr:?}");
    }

    #[test]
    fn dragon389_move_reaches_extension_on_each_side() {
        // Source 100×100, crop extends 30px on EVERY side → bounds (-30,-30,160,160).
        let bounds = AnnotRect { x: -30.0, y: -30.0, w: 160.0, h: 160.0 };
        let m = kind_draw_margin(&box_kind(0.0, 0.0, 10.0, 10.0)); // half-stroke overhang
        let far = 100_000.0;
        for (dx, dy) in [(-far, 0.0), (far, 0.0), (0.0, -far), (0.0, far)] {
            let moved = edited_kind_in_bounds(&box_kind(40.0, 40.0, 10.0, 10.0), Grab::Move, (0.0, 0.0), (dx, dy), bounds, false);
            let AnnotKind::Box { rect, .. } = moved else { panic!() };
            assert!(rect.x >= bounds.x + m - 0.01 && rect.x + rect.w <= bounds.x + bounds.w - m + 0.01, "x within extended bounds: {rect:?}");
            assert!(rect.y >= bounds.y + m - 0.01 && rect.y + rect.h <= bounds.y + bounds.h - m + 0.01, "y within extended bounds: {rect:?}");
        }
        // Pushing left reaches PAST the source's left edge (onto the extension).
        let left = edited_kind_in_bounds(&box_kind(40.0, 40.0, 10.0, 10.0), Grab::Move, (0.0, 0.0), (-far, 0.0), bounds, false);
        let AnnotKind::Box { rect, .. } = left else { panic!() };
        assert!(rect.x < 0.0, "reached the left extension: {rect:?}");
    }

    #[test]
    fn dragon389_wrappers_are_identity_at_zero_origin() {
        // A bounds at origin (0,0) is byte-identical to the bare-frame kernels (uncropped parity).
        let b = AnnotRect { x: 0.0, y: 0.0, w: 300.0, h: 200.0 };
        let start = box_kind(10.0, 10.0, 40.0, 30.0);
        assert_eq!(
            edited_kind_in_bounds(&start, Grab::Corner(Corner::Se), (50.0, 40.0), (90.0, 70.0), b, false),
            edited_kind(&start, Grab::Corner(Corner::Se), (50.0, 40.0), (90.0, 70.0), (300.0, 200.0), false),
        );
        assert_eq!(
            badge_placement_in_bounds((150.0, 100.0), DEFAULT_BADGE_SIZE, b, 2.0),
            badge_placement_rect((150.0, 100.0), DEFAULT_BADGE_SIZE, (300, 200), 2.0),
        );
        assert_eq!(
            spawn_placement_in_bounds(Tool::Rect, b, 2.0, DEFAULT_BADGE_SIZE),
            spawn_placement_rect(Tool::Rect, (300, 200), 2.0, DEFAULT_BADGE_SIZE),
        );
    }

    #[test]
    fn dragon389_badge_stays_square_at_the_extended_corner() {
        // Bounds extend up-left; place a badge into the extended corner.
        let bounds = AnnotRect { x: -80.0, y: -80.0, w: 260.0, h: 260.0 };
        let r = badge_placement_in_bounds((-70.0, -70.0), DEFAULT_BADGE_SIZE, bounds, 2.0);
        assert!((r.w - r.h).abs() < 0.01, "badge is 1:1 even at the extended corner: {r:?}");
        assert!(r.x < 0.0 && r.y < 0.0, "placed over the extension: {r:?}");
        assert!(r.x >= bounds.x + 2.0 - 0.01 && r.y >= bounds.y + 2.0 - 0.01, "clamped inside the inset bounds: {r:?}");
    }

    #[test]
    fn dragon389_text_keeps_five_px_against_the_extended_edge() {
        use super::super::text_annot::TextFont;
        // Bounds extend 200px left of the source; move a caption far past the left edge.
        let bounds = AnnotRect { x: -200.0, y: 0.0, w: 300.0, h: 200.0 };
        let seed = reflow_text_in_bounds("Caption", 24.0, TextFont::Clean, AnnotRect { x: 0.0, y: 20.0, w: 0.0, h: 0.0 }, false, 4.0, bounds);
        let AnnotKind::Text { rect: r0, .. } = &seed else { panic!("text kind") };
        let keep = TEXT_MIN_ON_CANVAS_PX.min(r0.w);
        // A pure MOVE far to the left: TEXT_MIN_ON_CANVAS_PX of the box must remain inside bounds.
        let moved = edited_kind_in_bounds(&seed, Grab::Move, (0.0, 0.0), (-100_000.0, 0.0), bounds, false);
        let AnnotKind::Text { rect, .. } = moved else { panic!() };
        assert!(rect.x + rect.w >= bounds.x + keep - 0.01, "at least 5px stays against the extended left edge: {rect:?} keep={keep}");
        assert!(rect.x < 0.0, "the box reached past the source's left edge onto the extension: {rect:?}");
    }

    #[test]
    fn dragon389_apply_annotations_at_identity_and_shift() {
        let items = vec![boxed(1, 10.0, 10.0, 20.0, 20.0)];
        // offset (0,0) == apply_annotations exactly.
        let mut a = RgbaImage::from_pixel(60, 60, ::image::Rgba([0, 0, 0, 255]));
        let mut b = a.clone();
        apply_annotations(&mut a, &items, DEFAULT_ANNOT_CURVE_RADIUS);
        apply_annotations_at(&mut b, &items, DEFAULT_ANNOT_CURVE_RADIUS, (0.0, 0.0));
        assert_eq!(a.as_raw(), b.as_raw(), "offset (0,0) is identical to apply_annotations");
        // A +10,+10 offset draws the overlay shifted by (-10,-10): the box at source (10,10) lands
        // at (0,0) on a canvas whose origin is (10,10).
        let mut c = RgbaImage::from_pixel(60, 60, ::image::Rgba([0, 0, 0, 255]));
        apply_annotations_at(&mut c, &items, DEFAULT_ANNOT_CURVE_RADIUS, (10.0, 10.0));
        let mut d = RgbaImage::from_pixel(60, 60, ::image::Rgba([0, 0, 0, 255]));
        apply_annotations(&mut d, &[boxed(1, 0.0, 0.0, 20.0, 20.0)], DEFAULT_ANNOT_CURVE_RADIUS);
        assert_eq!(c.as_raw(), d.as_raw(), "offset shifts the overlay by -offset");
    }
}
