//! Preview-overlay editing: a single covermark overlay (with zoom + undo/redo).
//!
//! The covermark is NON-destructive until a share action (Save / Save As / Copy)
//! bakes it into the file: an image is re-encoded in place from its decoded pixels;
//! a video is re-encoded through an `ffmpeg` `overlay` filter graph. Undo/redo moves
//! the covermark between history stacks — the display recomposites from the untouched
//! original (image) or stacks the covermark over the frame (video), so nothing is
//! lost until the user commits by sharing.

use super::annotate::{AnnotGesture, AnnotColor, AnnotId, AnnotationItem};
use super::layers::RasterSlot;
use super::timeline::{Span, Timeline};
use crate::widgets::annotation_canvas::Tool;
use ::image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The built-in "CONFIDENTIAL" covermark (also installed for packaging; embedded
/// so the default choice exists on every install).
const CONFIDENTIAL_SVG: &[u8] = include_bytes!("../../../res/covermarks/confidential.svg");

/// What a covermark draws.
#[derive(Clone, Debug, PartialEq)]
pub enum CovermarkKind {
    /// The built-in tiled red/white "CONFIDENTIAL" mark.
    Confidential,
    /// A custom tiled gray text mark (text snapshotted from settings when applied).
    Text(String),
    /// A user-supplied SVG from the covermarks folder.
    File(PathBuf),
}

impl CovermarkKind {
    /// Display name for the picker. The custom-text mark shows its configured text
    /// (unless blank once trimmed, then a generic label).
    pub fn name(&self) -> String {
        match self {
            CovermarkKind::Confidential => "Confidential".into(),
            CovermarkKind::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
            CovermarkKind::Text(_) => "Custom text".into(),
            CovermarkKind::File(p) => p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "covermark".into()),
        }
    }

    /// A stable key for remembering this option's zoom/opacity independently of the
    /// others. The "Custom text" option shares one slot (its text can change); files key
    /// by path; the built-in mark is its own slot.
    pub fn pref_key(&self) -> String {
        match self {
            CovermarkKind::Confidential => "confidential".to_string(),
            CovermarkKind::Text(_) => "text".to_string(),
            CovermarkKind::File(p) => format!("file:{}", p.display()),
        }
    }

    /// The SVG bytes this kind renders (generated for `Text`, read for `File`).
    fn svg(&self) -> Option<std::borrow::Cow<'static, [u8]>> {
        match self {
            CovermarkKind::Confidential => Some(std::borrow::Cow::Borrowed(CONFIDENTIAL_SVG)),
            CovermarkKind::Text(text) => Some(std::borrow::Cow::Owned(text_svg(text).into_bytes())),
            CovermarkKind::File(p) => std::fs::read(p).ok().map(std::borrow::Cow::Owned),
        }
    }
}

/// A covermark applied to the capture: what it draws, a zoom factor (0 = the
/// default cover fit; higher enlarges the pattern while still filling the frame),
/// and an opacity (0..1) applied to the whole mark at composite time.
#[derive(Clone, Debug, PartialEq)]
pub struct Covermark {
    pub kind: CovermarkKind,
    pub zoom: f32,
    pub opacity: f32,
}

/// What share action to run once a bake finishes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShareIntent {
    Save,
    Copy,
}

/// The covermark picker's entries while open (a dropdown under the covermark button). The
/// keyboard highlight/nav lives in the SHARED [`FlyoutNav`] (see [`EditState::flyout`]).
pub struct Picker {
    /// The choices, in display order. `None` is the "None" (disable) card, always
    /// first; `Some(kind)` are the real covermarks.
    pub entries: Vec<Option<CovermarkKind>>,
}

/// Which keyboard-navigable toolbar flyout is open. Both flyouts drive the SAME small state
/// machine ([`FlyoutNav`]) + the same `preview_modal_key` dispatch; only the entry LIST and
/// the apply ACTION differ per kind. A future third flyout adds a variant + its panel +
/// entry list + a hotkey, reusing all the open/nav/select/display plumbing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlyoutKind {
    /// The covermark picker (bottom bar).
    Covermark,
    /// The annotation color palette (top bar).
    Color,
}

/// The shared open/nav state of a toolbar flyout: which one is open, the highlighted entry
/// index (`None` = nothing highlighted yet), and the entry count (for wrap-around).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FlyoutNav {
    pub kind: FlyoutKind,
    pub selected: Option<usize>,
    pub len: usize,
}

impl FlyoutNav {
    /// Move the highlight by `delta`, wrapping. From "no highlight", +1 → first, −1 → last.
    pub fn nav(&mut self, delta: i32) {
        if self.len == 0 {
            return;
        }
        let n = self.len as i32;
        let base = self.selected.map(|s| s as i32).unwrap_or(if delta >= 0 { -1 } else { 0 });
        self.selected = Some(((base + delta).rem_euclid(n)) as usize);
    }
}

/// One undoable preview edit — the SHARED history holds both kinds in order,
/// so Ctrl+Z walks covermark changes and timeline cuts/deletes interleaved,
/// newest first, exactly as they were made.
#[derive(Clone, Debug, PartialEq)]
pub enum EditOp {
    /// A covermark change: the covermark state BEFORE the change.
    Covermark(Option<Covermark>),
    /// A timeline cut/delete: the kept spans BEFORE the change.
    Timeline(Vec<Span>),
    /// An annotation-scene change (add/move/resize/delete/reorder): the whole scene
    /// BEFORE the change. A continuous drag pushes ONE entry on gesture-commit.
    Annotations(Vec<AnnotationItem>),
    /// A global dim change (DRAGON-329): the dim value (0..1) BEFORE the change. One entry per
    /// slider DRAG (coalesced via [`EditState::dim_drag_start`]), like an annotation gesture.
    Dim(f32),
}

/// Which display artifact an undo/redo step touched, so the caller refreshes the right
/// raster: a covermark or annotation re-raster is owed; a timeline change redraws for free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Covermark,
    Timeline,
    Annotations,
    /// A global dim change — redraws for free through the GPU dim pass on the next view build.
    Dim,
}

/// A LIVE, debounced preview edit driven by a DRAGGING slider — the shared live-slider path
/// (covermark zoom/opacity today; any future RASTER-backed slider after). The dim/spotlight
/// slider (DRAGON-329) is NOT here: the dim renders straight from the model through the GPU
/// dim pass every view build (like the annotation effects), so it needs no off-thread raster.
/// Every raster-backed slider wires up identically, so no debounce is hand-rolled per control:
///   * each drag TICK updates the model value, then calls [`super::App::refresh_live_edit`],
///     which kicks a COALESCED off-thread re-raster of that edit's [`RasterSlot`]. The slot's
///     begin/finish coalescing IS the debounce — at most one raster is ever in flight, and
///     however many ticks arrive while it runs collapse into exactly ONE pending re-run — so a
///     fast drag re-renders CONTINUOUSLY without thrashing the GPU, and blink-free (the
///     persistent-texture shader updates in place).
///   * RELEASE fires the same refresh once more, so the final settled value is always rendered.
///
/// Add a live edit: add a variant here plus its raster-slot arm in
/// [`super::App::refresh_live_edit`]; the slider + handler wiring is then identical.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiveEdit {
    /// The covermark overlay — its zoom + opacity sliders share the one `cm_raster` slot.
    Covermark,
}

/// The preview's edit state — shared by image and video previews.
#[derive(Default)]
pub struct EditState {
    /// The active covermark (kind + zoom), or `None`.
    pub covermark: Option<Covermark>,
    /// Prior edit states (undo pops from here), covermark + timeline interleaved.
    pub undo_stack: Vec<EditOp>,
    /// Redone-from states (redo pops from here). Cleared by any new edit.
    pub redo_stack: Vec<EditOp>,
    /// The covermark picker dropdown's entries, when open (paired with a `flyout` of kind
    /// `Covermark`).
    pub picker: Option<Picker>,
    /// The SHARED keyboard-nav state of whichever toolbar flyout is open (covermark picker
    /// or color palette). Drives the highlight + arrow/Enter/Esc dispatch for both.
    pub flyout: Option<FlyoutNav>,
    /// A bake (export re-encode) is in flight; share/delete inputs are held off.
    pub baking: bool,
    /// The share action to run when the in-flight bake completes.
    pub pending: Option<ShareIntent>,
    /// The file the in-flight bake writes (the capture itself for Save/SaveAs; a
    /// throwaway temp for Copy, so copying never persists edits to the saved file).
    pub pending_output: Option<PathBuf>,
    /// Save was pressed on a `--preview` file with edits: confirm before overwriting.
    pub confirm_overwrite: bool,
    /// Cached covermark-overlay raster (raw RGBA), stacked over the base image/video via a
    /// persistent-texture shader so re-rasters don't churn iced's atlas (no blink). Built
    /// off-thread, coalesced/staleness-tracked by the slot itself. Shared by image + video
    /// previews.
    pub cm_raster: RasterSlot,
    /// The pixel dimensions the covermark raster was last produced at (DRAGON-324). The
    /// covermark raster resolution scales with the view zoom so a magnified mark stays crisp;
    /// this lets a zoom step SKIP a re-raster when the wanted resolution is unchanged.
    pub cm_raster_px: (u32, u32),
    /// The capture's pixel dimensions (set once probed/decoded) so preview rasters
    /// match the bake's aspect.
    pub frame: (u32, u32),

    // ── Annotation editor (DRAGON-321; IMAGES only) ──────────────────────────────────
    /// The annotation scene, in SOURCE-pixel coords. Z-order IS the vector order (later
    /// = on top).
    pub annotations: Vec<AnnotationItem>,
    /// The active annotation DRAW tool: `Some(Arrow | Box)` draws on an empty-canvas drag;
    /// `None` (the default) is NEUTRAL — existing items are still fully selectable /
    /// movable / resizable and an empty click deselects, but an empty drag draws nothing.
    pub tool: Option<Tool>,
    /// The current annotation color (`None` = the accent default, resolved when a shape
    /// is created so the off-thread raster never reads the theme).
    pub annot_color: Option<AnnotColor>,
    /// The SHARED stroke width (SOURCE px) seeded onto every new box AND arrow — the single
    /// source of truth a future width control drives. `0.0` means
    /// [`super::annotate::DEFAULT_ANNOT_STROKE`].
    pub annot_stroke_w: f32,
    /// The SHARED ABSOLUTE corner radius (SOURCE px) both the box (corner radius) and arrow
    /// (round caps when > 0) read. `0.0` means [`super::annotate::DEFAULT_ANNOT_CURVE_RADIUS`]
    /// (there is no way to set a deliberate sharp `0.0` yet, so the fallback is safe).
    pub annot_curve_radius: f32,
    /// The selected annotation, if any (drives chrome + Delete/reorder/Esc handling).
    pub selected: Option<AnnotId>,
    /// The in-flight pointer gesture (draw / move / resize), if any.
    pub gesture: Option<AnnotGesture>,
    /// The pre-gesture scene snapshot, pushed as ONE undo entry on gesture-commit.
    pub annot_snapshot: Option<Vec<AnnotationItem>>,
    /// The custom-color WHEEL picker's live model. `Some` = the picker is open — it owns the
    /// interactive hue/saturation-value spectrum + hex/rgb input (libcosmic's own
    /// cross-platform color picker). `None` (the `Default`) = closed.
    pub annot_picker: Option<cosmic::widget::ColorPickerModel>,
    /// The annotation right-click context menu anchor (widget-local point), when open.
    pub annot_menu: Option<(f32, f32)>,
    /// Monotonic id source for new annotations.
    pub next_annot_id: u64,
    /// The base picture pixels as a GPU-uploadable frame (DRAGON-330), cached once on decode so
    /// the real-time effects shader ([`crate::widgets::annotation_fx`]) can seed its ping-pong
    /// from the retained original without re-copying every view build. `None` when the decode
    /// fell back to a path handle (no retained pixels) — effects then appear only on export.
    pub fx_base: Option<Arc<super::layers::PixelFrame>>,

    // ── Global dim / spotlight (DRAGON-329; IMAGES only) ─────────────────────────────────
    /// The global dim amount (0..1): `0` = no dim (byte-identical to no dim), higher = darker.
    /// Punched out to full brightness inside the knockout rects (spotlight / box / highlight /
    /// box-highlight). ALWAYS starts at 0 (never persisted across previews). Renders via the GPU
    /// dim pass on display and [`super::annotate::apply_dim`] on bake.
    pub dim: f32,
    /// The dim value at the START of the active slider drag, `Some` while dragging — so a whole
    /// drag coalesces into ONE undo entry (pushed on release; the mirror of `annot_snapshot`).
    pub dim_drag_start: Option<f32>,
}

impl EditState {
    /// Whether an edit needs a bake before sharing: a covermark, a non-empty annotation scene
    /// (any spotlight is an item, so this counts it), OR a non-zero global dim (DRAGON-329) —
    /// any would be silently dropped otherwise.
    pub fn dirty(&self) -> bool {
        self.covermark.is_some() || !self.annotations.is_empty() || self.dim > 0.0
    }

    /// The kind of toolbar flyout currently open (covermark picker / color palette), if any.
    pub fn flyout_kind(&self) -> Option<FlyoutKind> {
        self.flyout.map(|f| f.kind)
    }

    /// The open flyout's highlighted entry index, if any.
    pub fn flyout_selected(&self) -> Option<usize> {
        self.flyout.and_then(|f| f.selected)
    }

    /// Open a flyout of `kind`, highlighting `selected`, over `len` entries.
    pub fn open_flyout(&mut self, kind: FlyoutKind, selected: Option<usize>, len: usize) {
        self.flyout = Some(FlyoutNav { kind, selected, len });
    }

    /// Close any open flyout (covermark picker or color palette) + drop covermark entries.
    pub fn close_flyout(&mut self) {
        self.flyout = None;
        self.picker = None;
    }

    /// The SHARED stroke width for new annotations (SOURCE px), falling back to the default.
    pub fn stroke(&self) -> f32 {
        if self.annot_stroke_w > 0.0 {
            self.annot_stroke_w
        } else {
            super::annotate::DEFAULT_ANNOT_STROKE
        }
    }

    /// The SHARED absolute corner radius (SOURCE px) both shapes rasterize with, falling
    /// back to the default.
    pub fn curve_radius(&self) -> f32 {
        if self.annot_curve_radius > 0.0 {
            self.annot_curve_radius
        } else {
            super::annotate::DEFAULT_ANNOT_CURVE_RADIUS
        }
    }

    /// Mint the next annotation id.
    pub fn next_annot_id(&mut self) -> AnnotId {
        self.next_annot_id += 1;
        AnnotId(self.next_annot_id)
    }

    /// Record an annotation mutation in the shared history: push the PRE-EDIT scene and
    /// clear redo, mirroring [`Self::push_timeline`].
    pub fn push_annotations(&mut self, prev: Vec<AnnotationItem>) {
        self.undo_stack.push(EditOp::Annotations(prev));
        self.redo_stack.clear();
    }

    /// Record a global-dim change (DRAGON-329) in the shared history: push the PRE-DRAG value
    /// and clear redo, mirroring [`Self::push_annotations`]. `prev` is the dim BEFORE the drag.
    pub fn push_dim(&mut self, prev: f32) {
        self.undo_stack.push(EditOp::Dim(prev));
        self.redo_stack.clear();
    }

    /// Holistic spotlight/dim rule (DRAGON-329): a spotlight knocks a hole in the dim, so with no
    /// dim it would be INVISIBLE. Whenever ≥1 spotlight EXISTS on the canvas and the frame isn't
    /// dimmed yet, seed a dim (30% transparent) as its own undo entry — so a spotlight always
    /// reads however it came to exist (drawn, converted from another box, or duplicated). No-op
    /// once any dim is present (so the user can still slide it wherever they want afterward).
    pub fn ensure_dim_for_spotlights(&mut self) {
        const SPOTLIGHT_SEED_DIM: f32 = 0.7; // 30% transparent on the reversed slider
        if self.dim > 0.0 {
            return;
        }
        let has_spotlight = self
            .annotations
            .iter()
            .any(|it| matches!(it.kind, super::annotate::AnnotKind::Spotlight { .. }));
        if !has_spotlight {
            return;
        }
        let prev = self.dim;
        self.push_dim(prev);
        self.dim = SPOTLIGHT_SEED_DIM;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Set (or clear) the active covermark, pushing the prior state onto the undo
    /// stack and clearing redo. The display recomposite is the caller's job (async).
    pub fn set_covermark(&mut self, cm: Option<Covermark>) {
        self.undo_stack.push(EditOp::Covermark(self.covermark.clone()));
        self.redo_stack.clear();
        self.covermark = cm;
        self.cm_raster.invalidate();
    }

    /// Record a timeline mutation (cut / segment delete) in the shared history:
    /// push the PRE-EDIT spans and clear redo, mirroring `set_covermark`. Called
    /// after the mutation succeeded (refused cuts/deletes never enter history).
    pub fn push_timeline(&mut self, prev: Vec<Span>) {
        self.undo_stack.push(EditOp::Timeline(prev));
        self.redo_stack.clear();
    }

    /// Live-adjust the active covermark's zoom (no undo entry — it's a continuous
    /// control). No-op when no covermark is set.
    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(cm) = &mut self.covermark {
            cm.zoom = zoom.max(0.0);
            self.cm_raster.invalidate();
        }
    }

    /// The active covermark's zoom, or 0 when none.
    pub fn zoom(&self) -> f32 {
        self.covermark.as_ref().map(|c| c.zoom).unwrap_or(0.0)
    }

    /// Undo the most recent edit — covermark, timeline, or annotation, whichever was
    /// made last. `timeline` is the video preview's timeline when there is one (an image
    /// preview passes `None`; it never accumulates timeline ops). Returns which artifact
    /// changed so the caller refreshes the owed raster (`None` = the stack was empty). Only a
    /// covermark change owes a raster refresh now — annotations redraw as vectors for free.
    pub fn undo(&mut self, timeline: Option<&mut Timeline>) -> Option<EditKind> {
        match self.undo_stack.pop() {
            Some(EditOp::Covermark(prev)) => {
                self.redo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = prev;
                self.cm_raster.invalidate();
                Some(EditKind::Covermark)
            }
            Some(EditOp::Timeline(prev)) => {
                if let Some(tl) = timeline {
                    self.redo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(prev);
                }
                Some(EditKind::Timeline)
            }
            Some(EditOp::Annotations(prev)) => {
                self.redo_stack.push(EditOp::Annotations(self.annotations.clone()));
                self.annotations = prev;
                self.selected = None;
                Some(EditKind::Annotations)
            }
            Some(EditOp::Dim(prev)) => {
                self.redo_stack.push(EditOp::Dim(self.dim));
                self.dim = prev;
                Some(EditKind::Dim)
            }
            None => None,
        }
    }

    /// Redo the most recently undone edit (any kind). Returns which artifact changed, as
    /// [`Self::undo`].
    pub fn redo(&mut self, timeline: Option<&mut Timeline>) -> Option<EditKind> {
        match self.redo_stack.pop() {
            Some(EditOp::Covermark(next)) => {
                self.undo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = next;
                self.cm_raster.invalidate();
                Some(EditKind::Covermark)
            }
            Some(EditOp::Timeline(next)) => {
                if let Some(tl) = timeline {
                    self.undo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(next);
                }
                Some(EditKind::Timeline)
            }
            Some(EditOp::Annotations(next)) => {
                self.undo_stack.push(EditOp::Annotations(self.annotations.clone()));
                self.annotations = next;
                self.selected = None;
                Some(EditKind::Annotations)
            }
            Some(EditOp::Dim(next)) => {
                self.undo_stack.push(EditOp::Dim(self.dim));
                self.dim = next;
                Some(EditKind::Dim)
            }
            None => None,
        }
    }

    /// The display-preview raster size for the current frame (a ≤1024 box at the
    /// capture's aspect) — the baseline covermark raster resolution at fit zoom.
    pub fn preview_raster_size(&self) -> (u32, u32) {
        let (fw, fh) = match self.frame {
            (0, _) | (_, 0) => (1280u32, 800u32),
            f => f,
        };
        let scale = (1024.0 / fw as f32).min(1024.0 / fh as f32).min(1.0);
        (((fw as f32 * scale) as u32).max(1), ((fh as f32 * scale) as u32).max(1))
    }

    /// The covermark display raster resolution at the current `view_zoom` (DRAGON-324): the
    /// ≤1024 baseline at fit zoom, growing PROPORTIONALLY as you zoom in — capped at the full
    /// source frame (beyond which there is no detail to gain, and it matches the bake exactly).
    /// So a magnified covermark re-rasters sharper instead of sampling a soft preview texture.
    pub fn covermark_raster_size(&self, view_zoom: f32) -> (u32, u32) {
        let (fw, fh) = match self.frame {
            (0, _) | (_, 0) => (1280u32, 800u32),
            f => f,
        };
        let (pw, ph) = self.preview_raster_size();
        let z = view_zoom.max(1.0);
        let w = ((pw as f32 * z).round() as u32).clamp(pw, fw);
        let h = ((ph as f32 * z).round() as u32).clamp(ph, fh);
        (w, h)
    }
}

/// Rasterize a covermark to a `w`×`h` straight-alpha RGBA (for the video overlay
/// preview). Public so the async recomposite in `preview::mod` can run it off-thread.
pub fn rasterize_preview(cm: &Covermark, w: u32, h: u32) -> Option<RgbaImage> {
    rasterize(cm, w, h)
}

/// The user covermark folder (`~/.config/cosmic-capture-kit/covermarks` on every OS
/// — see [`crate::util::app_config_dir`]), created on first use so it's discoverable.
pub fn covermark_dir() -> Option<PathBuf> {
    let dir = crate::util::app_config_dir()?.join("covermarks");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// The picker's choices: the built-in Confidential mark, the custom-text mark (its
/// text snapshotted from `custom_text`), then every `.svg` in the covermark folder.
pub fn covermark_entries(custom_text: &str) -> Vec<CovermarkKind> {
    let mut entries = vec![
        CovermarkKind::Confidential,
        CovermarkKind::Text(custom_text.to_string()),
    ];
    if let Some(dir) = covermark_dir()
        && let Ok(read) = std::fs::read_dir(dir)
    {
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
            })
            .collect();
        files.sort();
        entries.extend(files.into_iter().map(CovermarkKind::File));
    }
    entries
}

/// The default custom-covermark text before the user configures one in settings.
const DEFAULT_COVERMARK_TEXT: &str = "CONFIGURE TEXT IN SETTINGS";

/// The built-in Confidential SVG bytes (for the picker's preview thumbnail).
pub fn confidential_svg() -> &'static [u8] {
    CONFIDENTIAL_SVG
}

/// The generated text-covermark SVG bytes for `text` (for the picker's preview).
pub fn text_svg_bytes(text: &str) -> Vec<u8> {
    text_svg(text).into_bytes()
}

/// Build a tiled, −45°, gray, borderless text covermark SVG at FULL opacity — the
/// covermark opacity is applied later at composite time (a runtime slider), not baked
/// into the SVG.
fn text_svg(text: &str) -> String {
    // Escape XML-special chars so arbitrary user text can't break the document.
    let safe = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let safe = if safe.trim().is_empty() { DEFAULT_COVERMARK_TEXT.to_string() } else { safe };
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" width="1600" height="1000">
  <defs>
    <pattern id="mark" width="620" height="200" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)">
      <text x="0" y="70" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
      <text x="-310" y="170" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
      <text x="310" y="170" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
    </pattern>
  </defs>
  <rect width="1600" height="1000" fill="url(#mark)"/>
</svg>"##
    )
}

/// Rasterize a covermark to COVER a `w`×`h` frame (aspect-preserving fill, centered,
/// overflow cropped), returning straight-alpha RGBA the same size as the frame. The
/// covermark's `zoom` multiplies the fill scale (≥ cover, so it always fills). Text
/// elements need fonts: the system fontdb is loaded once and shared.
fn rasterize(cm: &Covermark, w: u32, h: u32) -> Option<RgbaImage> {
    static FONTS: std::sync::OnceLock<Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        use resvg::usvg::fontdb;
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // fontdb resolves generic families ("sans-serif") by its CONFIGURED
        // name — defaulting to the Windows names (Arial…) — not by fuzzy
        // matching; the host's fontconfig aliases normally correct that. On
        // systems without usable aliases (minimal distros, bare containers)
        // the name matches no face, usvg then drops the whole text run, and
        // <text> covermarks rasterize EMPTY. If the generic doesn't resolve,
        // repoint it (and serif, usvg's built-in last resort) at a face that
        // actually exists.
        let resolves = |db: &fontdb::Database, family: fontdb::Family| {
            db.query(&fontdb::Query { families: &[family], ..Default::default() }).is_some()
        };
        if !resolves(&db, fontdb::Family::SansSerif) {
            let pick = ["DejaVu Sans", "Liberation Sans", "Noto Sans", "Cantarell", "Ubuntu", "FreeSans"]
                .into_iter()
                .find(|n| resolves(&db, fontdb::Family::Name(n)))
                .map(str::to_string)
                .or_else(|| db.faces().next().map(|f| f.families[0].0.clone()));
            if let Some(name) = pick {
                db.set_sans_serif_family(name.clone());
                db.set_serif_family(name);
            }
        }
        Arc::new(db)
    });
    let bytes = cm.kind.svg()?;
    let opt = resvg::usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(&bytes, &opt).ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 || w == 0 || h == 0 {
        return None;
    }
    // Cover fit, then zoom enlarges from there (never below cover → always fills).
    let cover = (w as f32 / size.width()).max(h as f32 / size.height());
    let scale = cover * (1.0 + cm.zoom.max(0.0));
    let tx = (w as f32 - size.width() * scale) / 2.0;
    let ty = (h as f32 - size.height() * scale) / 2.0;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty),
        &mut pixmap.as_mut(),
    );
    // tiny-skia pixels are premultiplied; unmultiply into straight alpha, then scale
    // alpha by the covermark's opacity (kept out of the SVG so a slider drives it).
    let opacity = cm.opacity.clamp(0.0, 1.0);
    let mut rgba = RgbaImage::new(w, h);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        let a = (c.alpha() as f32 * opacity).round() as u8;
        *dst = ::image::Rgba([c.red(), c.green(), c.blue(), a]);
    }
    Some(rgba)
}

/// Alpha-blend `overlay` onto `base` centered (straight alpha, src-over).
fn composite_centered(base: &mut RgbaImage, overlay: &RgbaImage) {
    let (bw, bh) = base.dimensions();
    let (ow, oh) = overlay.dimensions();
    let x0 = (bw.saturating_sub(ow)) / 2;
    let y0 = (bh.saturating_sub(oh)) / 2;
    for (ox, oy, &::image::Rgba([r, g, b, a])) in overlay.enumerate_pixels() {
        if a == 0 {
            continue;
        }
        let (bx, by) = (x0 + ox, y0 + oy);
        if bx >= bw || by >= bh {
            continue;
        }
        let dst = base.get_pixel_mut(bx, by);
        let af = a as u32;
        for (d, s) in dst.0.iter_mut().take(3).zip([r, g, b]) {
            *d = ((s as u32 * af + *d as u32 * (255 - af)) / 255) as u8;
        }
    }
}

/// Composite `cm` onto decoded pixels (shared by the image display recomposite and
/// the image bake). No-op when `cm` is `None`.
pub fn apply_covermark(base: &mut RgbaImage, cm: Option<&Covermark>) {
    if let Some(cm) = cm {
        let (w, h) = base.dimensions();
        if let Some(overlay) = rasterize(cm, w, h) {
            composite_centered(base, &overlay);
        }
    }
}

/// Bake the pending edits onto an image, reading `src` and writing the result to `dst`
/// (they may be the same path for an in-place Save, or differ so Copy can produce an
/// edited file WITHOUT touching the saved original). Returns `dst`'s size. At least one
/// of a covermark or a non-empty annotation scene must hold (the image edits).
///
/// Compositing order (DRAGON-330 true-layer stack) — display and bake share the ONE core:
/// 0. the global DIM (DRAGON-329) darkens the base at the very bottom, punched out by the
///    knockout rects (spotlight / box / highlight / box-highlight) via
///    [`super::annotate::apply_dim`] — a no-op when `dim == 0`;
/// 1. the region EFFECTS (highlight / pixelate / blur) composite in true scene z-order via
///    [`super::annotate::apply_effects`], each reading the content accumulated below it;
/// 2. the covermark (privacy mark) as a source-over overlay;
/// 3. the box/arrow annotation scene ON TOP (the active markup, above the privacy mark) —
///    all at full source resolution, position-aware.
pub fn bake_image(
    src: &Path,
    dst: &Path,
    cm: Option<&Covermark>,
    annotations: &[AnnotationItem],
    curve: f32,
    dim: f32,
) -> std::io::Result<u64> {
    let err = |e: String| std::io::Error::other(e);
    let dst_png = super::ext_of(dst).as_deref() == Some("png");
    if cm.is_some() || !annotations.is_empty() || dim > 0.0 {
        let mut rgba = ::image::open(src).map_err(|e| err(e.to_string()))?.into_rgba8();
        // The PRISTINE full-res source, used ONLY to size the content-aware pixelate cell — the
        // SAME analysis source the GPU display uses (its retained base pixels), so display + bake
        // pick identical blocks. Cloned only when a pixelate item is present (otherwise unused).
        let analysis = annotations
            .iter()
            .any(|it| matches!(it.kind, super::annotate::AnnotKind::Pixelate { .. }))
            .then(|| rgba.clone());
        // The global dim darkens the base FIRST (the hard floor), punched out inside the
        // knockout rects; a `dim == 0` bake is byte-identical (apply_dim returns early).
        let knockouts = super::annotate::knockout_rects(annotations);
        super::annotate::apply_dim(&mut rgba, dim, &knockouts, curve);
        // The region effects composite in true z-order — the SAME `apply_effects` core the
        // real-time GPU display shader mirrors (DRAGON-330), so what the user saw is what saves.
        // An effect-free scene is a no-op, keeping the covermark-only / annotation-only bakes
        // byte-identical. The pixelate cell size reads the pristine `analysis` (pre-dim) source;
        // with no pixelate item `analysis` is None and a 1×1 placeholder is passed but never read.
        let placeholder = ::image::RgbaImage::new(1, 1);
        let analysis_ref = analysis.as_ref().unwrap_or(&placeholder);
        super::annotate::apply_effects(&mut rgba, analysis_ref, annotations, curve);
        apply_covermark(&mut rgba, cm);
        super::annotate::apply_annotations(&mut rgba, annotations, curve);
        if dst_png {
            rgba.save_with_format(dst, ::image::ImageFormat::Png).map_err(|e| err(e.to_string()))?;
        } else {
            // Encode PNG to a temp, then transcode to dst's own format (extension stays
            // truthful for a non-PNG external target).
            let tmp = dst.with_extension("baking.tmp.png");
            rgba.save_with_format(&tmp, ::image::ImageFormat::Png).map_err(|e| err(e.to_string()))?;
            let decoded = ::image::open(&tmp).map_err(|e| err(e.to_string()))?;
            decoded.save(dst).map_err(|e| err(e.to_string()))?;
            let _ = std::fs::remove_file(&tmp);
        }
    } else if src != dst {
        // No pixel edit but a distinct dst (Copy): start from a copy.
        std::fs::copy(src, dst)?;
    }
    std::fs::metadata(dst).map(|m| m.len())
}

/// What a video bake works from: the probed pixel size (for the covermark
/// raster), whether the file has a soundtrack (the cut filtergraph must know),
/// and the timeline's kept spans WHEN content was deleted (`None` = uncut, so
/// the historical no-timeline paths — and their exact ffmpeg invocations —
/// still run).
pub struct VideoBake {
    pub w: u32,
    pub h: u32,
    pub has_audio: bool,
    pub keep: Option<Vec<Span>>,
}

/// The `-filter_complex` graph exporting kept spans: per-span `trim`/`atrim`
/// chains re-stamped to zero, concatenated, with the covermark overlaid on the
/// joined video when present. Labels `[v]` (and `[a]` when `has_audio`) are
/// what the caller maps.
fn cut_filtergraph(keep: &[Span], has_audio: bool, overlay: bool) -> String {
    let mut graph = String::new();
    for (i, s) in keep.iter().enumerate() {
        graph.push_str(&format!(
            "[0:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS[v{i}];",
            s.start, s.end
        ));
    }
    for i in 0..keep.len() {
        graph.push_str(&format!("[v{i}]"));
    }
    let vout = if overlay { "[vc]" } else { "[v]" };
    graph.push_str(&format!("concat=n={}:v=1:a=0{vout}", keep.len()));
    if overlay {
        graph.push_str(";[vc][1:v]overlay=(W-w)/2:(H-h)/2[v]");
    }
    if has_audio {
        for (i, s) in keep.iter().enumerate() {
            graph.push_str(&format!(
                ";[0:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS[a{i}]",
                s.start, s.end
            ));
        }
        graph.push(';');
        for i in 0..keep.len() {
            graph.push_str(&format!("[a{i}]"));
        }
        graph.push_str(&format!("concat=n={}:v=0:a=1[a]", keep.len()));
    }
    graph
}

/// Bake the pending edits onto a video, reading `src` and writing `dst`. Deleted
/// timeline segments export through a `trim`+`concat` filtergraph (video re-encoded,
/// audio re-encoded once); a covermark overlays the (joined) video; with neither,
/// the streams are copied (fast). Either `cm.is_some()` or `video.keep.is_some()`
/// must hold.
pub fn bake_video(src: &Path, dst: &Path, cm: Option<&Covermark>, video: &VideoBake) -> std::io::Result<u64> {
    let err = |e: String| std::io::Error::other(e);
    let dir = PathBuf::from(crate::util::runtime_dir());
    // Rasterize the covermark (if any) up front; remember the temp PNG to clean up.
    let overlay_png = match cm {
        Some(cm) => {
            let overlay = rasterize(cm, video.w.max(1), video.h.max(1))
                .ok_or_else(|| err("covermark rasterize failed".into()))?;
            let p = dir.join("cck-cm.png");
            overlay.save_with_format(&p, ::image::ImageFormat::Png).map_err(|e| err(e.to_string()))?;
            Some(p)
        }
        None => None,
    };
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-y", "-v", "error", "-i"]).arg(src);
    if let Some(p) = &overlay_png {
        cmd.arg("-i").arg(p);
    }
    let ext = super::ext_of(dst).unwrap_or_else(|| "mp4".into());
    let tmp = dir.join(format!("cck-bake.{ext}"));
    let reencode: [&str; 8] =
        ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"];
    if let Some(keep) = video.keep.as_deref().filter(|k| !k.is_empty()) {
        // Timeline export: keep only the spans, hard-cut seams. Both streams
        // re-encode — trim points are arbitrary, so stream-copy can't hold them.
        let graph = cut_filtergraph(keep, video.has_audio, overlay_png.is_some());
        cmd.args(["-filter_complex", &graph]).args(["-map", "[v]"]);
        if video.has_audio {
            cmd.args(["-map", "[a]"]);
        }
        cmd.args(reencode);
        if video.has_audio {
            cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        }
    } else if overlay_png.is_some() {
        cmd.args(["-filter_complex", "[0:v][1:v]overlay=(W-w)/2:(H-h)/2[v]"])
            .args(["-map", "[v]", "-map", "0:a?"])
            .args(reencode)
            .args(["-c:a", "copy"]);
    } else {
        // No edit to bake (defensive): copy every stream, no re-encode.
        cmd.args(["-map", "0", "-c", "copy"]);
    }
    if ext == "mp4" || ext == "m4v" || ext == "mov" {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(&tmp);
    let out = cmd.output()?;
    if let Some(p) = &overlay_png {
        let _ = std::fs::remove_file(p);
    }
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    // Move the encoded result into place (copy+remove across filesystems).
    if std::fs::rename(&tmp, dst).is_err() {
        std::fs::copy(&tmp, dst)?;
        let _ = std::fs::remove_file(&tmp);
    }
    std::fs::metadata(dst).map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flyout_nav_wraps_and_starts_from_no_highlight() {
        let mut f = FlyoutNav { kind: FlyoutKind::Color, selected: None, len: 4 };
        f.nav(1);
        assert_eq!(f.selected, Some(0), "None + forward → first");
        f.nav(-1);
        assert_eq!(f.selected, Some(3), "0 - 1 wraps to last");
        f.nav(1);
        assert_eq!(f.selected, Some(0), "last + 1 wraps to first");
        // Covermark uses the same nav; None - 1 → last.
        let mut g = FlyoutNav { kind: FlyoutKind::Covermark, selected: None, len: 3 };
        g.nav(-1);
        assert_eq!(g.selected, Some(2));
        // Empty is a no-op (never a divide/panic).
        let mut h = FlyoutNav { kind: FlyoutKind::Color, selected: None, len: 0 };
        h.nav(1);
        assert_eq!(h.selected, None);
    }

    #[test]
    fn covermark_raster_size_scales_with_zoom_capped_at_frame() {
        // DRAGON-324: the covermark display raster grows with the view zoom (crisp when
        // magnified) but never past the source frame.
        let mut e = EditState { frame: (4000, 2000), ..Default::default() };
        let base = e.preview_raster_size();
        assert_eq!(base, (1024, 512), "≤1024 box preserving aspect");
        // At or below fit zoom, the baseline resolution.
        assert_eq!(e.covermark_raster_size(1.0), base);
        assert_eq!(e.covermark_raster_size(0.5), base, "zoom-out never shrinks below baseline");
        // Zooming in grows proportionally...
        assert_eq!(e.covermark_raster_size(2.0), (2048, 1024));
        // ...capped at the full source frame at high zoom.
        assert_eq!(e.covermark_raster_size(100.0), (4000, 2000));
        // A zero/unknown frame falls back to a sane default (never a 0-size raster).
        e.frame = (0, 0);
        let (w, h) = e.covermark_raster_size(3.0);
        assert!(w > 0 && h > 0);
    }

    fn flat(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, ::image::Rgba([v, v, v, 255]))
    }

    #[test]
    fn confidential_rasterizes_and_composites() {
        let cm = Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 };
        let Some(overlay) = rasterize(&cm, 400, 300) else {
            panic!("confidential covermark failed to rasterize");
        };
        assert_eq!(overlay.dimensions(), (400, 300));
        assert!(overlay.pixels().any(|p| p.0[3] > 0), "covermark rendered fully transparent");
        let mut base = flat(500, 400, 200);
        let before = base.clone();
        apply_covermark(&mut base, Some(&cm));
        assert_ne!(base, before, "composite left the base unchanged");
    }

    #[test]
    fn text_covermark_renders_configured_text() {
        let cm = Covermark { kind: CovermarkKind::Text("SECRET".into()), zoom: 0.0, opacity: 1.0 };
        assert!(rasterize(&cm, 300, 200).is_some(), "text covermark failed to rasterize");
        // Empty text falls back to the default prompt string, still valid SVG.
        let empty = Covermark { kind: CovermarkKind::Text("   ".into()), zoom: 0.0, opacity: 1.0 };
        assert!(rasterize(&empty, 300, 200).is_some());
    }

    #[test]
    fn zoom_still_covers_the_whole_frame() {
        // A zoomed covermark must still produce a full-frame raster (fill invariant).
        let mut cm = Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 };
        cm.zoom = 2.5;
        let overlay = rasterize(&cm, 320, 240).expect("zoomed rasterize");
        assert_eq!(overlay.dimensions(), (320, 240));
    }

    #[test]
    fn undo_redo_track_covermark_history() {
        let mut edit = EditState::default();
        assert!(!edit.can_undo() && !edit.can_redo());
        edit.set_covermark(Some(Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 }));
        assert!(edit.dirty() && edit.can_undo() && !edit.can_redo());
        assert_eq!(edit.undo(None), Some(EditKind::Covermark));
        assert!(!edit.dirty() && edit.can_redo());
        assert_eq!(edit.redo(None), Some(EditKind::Covermark));
        assert!(edit.dirty());
        assert_eq!(edit.redo(None), None, "redo with an empty stack must be a no-op");
        // Zoom adjusts in place without adding history.
        let undo_depth = edit.undo_stack.len();
        edit.set_zoom(1.5);
        assert_eq!(edit.zoom(), 1.5);
        assert_eq!(edit.undo_stack.len(), undo_depth, "zoom must not push undo history");
    }

    #[test]
    fn undo_redo_interleave_covermark_and_timeline_ops() {
        let mut edit = EditState::default();
        let mut tl = Timeline::new(10.0);
        // Edit sequence: covermark on → cut at 4 → delete the tail segment.
        edit.set_covermark(Some(Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 }));
        let prev = tl.spans.clone();
        assert!(tl.cut_at_source(4.0));
        edit.push_timeline(prev);
        let prev = tl.spans.clone();
        assert!(tl.delete(1));
        edit.push_timeline(prev);
        assert!(tl.edited());
        // Undo walks newest-first: delete, then cut, then covermark.
        assert_eq!(
            edit.undo(Some(&mut tl)),
            Some(EditKind::Timeline),
            "timeline undo must not report a covermark change"
        );
        assert_eq!(tl.spans.len(), 2);
        assert!(!tl.edited(), "undoing the delete restores the content");
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Timeline));
        assert_eq!(tl.spans.len(), 1, "undoing the cut re-joins the spans");
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Covermark), "covermark undo reports the change");
        assert!(!edit.dirty());
        // Redo replays in order: covermark, cut, delete.
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Covermark));
        assert!(edit.dirty());
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Timeline));
        assert_eq!(tl.spans.len(), 2);
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Timeline));
        assert!(tl.edited());
        assert!(!edit.can_redo());
        // A fresh timeline edit clears redo, like a fresh covermark choice.
        // (This undo pops the timeline delete — a timeline change, not covermark.)
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Timeline));
        assert!(edit.can_redo());
        edit.push_timeline(tl.spans.clone());
        assert!(!edit.can_redo());
    }

    #[test]
    fn dim_is_dirty_and_joins_the_shared_undo_history() {
        // DRAGON-329: a non-zero global dim is a bakeable edit; its slider commits ONE undo
        // entry that interleaves with the rest of the shared history.
        let mut edit = EditState::default();
        assert!(!edit.dirty(), "a fresh editor with dim 0 is clean");
        // A drag: pre-value latched, value moved, ONE entry pushed on commit.
        edit.dim = 0.5;
        edit.push_dim(0.0);
        assert!(edit.dirty(), "a non-zero dim needs a bake");
        assert!(edit.can_undo());
        // Undo restores the pre-drag dim and reports a Dim change (no raster owed).
        assert_eq!(edit.undo(None), Some(EditKind::Dim));
        assert_eq!(edit.dim, 0.0);
        assert!(!edit.dirty());
        // Redo replays it.
        assert_eq!(edit.redo(None), Some(EditKind::Dim));
        assert_eq!(edit.dim, 0.5);
        // A fresh edit clears the redo stack, like any other op.
        edit.push_dim(0.5);
        assert!(!edit.can_redo());
    }

    #[test]
    fn ensure_dim_for_spotlights_seeds_only_when_a_spotlight_exists() {
        use crate::app::preview::annotate::{AnnotId, AnnotKind, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let mut edit = EditState::default();
        // Nothing on the canvas → no-op.
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.0);
        // A non-spotlight annotation → still no dim.
        edit.annotations.push(AnnotationItem {
            id: AnnotId(1),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect, stroke_w: 4.0, fill: None },
        });
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.0, "a box doesn't dim");
        // A spotlight with no dim → seed 0.7, as one undo entry.
        edit.annotations.push(AnnotationItem {
            id: AnnotId(2),
            color: [0, 0, 0, 255],
            kind: AnnotKind::Spotlight { rect },
        });
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.7, "a spotlight seeds the dim");
        assert!(edit.can_undo(), "the seed is undoable");
        // Idempotent once dimmed — never fights a dim the user has already set.
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.7);
    }

    #[test]
    fn cut_filtergraph_trims_and_concats_both_streams() {
        let keep = [Span { start: 0.0, end: 2.5 }, Span { start: 5.0, end: 10.0 }];
        let g = cut_filtergraph(&keep, true, false);
        assert_eq!(
            g,
            "[0:v]trim=start=0.000:end=2.500,setpts=PTS-STARTPTS[v0];\
             [0:v]trim=start=5.000:end=10.000,setpts=PTS-STARTPTS[v1];\
             [v0][v1]concat=n=2:v=1:a=0[v];\
             [0:a]atrim=start=0.000:end=2.500,asetpts=PTS-STARTPTS[a0];\
             [0:a]atrim=start=5.000:end=10.000,asetpts=PTS-STARTPTS[a1];\
             [a0][a1]concat=n=2:v=0:a=1[a]"
        );
    }

    #[test]
    fn cut_filtergraph_overlays_the_covermark_after_the_join() {
        let keep = [Span { start: 1.0, end: 3.0 }];
        let g = cut_filtergraph(&keep, false, true);
        assert_eq!(
            g,
            "[0:v]trim=start=1.000:end=3.000,setpts=PTS-STARTPTS[v0];\
             [v0]concat=n=1:v=1:a=0[vc];\
             [vc][1:v]overlay=(W-w)/2:(H-h)/2[v]"
        );
        assert!(!g.contains("[a]"), "no audio chain for a silent recording");
    }
}
