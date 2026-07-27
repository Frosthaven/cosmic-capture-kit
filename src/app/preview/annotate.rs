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
//! 3. **Toolbar**: add a `tb.bordered_button` in `App::annotation_tools` (chrome.rs) and
//!    a [`Tool`] variant.
//! 4. **Hotkey**: add an `Action` (shortcuts.rs, contiguous in the "Annotation Tools"
//!    group) mapped to `PreviewMsg::SelectTool` in keyboard.rs.
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

/// The SHARED default stroke width (SOURCE px), seeded onto every new box AND arrow — the
/// single source of truth a future width control drives (see [`super::edit::EditState::stroke`]).
pub const DEFAULT_ANNOT_STROKE: f32 = 4.0;

/// The three selectable stroke-width presets (SOURCE px) the toggle group offers, thin →
/// thick. `DEFAULT_ANNOT_STROKE` (4px) is the middle option (default-selected).
pub const STROKE_WIDTHS: [f32; 3] = [2.0, 4.0, 6.0];

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

/// The next stroke-width preset after `current`, wrapping (2 → 5 → 8 → 2 …) — the `L` hotkey's
/// cycle. Keyed off the nearest preset to `current`. Pure — unit-tested.
pub fn cycle_stroke_width(current: f32) -> f32 {
    let i = stroke_width_nearest_index(current);
    STROKE_WIDTHS[(i + 1) % STROKE_WIDTHS.len()]
}

/// The SHARED default corner curve as an ABSOLUTE radius (SOURCE px), read by BOTH the box
/// (a CONSTANT corner radius regardless of box size, reduced only when the box is too small
/// to fit it) and the arrow (round caps/joins when > 0) — the single source of truth a
/// future curve control drives (see [`super::edit::EditState::curve_radius`]).
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
        )
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

/// The rect a DOUBLE-CLICKED tool spawns its item in (DRAGON-339): [`SPAWN_W`]×[`SPAWN_H`] or
/// [`SPAWN_MAX_FRAC`] of the image per axis — whichever FITS — CENTERED in the frame, and
/// further shrunk so the item's DRAWN extent (geometry grown by `margin`, i.e. half the stroke —
/// the `kind_draw_margin` overhang) still lands inside the picture. Each axis is independent, so a
/// wide-but-short image gets a wide-but-short item. Degenerate frames yield a zero rect (the
/// caller discards it, exactly like a degenerate drag). Pure — unit-tested.
pub fn default_placement_rect(frame: (u32, u32), margin: f32) -> AnnotRect {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let m = margin.max(0.0);
    let axis = |full: f32, want: f32| -> f32 {
        // The inset room left once the drawn margin is reserved on BOTH sides.
        let room = (full - 2.0 * m).max(0.0);
        want.min(full * SPAWN_MAX_FRAC).min(room).max(0.0)
    };
    let w = axis(fw, SPAWN_W);
    let h = axis(fh, SPAWN_H);
    AnnotRect { x: (fw - w) * 0.5, y: (fh - h) * 0.5, w, h }
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
        // A freehand stroke has no meaningful default geometry; the eraser creates no item at
        // all; and the POINTER (DRAGON-341) is pure selection — it must never place anything.
        // Double-clicking any of their tray buttons just picks the tool.
        Tool::Pen | Tool::Eraser | Tool::Pointer => return None,
    })
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
/// has none), or `None` when either side isn't a rect kind/tool (e.g. Arrow) or the kind is
/// unchanged.
pub(super) fn converted_rect_kind(
    cur: &AnnotKind,
    tool: Tool,
    default_stroke: f32,
) -> Option<AnnotKind> {
    // Rect-kind ids: 0 Box (outline), 1 Highlight, 2 Box Highlight, 3 Pixelate, 4 Blur, 5 Spotlight.
    let from = match cur {
        AnnotKind::Box { .. } => 0u8,
        AnnotKind::Highlight { .. } => 1,
        AnnotKind::BoxHighlight { .. } => 2,
        AnnotKind::Pixelate { .. } => 3,
        AnnotKind::Blur { .. } => 4,
        AnnotKind::Spotlight { .. } => 5,
        _ => return None,
    };
    let to = match tool {
        Tool::Rect => 0u8,
        Tool::Highlight => 1,
        Tool::BoxHighlight => 2,
        Tool::Pixelate => 3,
        Tool::Blur => 4,
        Tool::Spotlight => 5,
        _ => return None,
    };
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
            };
            Item { id: it.id.0, kind, stroke_w, color: stroke_color, fill, fx, curve_radius }
        })
        .collect()
}

// ── app-side gesture + scene handlers ────────────────────────────────────────────────

impl App {
    /// Push a CUSTOM color onto the last-5 recents queue via [`rotate_recent_color`].
    pub(super) fn push_recent_color(&mut self, c: AnnotColor) {
        rotate_recent_color(&mut self.annot_recent_colors, c);
    }

    /// Begin drawing a new shape of `tool` at image point `(x, y)`.
    pub(super) fn annot_draw_begin(&mut self, tool: Tool, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let color = p.edit.annot_color.unwrap_or_else(default_annot_color);
        let stroke_w = p.edit.stroke();
        // Clamp the start point inside the image (can't draw beyond the picture).
        let (fw, fh) = (p.edit.frame.0 as f32, p.edit.frame.1 as f32);
        let (x, y) = (x.clamp(0.0, fw), y.clamp(0.0, fh));
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
            // Neither non-creating tool ever reaches here: the eraser is handled above, and the
            // POINTER (DRAGON-341) never emits a `DrawBegin` at all (its empty-canvas drag is a
            // rubber band, not a draw). Defensive.
            Tool::Eraser | Tool::Pointer => return Task::none(),
        };
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
    pub(super) fn select_annot_tool(&mut self, tool: Tool) {
        // If a box-family annotation (Box Outline / Highlight / Box Highlight) is selected
        // and the user picks a DIFFERENT one of those three tools, CONVERT the selected
        // item in place (real-time, one undo entry) rather than only arming the tool for
        // the next draw. No-op for every other selection/tool combination.
        self.convert_selected_annotation_kind(tool);
        // Only ever SETS a tool — clicking/hotkeying the active tool is a no-op (no
        // re-click-to-neutral). Persist so the next preview opens with it.
        if let Some(p) = &mut self.preview {
            p.edit.tool = Some(tool);
            // Pen groups are selectable ONLY under the pointer, so arming anything else lets
            // them go — the visible selection and the real one never disagree.
            if !tool.is_pointer() {
                p.edit.drop_pen_selection();
            }
        }
        self.annot_tool = Some(tool);
        self.save_state();
    }

    /// Spawn a PRE-PLACED item of `tool` in the middle of the picture (DRAGON-339) — what a
    /// DOUBLE-CLICK on the tool's action-tray button does, so an item can be added without
    /// dragging one out. Geometry comes from [`default_placement_rect`] (200×100 or 80% of the
    /// image per axis, whichever fits, inset for the stroke); appearance from the SAME current
    /// color/stroke a dragged shape would get. The new item lands on TOP of the z-stack and
    /// becomes the selection, as ONE undo entry in the shared history (so it is undoable and
    /// counts toward `EditState::dirty()`'s bake gate exactly like a drawn one).
    ///
    /// Returns `false` (changing nothing) when there is no preview, the tool has no pre-placeable
    /// form ([`spawn_kind`] → `None`, e.g. a freehand tool), or the frame is too small for a
    /// non-degenerate item — the same degeneracy rule a discarded drag uses.
    pub(super) fn spawn_annotation(&mut self, tool: Tool) -> bool {
        let Some(p) = self.preview.as_mut() else {
            return false;
        };
        let stroke_w = p.edit.stroke();
        // The margin is kind-dependent (an arrow's caps overhang more than a box's outline), so
        // measure it on a probe of the kind itself at the nominal size.
        let probe = AnnotRect { x: 0.0, y: 0.0, w: SPAWN_W, h: SPAWN_H };
        let Some(margin) = spawn_kind(tool, probe, stroke_w).as_ref().map(kind_draw_margin) else {
            return false;
        };
        let rect = default_placement_rect(p.edit.frame, margin);
        let Some(kind) = spawn_kind(tool, rect, stroke_w) else {
            return false;
        };
        let id = p.edit.next_annot_id();
        let color = p.edit.annot_color.unwrap_or_else(default_annot_color);
        let item = AnnotationItem { id, color, kind };
        if is_degenerate(&item) {
            return false;
        }
        let prev = p.edit.annotations.clone();
        p.edit.annotations.push(item);
        p.edit.sel.set_one(id);
        p.edit.annot_menu = None;
        p.edit.push_annotations(prev);
        // Holistic dim rule: a spawned spotlight needs the frame dimmed to read (own undo entry).
        p.edit.ensure_dim_for_spotlights();
        true
    }

    /// Recolor the currently-SELECTED colorable annotation(s) to `color`, pushing ONE
    /// [`super::edit::EditOp::Annotations`] undo snapshot. No-op (no snapshot) when nothing is
    /// selected, the selection isn't colorable (pixelate/blur), or the color is unchanged.
    /// Iterates the selection so it already extends to multi-select. The caller sets
    /// `annot_color` separately; the view redraws the recolored item automatically.
    pub(super) fn recolor_selected_annotation(&mut self, color: AnnotColor) {
        let Some(p) = self.preview.as_mut() else {
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
        p.edit.push_annotations(prev);
    }

    /// Re-stroke the currently-SELECTED box/arrow to `stroke_w` (SOURCE px), pushing ONE
    /// [`super::edit::EditOp::Annotations`] undo snapshot — the width mirror of
    /// [`Self::recolor_selected_annotation`]. No-op (no snapshot) when nothing is selected,
    /// the selection has no stroke (highlight / pixelate / blur), or the width is unchanged.
    pub(super) fn restroke_selected_annotation(&mut self, stroke_w: f32) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        if p.edit.sel.is_empty() {
            return;
        }
        // Only a SELECTED, STROKED item (box / arrow / pen) whose width actually differs needs it.
        let needed = p.edit.annotations.iter().any(|it| {
            p.edit.sel.contains(it.id)
                && matches!(&it.kind, AnnotKind::Box { stroke_w: w, .. } | AnnotKind::Arrow { stroke_w: w, .. } | AnnotKind::BoxHighlight { stroke_w: w, .. } | AnnotKind::Pen { stroke_w: w, .. } if *w != stroke_w)
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
                    | AnnotKind::Pen { stroke_w: w, .. } => {
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
        p.edit.push_annotations(prev);
    }

    /// When a rect annotation (Box Outline / Highlight / Box Highlight / Pixelate / Blur) is
    /// SELECTED and the user picks a DIFFERENT one of those tools, convert the selected item to
    /// that kind IN PLACE (real-time), pushing ONE [`super::edit::EditOp::Annotations`] undo
    /// snapshot. No-op (no snapshot) when nothing is selected, the selection isn't a rect kind,
    /// the tool isn't a rect kind, or the kind is unchanged — so a normal tool pick just arms it.
    pub(super) fn convert_selected_annotation_kind(&mut self, tool: Tool) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let Some(id) = p.edit.selected() else {
            return;
        };
        let default_stroke = p.edit.stroke();
        let Some(idx) = p.edit.annotations.iter().position(|it| it.id == id) else {
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
    pub(super) fn apply_annot_stroke_w(&mut self, w: f32) {
        if let Some(p) = &mut self.preview {
            p.edit.annot_stroke_w = w;
        }
        // Picking a width also re-strokes the SELECTED box/arrow immediately (one undo entry).
        self.restroke_selected_annotation(w);
        // Persist so the next preview opens with this width.
        self.annot_stroke_w = w;
        self.save_state();
    }

    /// Begin manipulating the selection (`grab` from a handle / body).
    ///
    /// A MOVE with more than one item selected (DRAGON-341) opens a group gesture
    /// ([`AnnotGesture::MoveMany`]) that drags every selected item by ONE shared delta, clamped
    /// once on the selection's union bounds so the arrangement never distorts against an image
    /// edge. Every other grab (and any single selection) stays on the historical one-item
    /// [`AnnotGesture::Edit`] path — resize handles only ever exist on the PRIMARY item, so the
    /// whole `Grab` machinery is untouched by multi-select.
    pub(super) fn annot_grab_begin(&mut self, grab: Grab, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
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
        let Some(id) = p.edit.selected() else {
            return Task::none();
        };
        let Some(item) = p.edit.annotations.iter().find(|it| it.id == id) else {
            return Task::none();
        };
        p.edit.annot_snapshot = Some(p.edit.annotations.clone());
        p.edit.gesture = Some(AnnotGesture::Edit {
            press: (x, y),
            id,
            grab,
            original: item.kind.clone(),
        });
        Task::none()
    }

    /// Live drag update (image point). Updates the model geometry; box/arrow redraw as vector
    /// geometry on the view rebuild, while an effect (highlight/pixelate/blur) being drawn or
    /// resized re-rasters its display layer LIVE (coalesced) so the redaction tracks the drag.
    pub(super) fn annot_gesture_to(&mut self, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(gesture) = p.edit.gesture.clone() else {
            return Task::none();
        };
        // Clamp all gesture geometry to the image bounds (zoom-independent, in source px).
        let frame = (p.edit.frame.0 as f32, p.edit.frame.1 as f32);
        match gesture {
            AnnotGesture::New { press, id } => {
                // The pen's raw trail is a SIBLING field of the item vector — bound up front so
                // the freehand arm below can read/extend it while it holds the item.
                let raw = &mut p.edit.pen_raw;
                if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    // Clamp on the DRAWN extent so a shape drawn to the edge doesn't spill its
                    // outline/cap past it.
                    let m = kind_draw_margin(&item.kind);
                    let cl = |v: f32, hi: f32| v.clamp(m, (hi - m).max(m));
                    let pr = (cl(press.0, frame.0), cl(press.1, frame.1));
                    let cur = (cl(x, frame.0), cl(y, frame.1));
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
                                        x: cl(p.0, frame.0),
                                        y: cl(p.1, frame.1),
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
                    item.kind = edited_kind(&original, grab, press, (x, y), frame);
                }
            }
            // A group move (DRAGON-341): ONE delta, clamped ONCE on the union bounds, applied
            // verbatim to every member — so the selection travels as a rigid arrangement.
            AnnotGesture::MoveMany { press, ref originals, bounds } => {
                let (dx, dy) =
                    group_move_delta(bounds, frame, (x - press.0, y - press.1));
                for (id, original) in originals {
                    if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == *id) {
                        item.kind = translated_kind(original, dx, dy);
                    }
                }
            }
            // The eraser MARKS along the segment it just travelled (never only the sampled
            // point — a fast drag would jump clean over a stroke), then advances its anchor.
            AnnotGesture::Erase { last } => {
                let cur = (x.clamp(0.0, frame.0), y.clamp(0.0, frame.1));
                mark_erased(&mut p.edit, last, cur);
                p.edit.gesture = Some(AnnotGesture::Erase { last: cur });
            }
        }
        // A live drag mutates the model; the GPU effects shader re-renders from it every frame.
        Task::none()
    }

    /// Commit the active gesture: discard a degenerate new shape, else push ONE undo entry
    /// (the pre-gesture snapshot); the view redraws the final scene as vectors.
    pub(super) fn annot_gesture_end(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(gesture) = p.edit.gesture.take() else {
            return Task::none();
        };
        let snapshot = p.edit.annot_snapshot.take();
        let raw_trail = std::mem::take(&mut p.edit.pen_raw);
        match gesture {
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
            AnnotGesture::Edit { .. } | AnnotGesture::MoveMany { .. } => {
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
        // The committed geometry renders through the GPU effects shader on the next view build.
        Task::none()
    }

    /// Delete the WHOLE selection (DRAGON-341) — however many items — as ONE undo entry.
    pub(super) fn annot_delete_selected(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
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
            p.edit.push_annotations(prev);
            // Deleting an effect drops it from the GPU shader's item list on the next view build.
            return Task::none();
        }
        Task::none()
    }

    /// Select EVERY annotation in the scene (DRAGON-341 — the Ctrl+A action). The armed tool
    /// is NEVER touched (DRAGON-344, user decision): select-all is a selection action, not a
    /// mode switch. Pen groups join the set too — Ctrl+A is as deliberate as a pointer click —
    /// but the usual rule still applies afterwards: arming another tool prunes them
    /// ([`super::edit::EditState::drop_pen_selection`]). Returns whether anything is now
    /// selected — an empty scene changes nothing at all (no persisted state churn).
    pub(super) fn select_all_annotations(&mut self) -> bool {
        match self.preview.as_mut() {
            Some(p) if !p.edit.annotations.is_empty() => {
                let ids: Vec<AnnotId> = p.edit.annotations.iter().map(|it| it.id).collect();
                p.edit.sel.set_all(ids);
                p.edit.annot_menu = None;
                true
            }
            _ => false,
        }
    }

    /// Apply a POINTER rubber band (DRAGON-341): select every annotation the band
    /// `(x0, y0)`–`(x1, y1)` (image source px, either winding) TOUCHES. `additive` keeps the
    /// existing selection and adds to it; otherwise the band REPLACES it. A band that touches
    /// nothing simply clears (or leaves, when additive) the selection — never an undo entry,
    /// since selecting is not an edit.
    pub(super) fn band_select_annotations(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        additive: bool,
    ) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let band = AnnotRect::from_points((x0, y0), (x1, y1));
        let hits: Vec<AnnotId> = items_in_band(&p.edit.annotations, band);
        if additive {
            p.edit.sel.add_all(hits);
        } else {
            p.edit.sel.set_all(hits);
        }
        p.edit.annot_menu = None;
    }

    /// Duplicate the selected annotation: a clone with a new id, offset toward the frame CENTER
    /// by an equal x/y amount (so the copy is obviously distinct and easy to grab) and clamped to
    /// stay in the image. The copy lands on TOP of the z-stack and becomes the new selection.
    /// One undo entry. No-op when nothing is selected.
    pub(super) fn duplicate_selected_annotation(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(id) = p.edit.selected() else {
            return Task::none();
        };
        let Some(src) = p.edit.annotations.iter().find(|it| it.id == id).cloned() else {
            return Task::none();
        };
        let (fw, fh) = (p.edit.frame.0 as f32, p.edit.frame.1 as f32);
        // Offset toward the frame center, equal on both axes, scaled a little to the image size.
        let off = (fw.min(fh) * 0.04).clamp(16.0, 64.0);
        let (cx, cy) = kind_center(&src.kind);
        let dx = if fw * 0.5 >= cx { off } else { -off };
        let dy = if fh * 0.5 >= cy { off } else { -off };
        // A zero-press Move applies the offset AND clamps the copy's drawn bounds inside the image.
        let new_kind = edited_kind(&src.kind, Grab::Move, (0.0, 0.0), (dx, dy), (fw, fh));
        let new_id = p.edit.next_annot_id();
        let prev = p.edit.annotations.clone();
        p.edit.annotations.push(AnnotationItem { id: new_id, color: src.color, kind: new_kind });
        p.edit.sel.set_one(new_id);
        p.edit.annot_menu = None;
        p.edit.push_annotations(prev);
        // Duplicating a spotlight (e.g. after undo left the frame un-dimmed) re-ensures the dim.
        p.edit.ensure_dim_for_spotlights();
        Task::none()
    }

    /// Reorder the selected annotation in the z-stack (one undo entry when it moves).
    pub(super) fn annot_reorder(&mut self, how: Reorder) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(id) = p.edit.selected() else {
            return Task::none();
        };
        p.edit.annot_menu = None;
        let prev = p.edit.annotations.clone();
        let changed = match how {
            Reorder::Up => raise(&mut p.edit.annotations, id),
            Reorder::Down => lower(&mut p.edit.annotations, id),
            Reorder::Front => to_front(&mut p.edit.annotations, id),
            Reorder::Back => to_back(&mut p.edit.annotations, id),
        };
        if changed {
            p.edit.push_annotations(prev);
            // Reordering across effect TYPES changes the true-z-order composite — the GPU shader
            // walks the reordered item list on the next view build.
            return Task::none();
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
        | AnnotKind::Blur { rect } => rect.w < 2.0 || rect.h < 2.0,
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
        // A pen's ribbon straddles its centerline by half its WIDEST sample — a heavy
        // (pressure-swelled) stretch, not the nominal preset — so the margin rides `max_width`
        // and no inked pixel can land outside the picture.
        AnnotKind::Pen { stroke_w, .. } => crate::pen_stroke::max_width(*stroke_w) / 2.0,
        AnnotKind::Arrow { stroke_w, .. } => (stroke_w + 2.0) / 2.0,
        // Stroke-less kinds draw exactly within the rect (Spotlight is an invisible knockout).
        AnnotKind::Highlight { .. }
        | AnnotKind::Pixelate { .. }
        | AnnotKind::Blur { .. }
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
        AnnotKind::Pixelate { rect } => AnnotKind::Pixelate { rect: shift(rect) },
        AnnotKind::Blur { rect } => AnnotKind::Blur { rect: shift(rect) },
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
fn edited_kind(
    original: &AnnotKind,
    grab: Grab,
    press: (f32, f32),
    cur: (f32, f32),
    frame: (f32, f32),
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
        AnnotKind::Pixelate { rect } => {
            AnnotKind::Pixelate { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
        }
        AnnotKind::Blur { rect } => {
            AnnotKind::Blur { rect: edit_rect(rect, grab, dx, dy, fw, fh, m) }
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

    fn boxed(id: u64, x: f32, y: f32, w: f32, h: f32) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect: AnnotRect { x, y, w, h }, stroke_w: 4.0, fill: None },
        }
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
    fn stroke_width_cycle_wraps_2_4_6() {
        // The `L` hotkey advances thin → medium → thick → thin.
        assert_eq!(cycle_stroke_width(2.0), 4.0);
        assert_eq!(cycle_stroke_width(4.0), 6.0);
        assert_eq!(cycle_stroke_width(6.0), 2.0);
        // The default (4px) cycles to thick, then wraps back around.
        assert_eq!(cycle_stroke_width(DEFAULT_ANNOT_STROKE), 6.0);
        // A near-but-inexact width snaps to its nearest preset first, then advances.
        assert_eq!(cycle_stroke_width(3.9), 6.0); // nearest 4 → 6
        assert_eq!(cycle_stroke_width(1.0), 4.0); // nearest 2 → 4
        assert_eq!(cycle_stroke_width(100.0), 2.0); // nearest 6 → wraps to 2
    }

    #[test]
    fn stroke_width_nearest_index_picks_closest_preset() {
        assert_eq!(stroke_width_nearest_index(2.0), 0);
        assert_eq!(stroke_width_nearest_index(4.0), 1);
        assert_eq!(stroke_width_nearest_index(6.0), 2);
        // Off-preset values map to the closest segment (so exactly one reads active).
        assert_eq!(stroke_width_nearest_index(2.9), 0); // |2.9-2|<|2.9-4|
        assert_eq!(stroke_width_nearest_index(5.0), 1); // tie 4 vs 6 → lower index
        assert_eq!(stroke_width_nearest_index(0.0), 0); // 0 (unset) → 2px, nearest
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
    fn from_points_normalizes_either_drag_direction() {
        let a = AnnotRect::from_points((100.0, 100.0), (40.0, 60.0));
        assert_eq!((a.x, a.y, a.w, a.h), (40.0, 60.0, 60.0, 40.0));
        let b = AnnotRect::from_points((40.0, 60.0), (100.0, 100.0));
        assert_eq!((b.x, b.y, b.w, b.h), (40.0, 60.0, 60.0, 40.0));
    }

    #[test]
    fn edited_move_translates_a_box() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 }, stroke_w: 4.0, fill: None };
        let k = edited_kind(&orig, Grab::Move, (0.0, 0.0), (5.0, -7.0), (10000.0, 10000.0));
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (15.0, 13.0, 30.0, 40.0));
    }

    #[test]
    fn edited_corner_resizes_from_the_opposite_corner() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, stroke_w: 4.0, fill: None };
        // Drag the SE corner to (150, 130): the NW corner (0,0) stays put.
        let k = edited_kind(&orig, Grab::Corner(Corner::Se), (100.0, 100.0), (150.0, 130.0), (10000.0, 10000.0));
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
        let k = edited_kind(&orig, Grab::Corner(Corner::Se), (104.0, 104.0), (114.0, 114.0), (10000.0, 10000.0));
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.0, 0.0, 110.0, 110.0), "no 4px jump");
    }

    #[test]
    fn edited_edge_moves_only_that_side() {
        let orig = AnnotKind::Box { rect: AnnotRect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, stroke_w: 4.0, fill: None };
        let k = edited_kind(&orig, Grab::Edge(Edge::E), (100.0, 50.0), (140.0, 50.0), (10000.0, 10000.0));
        let AnnotKind::Box { rect, .. } = k else { panic!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.0, 0.0, 140.0, 100.0));
    }

    #[test]
    fn edited_arrow_endpoints_and_move() {
        let orig = AnnotKind::Arrow { a: AnnotPoint { x: 0.0, y: 0.0 }, b: AnnotPoint { x: 100.0, y: 0.0 }, stroke_w: 6.0 };
        // Drag endpoint B to (120, 30): A unchanged.
        let k = edited_kind(&orig, Grab::ArrowB, (100.0, 0.0), (120.0, 30.0), (10000.0, 10000.0));
        let AnnotKind::Arrow { a, b, .. } = k else { panic!() };
        assert_eq!((a.x, a.y, b.x, b.y), (0.0, 0.0, 120.0, 30.0));
        // Move translates both.
        let k = edited_kind(&orig, Grab::Move, (0.0, 0.0), (10.0, 5.0), (10000.0, 10000.0));
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
            edited_kind(&orig, Grab::Move, (0.0, 0.0), (500.0, 500.0), frame)
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
            edited_kind(&hl, Grab::Move, (0.0, 0.0), (500.0, 500.0), frame)
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

    #[test]
    fn pen_moves_and_resizes_through_its_bounding_box() {
        // The grab model: Move translates the whole drawing; a corner drag scales it — both
        // clamped inside the image like every other kind.
        let item = pen(1, [255, 0, 0, 255], 4.0, &[&[(10.0, 10.0), (30.0, 30.0)]]);
        let moved = edited_kind(&item.kind, Grab::Move, (0.0, 0.0), (5.0, 5.0), (200.0, 200.0));
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
}
