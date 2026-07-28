//! Shaping-aware advance measurement for text annotations (DRAGON-360) — mounted as
//! `text_annot::shape` (a `#[path]` child of [`super`], so it needs no `preview/mod.rs` entry).
//!
//! # Why a shaper at all
//! [`super`] hands resvg pre-broken display lines and resvg SHAPES each one through
//! `rustybuzz`. Shaping turns a whole multi-codepoint grapheme cluster — a ZWJ family
//! `👨‍👩‍👧‍👦`, a skin-tone modifier `👍🏽`, a regional-indicator flag `🇯🇵` — into ONE ligature
//! glyph carrying ONE advance. Summing per-codepoint `ttf-parser` advances instead counts every
//! member of the sequence (a 4-emoji family measured 7× too wide at 64px), so the caret
//! overshot the glyph and selection rects and wrap decisions were wrong with it. DRAGON-359
//! fixed the *single*-codepoint case (measure through the embedded emoji face instead of a
//! half-em stub); this module fixes the *sequence* case by measuring through the SAME shaper,
//! the SAME faces and the SAME fallback order the renderer uses.
//!
//! # The fallback order, mirrored from the renderer
//! usvg's `shape_text` shapes the run with the PRIMARY face, and if any glyph came back
//! `.notdef` it re-shapes the whole run with the first face in `fontdb` insertion order that
//! covers the missing char, keeping the fallback wholesale when it resolves everything. Our
//! private db (see [`super::fontdb`]) is loaded Excalifont → Inter → Twemoji COLR → system, so
//! for a cluster the ladder is exactly: named text face → embedded Twemoji → (system). We
//! reproduce it per CLUSTER in [`cluster_metric`]: shape with the primary, accept it when
//! nothing is missing, else shape with the embedded emoji face and accept THAT when nothing is
//! missing, else fall back to DRAGON-359's per-codepoint approximation (the only tier we cannot
//! resolve deterministically, because it lands in system fonts).
//!
//! # …and the renderer is made to agree
//! Mirroring the ladder is only half of it: usvg's own fallback path *breaks* when a line MIXES
//! scripts. Its wholesale re-shape is kept only if the fallback face resolves the ENTIRE run,
//! and its per-glyph patch-up bails out whenever the two shapings produce different glyph
//! counts — which is exactly what a ligating emoji sequence does. So `a👍🏽b` used to rasterize
//! as `a`, a tofu box, and an OVERLAPPING `b`. [`face_runs`] closes that: the line is split into
//! maximal same-face runs and [`super::block_svg`] emits one `<text>` per run at an explicit
//! `x`, so the emoji run NAMES "Twemoji Mozilla" and is shaped by it as the primary face. The
//! run split is derived from the same per-cluster ladder as the measurement, so the advance we
//! measure and the pen the renderer advances are the same number by construction — not by
//! coincidence. An all-primary line (every ASCII caption) is ONE run at `x=0`, i.e. the
//! historical single `<text>` element, unchanged.
//!
//! # Cost
//! Measurement runs on every keystroke, every wrap probe and every caret move, so shaping is
//! never on the hot path twice:
//! - Advances are cached EM-RELATIVE (font units / `units_per_em`), so the font SIZE is not part
//!   of the cache key — one entry serves every size.
//! - Pure-ASCII runs (the overwhelmingly common case) skip segmentation and hashing entirely and
//!   sum a precomputed 128-entry table; that path is strictly cheaper than the per-char
//!   `glyph_index` + `glyph_hor_advance` lookups it replaces.
//! - Anything else costs one grapheme segmentation plus one hash lookup per cluster; only a
//!   cluster never seen before is actually shaped.
//! - The caches (parsed `rustybuzz::Face`s, the ASCII tables, the cluster map) are
//!   THREAD-LOCAL, so measuring from the UI thread and from a bake thread never contends on a
//!   lock. The cluster map is capped and cleared wholesale on overflow.

use std::cell::RefCell;
use std::collections::HashMap;

use super::{TextFont, EMOJI, EXCALIFONT, INTER};

/// The family name of the embedded COLR emoji face, as it appears in the face's own `name`
/// table — what an emoji run's `<text font-family>` must say for usvg to resolve it as the
/// PRIMARY face for that run. Pinned by a unit test against the embedded bytes.
pub const EMOJI_FAMILY: &str = "Twemoji Mozilla";

/// Which embedded face shapes a given cluster — the tier the ladder in [`cluster_metric`]
/// stopped at, and therefore which family the rendered run must name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunFace {
    /// The caption's named text face (Excalifont / Inter). Also carries the LAST-RESORT tier:
    /// a cluster no embedded face shapes stays in the primary run so usvg can reach system
    /// fonts for it, exactly as before DRAGON-360.
    Primary,
    /// The embedded Twemoji COLR face.
    Emoji,
}

impl RunFace {
    /// The `font-family` an SVG run in this face must name.
    pub fn family(self, font: TextFont) -> &'static str {
        match self {
            RunFace::Primary => font.family(),
            RunFace::Emoji => EMOJI_FAMILY,
        }
    }
}

/// One grapheme cluster's resolved face plus its advance as a fraction of the font EM — size
/// independent, so `em_adv * size` is the advance in SOURCE px at any size.
#[derive(Clone, Copy, Debug)]
struct ClusterMetric {
    face: RunFace,
    em_adv: f32,
}

/// A maximal stretch of consecutive clusters that resolve to the SAME face, with the pen
/// position it starts at. [`super::block_svg`] emits one `<text>` per run.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceRun {
    /// Which face shapes this run (and therefore which family its `<text>` names).
    pub face: RunFace,
    /// The run's text.
    pub text: String,
    /// The run's pen origin in SOURCE px, relative to the line start — the sum of every
    /// preceding cluster's advance, i.e. the SAME accumulation [`measure_px`] performs.
    pub x: f32,
}

/// The half-em advance charged to a codepoint NO embedded face covers — DRAGON-359's
/// approximation, kept verbatim (the renderer resolves those through system fonts, which
/// measurement cannot consult deterministically). Keeps wrap monotone.
const NOTDEF_EM: f32 = 0.5;

/// How many distinct clusters the per-thread cache holds before it is cleared wholesale. Far
/// above any real caption's distinct-cluster count; the cap only bounds a pathological paste.
const CLUSTER_CACHE_CAP: usize = 4096;

/// The three embedded faces, in the db's own insertion order — the slot index is the cache tag.
const FACES: [&[u8]; 3] = [EXCALIFONT, INTER, EMOJI];

/// The [`FACES`] slot the caption's named text face lives in.
fn primary_slot(font: TextFont) -> usize {
    match font {
        TextFont::Hand => 0,
        TextFont::Clean => 1,
    }
}

/// The [`FACES`] slot of the embedded COLR emoji face.
const EMOJI_SLOT: usize = 2;

/// The per-thread shaping caches. Thread-local (not a `Mutex` static) so the UI thread and any
/// bake thread each measure lock-free; the cost is one parsed face + one ASCII table per thread
/// that actually measures, which is at most a few hundred KB of borrowed-slice state.
#[derive(Default)]
struct Caches {
    /// Parsed shaper faces, lazily filled per [`FACES`] slot.
    faces: [Option<rustybuzz::Face<'static>>; 3],
    /// Per text face, the EM-relative advance of every ASCII codepoint (`None` until built).
    ascii: [Option<Box<[f32; 128]>>; 2],
    /// Per text face, `cluster -> metric` for everything the ASCII table cannot answer. Kept as
    /// one map PER primary slot (rather than one map keyed by a `(slot, cluster)` tuple) so a
    /// lookup borrows the cluster `&str` straight from the caller — a tuple key would allocate
    /// a `Box<str>` on every probe, in the hot path.
    clusters: [HashMap<Box<str>, ClusterMetric>; 2],
}

thread_local! {
    static CACHES: RefCell<Caches> = RefCell::new(Caches::default());
}

impl Caches {
    /// The parsed shaper face for `slot`, parsing it on first use. `None` only if an embedded
    /// face ever failed to parse (it does not — [`super::Metrics::parse`] already asserts that).
    fn face(&mut self, slot: usize) -> Option<&rustybuzz::Face<'static>> {
        if self.faces[slot].is_none() {
            self.faces[slot] = rustybuzz::Face::from_slice(FACES[slot], 0);
        }
        self.faces[slot].as_ref()
    }
}

/// Shape `text` with `face` and return `(advance in EM units, every glyph resolved)`.
///
/// The shaping call mirrors usvg's `shape_text_with_font` for our SVG: left-to-right, and NO
/// features — usvg only pushes `smcp` for small-caps (we never set `font-variant`) and a
/// kerning-OFF feature when `font-kerning: none` (we never set it), so the default feature set
/// is the same one the renderer uses. `.notdef` (glyph id 0) anywhere means this face did not
/// cover the run, which is precisely usvg's `is_missing` fallback trigger.
fn shape_em(face: &rustybuzz::Face<'static>, text: &str) -> (f32, bool) {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    let out = rustybuzz::shape(face, &[], buffer);
    let upm = (face.units_per_em() as f32).max(1.0);
    let mut units = 0i32;
    let mut all_resolved = true;
    let infos = out.glyph_infos();
    for (i, pos) in out.glyph_positions().iter().enumerate() {
        units += pos.x_advance;
        if infos[i].glyph_id == 0 {
            all_resolved = false;
        }
    }
    (units as f32 / upm, all_resolved)
}

/// The DRAGON-359 per-codepoint ladder, in EM units — the LAST-RESORT tier for a cluster
/// neither embedded face shapes cleanly (CJK and other scripts the renderer resolves through
/// system fonts). Unchanged behaviour: primary glyph advance, else the emoji face's, else the
/// half-em stub.
fn per_codepoint_em(c: &mut Caches, primary: usize, cluster: &str) -> f32 {
    let mut em = 0.0;
    for ch in cluster.chars() {
        let mut adv = None;
        for slot in [primary, EMOJI_SLOT] {
            if adv.is_some() {
                break;
            }
            if let Some(face) = c.face(slot) {
                let upm = (face.units_per_em() as f32).max(1.0);
                adv = face
                    .glyph_index(ch)
                    .and_then(|g| face.glyph_hor_advance(g))
                    .map(|u| u as f32 / upm);
            }
        }
        em += adv.unwrap_or(NOTDEF_EM);
    }
    em
}

/// Resolve ONE grapheme cluster: which face shapes it, and its EM-relative advance. This is the
/// mirror of usvg's fallback ladder described in the module doc. Uncached — [`cached_metric`]
/// is the entry point callers use.
fn cluster_metric(c: &mut Caches, primary: usize, cluster: &str) -> ClusterMetric {
    if let Some(face) = c.face(primary) {
        let (em, ok) = shape_em(face, cluster);
        if ok {
            return ClusterMetric { face: RunFace::Primary, em_adv: em };
        }
    }
    if let Some(face) = c.face(EMOJI_SLOT) {
        let (em, ok) = shape_em(face, cluster);
        if ok {
            return ClusterMetric { face: RunFace::Emoji, em_adv: em };
        }
    }
    // Neither embedded face resolves it: keep it in the PRIMARY run (so usvg can still reach a
    // system font for it, as it always could) and charge the per-codepoint approximation.
    ClusterMetric { face: RunFace::Primary, em_adv: per_codepoint_em(c, primary, cluster) }
}

/// [`cluster_metric`] behind the per-thread cache. Only a cluster never measured before on this
/// thread is actually shaped.
fn cached_metric(c: &mut Caches, primary: usize, cluster: &str) -> ClusterMetric {
    if let Some(m) = c.clusters[primary].get(cluster) {
        return *m;
    }
    let m = cluster_metric(c, primary, cluster);
    let map = &mut c.clusters[primary];
    if map.len() >= CLUSTER_CACHE_CAP {
        map.clear();
    }
    map.insert(Box::from(cluster), m);
    m
}

/// The EM-relative advance of every ASCII codepoint in `font`, built once per thread. Every
/// ASCII char is its own grapheme cluster, so this table answers a pure-ASCII run exactly as the
/// general path would — just without segmenting or hashing.
fn ascii_table(c: &mut Caches, font: TextFont) -> &[f32; 128] {
    let primary = primary_slot(font);
    if c.ascii[primary].is_none() {
        let mut t = Box::new([0.0f32; 128]);
        let mut buf = [0u8; 4];
        for (i, slot) in t.iter_mut().enumerate() {
            let ch = char::from(i as u8);
            *slot = cluster_metric(c, primary, ch.encode_utf8(&mut buf)).em_adv;
        }
        c.ascii[primary] = Some(t);
    }
    c.ascii[primary].as_deref().expect("just filled")
}

/// The total advance of `run` in SOURCE px at `size` — the shaping-aware replacement for the
/// old sum of per-codepoint advances.
///
/// Clusters are summed INDEPENDENTLY rather than shaping the whole run in one call, and that is
/// deliberate: the caret and selection geometry measure PREFIXES of a line, so measurement must
/// stay additive at cluster boundaries (`measure(a) + measure(b) == measure(a ++ b)`). The cost
/// is that cross-cluster kerning is not applied — the same documented "upper bound on the shaped
/// width" the per-codepoint sum already was, which keeps a line we accept for wrap from
/// overflowing once resvg kerns it in.
pub fn measure_px(font: TextFont, size: f32, run: &str) -> f32 {
    if run.is_empty() {
        return 0.0;
    }
    CACHES.with(|cell| {
        let c = &mut *cell.borrow_mut();
        // Fast path: a pure-ASCII run is one cluster per byte, answered from the table.
        if run.is_ascii() {
            let t = ascii_table(c, font);
            let em: f32 = run.bytes().map(|b| t[b as usize]).sum();
            return em * size;
        }
        let primary = primary_slot(font);
        let em: f32 = graphemes(run).map(|g| cached_metric(c, primary, g).em_adv).sum();
        em * size
    })
}

/// Split `run` into maximal same-face runs with their pen origins in SOURCE px at `size` — what
/// [`super::block_svg`] emits one `<text>` per, so the renderer shapes each cluster with the
/// face measurement charged it. An all-primary run (every ASCII caption) yields exactly ONE run
/// at `x = 0.0`, i.e. the historical single `<text>` element.
pub fn face_runs(font: TextFont, size: f32, run: &str) -> Vec<FaceRun> {
    if run.is_empty() {
        return Vec::new();
    }
    if run.is_ascii() {
        // Every ASCII cluster resolves to the primary face (the embedded text faces cover ASCII
        // outright), so the split is trivially one run — and this skips segmentation entirely.
        return vec![FaceRun { face: RunFace::Primary, text: run.to_string(), x: 0.0 }];
    }
    CACHES.with(|cell| {
        let c = &mut *cell.borrow_mut();
        let primary = primary_slot(font);
        let mut runs: Vec<FaceRun> = Vec::new();
        // Accumulate in EM and scale at the end of each step, so a run's `x` is bit-for-bit the
        // `measure_px` of everything before it (which also sums EM then scales once).
        let mut em = 0.0f32;
        for g in graphemes(run) {
            let m = cached_metric(c, primary, g);
            match runs.last_mut() {
                Some(last) if last.face == m.face => last.text.push_str(g),
                _ => runs.push(FaceRun { face: m.face, text: g.to_string(), x: em * size }),
            }
            em += m.em_adv;
        }
        runs
    })
}

/// The grapheme clusters of `s` — the ONE segmentation the whole module rides, identical to the
/// one the editor's caret/backspace semantics already use ([`super::grapheme_char_boundaries`]).
/// DRAGON-360 changes only the WIDTH math; cluster boundaries are untouched.
fn graphemes(s: &str) -> impl Iterator<Item = &str> {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emoji family name an emoji `<text>` run names MUST equal the embedded face's own
    /// `name`-table family — otherwise usvg would not resolve the run to Twemoji and the whole
    /// measure/render agreement collapses. Reads the embedded bytes directly, so it is VM-safe
    /// on a fontless Linux box.
    #[test]
    fn emoji_family_matches_the_embedded_face_name_table() {
        let face = ttf_parser::Face::parse(EMOJI, 0).expect("emoji face parses");
        let mut legacy = None;
        let mut typographic = None;
        for name in face.names() {
            let Some(s) = name.to_string() else { continue };
            match name.name_id {
                16 => typographic = Some(s),
                1 if legacy.is_none() => legacy = Some(s),
                _ => {}
            }
        }
        let family = typographic.or(legacy).expect("a family name");
        assert_eq!(family, EMOJI_FAMILY);
    }

    /// The ASCII fast path and the general grapheme path must agree exactly — they are two
    /// implementations of the same sum, and the fast one is only sound while that holds.
    #[test]
    fn ascii_fast_path_equals_the_general_cluster_path() {
        for font in [TextFont::Hand, TextFont::Clean] {
            let general = |run: &str| -> f32 {
                CACHES.with(|cell| {
                    let c = &mut *cell.borrow_mut();
                    let p = primary_slot(font);
                    graphemes(run).map(|g| cached_metric(c, p, g).em_adv).sum::<f32>() * 24.0
                })
            };
            for run in ["hello world", "The quick brown fox!", " ", "a", "()[]{}<>@#$%"] {
                let fast = measure_px(font, 24.0, run);
                assert!(
                    (fast - general(run)).abs() < 1e-3,
                    "{font:?} {run:?}: ascii table {fast} != cluster sum {}",
                    general(run)
                );
            }
        }
    }

    /// What makes [`face_runs`]'s ASCII shortcut sound: EVERY ASCII codepoint resolves to the
    /// PRIMARY face, so a pure-ASCII line is provably one run and never needs segmenting. (The
    /// embedded text faces cover printable ASCII outright, and anything they miss lands in the
    /// last-resort tier — which is also `Primary`, by design.)
    #[test]
    fn every_ascii_codepoint_resolves_to_the_primary_face() {
        CACHES.with(|cell| {
            let c = &mut *cell.borrow_mut();
            for font in [TextFont::Hand, TextFont::Clean] {
                let p = primary_slot(font);
                let mut buf = [0u8; 4];
                for i in 0u8..128 {
                    let s = char::from(i).encode_utf8(&mut buf);
                    assert_eq!(
                        cluster_metric(c, p, s).face,
                        RunFace::Primary,
                        "{font:?}: ASCII U+{i:02X} left the primary face"
                    );
                }
            }
        });
    }

    /// A multi-codepoint sequence resolves to the EMOJI face as ONE shaped cluster, and its
    /// advance is the LIGATURE's — not the sum of its members'.
    #[test]
    fn sequences_resolve_to_one_shaped_emoji_advance() {
        let size = 64.0;
        for seq in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}", // 👨‍👩‍👧‍👦
            "\u{1F44D}\u{1F3FD}",                                          // 👍🏽
            "\u{1F1EF}\u{1F1F5}",                                          // 🇯🇵
        ] {
            for font in [TextFont::Hand, TextFont::Clean] {
                let runs = face_runs(font, size, seq);
                assert_eq!(runs.len(), 1, "{seq:?} is one run");
                assert_eq!(runs[0].face, RunFace::Emoji, "{seq:?} shapes in the emoji face");
                // One em: Twemoji's emoji glyphs all advance a full em.
                let w = measure_px(font, size, seq);
                assert!((w - size).abs() < 0.5, "{seq:?} measured {w}, want one em ({size})");
            }
        }
    }

    /// A mixed line splits into primary / emoji / primary runs whose pen origins are the running
    /// sum of the cluster advances — the exact positions `block_svg` places the `<text>` at.
    #[test]
    fn mixed_line_splits_into_face_runs_at_accumulated_pen_positions() {
        let font = TextFont::Clean;
        let size = 48.0;
        let seq = "\u{1F44D}\u{1F3FD}";
        let line = format!("a{seq}b");
        let runs = face_runs(font, size, &line);
        assert_eq!(runs.len(), 3, "primary | emoji | primary");
        assert_eq!(runs[0].text, "a");
        assert_eq!((runs[1].face, runs[1].text.as_str()), (RunFace::Emoji, seq));
        assert_eq!(runs[2].text, "b");
        let a = measure_px(font, size, "a");
        assert!((runs[0].x - 0.0).abs() < 1e-4);
        assert!((runs[1].x - a).abs() < 0.01, "the emoji run starts after 'a'");
        assert!((runs[2].x - (a + size)).abs() < 0.01, "'b' starts one emoji em further on");
        // The runs tile the line: the last origin plus its own width is the whole measurement.
        let total = runs[2].x + measure_px(font, size, &runs[2].text);
        assert!((total - measure_px(font, size, &line)).abs() < 0.01);
    }
}
