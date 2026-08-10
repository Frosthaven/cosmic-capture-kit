//! Text annotations (DRAGON-354): the EMBEDDED-font layout, greedy word-wrap, caret
//! geometry, pure edit ops, and resvg rasterization shared by BOTH the live preview text
//! layer and the full-resolution bake.
//!
//! # Why embedded fonts + rasterize-once
//! The annotation scene is rendered twice — live (the preview text layer) and at bake
//! (`rasterize_scene`) — and the two MUST agree pixel-for-pixel (the same parity contract
//! the sequence badge dodged with hand-authored polylines, `crate::badge`). Real text can't
//! be hand-authored, so instead BOTH paths call the ONE renderer here: the line breaks are
//! OURS (each display line is pre-broken and handed to resvg as its own `<text>`, so resvg
//! never re-wraps), and the glyphs come from two EMBEDDED font files fed to a private
//! `fontdb` — never a system-font lookup — so a caption renders identically on Linux, macOS
//! and Windows. Two families ship: the handwritten Excalifont (from Excalidraw, OFL) and the
//! clean Inter (the cosmic UI family, OFL); both licenses sit beside the `.ttf` in
//! `res/fonts/`.
//!
//! # Fallback for non-Latin glyphs (DRAGON-354 item 18c)
//! The two embedded faces stay PRIMARY — every `<text>` run names them, so Latin captions are
//! byte-identical on every OS. System fonts are additionally loaded into the private db as
//! FALLBACK ONLY: resvg consults them purely for codepoints the named face lacks (CJK, emoji,
//! other scripts), where a per-platform fallback beats tofu. The cost is that a missing-glyph
//! caption can rasterize differently per OS — an accepted, documented relaxation of the
//! original no-system-fonts determinism rule, scoped to the fallback path. Color emoji depend
//! on resvg's COLR/bitmap support AND a color-capable emoji font being installed; where either
//! is absent the glyph renders monochrome or as tofu rather than in color.
//!
//! # Measurement is SHAPED, and the SVG is emitted per face run (DRAGON-360)
//! resvg shapes every `<text>` run, so a multi-codepoint grapheme cluster (a ZWJ family, a
//! skin-tone modifier, a regional-indicator flag) becomes ONE ligature glyph with ONE advance.
//! Measuring by summing per-codepoint advances counted every member of the sequence and the
//! caret overshot; [`shape`] now measures each cluster through the SAME shaper (`rustybuzz`)
//! and the SAME fallback ladder the private [`fontdb`] establishes. The same ladder also decides
//! the SVG: [`block_svg`] emits one `<text>` per maximal same-face run at an explicit `x`, so an
//! emoji run NAMES the embedded Twemoji family and is shaped by it. That second half is not
//! cosmetic — usvg's own fallback gives up when a mixed line's fallback shaping produces a
//! different glyph count (exactly what a ligature does), which used to rasterize `a👍🏽b` as `a`,
//! a tofu box and an overlapping `b`. An all-primary line still emits the historical single
//! `<text x="0">`, so ordinary captions are unchanged. See `text_shape.rs`'s module doc.
//!
//! # The pure core
//! Everything the in-canvas editor needs is a pure function of `(text, caret, size, font,
//! wrap width)`: [`layout`] (greedy wrap + long-word break), [`caret_geometry`],
//! [`caret_at_point`], and the string edit ops ([`insert`]/[`backspace`]/… /[`move_up`]).
//! They are unit-tested with a mock advance so the wrap algorithm is verified without a font,
//! and with the real embedded face so the metrics path is exercised too. Sizes are in SOURCE
//! (image) pixels, exactly like the stroke width and badge side, so text zooms locked to the
//! picture and the bake composites it at full resolution.

use resvg::tiny_skia::Pixmap;
use std::sync::{Arc, OnceLock};

/// The shaper (DRAGON-360) — this module's private measurement engine, mounted from the sibling
/// FILE `text_shape.rs` so it needs no entry in `preview/mod.rs`. It is a CHILD of this module
/// on purpose: it reads the embedded face bytes and [`TextFont`] straight from here, and nothing
/// outside text annotation has any business shaping.
#[path = "text_shape.rs"]
mod shape;

/// The handwritten Excalifont face (Excalidraw, OFL-1.1 — see `res/fonts/Excalifont-LICENSE.txt`).
const EXCALIFONT: &[u8] = include_bytes!("../../../res/fonts/Excalifont-Regular.ttf");
/// The clean Inter face (the cosmic UI family, OFL-1.1 — see `res/fonts/Inter-LICENSE.txt`).
const INTER: &[u8] = include_bytes!("../../../res/fonts/Inter-Regular.ttf");
/// The COLOR emoji fallback face: Twemoji (Mozilla COLRv0 build — Apache-2.0 code + CC-BY-4.0
/// artwork, see `res/fonts/Twemoji-LICENSE.md`), embedded (DRAGON-354 item 10). It is a `COLR`
/// v0 + `CPAL` face, which usvg 0.45's `fontdb.colr()` path flattens to vector color layers — so
/// emoji render in FULL COLOUR and, being embedded + vector, render IDENTICALLY on
/// Linux/macOS/Windows (a system emoji font would differ per OS, and macOS's Apple Color Emoji is
/// `sbix` bitmap whose rasterization is platform-shaped). This replaced the earlier monochrome
/// Noto Emoji plan once resvg's COLR path was verified to rasterize non-empty colour through our
/// pipeline (see the render test). Full-COLRv1 gradients stay out of scope; COLRv0 flat layers
/// cover the emoji set.
const EMOJI: &[u8] = include_bytes!("../../../res/fonts/TwemojiMozilla-COLR.ttf");

/// The embedded UI-font faces as `(family name, raw bytes)` — the SAME bytes the rasterizer's
/// private fontdb uses (no second copy). Registered with the iced/libcosmic font system at
/// startup so the font dropdown can preview each family's label in its OWN face (DRAGON-354
/// item 19). The family names match the faces' own name tables, so `Font::with_name` resolves.
pub const UI_FONT_FACES: [(&str, &[u8]); 2] = [("Excalifont", EXCALIFONT), ("Inter", INTER)];

/// The web-standard text-size scale in logical POINTS (DRAGON-383) the dropdown offers,
/// small → large. [`DEFAULT_TEXT_SIZE`] (32pt) is the mid default. The upper reach continues the
/// scale's own doubling cadence (…48, 64, 96, 128) so the top entry lands exactly on 128 (DRAGON-354
/// item 8). A preset is a POINT measure so a "32px" box reads the same on a 1x and a 2x capture;
/// the box's stored `size_px` is the scaled SOURCE-px value. Identity on an unscaled (1x) output.
pub const TEXT_SIZES: [f32; 10] = [12.0, 14.0, 16.0, 18.0, 24.0, 32.0, 48.0, 64.0, 96.0, 128.0];

/// The default text size in logical POINTS (DRAGON-383) a fresh editor starts at — the
/// unset-persistence fallback (DRAGON-354 item 11a: a fresh install lands on 32 + the Hand face,
/// `TextFont`'s own `#[default]`). A preset in [`TEXT_SIZES`].
pub const DEFAULT_TEXT_SIZE: f32 = 32.0;

/// The SMALLEST type size (SOURCE px) a handle drag can scale text down to — a sixth of the
/// dropdown's smallest preset. Small enough to be fine print on any capture; what it actually
/// guards is the scale reaching zero or going negative (which would invert the glyphs).
pub const TEXT_SCALE_MIN_PX: f32 = 2.0;

/// The LARGEST type size (SOURCE px) a handle drag can scale text up to — sixteen times the
/// dropdown's largest preset and taller than a 4K capture is high, so it is unreachable in
/// practice on a real picture.
///
/// WHY a bound at all, when the owner's instruction is that scaling "should exceed beyond the
/// min/max of the options in dropdown" (DRAGON-367): the presets are quick picks, not the allowed
/// range, and the clamp that pinned scaling to their span is gone. But `size_px` feeds the text
/// layer's padded region (see [`super::annotate::TEXT_INK_OVERHANG_EM`]), and a size that ran
/// away with a fast drag — or a NaN out of a degenerate scale factor — has no rasterizable region
/// at all. It is purely the "no limit" cannot mean "no guard" ceiling.
pub const TEXT_SCALE_MAX_PX: f32 = 2048.0;

/// `size` (SOURCE px) held inside the guard bounds — the WHOLE of what a handle drag does to a
/// normal text box's type size (DRAGON-368).
///
/// DRAGON-367 snapped this to a discrete ladder so a resize read as deliberate steps; the owner
/// asked for that to go ("we need to remove snapping from the sizer"), so scaling is now
/// CONTINUOUS and the type tracks the pointer the way the box does. The ladder's two ends survive
/// as [`TEXT_SCALE_MIN_PX`]/[`TEXT_SCALE_MAX_PX`]; the rungs between them are gone.
///
/// Snapping was also, accidentally, a performance win: two nearby motion events usually landed on
/// the SAME rung, so the whole text kind came out bit-identical and DRAGON-367's reuse fast path
/// took it for free. Continuous scaling removes that, which is why DRAGON-368 had to make the
/// fast path handle a genuine SCALE (see [`super::annotate::text_layer_xform`]) rather than only
/// a translation — otherwise removing the snap would have made every resize event re-render.
///
/// A non-finite or non-positive size degrades to the floor rather than propagating. Pure —
/// unit-tested.
pub fn text_scale_clamp(size: f32) -> f32 {
    if !(size.is_finite() && size > 0.0) {
        return TEXT_SCALE_MIN_PX;
    }
    size.clamp(TEXT_SCALE_MIN_PX, TEXT_SCALE_MAX_PX)
}

/// Which [`TEXT_SIZES`] row the dropdown should highlight for `size` — `None` when the size is
/// OFF-preset (DRAGON-367: handle scaling can now land between or beyond the listed entries).
///
/// Deliberately NOT [`text_size_nearest_index`]: highlighting the nearest row for a 192px box
/// would claim it is 128px, and the closed chip beside it says 192px. Nothing highlighted is the
/// honest answer, and the flyout's keyboard nav already starts from "no highlight". Pure.
pub fn text_size_preset_index(size: f32) -> Option<usize> {
    TEXT_SIZES.iter().position(|&s| (s - size).abs() < 0.5)
}

/// A hard cap on an AUTO (click-placed) box's wrap width as a fraction of the run before it
/// is forced to wrap — used only when the caller has no frame-derived cap. Kept generous so
/// short captions never wrap unexpectedly.
pub const AUTO_WRAP_FALLBACK: f32 = 4096.0;

/// How much a pencil px ABOVE the hairline floor thickens text, as a fraction of the font em
/// per px (DRAGON-358). Small on purpose: the outline is an ADDED weight, not a second glyph.
const TEXT_STROKE_EM_PER_PX: f32 = 0.012;

/// The pencil width (SOURCE px) at/below which text renders with NO outline at all — a plain
/// filled glyph (DRAGON-358). One pencil unit is a hairline; a fresh 1px width in the extended
/// scale (DRAGON-357) lands here.
const TEXT_STROKE_HAIRLINE_W: f32 = 1.0;

/// The hard cap on the outline weight as a fraction of the font em (DRAGON-358): even the
/// widest pencil never paints an outline thicker than this, so a big width THICKENS text
/// (bolder) instead of swallowing the counters into an illegible blob.
const TEXT_STROKE_MAX_EM_FRAC: f32 = 0.09;

/// The text-outline weight as a FRACTION of the font em for a given pencil line width
/// (SOURCE px) — the pure core of [`text_stroke_width`] (DRAGON-358). A hairline pencil
/// ([`TEXT_STROKE_HAIRLINE_W`] or less) maps to `0.0` (no outline); above it the fraction grows
/// linearly at [`TEXT_STROKE_EM_PER_PX`] per px, clamped to [`TEXT_STROKE_MAX_EM_FRAC`]. Keyed
/// off the px VALUE (never a fixed set of presets), so the extended width scale (DRAGON-357)
/// needs no change here. Pure — unit-tested.
pub fn text_stroke_em_frac(pencil_w: f32) -> f32 {
    ((pencil_w - TEXT_STROKE_HAIRLINE_W).max(0.0) * TEXT_STROKE_EM_PER_PX).min(TEXT_STROKE_MAX_EM_FRAC)
}

/// The text-outline STROKE width (SOURCE px) the annotation paints around its glyphs, for a
/// pencil line width `pencil_w` (SOURCE px) at font `size` (SOURCE px) — DRAGON-358, so the
/// active line-width choice thickens text the way it thickens a box/arrow stroke. Em-relative
/// ([`text_stroke_em_frac`] × `size`) so a wide pencil reads as the same relative weight on a
/// 12px caption and a 64px one; `0.0` means "no outline". Painted UNDER the fill (paint-order),
/// so it never eats the glyph interior and the layout advances stay untouched — which is what
/// keeps the stored geometry, the live raster and the bake pixel-identical. Pure — unit-tested.
pub fn text_stroke_width(pencil_w: f32, size: f32) -> f32 {
    text_stroke_em_frac(pencil_w) * size.max(0.0)
}

/// Which embedded family a text annotation uses. The default is the handwritten Excalifont
/// (the tool's personality); the clean Inter is the offered alternative.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TextFont {
    /// Excalifont — the handwritten look.
    #[default]
    Hand,
    /// Inter — a clean non-handwritten sans.
    Clean,
}

impl TextFont {
    /// The fontdb family name (matches the face's own name table, so the private db resolves
    /// it exactly).
    pub fn family(self) -> &'static str {
        match self {
            TextFont::Hand => "Excalifont",
            TextFont::Clean => "Inter",
        }
    }

    /// The raw embedded face bytes (for the metrics face + the fontdb).
    fn bytes(self) -> &'static [u8] {
        match self {
            TextFont::Hand => EXCALIFONT,
            TextFont::Clean => INTER,
        }
    }

    /// The persistence token.
    pub fn as_str(self) -> &'static str {
        match self {
            TextFont::Hand => "hand",
            TextFont::Clean => "clean",
        }
    }

    /// Parse a persisted token; unknown → `None`.
    pub fn from_str(s: &str) -> Option<TextFont> {
        match s {
            "hand" => Some(TextFont::Hand),
            "clean" => Some(TextFont::Clean),
            _ => None,
        }
    }

    /// A short user-facing label (DRAGON-354 item 5): exactly "Hand" / "Clean".
    pub fn label(self) -> &'static str {
        match self {
            TextFont::Hand => "Hand",
            TextFont::Clean => "Clean",
        }
    }
}

/// The private fontdb holding ONLY our two embedded faces — a process-wide `OnceLock` so the
/// faces parse once. Never loads system fonts (determinism across platforms).
fn fontdb() -> &'static Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        // Fallback PRIORITY is db insertion order (usvg picks the first loaded face that covers a
        // missing codepoint), so the order here IS the policy (DRAGON-354 item 10): the two
        // embedded TEXT faces first (they stay PRIMARY — every `<text>` run names them), then the
        // embedded monochrome emoji face, and system fonts LAST.
        db.load_font_data(EXCALIFONT.to_vec());
        db.load_font_data(INTER.to_vec());
        // The EMBEDDED COLOR emoji face (DRAGON-354 item 10) — before any system font, so a
        // typed/pasted emoji resolves to Twemoji's COLR layers and renders in FULL COLOUR the SAME
        // on every OS, instead of tofu (or a per-OS system emoji face). usvg's `fontdb.colr()`
        // path flattens the COLRv0 glyph to vector layers; no bitmap/sbix dependency, so the
        // result is deterministic cross-platform.
        db.load_font_data(EMOJI.to_vec());
        // System fonts as the LAST-RESORT fallback (DRAGON-354 item 18c): our `<text>` runs always
        // name "Excalifont" / "Inter", so the embedded faces stay primary and Latin captions render
        // deterministically cross-platform; the emoji face above owns emoji. resvg only reaches
        // system fonts for codepoints NONE of the embedded faces cover (CJK and other scripts),
        // where a per-platform face beats tofu — an accepted, documented relaxation scoped to that
        // path. NOTE: the pure wrap/metrics tests never touch this db (they use the embedded faces
        // via `face()`), and the emoji-coverage test loads the embedded bytes directly, so both
        // stay VM-safe on a fontless Linux box (the covermark-test gotcha).
        db.load_system_fonts();
        Arc::new(db)
    })
}

/// A parsed metrics face for `font`, cached per family so wrap measuring doesn't re-parse.
fn face(font: TextFont) -> &'static Metrics {
    static HAND: OnceLock<Metrics> = OnceLock::new();
    static CLEAN: OnceLock<Metrics> = OnceLock::new();
    let slot = match font {
        TextFont::Hand => &HAND,
        TextFont::Clean => &CLEAN,
    };
    slot.get_or_init(|| Metrics::parse(font.bytes()))
}

/// The advance-width + vertical metrics of ONE face, in font units, plus `units_per_em` —
/// enough to lay out and wrap without re-parsing on every keystroke.
struct Metrics {
    /// Owns the face's advance/vertical metrics lookups. `'static` bytes, so the face borrows
    /// the embedded slice for the whole process.
    face: ttf_parser::Face<'static>,
}

impl Metrics {
    fn parse(bytes: &'static [u8]) -> Metrics {
        let face = ttf_parser::Face::parse(bytes, 0).expect("embedded font face parses");
        Metrics { face }
    }

    fn units_per_em(&self) -> f32 {
        self.face.units_per_em() as f32
    }

    /// The natural line height in SOURCE px at `size`: ascender - descender + line gap.
    fn line_height(&self, size: f32) -> f32 {
        let upm = self.units_per_em().max(1.0);
        let a = self.face.ascender() as f32;
        let d = self.face.descender() as f32;
        let g = self.face.line_gap() as f32;
        ((a - d + g) * size / upm).max(size)
    }

    /// The baseline offset from the line top in SOURCE px at `size` (the ascender).
    fn ascent(&self, size: f32) -> f32 {
        let upm = self.units_per_em().max(1.0);
        (self.face.ascender() as f32) * size / upm
    }
}

/// Measure a whole `run` in SOURCE px at `size` — the sum of its GRAPHEME CLUSTERS' SHAPED
/// advances (DRAGON-360).
///
/// Each cluster is shaped by [`shape`] through the same `rustybuzz` the renderer uses, resolved
/// through the same fallback ladder the private [`fontdb`] establishes (named text face →
/// embedded Twemoji → the per-codepoint approximation for what only system fonts cover). That is
/// what keeps the caret, selection rects and wrap aligned with the RENDERED glyphs: a
/// multi-codepoint sequence — a ZWJ family, a skin-tone modifier, a flag — is ONE ligature glyph
/// with ONE advance, where the old per-codepoint sum charged every member of the sequence.
///
/// Clusters are summed independently (no CROSS-cluster kerning), which keeps measurement
/// additive at cluster boundaries — [`caret_x_in_line`] measures line PREFIXES — and keeps the
/// result an upper bound on the shaped width, so a line accepted by wrap never overflows once
/// resvg kerns it in.
pub fn measure(font: TextFont, size: f32, run: &str) -> f32 {
    shape::measure_px(font, size, run)
}

/// One display line: its visible text and the CHAR range `[start, end)` it spans in the full
/// string. `end` is exclusive and INCLUDES a consumed soft-wrap space or hard newline (so the
/// caret can sit on it); `text` is the visible run with that separator trimmed.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// How many trailing SPACES were trimmed from `text` that still occupy visual width on
    /// this line (a soft-wrap space, or spaces typed before a hard newline / at the buffer
    /// end). The caret geometry adds their width so the caret visibly advances after a space
    /// (DRAGON-354 item 2) instead of freezing until the next glyph lands.
    pub trail: usize,
}

/// A laid-out text block: its display lines plus the metrics the caret + box geometry ride.
/// All fields SOURCE px, relative to the box top-left.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<TextLine>,
    /// Line height (line advance) in source px.
    pub line_h: f32,
    /// Baseline offset from a line's top in source px.
    pub ascent: f32,
    /// Widest visible line (source px), floored so an empty box is still grabbable.
    pub box_w: f32,
    /// Total height (source px) = line count * line_h.
    pub box_h: f32,
}

/// The smallest a text box may be (source px) so an empty/short caption is still selectable.
pub const MIN_BOX_W: f32 = 12.0;

/// Lay out `text` at `size` in `font`. `wrap_w = Some(w)` wraps within a FIXED width `w` (the
/// drag box); `None` auto-sizes, wrapping only when a line would exceed `auto_cap` (the click
/// box, whose cap is the PICTURE's width — see `annotate::text_auto_cap`). Greedy word wrap; a
/// word longer than the width is broken mid-word so it can never overflow. Pure.
pub fn layout(text: &str, font: TextFont, size: f32, wrap_w: Option<f32>, auto_cap: f32) -> TextLayout {
    let m = face(font);
    let line_h = m.line_height(size);
    let ascent = m.ascent(size);
    let max_w = wrap_w.unwrap_or(auto_cap).max(size); // never below one glyph's room
    let lines = wrap_with(&|run| measure(font, size, run), text, max_w);
    // A line's VISUAL width includes any trailing spaces trimmed from its text: they occupy
    // room and the caret can sit past them (item 2), so an auto box hugs them too.
    let space_adv = measure(font, size, " ");
    let box_w = lines
        .iter()
        .map(|l| measure(font, size, &l.text) + l.trail as f32 * space_adv)
        .fold(0.0_f32, f32::max)
        .max(MIN_BOX_W);
    // A constrained (drag) box keeps its dragged width; an auto box hugs its widest line.
    let box_w = match wrap_w {
        Some(w) => w.max(MIN_BOX_W),
        None => box_w,
    };
    let box_h = (lines.len().max(1) as f32) * line_h;
    TextLayout { lines, line_h, ascent, box_w, box_h }
}

/// How much a run may EXCEED the wrap width and still be called a fit, relative to that width
/// (DRAGON-379). One part in 100,000 — at a 1200px picture, 0.012px.
///
/// # Why a fit test needs any slack at all
///
/// Because the wrap width is routinely the SAME MEASUREMENT it is compared against, carried
/// through arithmetic. Both scaling paths do it deliberately: DRAGON-370 scales a paragraph box's
/// prison by the type factor, and DRAGON-378's auto cap floors at the box's own width — which for
/// a click-created box IS the widest laid-out line. So the predicate routinely evaluates
/// `measure(text, s·k) <= measure(text, s)·k`, whose two sides are one number computed two ways.
///
/// [`super::text_shape::measure_px`] is exactly linear in the size (`Σ em_adv × size`), so the
/// two sides agree to a rounding step — and a rounding step is enough to flip the decision.
/// Measured on the bug: a caption scaled through a drag, cap vs measured line width
/// `1369.843628` vs `1369.843750` — ONE f32 ulp — which wrapped the caption for that one frame
/// and unwrapped it on the next, the "quickly alternates between word wrapping and just allowing
/// text to go beyond the frame" the owner saw. Without slack the whole gesture rides the knife
/// edge, so the flicker is not a corner case there but the norm.
///
/// The value has margin at both ends and cannot change any real wrap decision: it is ~100× the
/// f32 round-trip error at any width, and ~10× SMALLER than the narrowest advance the smallest
/// permitted type ([`TEXT_SCALE_MIN_PX`], 2px) can produce — so no glyph, and no space, can ever
/// squeeze into the slack.
pub const WRAP_FIT_SLACK_REL: f32 = 1e-5;

/// The width a run must EXCEED to be wrapped: `max_w` plus [`WRAP_FIT_SLACK_REL`] of it.
fn wrap_fit_limit(max_w: f32) -> f32 {
    max_w + max_w.abs() * WRAP_FIT_SLACK_REL
}

/// The greedy wrap algorithm, factored on an injectable `advance` measurer so it is verified
/// without a font. Splits on hard newlines FIRST (each paragraph wraps independently; a
/// trailing newline yields a trailing empty line), then greedily packs words, breaking a word
/// wider than `max_w` mid-word. A run counts as fitting until it passes `max_w` by more than
/// [`WRAP_FIT_SLACK_REL`] — see there for why an exact comparison flickers. Char ranges index
/// the FULL string. Pure.
pub fn wrap_with(measure_run: &dyn Fn(&str) -> f32, text: &str, max_w: f32) -> Vec<TextLine> {
    // EVERY fit test below — the mid-word break here, and `place_word`'s — is against the
    // slackened width, so the two can never disagree about what fits.
    let max_w = wrap_fit_limit(max_w);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Grapheme-boundary map (item 18b): `is_boundary[k]` is true when char index k begins a
    // new grapheme cluster (or is the end). The mid-word hard-break may only land on a
    // boundary, so a wrap can never cut through an emoji ZWJ sequence / combining accent.
    let is_boundary: Vec<bool> = {
        use unicode_segmentation::UnicodeSegmentation;
        let mut v = vec![false; n + 1];
        let mut idx = 0usize;
        for g in text.graphemes(true) {
            v[idx] = true;
            idx += g.chars().count();
        }
        if idx <= n {
            v[idx] = true;
        }
        v
    };
    let mut lines: Vec<TextLine> = Vec::new();
    // Paragraph boundaries: char index just past each '\n'. We walk char by char building the
    // current visual line, flushing on wrap and on newline.
    let mut i = 0usize;
    // The start (char idx) of the current visual line.
    let mut line_start = 0usize;
    // Chars accumulated on the current visual line (excludes a pending word not yet committed).
    let mut cur: Vec<char> = Vec::new();
    // The in-progress word (since the last space) and its start index.
    let mut word: Vec<char> = Vec::new();
    let mut word_start = 0usize;

    let width = |cs: &[char]| -> f32 {
        let s: String = cs.iter().collect();
        measure_run(&s)
    };

    // Flush the current visual line as a TextLine spanning [line_start, end).
    let push_line =
        |lines: &mut Vec<TextLine>, cur: &[char], line_start: usize, end: usize| {
            let raw: String = cur.iter().collect();
            let text = raw.trim_end().to_string();
            let trail = raw.chars().count() - text.chars().count();
            lines.push(TextLine { text, start: line_start, end, trail });
        };

    while i < n {
        let ch = chars[i];
        if ch == '\n' {
            // Commit any pending word onto the current line, then hard-break.
            cur.append(&mut word);
            push_line(&mut lines, &cur, line_start, i + 1);
            cur.clear();
            i += 1;
            line_start = i;
            word_start = i;
            continue;
        }
        if ch == ' ' {
            // The word ends: try to place it on the current line.
            place_word(&width, max_w, &mut lines, &mut cur, &mut line_start, &mut word, word_start);
            // The space follows the word on whatever line the word landed on.
            cur.push(' ');
            word.clear();
            word_start = i + 1;
            i += 1;
            continue;
        }
        // A normal glyph joins the pending word.
        if word.is_empty() {
            word_start = i;
        }
        word.push(ch);
        // A single word wider than the line: hard-break it so it can never overflow — but ONLY
        // at a grapheme boundary (item 18b), so a cluster is never split. A char that continues
        // the previous grapheme (a combining mark / ZWJ member) adds ~no width anyway, so
        // deferring the break to the next boundary never overflows meaningfully.
        if width(&word) > max_w && word.len() > 1 && is_boundary[i] {
            // Move all-but-last onto the current line context and flush.
            let last = word.pop().expect("len > 1");
            place_word(&width, max_w, &mut lines, &mut cur, &mut line_start, &mut word, word_start);
            // Start a new line carrying the last char as the new word.
            if !cur.is_empty() {
                push_line(&mut lines, &cur, line_start, i);
                cur.clear();
                line_start = i;
            }
            word.clear();
            word.push(last);
            word_start = i;
        }
        i += 1;
    }
    // The trailing word goes through the SAME wrap check as every other (it may not fit the
    // current line either), then the final line is flushed.
    place_word(&width, max_w, &mut lines, &mut cur, &mut line_start, &mut word, word_start);
    push_line(&mut lines, &cur, line_start, n);
    lines
}

/// Place the pending `word` onto the current line, wrapping to a new line first if it would
/// not fit (and the line already has content). Shared by the space and end-of-text paths.
/// `max_w` arrives ALREADY slackened by [`wrap_fit_limit`] — never slacken it twice.
fn place_word(
    width: &dyn Fn(&[char]) -> f32,
    max_w: f32,
    lines: &mut Vec<TextLine>,
    cur: &mut Vec<char>,
    line_start: &mut usize,
    word: &mut Vec<char>,
    word_start: usize,
) {
    if word.is_empty() {
        return;
    }
    let mut candidate = cur.clone();
    candidate.extend(word.iter());
    if !cur.is_empty() && width(&candidate) > max_w {
        // Wrap: flush the current line (without this word), start a new one at the word.
        let raw: String = cur.iter().collect();
        let text = raw.trim_end().to_string();
        let trail = raw.chars().count() - text.chars().count();
        lines.push(TextLine { text, start: *line_start, end: word_start, trail });
        cur.clear();
        *line_start = word_start;
    }
    cur.append(word);
}

/// The caret geometry `(x, y_top, height)` in SOURCE px relative to the box top-left, for
/// caret char index `caret` (0..=char_len). Pure.
pub fn caret_geometry(layout: &TextLayout, font: TextFont, size: f32, caret: usize) -> (f32, f32, f32) {
    let (li, _col) = caret_line_col(layout, caret);
    let line = &layout.lines[li];
    let x = caret_x_in_line(line, font, size, caret, layout.box_w);
    let y_top = li as f32 * layout.line_h;
    (x, y_top, layout.line_h)
}

/// The caret's x (SOURCE px, box-relative) for char index `caret` measured WITHIN `line` — the
/// glyphs up to the caret, plus the width of any trailing spaces the caret has passed (item 2),
/// clamped to the box edge. Shared by [`caret_geometry`] and [`selection_rects`]. Pure.
fn caret_x_in_line(line: &TextLine, font: TextFont, size: f32, caret: usize, box_w: f32) -> f32 {
    let vis_len = line.text.chars().count();
    let raw_col = caret.saturating_sub(line.start);
    let visible: String = line.text.chars().take(raw_col.min(vis_len)).collect();
    let mut x = measure(font, size, &visible);
    if raw_col > vis_len {
        let past = (raw_col - vis_len).min(line.trail);
        x += past as f32 * measure(font, size, " ");
    }
    x.min(box_w)
}

/// The highlight rectangles `(x0, y_top, x1, height)` (SOURCE px, box-relative) covering the
/// selected char range `[start, end)` (DRAGON-354 item 12) — one per display line the selection
/// touches. Empty when the range is empty. Pure — unit-tested.
pub fn selection_rects(
    layout: &TextLayout,
    font: TextFont,
    size: f32,
    start: usize,
    end: usize,
) -> Vec<(f32, f32, f32, f32)> {
    let (s, e) = (start.min(end), start.max(end));
    if s == e {
        return Vec::new();
    }
    let mut rects = Vec::new();
    for (li, line) in layout.lines.iter().enumerate() {
        // The line owns char span [line.start, line.end); intersect the selection with it.
        if e <= line.start || s >= line.end {
            continue;
        }
        let a = s.max(line.start);
        let b = e.min(line.end);
        let x0 = caret_x_in_line(line, font, size, a, layout.box_w);
        let x1 = caret_x_in_line(line, font, size, b, layout.box_w);
        if x1 > x0 {
            rects.push((x0, li as f32 * layout.line_h, x1, layout.line_h));
        }
    }
    rects
}

/// Which (line index, column-within-visible-text) the caret falls on. A caret sitting on a
/// consumed soft-wrap space or newline clamps to the END of the visible line it belongs to.
fn caret_line_col(layout: &TextLayout, caret: usize) -> (usize, usize) {
    if layout.lines.is_empty() {
        return (0, 0);
    }
    for (li, line) in layout.lines.iter().enumerate() {
        let is_last = li + 1 == layout.lines.len();
        let within = caret >= line.start && (caret < line.end || (is_last && caret <= line.end));
        if within {
            let vis_len = line.text.chars().count();
            let col = caret.saturating_sub(line.start).min(vis_len);
            return (li, col);
        }
    }
    // Past the end: end of the last line.
    let li = layout.lines.len() - 1;
    (li, layout.lines[li].text.chars().count())
}

/// The caret char index nearest local point `(lx, ly)` (SOURCE px, box-relative) — click to
/// place the caret, and double-click-to-edit lands here. Pure.
pub fn caret_at_point(layout: &TextLayout, font: TextFont, size: f32, lx: f32, ly: f32) -> usize {
    if layout.lines.is_empty() {
        return 0;
    }
    let li = ((ly / layout.line_h).floor().max(0.0) as usize).min(layout.lines.len() - 1);
    let line = &layout.lines[li];
    // Walk GRAPHEME CLUSTERS until the midpoint of one passes lx (DRAGON-360). Clusters, not
    // chars: a multi-codepoint sequence is one shaped glyph, so its members have no individual
    // width to hit-test against — and the caret's legal resting positions are cluster starts
    // anyway (the same boundaries [`prev_grapheme`]/[`next_grapheme`] already move by). For
    // plain text every char IS its own cluster, so the common path is unchanged.
    use unicode_segmentation::UnicodeSegmentation;
    let mut acc = 0.0;
    let mut col = 0usize;
    for g in line.text.graphemes(true) {
        let w = measure(font, size, g);
        if lx < acc + w * 0.5 {
            return line.start + col;
        }
        acc += w;
        col += g.chars().count();
    }
    // Past the last glyph: end of the visible line (not the consumed separator).
    line.start + line.text.chars().count()
}

// ── Pure text edit ops on (text, caret in CHARS) ────────────────────────────────────────
//
// The buffer indexes by CHAR (so the wrap ranges and caret geometry can share one index),
// but every MOVEMENT and DELETION snaps to whole GRAPHEME clusters (DRAGON-354 item 18b):
// emoji ZWJ sequences, skin-tone/flag pairs and combining accents are one user-visible
// character, so backspace removes the whole thing and the caret never lands inside one.

/// The char length of `text`.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The text a key press should INSERT into the editor, or `None` for "insert nothing"
/// (DRAGON-354 item 1 — the pure decision). A PRIMARY-modifier chord (Cmd on macOS, Ctrl
/// elsewhere) is a shortcut, never text; a bare compose press (which produces no text) inserts
/// nothing; otherwise the PRODUCED text is inserted verbatim — this is what makes shifted,
/// dead-key-composed and precomposed characters type correctly (the raw key name does not).
/// Shift and Alt/AltGr do NOT block insertion (they compose real characters). Pure.
pub fn insertable_text(primary_combo: bool, typed: Option<&str>) -> Option<&str> {
    if primary_combo {
        return None;
    }
    let t = typed?;
    (!t.is_empty() && !t.chars().all(char::is_control)).then_some(t)
}

/// The char indices where each grapheme cluster STARTS, plus a final entry equal to
/// [`char_len`]. So `[0, .., n]` — the caret's legal resting positions. Pure.
fn grapheme_char_boundaries(text: &str) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut b = Vec::new();
    let mut idx = 0usize;
    for g in text.graphemes(true) {
        b.push(idx);
        idx += g.chars().count();
    }
    b.push(idx);
    b
}

/// `text` capped at `max_chars` CHARS, truncated at a GRAPHEME boundary so the cut can never
/// split an emoji ZWJ sequence / combining cluster (the paste cap, DRAGON-354). Returns the
/// whole string when it fits. Pure — unit-tested.
pub fn cap_graphemes(text: &str, max_chars: usize) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    let mut chars = 0usize;
    for (byte_idx, g) in text.grapheme_indices(true) {
        let c = g.chars().count();
        if chars + c > max_chars {
            return &text[..byte_idx];
        }
        chars += c;
    }
    text
}

/// The grapheme boundary immediately BEFORE `caret` (caret moved left one grapheme). Pure.
pub fn prev_grapheme(text: &str, caret: usize) -> usize {
    let b = grapheme_char_boundaries(text);
    let caret = caret.min(*b.last().unwrap_or(&0));
    b.iter().rev().copied().find(|&x| x < caret).unwrap_or(0)
}

/// The grapheme boundary immediately AFTER `caret` (caret moved right one grapheme). Pure.
pub fn next_grapheme(text: &str, caret: usize) -> usize {
    let b = grapheme_char_boundaries(text);
    let caret = caret.min(*b.last().unwrap_or(&0));
    b.iter().copied().find(|&x| x > caret).unwrap_or(caret)
}

/// The grapheme-cluster range `[start, end)` (CHAR indices) of the WORD under `caret` — the
/// double-click-to-select-word target (DRAGON-354 item 12). Uses unicode word bounds; a caret
/// on whitespace/punctuation selects that run. Empty text → `(0, 0)`. Pure.
pub fn word_range_at(text: &str, caret: usize) -> (usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    let n = char_len(text);
    let caret = caret.min(n);
    let mut start = 0usize;
    for word in text.split_word_bounds() {
        let wlen = word.chars().count();
        let end = start + wlen;
        // The word covering the caret (caret inside, or at the trailing edge of the last).
        if (caret >= start && caret < end) || (end == n && caret == n && wlen > 0) {
            return (start, end);
        }
        start = end;
    }
    (caret, caret)
}

/// Delete the char range `[start, end)` (order-independent), returning `(text, caret)` with the
/// caret at the range start. The whole-selection delete (typing over / Backspace with a
/// selection). Pure.
pub fn delete_range(text: &str, start: usize, end: usize) -> (String, usize) {
    let mut chars: Vec<char> = text.chars().collect();
    let e = start.max(end).min(chars.len());
    let s = start.min(end).min(e);
    chars.drain(s..e);
    (chars.into_iter().collect(), s)
}

/// Replace the char range `[start, end)` (order-independent) with `ins`, returning
/// `(text, caret)` with the caret after the inserted text. Backs typing-over-a-selection and
/// paste-over-a-selection. Pure.
pub fn replace_range(text: &str, start: usize, end: usize, ins: &str) -> (String, usize) {
    let mut chars: Vec<char> = text.chars().collect();
    let e = start.max(end).min(chars.len());
    let s = start.min(end).min(e);
    let insc: Vec<char> = ins.chars().collect();
    let added = insc.len();
    chars.splice(s..e, insc);
    (chars.into_iter().collect(), s + added)
}

/// Backspace: delete the whole GRAPHEME before the caret (item 18b).
pub fn backspace(text: &str, caret: usize) -> (String, usize) {
    if caret == 0 || text.is_empty() {
        return (text.to_string(), 0);
    }
    let prev = prev_grapheme(text, caret);
    delete_range(text, prev, caret)
}

/// Delete: remove the whole GRAPHEME at the caret (item 18b).
pub fn delete_forward(text: &str, caret: usize) -> (String, usize) {
    let next = next_grapheme(text, caret);
    delete_range(text, caret, next)
}

/// Move the caret one grapheme left (item 18b).
pub fn move_left(text: &str, caret: usize) -> usize {
    prev_grapheme(text, caret)
}

/// Move the caret one grapheme right (item 18b).
pub fn move_right(text: &str, caret: usize) -> usize {
    next_grapheme(text, caret)
}

/// Move the caret to the start of its visible line (Home).
pub fn line_home(layout: &TextLayout, caret: usize) -> usize {
    let (li, _) = caret_line_col(layout, caret);
    layout.lines.get(li).map_or(caret, |l| l.start)
}

/// Move the caret to the end of its visible line (End).
pub fn line_end(layout: &TextLayout, caret: usize) -> usize {
    let (li, _) = caret_line_col(layout, caret);
    layout
        .lines
        .get(li)
        .map_or(caret, |l| l.start + l.text.chars().count())
}

/// Move the caret up one visual line, keeping the horizontal target column. Pure.
pub fn move_up(layout: &TextLayout, font: TextFont, size: f32, caret: usize) -> usize {
    let (li, _) = caret_line_col(layout, caret);
    if li == 0 {
        return caret;
    }
    let (x, _, _) = caret_geometry(layout, font, size, caret);
    caret_at_point(layout, font, size, x, (li as f32 - 0.5) * layout.line_h)
}

/// Move the caret down one visual line, keeping the horizontal target column. Pure.
pub fn move_down(layout: &TextLayout, font: TextFont, size: f32, caret: usize) -> usize {
    let (li, _) = caret_line_col(layout, caret);
    if li + 1 >= layout.lines.len() {
        return caret;
    }
    let (x, _, _) = caret_geometry(layout, font, size, caret);
    caret_at_point(layout, font, size, x, (li as f32 + 1.5) * layout.line_h)
}

// ── Rasterization (shared by the live layer AND the bake) ───────────────────────────────

/// Escape XML-special chars so arbitrary user text can't break the generated SVG.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Build the SVG for a laid-out block: one `<text>` per display line at its baseline, in
/// `font` at `size`, filled with `color`. Left-aligned; the box is `box_w`×`box_h` SOURCE px.
/// `pencil_w` is the active line width (SOURCE px): it maps through [`text_stroke_width`] to an
/// OUTLINE stroke in the SAME ink, painted UNDER the fill (`paint-order="stroke"`), so a wider
/// pencil thickens the glyphs (bolder) without touching the layout advances (DRAGON-358). A
/// hairline pencil yields no stroke and the historical fill-only run.
fn block_svg(layout: &TextLayout, font: TextFont, size: f32, color: [u8; 4], pencil_w: f32) -> String {
    let fill = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
    let opacity = color[3] as f32 / 255.0;
    let stroke_w = text_stroke_width(pencil_w, size);
    // The outline is the same ink as the fill, drawn UNDERNEATH it (paint-order), with round
    // joins so it thickens smoothly instead of spiking at glyph corners. Empty when hairline.
    let stroke_attrs = if stroke_w > 0.0 {
        format!(
            r#" stroke="{fill}" stroke-width="{sw:.2}" stroke-opacity="{op:.3}" stroke-linejoin="round" paint-order="stroke""#,
            fill = fill,
            sw = stroke_w,
            op = opacity,
        )
    } else {
        String::new()
    };
    let mut runs = String::new();
    for (li, line) in layout.lines.iter().enumerate() {
        if line.text.is_empty() {
            continue;
        }
        let baseline = li as f32 * layout.line_h + layout.ascent;
        // One `<text>` per maximal SAME-FACE run at the pen position measurement charged it
        // (DRAGON-360). An all-primary line — every ordinary caption — is one run at x = 0.0,
        // which formats as `x="0"`: the historical single element, unchanged. A run that
        // resolved to the embedded emoji face names Twemoji instead, so usvg shapes it with
        // that face as the PRIMARY one and a ligating sequence renders as the one colour glyph
        // it is (its own fallback path collapses on mixed lines — see the module doc).
        for run in shape::face_runs(font, size, &line.text) {
            runs.push_str(&format!(
                r#"<text x="{x}" y="{y:.2}" font-family="{fam}" font-size="{sz:.2}" fill="{fill}" fill-opacity="{op:.3}"{stroke_attrs} xml:space="preserve">{txt}</text>"#,
                x = run.x,
                y = baseline,
                fam = run.face.family(font),
                sz = size,
                fill = fill,
                op = opacity,
                stroke_attrs = stroke_attrs,
                txt = xml_escape(&run.text),
            ));
        }
    }
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.2}" height="{h:.2}" viewBox="0 0 {w:.2} {h:.2}">{runs}</svg>"#,
        w = layout.box_w.max(1.0),
        h = layout.box_h.max(1.0),
    )
}

/// Render a laid-out text block into `pixmap` with its top-left at `origin` (RASTER px),
/// scaled by `scale` (raster px per source px). Source-over composited (like the covermark
/// raster), so it stacks correctly onto whatever is already in the pixmap. No-op for empty
/// text. `pencil_w` is the active line width (SOURCE px) driving the glyph OUTLINE weight
/// (DRAGON-358; see [`block_svg`]). Used by BOTH the live layer and the bake, so the two are
/// one drawing at two resolutions — the outline included.
#[allow(clippy::too_many_arguments)]
pub fn render_into(
    pixmap: &mut Pixmap,
    layout: &TextLayout,
    font: TextFont,
    size: f32,
    color: [u8; 4],
    pencil_w: f32,
    origin: (f32, f32),
    scale: f32,
) {
    if layout.lines.iter().all(|l| l.text.is_empty()) {
        return;
    }
    let svg = block_svg(layout, font, size, color, pencil_w);
    let opt = resvg::usvg::Options { fontdb: fontdb().clone(), ..Default::default() };
    let Ok(tree) = resvg::usvg::Tree::from_data(svg.as_bytes(), &opt) else {
        return;
    };
    // svg point p (source px) -> (origin + p*scale) raster px.
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale)
        .post_translate(origin.0, origin.1);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock advance: every char is 10 wide, so wrap math is checkable by counting chars —
    /// no font needed. Spaces count too.
    fn mock10(run: &str) -> f32 {
        run.chars().count() as f32 * 10.0
    }

    /// DRAGON-367: the dropdown highlights a row ONLY for a size that really is that preset.
    /// An off-preset size — which handle scaling can now produce, in or out of the listed span —
    /// highlights nothing, because the chip beside the menu is showing the real number and a
    /// highlighted row would contradict it.
    #[test]
    fn only_an_on_preset_size_highlights_a_dropdown_row() {
        assert_eq!(text_size_preset_index(24.0), Some(4));
        assert_eq!(text_size_preset_index(128.0), Some(TEXT_SIZES.len() - 1));
        assert_eq!(text_size_preset_index(96.0), Some(TEXT_SIZES.len() - 2));
        // Between rows, below the smallest and above the largest: no row is the truth.
        assert_eq!(text_size_preset_index(13.5), None);
        assert_eq!(text_size_preset_index(120.0), None);
        assert_eq!(text_size_preset_index(6.0), None, "an extended-ladder rung is not a preset");
        assert_eq!(text_size_preset_index(192.0), None);
        // DRAGON-354 item 11a: the fresh-editor default is 32px, a preset in the scale.
        assert_eq!(DEFAULT_TEXT_SIZE, 32.0);
        assert_eq!(TEXT_SIZES[text_size_preset_index(DEFAULT_TEXT_SIZE).expect("a preset")], 32.0);
        assert_eq!(TextFont::default(), TextFont::Hand);
        // DRAGON-354 item 8: the listed scale still reaches 128px at the top, via a 96px step.
        assert_eq!(*TEXT_SIZES.last().unwrap(), 128.0);
    }

    /// DRAGON-368: handle scaling is CONTINUOUS — the owner asked for the DRAGON-367 ladder to
    /// go ("we need to remove snapping from the sizer"), so the only thing left between the
    /// pointer and `size_px` is the guard bounds. Those still reach past BOTH ends of the listed
    /// presets, which is the DRAGON-367 instruction that survives.
    #[test]
    fn the_sizer_is_continuous_and_only_the_guard_bounds_hold() {
        // Anything inside the guard bounds passes through UNCHANGED — that is what "no snapping"
        // means, and the awkward values are the ones a continuous drag actually produces.
        for s in [2.0f32, 2.5, 12.3, 31.999, 78.4, 191.7, 512.5, 2047.9, 2048.0] {
            assert_eq!(text_scale_clamp(s), s, "{s}px must survive the sizer untouched");
        }
        // The bounds genuinely reach past the dropdown in both directions (compile-time, so this
        // can never silently become a runtime no-op).
        const _: () = assert!(TEXT_SCALE_MIN_PX < TEXT_SIZES[0], "must reach below the smallest");
        const _: () =
            assert!(TEXT_SCALE_MAX_PX > TEXT_SIZES[TEXT_SIZES.len() - 1], "must reach above");
        // Every listed preset is reachable by a drag (it is inside the bounds).
        for &s in TEXT_SIZES.iter() {
            assert_eq!(text_scale_clamp(s), s, "{s}px is offered but not reachable by a drag");
        }
        // The guard bounds hold against a runaway drag, a zero/negative factor, and a NaN.
        assert_eq!(text_scale_clamp(1.0e9), TEXT_SCALE_MAX_PX);
        assert_eq!(text_scale_clamp(0.001), TEXT_SCALE_MIN_PX);
        assert_eq!(text_scale_clamp(0.0), TEXT_SCALE_MIN_PX);
        assert_eq!(text_scale_clamp(-64.0), TEXT_SCALE_MIN_PX);
        assert_eq!(text_scale_clamp(f32::NAN), TEXT_SCALE_MIN_PX);
        assert_eq!(text_scale_clamp(f32::INFINITY), TEXT_SCALE_MIN_PX, "a degenerate factor floors");
        // Monotone and never quantized: strictly increasing wherever the bounds do not bind.
        let mut prev = 0.0f32;
        for i in 4..2700 {
            let got = text_scale_clamp(i as f32 * 0.75);
            assert!(got > prev || got == TEXT_SCALE_MAX_PX, "the sizer stepped or went back at {i}");
            prev = got;
        }
    }

    /// DRAGON-354 item 1 (the compile-time pin): the family name each [`TextFont`] hands
    /// `Font::with_name` — and the names in [`UI_FONT_FACES`] — MUST equal the family recorded in
    /// the embedded TTF's own `name` table (ttf-parser name ID 1 / the typographic family ID 16).
    /// If a font is ever revved and its family renamed, `Font::with_name` would silently fall back
    /// to the UI font and the dropdown previews would stop rendering in-face — this catches that
    /// at test time instead.
    #[test]
    fn embedded_face_family_names_match_font_with_name() {
        fn family_name(bytes: &[u8]) -> String {
            let face = ttf_parser::Face::parse(bytes, 0).expect("face parses");
            // Prefer the typographic family (ID 16), else the legacy family (ID 1), in any
            // Unicode/English record.
            let mut legacy = None;
            for name in face.names() {
                let Some(s) = name.to_string() else { continue };
                match name.name_id {
                    16 => return s,
                    1 if legacy.is_none() => legacy = Some(s),
                    _ => {}
                }
            }
            legacy.expect("a family name")
        }
        for (declared, bytes) in UI_FONT_FACES {
            assert_eq!(family_name(bytes), declared, "UI_FONT_FACES name mismatch");
        }
        // And the enum's `family()` (what the dropdown actually passes to `Font::with_name`)
        // agrees with the bytes it renders.
        for f in [TextFont::Hand, TextFont::Clean] {
            assert_eq!(family_name(f.bytes()), f.family(), "{f:?} family mismatch");
        }
    }

    #[test]
    fn font_token_roundtrips() {
        for f in [TextFont::Hand, TextFont::Clean] {
            assert_eq!(TextFont::from_str(f.as_str()), Some(f));
        }
        assert_ne!(TextFont::Hand.family(), TextFont::Clean.family());
        assert_eq!(TextFont::from_str("bogus"), None);
    }

    #[test]
    fn wrap_breaks_on_word_boundaries() {
        // width 55 fits five 10-wide chars. "hello world" -> "hello" | "world".
        let lines = wrap_with(&mock10, "hello world", 55.0);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["hello", "world"]);
    }

    #[test]
    fn wrap_keeps_words_together_when_they_fit() {
        // width 200 holds "hello world" (11 chars = 110) on one line.
        let lines = wrap_with(&mock10, "hello world", 200.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello world");
    }

    #[test]
    fn wrap_breaks_a_too_long_word_mid_word() {
        // width 35 -> at most 3 chars. "abcdefgh" breaks into 3+3+2.
        let lines = wrap_with(&mock10, "abcdefgh", 35.0);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["abc", "def", "gh"]);
    }

    #[test]
    fn wrap_honors_hard_newlines_including_a_trailing_one() {
        let lines = wrap_with(&mock10, "a\nb\n", 500.0);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        // "a", "b", then a trailing empty line for the final newline.
        assert_eq!(texts, vec!["a", "b", ""]);
    }

    #[test]
    fn wrap_char_ranges_cover_the_string_and_track_the_consumed_space() {
        let lines = wrap_with(&mock10, "hello world", 55.0);
        assert_eq!((lines[0].start, lines[0].end), (0, 6)); // "hello" + the consumed space
        assert_eq!((lines[1].start, lines[1].end), (6, 11)); // "world"
    }

    /// DRAGON-379: the fit test must not turn on the last rounding step, because the wrap width
    /// is routinely the SAME measurement it is compared against, carried through a multiply — a
    /// scaled paragraph prison (DRAGON-370) or the auto cap's own-width floor (DRAGON-378). The
    /// slack absorbs that round trip and nothing else.
    #[test]
    fn the_wrap_fit_test_absorbs_a_scaled_round_trip_but_no_real_glyph() {
        // A run over the width by less than the slack still fits…
        let just_over = 110.0 / (1.0 + WRAP_FIT_SLACK_REL * 0.5);
        assert_eq!(wrap_with(&mock10, "hello world", just_over).len(), 1, "a hair over must fit");
        // …while a run over by a real amount (here a whole 10-wide glyph) still wraps.
        assert_eq!(wrap_with(&mock10, "hello world", 100.0).len(), 2, "a glyph over must wrap");
        // The real thing, with the real fonts: a caption laid out at `size`, then re-wrapped at
        // `size × k` against its own width × k, must stay ONE line at every k. Without the slack
        // this alternates as k sweeps — measured at 23 flips in 121 steps through the editor.
        let text = "The quick brown fox jumps over the lazy dog";
        for f in [TextFont::Clean, TextFont::Hand] {
            let (size, base) = (55.0f32, measure(f, 55.0, text));
            for step in 0..=120 {
                let k = 1.0 + step as f32 * 10.0 / base;
                let (scaled, cap) = (size * k, base * k);
                let lines = wrap_with(&|run| measure(f, scaled, run), text, cap);
                assert_eq!(lines.len(), 1, "{f:?}: k={k} split the caption at its own width");
            }
        }
    }

    #[test]
    fn wrap_narrower_box_produces_more_lines() {
        let wide = wrap_with(&mock10, "one two three", 200.0);
        let narrow = wrap_with(&mock10, "one two three", 45.0);
        assert!(narrow.len() > wide.len());
    }

    #[test]
    fn edit_ops_are_char_correct_including_unicode() {
        let (t, c) = replace_range("café", 4, 4, "!");
        assert_eq!((t.as_str(), c), ("café!", 5));
        let (t, c) = backspace("café", 4);
        assert_eq!((t.as_str(), c), ("caf", 3));
        let (t, c) = delete_forward("café", 0);
        assert_eq!((t.as_str(), c), ("afé", 0));
        assert_eq!(move_left("café", 0), 0);
        assert_eq!(move_right("café", 4), 4);
        assert_eq!(move_right("café", 2), 3);
    }

    #[test]
    fn insert_at_caret_middle() {
        let (t, c) = replace_range("ac", 1, 1, "b");
        assert_eq!((t.as_str(), c), ("abc", 2));
    }

    #[test]
    fn insertable_text_decides_by_primary_combo_only() {
        // DRAGON-354 item 1: a shifted letter (arrives as produced text "A") inserts; a Cmd/Ctrl
        // chord never inserts; a bare compose press (no produced text) inserts nothing.
        assert_eq!(insertable_text(false, Some("A")), Some("A"));
        assert_eq!(insertable_text(false, Some("!")), Some("!"));
        assert_eq!(insertable_text(false, Some("é")), Some("é")); // dead-key composed
        assert_eq!(insertable_text(true, Some("c")), None); // Cmd/Ctrl+C is a shortcut, not text
        assert_eq!(insertable_text(false, None), None); // compose press, no text yet
        assert_eq!(insertable_text(false, Some("\u{1b}")), None); // a control char is not text
    }

    #[test]
    fn caret_advances_after_a_trailing_space() {
        // DRAGON-354 item 2: after typing a space the caret must sit PAST the last glyph by a
        // space width, not frozen at the glyph — an auto box hugs the trailing space too.
        let f = TextFont::Clean;
        let lay = layout("hi ", f, 24.0, None, AUTO_WRAP_FALLBACK);
        let (x_glyph, _, _) = caret_geometry(&lay, f, 24.0, 2); // caret after "hi"
        let (x_space, _, _) = caret_geometry(&lay, f, 24.0, 3); // caret after "hi "
        let space = measure(f, 24.0, " ");
        assert!(x_space > x_glyph, "caret advances past the trailing space");
        assert!((x_space - x_glyph - space).abs() < 0.6, "by one space width");
        assert!(x_space <= lay.box_w + 0.01, "and stays within the (space-inclusive) box");
    }

    #[test]
    fn backspace_deletes_a_whole_zwj_emoji_grapheme() {
        // DRAGON-354 item 18b: 👨‍👩‍👧 is one grapheme spanning several codepoints; one backspace
        // removes the WHOLE cluster, not a stray joiner/codepoint.
        let family = "👨‍👩‍👧";
        let n = char_len(family);
        let (t, c) = backspace(family, n);
        assert_eq!(t, "", "the whole family emoji is gone in one backspace");
        assert_eq!(c, 0);
    }

    #[test]
    fn caret_moves_by_grapheme_over_a_combining_accent() {
        // "e" + combining acute (U+0301) is one grapheme (2 chars). move_left/right skip it whole.
        let s = "a\u{0301}b"; // á (decomposed) + b, char_len 3
        assert_eq!(char_len(s), 3);
        assert_eq!(move_right(s, 0), 2, "past the whole á cluster");
        assert_eq!(move_left(s, 2), 0, "back over the whole á cluster");
        let (t, c) = backspace(s, 2);
        assert_eq!((t.as_str(), c), ("b", 0), "backspace removes base+mark together");
    }

    #[test]
    fn wrap_never_splits_a_grapheme_cluster() {
        // A run of combining-accent clusters, at a width that forces a break: no line may start
        // or end mid-cluster (every line boundary is a grapheme boundary).
        let text = "a\u{0301}a\u{0301}a\u{0301}a\u{0301}";
        // Mock: each CHAR is 10 wide, so a 2-char cluster is 20; width 25 fits one cluster.
        let lines = wrap_with(&mock10, text, 25.0);
        for l in &lines {
            // Each visible line is a whole number of "á" clusters — even char count, and the
            // combining mark never leads a line.
            assert!(!l.text.starts_with('\u{0301}'), "a line never starts with a combining mark");
        }
        // And the concatenated line texts reproduce the input clusters (nothing dropped/split).
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn selection_rects_span_the_selected_glyphs_per_line() {
        let f = TextFont::Clean;
        // Two hard lines so the selection crosses a line boundary.
        let lay = layout("abcd\nefgh", f, 24.0, None, AUTO_WRAP_FALLBACK);
        // Select from mid-line-0 (char 2) through mid-line-1 (char 7).
        let rects = selection_rects(&lay, f, 24.0, 2, 7);
        assert_eq!(rects.len(), 2, "one rect per touched line");
        // Line 0 rect starts at the width of "ab" and runs to the line end ("abcd").
        let want_x0 = measure(f, 24.0, "ab");
        assert!((rects[0].0 - want_x0).abs() < 0.5);
        assert!(rects[0].2 > rects[0].0, "non-empty span on line 0");
        // Line 1 rect starts at x 0 (line start) — its y is one line down.
        assert!((rects[1].0 - 0.0).abs() < 0.5);
        assert!((rects[1].1 - lay.line_h).abs() < 0.01);
        // An empty range yields nothing.
        assert!(selection_rects(&lay, f, 24.0, 3, 3).is_empty());
    }

    #[test]
    fn cap_graphemes_truncates_on_cluster_boundaries_only() {
        // Fits: returned whole.
        assert_eq!(cap_graphemes("hello", 10), "hello");
        assert_eq!(cap_graphemes("hello", 5), "hello");
        // Plain ASCII: cut at the char cap.
        assert_eq!(cap_graphemes("hello", 3), "hel");
        // A ZWJ emoji (several chars, ONE grapheme) is never split: a cap that lands inside
        // the cluster drops the whole cluster.
        let family = "👨‍👩‍👧"; // 5 chars, 1 grapheme
        let s = format!("ab{family}cd");
        assert_eq!(cap_graphemes(&s, 3), "ab", "a cap inside the cluster drops it whole");
        assert_eq!(cap_graphemes(&s, 7), format!("ab{family}"), "the whole cluster fits at 7");
        assert_eq!(cap_graphemes("", 4), "");
    }

    /// DRAGON-572: paste-shaped insertion ([`replace_range`], at a caret and over a
    /// selection) is char-correct for emoji — a single-codepoint emoji, a multi-codepoint
    /// ZWJ sequence, and mixed ASCII+emoji all land whole, with the caret after the
    /// insertion. This is the pure half of the editor's paste, on both read routes.
    #[test]
    fn paste_insertion_is_char_correct_for_emoji() {
        // Single codepoint: 😀 is one char, one grapheme.
        let (t, c) = replace_range("ab", 1, 1, "😀");
        assert_eq!((t.as_str(), c), ("a😀b", 2));
        // A ZWJ sequence (👨‍👩‍👧 = 5 chars, ONE grapheme) pasted over a selection: the
        // selection goes, the whole cluster lands, the caret sits after it.
        let family = "👨‍👩‍👧";
        assert_eq!(char_len(family), 5);
        let (t, c) = replace_range("hello", 1, 4, family);
        assert_eq!(t, format!("h{family}o"));
        assert_eq!(c, 6, "caret = selection start + the pasted char count");
        // Mixed ASCII+emoji, order-independent range, caret at the end of the insertion.
        let (t, c) = replace_range("xy", 1, 0, "a😀b");
        assert_eq!((t.as_str(), c), ("a😀by", 3));
        // The paste cap never bites a paste-sized emoji string (it bounds pathology, not use).
        let mixed = format!("a😀{family}b");
        assert_eq!(cap_graphemes(&mixed, 32 * 1024), mixed.as_str());
    }

    /// DRAGON-572: caret math around pasted emoji moves by whole grapheme — over the
    /// single-codepoint kind, the ZWJ kind, and their ASCII neighbours in mixed text.
    #[test]
    fn caret_math_steps_over_pasted_emoji_whole() {
        let family = "👨‍👩‍👧"; // 5 chars, one grapheme
        let s = format!("a😀{family}b"); // char indices: a=0, 😀=1, family=2..7, b=7
        assert_eq!(char_len(&s), 8);
        // Rightward: a → 😀 → the whole family → b, one grapheme per step.
        assert_eq!(move_right(&s, 0), 1);
        assert_eq!(move_right(&s, 1), 2);
        assert_eq!(move_right(&s, 2), 7, "the whole ZWJ family is one step");
        assert_eq!(move_right(&s, 7), 8);
        // And back left over the same boundaries.
        assert_eq!(move_left(&s, 8), 7);
        assert_eq!(move_left(&s, 7), 2);
        assert_eq!(move_left(&s, 2), 1);
        // Backspace after the family removes the whole cluster, never a stray joiner…
        let (t, c) = backspace(&s, 7);
        assert_eq!((t.as_str(), c), ("a😀b", 2));
        // …and forward-delete at its start is its mirror.
        let (t, c) = delete_forward(&s, 2);
        assert_eq!((t.as_str(), c), ("a😀b", 2));
    }

    /// DRAGON-572: wrap (and so measure, which it drives) treats a pasted emoji cluster as
    /// an unsplittable unit in mixed ASCII+emoji text — a ZWJ family wider than the line
    /// gets its own line rather than a mid-cluster break.
    #[test]
    fn wrap_gives_an_oversize_emoji_cluster_its_own_line() {
        let family = "👨‍👩‍👧"; // 5 chars = 50 wide under mock10 — wider than the 45px line
        let text = format!("aa {family} bb");
        let lines = wrap_with(&mock10, &text, 45.0);
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["aa", family, "bb"],
            "the cluster overflows whole on its own line; its neighbours wrap around it"
        );
        // Nothing was dropped or split: the ranges cover the string end to end.
        assert_eq!(lines.first().map(|l| l.start), Some(0));
        assert_eq!(lines.last().map(|l| l.end), Some(char_len(&text)));
    }

    #[test]
    fn word_range_at_selects_the_word_under_the_caret() {
        // "hello world": a caret inside "world" selects [6, 11).
        let (s, e) = word_range_at("hello world", 8);
        assert_eq!((s, e), (6, 11));
        // A caret inside "hello" selects [0, 5).
        let (s, e) = word_range_at("hello world", 2);
        assert_eq!((s, e), (0, 5));
    }

    /// The real embedded faces parse and give positive, monotone advances + a sane line
    /// height — the metrics path the pure wrap tests mock.
    #[test]
    fn embedded_faces_have_usable_metrics() {
        for f in [TextFont::Hand, TextFont::Clean] {
            let wide = measure(f, 24.0, "WWWW");
            let narrow = measure(f, 24.0, "W");
            assert!(wide > narrow && narrow > 0.0, "advances grow with text ({:?})", f);
            let lay = layout("hi", f, 24.0, None, AUTO_WRAP_FALLBACK);
            assert!(lay.line_h >= 24.0 && lay.ascent > 0.0);
            assert!(lay.box_w >= MIN_BOX_W && lay.box_h >= lay.line_h);
        }
    }

    #[test]
    fn layout_constrained_keeps_its_width_and_wraps() {
        let lay = layout("one two three four", TextFont::Clean, 24.0, Some(80.0), AUTO_WRAP_FALLBACK);
        assert_eq!(lay.box_w, 80.0, "a drag box keeps its dragged width");
        assert!(lay.lines.len() > 1, "and wraps within it");
        assert!((lay.box_h - lay.lines.len() as f32 * lay.line_h).abs() < 0.01);
    }

    #[test]
    fn layout_auto_hugs_its_widest_line() {
        let lay = layout("hello", TextFont::Clean, 24.0, None, AUTO_WRAP_FALLBACK);
        assert_eq!(lay.lines.len(), 1);
        let want = measure(TextFont::Clean, 24.0, "hello");
        assert!((lay.box_w - want).abs() < 0.5, "box hugs the single line width");
    }

    #[test]
    fn caret_geometry_advances_across_a_line_and_down_lines() {
        let lay = layout("ab\ncd", TextFont::Clean, 24.0, None, AUTO_WRAP_FALLBACK);
        let (x0, y0, h) = caret_geometry(&lay, TextFont::Clean, 24.0, 0);
        assert_eq!((x0, y0), (0.0, 0.0));
        assert!(h >= 24.0);
        // Caret after "ab" (index 2) is at the end of line 0.
        let (x2, y2, _) = caret_geometry(&lay, TextFont::Clean, 24.0, 2);
        assert!(x2 > x0 && (y2 - 0.0).abs() < 0.01);
        // Caret at index 3 (start of "cd") drops to line 1, x back to 0.
        let (x3, y3, _) = caret_geometry(&lay, TextFont::Clean, 24.0, 3);
        assert!((x3 - 0.0).abs() < 0.01 && (y3 - lay.line_h).abs() < 0.01);
    }

    #[test]
    fn caret_at_point_round_trips_with_caret_geometry() {
        let lay = layout("hello world foo", TextFont::Clean, 24.0, Some(120.0), AUTO_WRAP_FALLBACK);
        for caret in [0usize, 3, 6, 9] {
            let (x, y_top, _) = caret_geometry(&lay, TextFont::Clean, 24.0, caret);
            let back = caret_at_point(&lay, TextFont::Clean, 24.0, x + 0.1, y_top + 1.0);
            assert_eq!(back, caret, "caret {caret} did not round-trip");
        }
    }

    #[test]
    fn move_up_down_keeps_column() {
        let lay = layout("abcd\nefgh", TextFont::Clean, 24.0, None, AUTO_WRAP_FALLBACK);
        // Caret at index 2 (after "ab" on line 0). Down should land near col 2 on line 1.
        let down = move_down(&lay, TextFont::Clean, 24.0, 2);
        assert_eq!(down, 7, "col 2 of line 1 (start 5) is index 7");
        let up = move_up(&lay, TextFont::Clean, 24.0, down);
        assert_eq!(up, 2, "back up to the original column");
    }

    #[test]
    fn home_and_end_snap_within_the_visible_line() {
        // A box between the widest single word and the whole run: each word fits (so no
        // mid-word break) but not both together, forcing exactly one wrap after the space.
        // Derived from the REAL font, so no hard-coded pixel width.
        let both = measure(TextFont::Clean, 24.0, "hello world");
        let longest =
            measure(TextFont::Clean, 24.0, "hello").max(measure(TextFont::Clean, 24.0, "world"));
        let w = (longest + both) / 2.0;
        let lay = layout("hello world", TextFont::Clean, 24.0, Some(w), AUTO_WRAP_FALLBACK);
        assert_eq!(lay.lines.len(), 2, "wraps into two lines");
        assert_eq!(lay.lines[1].start, 6, "second line is 'world' (start 6)");
        // A caret mid-"world" (index 8) homes to 6, ends to 11.
        assert_eq!(line_home(&lay, 8), 6);
        assert_eq!(line_end(&lay, 8), 11);
    }

    /// The bake/live render path produces non-empty pixels for real text and stays empty for
    /// empty text (the discard-empty contract).
    #[test]
    fn render_into_draws_text_and_skips_empty() {
        let mut pm = Pixmap::new(200, 80).unwrap();
        let empty = layout("", TextFont::Hand, 24.0, None, AUTO_WRAP_FALLBACK);
        render_into(&mut pm, &empty, TextFont::Hand, 24.0, [255, 0, 0, 255], 0.0, (0.0, 0.0), 1.0);
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0), "empty text draws nothing");
        let lay = layout("Hi", TextFont::Hand, 24.0, None, AUTO_WRAP_FALLBACK);
        render_into(&mut pm, &lay, TextFont::Hand, 24.0, [255, 0, 0, 255], 0.0, (0.0, 0.0), 1.0);
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0), "real text draws ink");
    }

    /// DRAGON-354 item 10: the embedded emoji face is a COLOR (`COLR`/`CPAL`) face and covers the
    /// members of a ZWJ emoji sequence. Reads the embedded bytes directly (ttf-parser) —
    /// embedded-only, so it passes on a fontless Linux VM.
    #[test]
    fn embedded_emoji_face_is_color_and_covers_zwj_members() {
        let face = ttf_parser::Face::parse(EMOJI, 0).expect("emoji face parses");
        // A COLR table is what usvg's `fontdb.colr()` path flattens into vector colour layers
        // (ttf-parser only yields `colr` when its companion CPAL palette parsed too).
        assert!(face.tables().colr.is_some(), "the emoji face must carry a COLR colour table");
        // 👨‍👩‍👧 = man (U+1F468) ZWJ woman (U+1F469) ZWJ girl (U+1F467), plus common singletons.
        for cp in ['\u{1F468}', '\u{1F469}', '\u{1F467}', '\u{1F600}', '\u{1F44D}', '\u{2764}'] {
            assert!(
                face.glyph_index(cp).is_some(),
                "emoji face has no glyph for U+{:04X}",
                cp as u32
            );
        }
    }

    /// DRAGON-354 item 10: an emoji in a caption whose run NAMES a text face (which lacks it)
    /// falls back to the EMBEDDED emoji face and rasterizes in COLOUR through our resvg pipeline —
    /// verified with a db holding ONLY the three embedded faces (NO system fonts), so the fallback
    /// is provably embedded and the test is VM-safe on a fontless Linux box. Asserting MULTIPLE
    /// distinct opaque colours proves the COLR colour-layer path ran (not a flat monochrome glyph
    /// or tofu).
    #[test]
    fn emoji_renders_in_color_from_embedded_fallback_without_system_fonts() {
        use resvg::usvg;
        use std::collections::HashSet;
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(EXCALIFONT.to_vec());
        db.load_font_data(INTER.to_vec());
        db.load_font_data(EMOJI.to_vec());
        let opt = usvg::Options { fontdb: Arc::new(db), ..Default::default() };
        // The run names "Excalifont" (no emoji glyphs), so 👍 must fall back to the embedded emoji.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96"><text x="4" y="72" font-family="Excalifont" font-size="64">&#x1F44D;</text></svg>"##;
        let tree = usvg::Tree::from_data(svg.as_bytes(), &opt).expect("emoji svg parses");
        let mut pm = Pixmap::new(96, 96).unwrap();
        resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pm.as_mut());
        // Count distinct colours among FULLY-OPAQUE pixels only, after DEMULTIPLYING: tiny-skia
        // stores PREMULTIPLIED bytes, so a flat monochrome (non-black) glyph's antialiased edge
        // alone would yield many distinct premultiplied (r,g,b) tuples — `alpha == 255` +
        // `demultiply()` removes that confound. Two or more distinct opaque hues can then only
        // come from the COLR colour LAYERS (a flat glyph has exactly one interior colour; tofu
        // has none).
        let colors: HashSet<(u8, u8, u8)> = pm
            .pixels()
            .iter()
            .filter(|p| p.alpha() == 255)
            .map(|p| {
                let c = p.demultiply();
                (c.red(), c.green(), c.blue())
            })
            .collect();
        assert!(!colors.is_empty(), "the emoji fell back to the embedded face and drew ink");
        assert!(
            colors.len() >= 2,
            "the COLR colour path produced multiple distinct opaque hues, not a flat/tofu glyph (saw {})",
            colors.len()
        );
    }

    /// The emoji advance the RENDERER uses: the embedded Twemoji face's own horizontal advance
    /// for `cp`, in SOURCE px at `size`. Read straight from the embedded bytes (ttf-parser), so
    /// the expectation is deterministic and VM-safe — the same value resvg advances the pen by.
    fn emoji_render_advance(cp: char, size: f32) -> f32 {
        let face = ttf_parser::Face::parse(EMOJI, 0).expect("emoji face parses");
        let g = face.glyph_index(cp).expect("emoji face covers the codepoint");
        let units = face.glyph_hor_advance(g).expect("emoji glyph has an advance") as f32;
        units * size / face.units_per_em() as f32
    }

    /// DRAGON-359: an emoji the PRIMARY text face lacks now measures at the EMBEDDED emoji face's
    /// real advance (what the raster uses), NOT the half-em notdef stub — so the caret sits where
    /// the glyph actually ends instead of lagging inside it.
    #[test]
    fn emoji_measures_at_the_embedded_emoji_advance_not_the_notdef_stub() {
        let size = 64.0;
        let thumbs = '\u{1F44D}'; // 👍 — a single-codepoint emoji, no ZWJ shaping
        for f in [TextFont::Hand, TextFont::Clean] {
            // The primary text face has no glyph for the emoji (else there is nothing to fall back).
            assert!(
                face(f).face.glyph_index(thumbs).is_none(),
                "{f:?} unexpectedly carries the emoji glyph"
            );
            let measured = measure(f, size, &thumbs.to_string());
            let rendered = emoji_render_advance(thumbs, size);
            assert!(
                (measured - rendered).abs() < 0.5,
                "{f:?}: emoji measured {measured} must equal the rendered advance {rendered}"
            );
            // And it is genuinely the emoji face, not the old half-em (0.5 * size) approximation.
            assert!(
                (measured - 0.5 * size).abs() > 1.0,
                "{f:?}: emoji width must not be the notdef half-em stub"
            );
        }
    }

    /// DRAGON-359: the caret x after an emoji, and a selection rect drawn over it, span the
    /// emoji's REAL advance — the drift the user reported (caret overlapping wide glyphs) is gone.
    #[test]
    fn caret_and_selection_span_the_real_emoji_width() {
        let f = TextFont::Clean;
        let size = 48.0;
        let emoji = '\u{1F600}'; // 😀
        let line = format!("a{emoji}b");
        let lay = layout(&line, f, size, None, AUTO_WRAP_FALLBACK);
        let a = measure(f, size, "a");
        let emoji_adv = emoji_render_advance(emoji, size);
        // Caret index 1 sits after "a"; index 2 sits after the emoji — one emoji advance further.
        let (x1, _, _) = caret_geometry(&lay, f, size, 1);
        let (x2, _, _) = caret_geometry(&lay, f, size, 2);
        assert!((x1 - a).abs() < 0.5, "caret after 'a' is at the 'a' advance");
        assert!(
            (x2 - x1 - emoji_adv).abs() < 0.6,
            "caret jumps a full emoji advance ({emoji_adv}) past 'a', got {}",
            x2 - x1
        );
        // The selection rect over just the emoji (chars [1, 2)) spans its real advance.
        let rects = selection_rects(&lay, f, size, 1, 2);
        assert_eq!(rects.len(), 1, "one rect on the single line");
        let (rx0, _, rx1, _) = rects[0];
        assert!(
            (rx1 - rx0 - emoji_adv).abs() < 0.6,
            "selection spans the emoji advance ({emoji_adv}), got {}",
            rx1 - rx0
        );
    }

    /// DRAGON-359: wrap uses the emoji's real advance, so a row of emoji wraps where the raster
    /// would overflow — under the old half-em stub the same row was measured too narrow and packed
    /// too many onto a line (visual overrun past the box edge).
    #[test]
    fn wrap_accounts_for_emoji_advances() {
        let f = TextFont::Clean;
        let size = 32.0;
        let emoji = '\u{1F44D}'; // 👍
        let adv = emoji_render_advance(emoji, size);
        // A box wide enough for exactly three emoji at their REAL advance.
        let box_w = adv * 3.5;
        let row: String = std::iter::repeat_n(emoji, 6).collect();
        let lay = layout(&row, f, size, Some(box_w), AUTO_WRAP_FALLBACK);
        // Every visible line must fit the box at the REAL advance (no line packs > 3 emoji).
        for l in &lay.lines {
            let w = measure(f, size, &l.text);
            assert!(w <= box_w + 0.5, "a wrapped line ({w}) overflows the box ({box_w})");
        }
        // Six emoji at 3-per-line wrap into at least two lines (the stub would have fit ~7 wide).
        assert!(lay.lines.len() >= 2, "the emoji row wrapped at the real advance");
    }

    #[test]
    fn text_stroke_maps_pencil_width_to_a_sane_outline() {
        // DRAGON-358: a hairline pencil paints NO outline (plain fill), a wider one thickens it,
        // and the weight is capped so it never becomes a blob. Keyed off the px value, not a set
        // of presets — so the extended width scale (DRAGON-357) works with no change here.
        assert_eq!(text_stroke_em_frac(0.0), 0.0, "sub-hairline: no outline");
        assert_eq!(text_stroke_em_frac(1.0), 0.0, "the hairline pencil: no outline");
        assert!(text_stroke_em_frac(2.0) > 0.0, "above the hairline: an outline appears");
        // Monotone in the pencil width up to the cap.
        assert!(text_stroke_em_frac(6.0) > text_stroke_em_frac(4.0));
        assert!(text_stroke_em_frac(4.0) > text_stroke_em_frac(2.0));
        // Capped: an absurd width never exceeds the em-fraction ceiling.
        assert_eq!(text_stroke_em_frac(1000.0), TEXT_STROKE_MAX_EM_FRAC);
        // The px form is the fraction times the font size, and stays em-relative: the SAME
        // pencil is the same fraction of a 12px and a 64px caption.
        assert!((text_stroke_width(6.0, 24.0) - text_stroke_em_frac(6.0) * 24.0).abs() < 1e-6);
        assert!(
            (text_stroke_width(6.0, 64.0) / 64.0 - text_stroke_width(6.0, 12.0) / 12.0).abs() < 1e-6,
            "the outline is a fixed fraction of the em at any size"
        );
        assert_eq!(text_stroke_width(1.0, 48.0), 0.0, "hairline stays fill-only at any size");
    }

    #[test]
    fn text_outline_thickens_the_inked_area() {
        // A wider pencil must paint MORE ink than the hairline (fill only) for the same caption,
        // proving the outline actually reaches the raster on BOTH the live layer and the bake
        // (they share this one path).
        let lay = layout("Hello", TextFont::Clean, 48.0, None, AUTO_WRAP_FALLBACK);
        let ink = |pencil_w: f32| -> usize {
            let mut pm = Pixmap::new(400, 120).unwrap();
            render_into(&mut pm, &lay, TextFont::Clean, 48.0, [0, 0, 0, 255], pencil_w, (0.0, 0.0), 1.0);
            pm.pixels().iter().filter(|p| p.alpha() > 0).count()
        };
        let hairline = ink(1.0); // no outline
        let bold = ink(6.0); // a heavy pencil
        assert!(hairline > 0, "the fill alone inks pixels");
        assert!(bold > hairline, "a wider pencil thickens the glyphs (more ink)");
    }

    // ── DRAGON-360: shaping-aware measurement ────────────────────────────────────────────

    /// A ZWJ family, a skin-tone modifier and a regional-indicator flag — the three shapes of
    /// multi-codepoint sequence the ticket names. Each is ONE grapheme cluster that shapes to ONE
    /// ligature glyph.
    const SEQUENCES: [(&str, &str); 3] = [
        ("ZWJ family", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"), // 👨‍👩‍👧‍👦
        ("skin tone", "\u{1F44D}\u{1F3FD}"),                                            // 👍🏽
        ("flag", "\u{1F1EF}\u{1F1F5}"),                                                 // 🇯🇵
    ];

    /// What measurement did BEFORE DRAGON-360: the sum of each CODEPOINT's own advance through
    /// the primary → emoji face ladder (DRAGON-359's rule). Read straight from the embedded
    /// bytes, so the comparison is deterministic and VM-safe.
    fn per_codepoint_sum(font: TextFont, size: f32, run: &str) -> f32 {
        let primary = ttf_parser::Face::parse(font.bytes(), 0).expect("text face parses");
        let emoji = ttf_parser::Face::parse(EMOJI, 0).expect("emoji face parses");
        let adv = |f: &ttf_parser::Face, ch: char| -> Option<f32> {
            let u = f.glyph_index(ch).and_then(|g| f.glyph_hor_advance(g))? as f32;
            Some(u * size / f.units_per_em() as f32)
        };
        run.chars()
            .map(|ch| adv(&primary, ch).or_else(|| adv(&emoji, ch)).unwrap_or(0.5 * size))
            .sum()
    }

    /// THE BUG (DRAGON-360): a multi-codepoint sequence renders as ONE shaped glyph, so it must
    /// measure as ONE cluster advance — not as the sum of its members'. Under the old
    /// per-codepoint sum the ZWJ family measured SEVEN advances wide (four emoji + three ZWJ),
    /// so the caret overshot the glyph by ~6 ems.
    #[test]
    fn multi_codepoint_sequences_measure_as_one_shaped_cluster() {
        let size = 64.0;
        for (name, seq) in SEQUENCES {
            // Sanity: each really is a single grapheme cluster spanning several codepoints.
            assert_eq!(char_len(seq), next_grapheme(seq, 0), "{name} is one cluster");
            assert!(char_len(seq) > 1, "{name} spans several codepoints");
            for f in [TextFont::Hand, TextFont::Clean] {
                let shaped = measure(f, size, seq);
                // The embedded Twemoji face advances every emoji glyph a full em.
                assert!(
                    (shaped - size).abs() < 0.5,
                    "{f:?} {name}: shaped width {shaped} must be one glyph advance ({size})"
                );
                // And it is strictly narrower than the pre-fix per-codepoint sum — the bug.
                let naive = per_codepoint_sum(f, size, seq);
                assert!(
                    naive > shaped + 1.0,
                    "{f:?} {name}: the old sum-of-parts ({naive}) must exceed the shaped width \
                     ({shaped}) — otherwise this test proves nothing"
                );
            }
        }
    }

    /// The caret after a sequence, and a selection rect drawn over one, span exactly the shaped
    /// cluster advance — the user-visible symptom (caret overshooting past the glyph) is gone.
    #[test]
    fn caret_and_selection_track_a_shaped_sequence() {
        let f = TextFont::Clean;
        let size = 48.0;
        for (name, seq) in SEQUENCES {
            let n = char_len(seq);
            let line = format!("a{seq}b");
            let lay = layout(&line, f, size, None, AUTO_WRAP_FALLBACK);
            assert_eq!(lay.lines.len(), 1, "{name}: one line");
            let a = measure(f, size, "a");
            let seq_w = measure(f, size, seq);
            // Caret 1 sits after "a"; caret 1+n sits after the whole cluster.
            let (x1, _, _) = caret_geometry(&lay, f, size, 1);
            let (x2, _, _) = caret_geometry(&lay, f, size, 1 + n);
            assert!((x1 - a).abs() < 0.5, "{name}: caret after 'a'");
            assert!(
                (x2 - x1 - seq_w).abs() < 0.6,
                "{name}: caret advances ONE shaped cluster ({seq_w}), got {}",
                x2 - x1
            );
            // The selection over just the cluster spans exactly that advance.
            let rects = selection_rects(&lay, f, size, 1, 1 + n);
            assert_eq!(rects.len(), 1, "{name}: one rect on the single line");
            let (rx0, _, rx1, _) = rects[0];
            assert!((rx0 - a).abs() < 0.5, "{name}: the rect starts at the cluster");
            assert!(
                (rx1 - rx0 - seq_w).abs() < 0.6,
                "{name}: the rect spans the shaped advance ({seq_w}), got {}",
                rx1 - rx0
            );
            // Clicking just past the cluster's midpoint lands the caret AFTER it, never inside.
            let hit = caret_at_point(&lay, f, size, a + seq_w * 0.75, 1.0);
            assert_eq!(hit, 1 + n, "{name}: a click past the cluster midpoint lands after it");
        }
    }

    /// Wrap rides the SAME shaped width, so a box that comfortably fits `aa` + one sequence keeps
    /// them on ONE line — where the old per-codepoint sum measured the sequence several times too
    /// wide and wrapped it away.
    #[test]
    fn wrap_decides_on_the_shaped_sequence_width() {
        let f = TextFont::Clean;
        let size = 32.0;
        for (name, seq) in SEQUENCES {
            let text = format!("aa{seq}");
            let shaped = measure(f, size, &text);
            let naive = per_codepoint_sum(f, size, &text);
            // A box that fits the SHAPED run with room to spare, but is far too narrow for the
            // pre-fix per-codepoint measurement.
            let box_w = shaped + 4.0;
            assert!(naive > box_w, "{name}: the old sum would have forced a wrap");
            let lay = layout(&text, f, size, Some(box_w), AUTO_WRAP_FALLBACK);
            assert_eq!(lay.lines.len(), 1, "{name}: the shaped run fits on one line");
            assert_eq!(lay.lines[0].text, text, "{name}: nothing was broken off");
            // And the converse still holds: a box narrower than the shaped run DOES wrap, with
            // the cluster kept whole on its own line.
            let narrow = layout(&text, f, size, Some(shaped - size * 0.5), AUTO_WRAP_FALLBACK);
            assert!(narrow.lines.len() > 1, "{name}: too narrow still wraps");
            for l in &narrow.lines {
                assert!(
                    l.text.is_empty() || l.text == "aa" || l.text == seq,
                    "{name}: a wrapped line split the cluster ({:?})",
                    l.text
                );
            }
        }
    }

    /// No regression in the common path: plain ASCII still measures EXACTLY what the
    /// per-codepoint advance sum gave before DRAGON-360 (shaping one ASCII char in isolation is
    /// its plain advance), so every existing caption's wrap and caret are bit-stable.
    #[test]
    fn plain_ascii_measures_identically_to_the_pre_shaping_sum() {
        for f in [TextFont::Hand, TextFont::Clean] {
            for size in [12.0f32, 24.0, 32.0, 128.0] {
                for run in [
                    "hello world",
                    "The quick brown fox jumps over the lazy dog.",
                    "AV Ta W. (parens) [brackets] {braces}",
                    " ",
                    "",
                    "i",
                ] {
                    let now = measure(f, size, run);
                    let before = per_codepoint_sum(f, size, run);
                    assert!(
                        (now - before).abs() < 1e-3,
                        "{f:?}@{size} {run:?}: shaped {now} != pre-shaping {before}"
                    );
                }
            }
        }
    }

    /// The parity that makes all of the above meaningful: the pen position the RENDERER advances
    /// to is the number [`measure`] reports. Parses the SVG [`block_svg`] actually emits with a
    /// db holding ONLY the three embedded faces (NO system fonts — VM-safe on a fontless Linux
    /// box) and reads usvg's own laid-out glyphs back.
    ///
    /// It also pins the rendering half of DRAGON-360: the sequence must come back as ONE glyph
    /// with a RESOLVED id. usvg's built-in fallback gives up on a mixed line (its wholesale
    /// re-shape needs the fallback face to cover the entire run, and its per-glyph patch-up bails
    /// when the glyph counts differ — which is exactly what a ligature does), which used to
    /// rasterize `a👍🏽b` as `a`, a `.notdef` box and an OVERLAPPING `b`.
    #[test]
    fn the_renderer_pen_lands_where_measurement_says() {
        use resvg::usvg;
        let f = TextFont::Clean;
        let size = 64.0;
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(EXCALIFONT.to_vec());
        db.load_font_data(INTER.to_vec());
        db.load_font_data(EMOJI.to_vec());
        let opt = usvg::Options { fontdb: Arc::new(db), ..Default::default() };

        /// Every laid-out glyph in the tree as `(pen x, glyph id)`, in document order.
        fn glyphs(g: &usvg::Group, out: &mut Vec<(f32, u16)>) {
            for node in g.children() {
                match node {
                    usvg::Node::Text(t) => {
                        for span in t.layouted() {
                            for pg in &span.positioned_glyphs {
                                out.push((pg.transform().tx, pg.id.0));
                            }
                        }
                    }
                    usvg::Node::Group(inner) => glyphs(inner, out),
                    _ => {}
                }
            }
        }

        for (name, seq) in SEQUENCES {
            let line = format!("a{seq}b");
            let lay = layout(&line, f, size, None, AUTO_WRAP_FALLBACK);
            let svg = block_svg(&lay, f, size, [0, 0, 0, 255], 0.0);
            let tree = usvg::Tree::from_data(svg.as_bytes(), &opt).expect("the block svg parses");
            let mut g = Vec::new();
            glyphs(tree.root(), &mut g);
            // 'a', the ONE ligature glyph for the whole sequence, then 'b'.
            assert_eq!(g.len(), 3, "{name}: the sequence rendered as one glyph, got {g:?}");
            assert!(g.iter().all(|&(_, id)| id != 0), "{name}: a .notdef (tofu) glyph, got {g:?}");
            // And each pen position is exactly what measurement charged for the text before it.
            let a = measure(f, size, "a");
            let seq_w = measure(f, size, seq);
            assert!((g[0].0 - 0.0).abs() < 0.01, "{name}: 'a' starts at the origin");
            assert!(
                (g[1].0 - a).abs() < 0.01,
                "{name}: the sequence's pen ({}) is measure(\"a\") = {a}",
                g[1].0
            );
            assert!(
                (g[2].0 - (a + seq_w)).abs() < 0.01,
                "{name}: 'b' lands at {} but measurement says {}",
                g[2].0,
                a + seq_w
            );
        }
    }

    /// End to end through the ONE renderer both the live text layer and the bake call
    /// ([`render_into`]): a caption mixing Latin text with a ZWJ family paints the Latin ink in
    /// the caption's own colour AND the sequence in Twemoji's colour layers. Before DRAGON-360
    /// the sequence rasterized as a monochrome `.notdef` box with the following letter drawn on
    /// top of it, so BOTH halves of this assertion failed.
    #[test]
    fn a_mixed_caption_rasterizes_the_sequence_in_colour_not_tofu() {
        use std::collections::HashSet;
        let f = TextFont::Clean;
        let size = 64.0;
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let lay = layout(&format!("a{family}b"), f, size, None, AUTO_WRAP_FALLBACK);
        let mut pm = Pixmap::new(lay.box_w.ceil() as u32, lay.box_h.ceil() as u32).unwrap();
        // The caption's ink is pure RED, so any OTHER opaque hue can only come from the emoji's
        // COLR colour layers.
        render_into(&mut pm, &lay, f, size, [255, 0, 0, 255], 0.0, (0.0, 0.0), 1.0);
        let hues: HashSet<(u8, u8, u8)> = pm
            .pixels()
            .iter()
            .filter(|p| p.alpha() == 255)
            .map(|p| {
                let c = p.demultiply();
                (c.red(), c.green(), c.blue())
            })
            .collect();
        assert!(hues.contains(&(255, 0, 0)), "the Latin glyphs painted in the caption ink");
        assert!(
            hues.iter().filter(|&&h| h != (255, 0, 0)).count() >= 2,
            "the emoji painted its own COLR colour layers, not a monochrome tofu box (saw {hues:?})"
        );
        // And the caption box is only as wide as ONE emoji plus the two letters — the old
        // sum-of-parts would have sized it for seven.
        let want = measure(f, size, "a") + size + measure(f, size, "b");
        assert!((lay.box_w - want).abs() < 1.0, "box {} sized for one cluster ({want})", lay.box_w);
    }

    /// The all-primary path — every ordinary caption — still emits the ONE historical
    /// `<text x="0" …>` element per display line: splitting by face must not churn the SVG (or
    /// the raster) for text that never needed a fallback.
    #[test]
    fn an_all_primary_line_still_emits_one_text_element_at_x_zero() {
        let f = TextFont::Clean;
        let lay = layout("hello world\nsecond line", f, 24.0, None, AUTO_WRAP_FALLBACK);
        let svg = block_svg(&lay, f, 24.0, [0, 0, 0, 255], 0.0);
        assert_eq!(svg.matches("<text ").count(), 2, "one <text> per display line");
        assert_eq!(svg.matches(r#"<text x="0" "#).count(), 2, "each at x=\"0\"");
        assert!(svg.contains(r#"font-family="Inter""#));
        assert!(!svg.contains(shape::EMOJI_FAMILY), "no emoji run for plain text");
    }
}

