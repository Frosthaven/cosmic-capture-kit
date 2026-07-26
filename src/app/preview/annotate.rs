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
//! (line / ellipse / text / numbered / pen / …):
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
        )
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
        // Spotlight is NOT an effect (it composites nothing) — like box/arrow, no-op here.
        AnnotKind::Box { .. } | AnnotKind::Arrow { .. } | AnnotKind::Spotlight { .. } => return,
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
        AnnotKind::Box { .. } | AnnotKind::Arrow { .. } | AnnotKind::Spotlight { .. } => {
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
            AnnotKind::Arrow { .. } | AnnotKind::Pixelate { .. } | AnnotKind::Blur { .. } => None,
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

/// Straight-alpha RGBA bytes → an iced [`Color`](cosmic::iced::Color).
fn to_iced_color(c: AnnotColor) -> cosmic::iced::Color {
    cosmic::iced::Color::from_rgba8(c[0], c[1], c[2], c[3] as f32 / 255.0)
}

/// Convert model items into the widget's hit-test/chrome/DRAW geometry. `curve_radius` (the
/// shared corner curve, SOURCE px) is stamped onto each item so the canvas draws the SAME
/// rounded corners / soft caps the bake rasterizes — the vector display and the raster bake
/// stay visually consistent.
pub fn widget_items(items: &[AnnotationItem], curve_radius: f32) -> Vec<Item> {
    items
        .iter()
        .map(|it| {
            let stroke_color = to_iced_color(it.color);
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
    /// Push a CUSTOM color onto the last-5 MRU (most-recent-first, RGB-deduped, capped at 5).
    pub(super) fn push_recent_color(&mut self, c: AnnotColor) {
        self.annot_recent_colors.retain(|x| x[..3] != c[..3]);
        self.annot_recent_colors.insert(0, c);
        self.annot_recent_colors.truncate(5);
    }

    /// Begin drawing a new shape of `tool` at image point `(x, y)`.
    pub(super) fn annot_draw_begin(&mut self, tool: Tool, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let color = p.edit.annot_color.unwrap_or_else(default_annot_color);
        let stroke_w = p.edit.stroke();
        let id = p.edit.next_annot_id();
        // Clamp the start point inside the image (can't draw beyond the picture).
        let (fw, fh) = (p.edit.frame.0 as f32, p.edit.frame.1 as f32);
        let (x, y) = (x.clamp(0.0, fw), y.clamp(0.0, fh));
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
        };
        p.edit.annot_snapshot = Some(p.edit.annotations.clone());
        p.edit.annotations.push(AnnotationItem { id, color, kind });
        p.edit.selected = Some(id);
        p.edit.gesture = Some(AnnotGesture::New { press: (x, y), id });
        // Holistic dim rule: with a spotlight now on the canvas, make sure the frame is dimmed so
        // it reads while you draw it (own undo entry; undo removes the spotlight, then the dim).
        p.edit.ensure_dim_for_spotlights();
        // The GPU effects shader re-renders from the model on the next view build (DRAGON-330) —
        // no async raster to kick; a new effect item shows on the very next frame.
        Task::none()
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
        let Some(id) = p.edit.selected else {
            return;
        };
        // Change is needed only if a SELECTED, COLORABLE item is actually a different color.
        let needed = p
            .edit
            .annotations
            .iter()
            .any(|it| it.id == id && it.kind.is_colorable() && it.color != color);
        if !needed {
            return;
        }
        let prev = p.edit.annotations.clone();
        for it in p.edit.annotations.iter_mut() {
            if it.id == id && it.kind.is_colorable() {
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
        let Some(id) = p.edit.selected else {
            return;
        };
        // Only a SELECTED, STROKED item (box / arrow) whose width actually differs needs it.
        let needed = p.edit.annotations.iter().any(|it| {
            it.id == id
                && matches!(&it.kind, AnnotKind::Box { stroke_w: w, .. } | AnnotKind::Arrow { stroke_w: w, .. } | AnnotKind::BoxHighlight { stroke_w: w, .. } if *w != stroke_w)
        });
        if !needed {
            return;
        }
        let prev = p.edit.annotations.clone();
        for it in p.edit.annotations.iter_mut() {
            if it.id == id {
                match &mut it.kind {
                    AnnotKind::Box { stroke_w: w, .. }
                    | AnnotKind::Arrow { stroke_w: w, .. }
                    // BoxHighlight's OUTLINE stroke re-widths like a box (DRAGON-333).
                    | AnnotKind::BoxHighlight { stroke_w: w, .. } => {
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
        let Some(id) = p.edit.selected else {
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

    /// Begin manipulating the selected item (`grab` from a handle / body).
    pub(super) fn annot_grab_begin(&mut self, grab: Grab, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(id) = p.edit.selected else {
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
                    }
                }
            }
            AnnotGesture::Edit { press, id, grab, original } => {
                if let Some(item) = p.edit.annotations.iter_mut().find(|it| it.id == id) {
                    item.kind = edited_kind(&original, grab, press, (x, y), frame);
                }
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
        match gesture {
            AnnotGesture::New { id, .. } => {
                let degenerate = p
                    .edit
                    .annotations
                    .iter()
                    .find(|it| it.id == id)
                    .is_none_or(is_degenerate);
                if degenerate {
                    // Discard: never entered history, so just drop it (no undo entry).
                    p.edit.annotations.retain(|it| it.id != id);
                    if p.edit.selected == Some(id) {
                        p.edit.selected = None;
                    }
                    // A discarded in-progress effect vanishes on the next view build (GPU shader).
                    return Task::none();
                }
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
            AnnotGesture::Edit { .. } => {
                if let Some(prev) = snapshot {
                    p.edit.push_annotations(prev);
                }
            }
        }
        // The committed geometry renders through the GPU effects shader on the next view build.
        Task::none()
    }

    /// Delete the selected annotation (one undo entry).
    pub(super) fn annot_delete_selected(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(id) = p.edit.selected else {
            return Task::none();
        };
        p.edit.annot_menu = None;
        let prev = p.edit.annotations.clone();
        p.edit.annotations.retain(|it| it.id != id);
        if prev.len() != p.edit.annotations.len() {
            p.edit.selected = None;
            p.edit.push_annotations(prev);
            // Deleting an effect drops it from the GPU shader's item list on the next view build.
            return Task::none();
        }
        Task::none()
    }

    /// Duplicate the selected annotation: a clone with a new id, offset toward the frame CENTER
    /// by an equal x/y amount (so the copy is obviously distinct and easy to grab) and clamped to
    /// stay in the image. The copy lands on TOP of the z-stack and becomes the new selection.
    /// One undo entry. No-op when nothing is selected.
    pub(super) fn duplicate_selected_annotation(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_mut() else {
            return Task::none();
        };
        let Some(id) = p.edit.selected else {
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
        p.edit.selected = Some(new_id);
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
        let Some(id) = p.edit.selected else {
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
    }
}

fn kind_draw_margin(kind: &AnnotKind) -> f32 {
    match kind {
        AnnotKind::Box { stroke_w, .. } | AnnotKind::BoxHighlight { stroke_w, .. } => stroke_w / 2.0,
        AnnotKind::Arrow { stroke_w, .. } => (stroke_w + 2.0) / 2.0,
        // Stroke-less kinds draw exactly within the rect (Spotlight is an invisible knockout).
        AnnotKind::Highlight { .. }
        | AnnotKind::Pixelate { .. }
        | AnnotKind::Blur { .. }
        | AnnotKind::Spotlight { .. } => 0.0,
    }
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
        let w = widget_items(&items, 7.0);
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
        let w = widget_items(&items, 8.0);
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
        let w = widget_items(&e.annotations, DEFAULT_ANNOT_CURVE_RADIUS);
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
}
