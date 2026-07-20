//! Preview-overlay editing: a single covermark overlay (with zoom + undo/redo).
//!
//! The covermark is NON-destructive until a share action (Save / Save As / Copy)
//! bakes it into the file: an image is re-encoded in place from its decoded pixels;
//! a video is re-encoded through an `ffmpeg` `overlay` filter graph. Undo/redo moves
//! the covermark between history stacks — the display recomposites from the untouched
//! original (image) or stacks the covermark over the frame (video), so nothing is
//! lost until the user commits by sharing.

use super::layers::RasterSlot;
use super::timeline::{Span, Timeline};
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

/// The covermark picker's state while open (a dropdown under the covermark button).
pub struct Picker {
    /// The choices, in display order. `None` is the "None" (disable) card, always
    /// first; `Some(kind)` are the real covermarks.
    pub entries: Vec<Option<CovermarkKind>>,
    /// Keyboard-selected index.
    pub selected: usize,
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
    /// The covermark picker dropdown, when open.
    pub picker: Option<Picker>,
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
    /// The capture's pixel dimensions (set once probed/decoded) so preview rasters
    /// match the bake's aspect.
    pub frame: (u32, u32),
}

impl EditState {
    /// Whether a covermark is applied (drives bake-before-share and button states).
    pub fn dirty(&self) -> bool {
        self.covermark.is_some()
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

    /// Undo the most recent edit — covermark or timeline, whichever was made
    /// last. `timeline` is the video preview's timeline when there is one (an
    /// image preview passes `None`; it never accumulates timeline ops). Returns
    /// whether the COVERMARK changed (the caller then refreshes its raster —
    /// timeline changes redraw for free on the next view).
    pub fn undo(&mut self, timeline: Option<&mut Timeline>) -> bool {
        match self.undo_stack.pop() {
            Some(EditOp::Covermark(prev)) => {
                self.redo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = prev;
                self.cm_raster.invalidate();
                true
            }
            Some(EditOp::Timeline(prev)) => {
                if let Some(tl) = timeline {
                    self.redo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(prev);
                }
                false
            }
            None => false,
        }
    }

    /// Redo the most recently undone edit (either kind). Returns whether the
    /// covermark changed, as [`Self::undo`].
    pub fn redo(&mut self, timeline: Option<&mut Timeline>) -> bool {
        match self.redo_stack.pop() {
            Some(EditOp::Covermark(next)) => {
                self.undo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = next;
                self.cm_raster.invalidate();
                true
            }
            Some(EditOp::Timeline(next)) => {
                if let Some(tl) = timeline {
                    self.undo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(next);
                }
                false
            }
            None => false,
        }
    }

    /// The display-preview raster size for the current frame (a ≤1024 box at the
    /// capture's aspect), used by the async video-overlay recomposite.
    pub fn preview_raster_size(&self) -> (u32, u32) {
        let (fw, fh) = match self.frame {
            (0, _) | (_, 0) => (1280u32, 800u32),
            f => f,
        };
        let scale = (1024.0 / fw as f32).min(1024.0 / fh as f32).min(1.0);
        (((fw as f32 * scale) as u32).max(1), ((fh as f32 * scale) as u32).max(1))
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

/// Bake the covermark onto an image, reading `src` and writing the result to `dst`
/// (they may be the same path for an in-place Save, or differ so Copy can produce an
/// edited file WITHOUT touching the saved original). Returns `dst`'s size. `cm.is_some()`
/// must hold (the only image edit).
pub fn bake_image(src: &Path, dst: &Path, cm: Option<&Covermark>) -> std::io::Result<u64> {
    let err = |e: String| std::io::Error::other(e);
    let dst_png = super::ext_of(dst).as_deref() == Some("png");
    if cm.is_some() {
        let mut rgba = ::image::open(src).map_err(|e| err(e.to_string()))?.into_rgba8();
        apply_covermark(&mut rgba, cm);
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
        assert!(edit.undo(None));
        assert!(!edit.dirty() && edit.can_redo());
        assert!(edit.redo(None));
        assert!(edit.dirty());
        assert!(!edit.redo(None), "redo with an empty stack must be a no-op");
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
        assert!(!edit.undo(Some(&mut tl)), "timeline undo must not report a covermark change");
        assert_eq!(tl.spans.len(), 2);
        assert!(!tl.edited(), "undoing the delete restores the content");
        assert!(!edit.undo(Some(&mut tl)));
        assert_eq!(tl.spans.len(), 1, "undoing the cut re-joins the spans");
        assert!(edit.undo(Some(&mut tl)), "covermark undo reports the change");
        assert!(!edit.dirty());
        // Redo replays in order: covermark, cut, delete.
        assert!(edit.redo(Some(&mut tl)));
        assert!(edit.dirty());
        assert!(!edit.redo(Some(&mut tl)));
        assert_eq!(tl.spans.len(), 2);
        assert!(!edit.redo(Some(&mut tl)));
        assert!(tl.edited());
        assert!(!edit.can_redo());
        // A fresh timeline edit clears redo, like a fresh covermark choice.
        // (This undo pops the timeline delete — no covermark change reported.)
        assert!(!edit.undo(Some(&mut tl)));
        assert!(edit.can_redo());
        edit.push_timeline(tl.spans.clone());
        assert!(!edit.can_redo());
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
