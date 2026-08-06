//! OCR text detection via the `tesseract` CLI (an optional system-binary dependency,
//! like ffmpeg — the feature hints + no-ops when it's missing).
//!
//! The region is pre-processed (grayscale, auto-invert for light-on-dark UIs, contrast
//! stretch, then upscaled toward ~300dpi) so small on-screen text resolves, and the
//! TSV output is parsed into per-word boxes in reading order — the data behind the
//! selectable text layer.
//!
//! Split into [`preprocess`] (image prep + deskew + the tesseract call), [`parse`]
//! (TSV → word boxes + token heuristics), and [`layout`] (word boxes → spaced text).

mod layout;
mod parse;
mod preprocess;

pub use layout::join_words;

use crate::detect::luma;

/// A recognised word: its box (global logical coords) and the reading-order line it
/// sits on. Words are returned in reading order, so a span between two of them (by
/// index) selects the natural run of text and breaks lines where `line` changes.
#[derive(Clone, Debug)]
pub struct TextWord {
    /// Axis-aligned bounding box (global logical coords) — used for region filtering,
    /// layout (`join_words`), and merge ordering.
    pub rect: (i32, i32, i32, i32),
    /// Four-corner outline (global logical coords) following the text's true slant when
    /// the region was deskewed for OCR; the highlight + hit-test use this. For straight
    /// text it's just the `rect` corners.
    pub poly: [(i32, i32); 4],
    pub text: String,
    pub line: u32,
}

/// Whether the `tesseract` OCR binary is on PATH (text scanning requires it).
pub fn tesseract_available() -> bool {
    // DRAGON-244: via `tool_available` so the on-disk `EXE_SUFFIX` is honored — on
    // Windows the binary is `tesseract.exe`, so a bare `tesseract` file check wrongly
    // reported "not installed" even though the langs probe (which SPAWNS `tesseract`,
    // resolving `.exe`) found language data. Byte-identical on Linux/macOS (empty suffix).
    //
    // DRAGON-527: resolved through `util::tesseract_path` rather than the bare name, so a
    // sidecar shipped beside our own binary counts. Without that a packaged mac/Windows
    // build could never find tesseract at all (no Homebrew, nothing on PATH) and OCR was
    // silently unavailable for exactly the builds people pay for.
    crate::util::tool_available(&crate::util::tesseract_path())
}

/// Every language pack tesseract can currently see, as its short codes (`eng`, `deu`, …),
/// sorted. Empty when the binary is missing or has no data at all.
///
/// This is what the Settings dropdown lists, so it deliberately asks TESSERACT rather than
/// reading the directory itself: a bundled build points at our writable dir, a distro build
/// points at `/usr/share/tessdata`, and `--list-langs` is the one answer that is right in
/// both cases without us duplicating tesseract's own search rules.
#[cfg_attr(not(test), allow(dead_code))]
pub fn installed_langs() -> Vec<String> {
    lang_list().0
}

/// The language codes AND the directory tesseract read them from.
///
/// One invocation for both, because they come from the same output and the Settings row
/// needs each: the dropdown lists the codes, and its description names the folder to put
/// more into. Asking tesseract rather than deriving the path ourselves is the whole point,
/// it is the only answer that is right in BOTH cases without duplicating tesseract's own
/// search rules. A bundled build reports the folder we seeded; a distro build reports its
/// own `/usr/share/tessdata`.
pub fn lang_list() -> (Vec<String>, Option<String>) {
    let Ok(out) = tesseract_cmd().arg("--list-langs").output() else {
        return (Vec::new(), None);
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut langs: Vec<String> = parse_lang_list(&text)
        .into_iter()
        .filter(|c| !is_non_language(c))
        .collect();
    langs.sort();
    langs.dedup();
    (langs, parse_lang_dir(&text))
}

/// **Pure**, unit-tested: the directory out of `tesseract --list-langs`'s header line,
/// which reads `List of available languages in "/usr/share/tessdata/" (2):`.
///
/// `None` when the header is absent or shaped differently, which is a real possibility
/// across tesseract versions, so the caller must have something sensible to say without it
/// rather than printing an empty path.
fn parse_lang_dir(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("available languages"))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    let dir = line[start..end].trim_end_matches('/');
    (!dir.is_empty()).then(|| dir.to_string())
}

/// Whether a `--list-langs` entry is tesseract DATA rather than a language anyone can
/// pick. **Pure**, unit-tested.
///
/// `--list-langs` lists every `.traineddata` in the directory, and two of them are not
/// language models at all:
///
/// * `osd` is Orientation and Script Detection, used with `--psm 0` to work out page
///   rotation and which script the text is in. It ships with every tesseract package.
///   Passing `-l osd` for recognition does not fail, which is the trap: it returns
///   CONFIDENT GARBAGE (a run of English test text came back as Arabic-looking noise).
/// * `equ` is the maths/equation model, meaningful only combined with a real language.
///
/// Offering either as a language is offering a way to get nonsense out, so they are
/// filtered rather than labelled.
fn is_non_language(code: &str) -> bool {
    matches!(code, "osd" | "equ")
}

/// **Pure**, unit-tested: pull the language codes out of `tesseract --list-langs` output.
///
/// The format is a `"List of available languages (N):"` header followed by one code per
/// line, but which STREAM the header lands on varies by build, so callers concatenate both
/// and this drops anything that looks like prose (a colon or an embedded space) rather than
/// assuming stdout is clean.
fn parse_lang_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(':') && !l.contains(' '))
        .map(str::to_string)
        .collect()
}

/// A `tesseract` command with the resolved binary and, when we own the language directory,
/// an explicit `--tessdata-dir`.
///
/// `--tessdata-dir` rather than the `TESSDATA_PREFIX` environment variable ON PURPOSE: the
/// variable's meaning CHANGED between tesseract 4 (the parent of `tessdata/`) and 5 (the
/// directory itself), so the same value is wrong on one of them. The flag means the same
/// thing on both. It also keeps this off the environment, which on Linux is inherited from
/// login time and cannot be relied on to reflect anything current.
fn tesseract_cmd() -> std::process::Command {
    let mut cmd = crate::util::quiet_command(crate::util::tesseract_path());
    if let Some(dir) = crate::util::tessdata_dir() {
        cmd.arg("--tessdata-dir").arg(dir);
    }
    cmd
}

/// Whether tesseract has at least one usable LANGUAGE pack installed. The binary
/// alone isn't enough: without language data (e.g. tesseract-data-eng) every OCR
/// pass fails at runtime. Runs `tesseract --list-langs` (fast), so callers should
/// cache the result; false when the binary itself is missing.
pub fn tesseract_langs_available() -> bool {
    !installed_langs().is_empty()
}

/// OCR `img` (region cropped from the snapshot) with tesseract, returning per-word
/// boxes mapped to global logical coords. `rx,ry` is the region's top-left, `rw,rh` its
/// logical size.
#[allow(clippy::too_many_arguments)]
pub fn scan_text(
    img: &image::RgbaImage,
    rx: i32,
    ry: i32,
    rw: u32,
    rh: u32,
    conf_thresh: f32,
) -> Vec<TextWord> {
    let (iw, ih) = (img.width(), img.height());
    if iw < 8 || ih < 8 {
        return Vec::new();
    }
    // Grayscale (BT.601), with the running sum for the mixed-contrast check.
    let gray = luma::to_luma(img);
    let total: u64 = gray.iter().map(|&v| v as u64).sum();
    let region = (rx, ry, rw, rh);
    // Estimate text skew once (polarity-independent) so both mixed-contrast passes
    // deskew by the same angle; 0.0 when straight enough to skip rotation.
    let skew = preprocess::estimate_skew(&gray, iw, ih);
    // Resolved ONCE per scan, not per pass: the two mixed-contrast passes must use the
    // same language, and re-reading config per pass would let a mid-scan settings save
    // split them.
    let lang = crate::state::load().ocr_language.clone();
    if std::env::var_os("CCK_SKEW").is_some() {
        eprintln!("SKEW {:.2} deg", skew * 180.0 / std::f32::consts::PI);
    }

    // Mixed contrast — a bright horizontal band AND a dark band in the same selection
    // (e.g. a light header over a dark card) — can't be served by one global invert:
    // whichever polarity is wrong becomes garbage. Detect it by row brightness (robust
    // even when the bright region is a small fraction of the pixels), then OCR both
    // polarities and merge — each reads its matching-contrast region and the confidence
    // gate drops the wrong-polarity junk. Uniform regions stay a single pass.
    let row_mean = |y: u32| -> u64 {
        gray[(y * iw) as usize..((y + 1) * iw) as usize]
            .iter()
            .map(|&v| v as u64)
            .sum::<u64>()
            / iw as u64
    };
    let min_band = (ih as usize / 40).max(4);
    let bright_rows = (0..ih).filter(|&y| row_mean(y) > 170).count();
    let dark_rows = (0..ih).filter(|&y| row_mean(y) < 90).count();
    if bright_rows >= min_band && dark_rows >= min_band {
        let a = preprocess::run_ocr(&gray, iw, ih, false, region, skew, conf_thresh, &lang);
        let b = preprocess::run_ocr(&gray, iw, ih, true, region, skew, conf_thresh, &lang);
        parse::merge_words(a, b)
    } else {
        let invert = total / ((iw * ih) as u64).max(1) < 110; // mostly dark => light on dark
        preprocess::run_ocr(&gray, iw, ih, invert, region, skew, conf_thresh, &lang)
    }
}

/// DRAGON-527: the language list drives the Settings dropdown, so a parse slip shows up
/// as "my installed pack isn't offered".
#[cfg(test)]
mod lang_list_tests {
    use super::parse_lang_list;

    /// The real shape, header included. The header carries a colon and so is dropped.
    #[test]
    fn it_takes_the_codes_and_drops_the_header() {
        let out = "List of available languages (3):\neng\ndeu\nosd\n";
        assert_eq!(parse_lang_list(out), vec!["eng", "deu", "osd"]);
    }

    /// Some builds put the header on stderr and the codes on stdout, so callers hand us
    /// the two concatenated. Neither order may lose a language.
    #[test]
    fn it_survives_the_streams_being_concatenated_either_way() {
        let header = "List of available languages (2):\n";
        let codes = "eng\nfra\n";
        assert_eq!(parse_lang_list(&format!("{header}{codes}")), vec!["eng", "fra"]);
        assert_eq!(parse_lang_list(&format!("{codes}{header}")), vec!["eng", "fra"]);
    }

    /// Warnings and blank lines are prose, not languages. A tesseract that prints
    /// nothing usable yields an empty list, which is what `tesseract_langs_available`
    /// reads as "no packs installed".
    #[test]
    fn it_ignores_prose_and_blanks() {
        assert!(parse_lang_list("").is_empty());
        assert!(parse_lang_list("Error opening data file\n\n").is_empty());
        assert_eq!(
            parse_lang_list("Warning: something happened\neng\n\n"),
            vec!["eng"]
        );
    }
}

/// Human label for a tesseract language code. Only the handful a screenshot tool
/// realistically sees are named; anything else shows its raw code, which is still the
/// thing the user typed into a filename when they downloaded the pack.
///
/// **Pure**, unit-tested.
pub fn lang_label(code: &str) -> String {
    let name = match code {
        "eng" => "English",
        "deu" => "German",
        "fra" => "French",
        "spa" => "Spanish",
        "ita" => "Italian",
        "por" => "Portuguese",
        "nld" => "Dutch",
        "pol" => "Polish",
        "rus" => "Russian",
        "ukr" => "Ukrainian",
        "jpn" => "Japanese",
        "kor" => "Korean",
        "chi_sim" => "Chinese (Simplified)",
        "chi_tra" => "Chinese (Traditional)",
        "ara" => "Arabic",
        "hin" => "Hindi",
        "tur" => "Turkish",
        "swe" => "Swedish",
        "dan" => "Danish",
        "nor" => "Norwegian",
        "fin" => "Finnish",
        "ces" => "Czech",
        "ell" => "Greek",
        "heb" => "Hebrew",
        "tha" => "Thai",
        "vie" => "Vietnamese",
        _ => return code.to_string(),
    };
    format!("{name} ({code})")
}

/// DRAGON-527: the dropdown labels.
#[cfg(test)]
mod lang_label_tests {
    use super::lang_label;

    #[test]
    fn known_codes_are_named_and_keep_their_code() {
        assert_eq!(lang_label("eng"), "English (eng)");
        assert_eq!(lang_label("chi_sim"), "Chinese (Simplified) (chi_sim)");
    }

    /// An unknown pack must still be selectable rather than hidden or blank. The code
    /// is exactly what the user saw on the file they downloaded.
    #[test]
    fn an_unknown_code_shows_itself() {
        assert_eq!(lang_label("kat"), "kat");
        assert_eq!(lang_label(""), "");
    }

    /// `osd` and `equ` are not languages and must never reach the dropdown. Verified
    /// empirically: `-l osd` on English text returns confident garbage rather than
    /// failing, so a user who picked it would see nonsense with no explanation.
    #[test]
    fn tesseract_data_files_are_not_offered_as_languages() {
        use super::is_non_language;
        assert!(is_non_language("osd"));
        assert!(is_non_language("equ"));
        assert!(!is_non_language("eng"));
        assert!(!is_non_language("deu"));
    }
}

/// DRAGON-531: the directory shown in the Settings row is asked of tesseract, not guessed.
#[cfg(test)]
mod lang_dir_tests {
    use super::parse_lang_dir;

    /// The real header, verified against tesseract 5 output.
    #[test]
    fn it_reads_the_directory_from_the_header() {
        let out = "List of available languages in \"/usr/share/tessdata/\" (2):\neng\nosd\n";
        assert_eq!(parse_lang_dir(out).as_deref(), Some("/usr/share/tessdata"));
    }

    /// A bundled build reports our own folder through the same header, which is the whole
    /// reason the path is asked for rather than derived.
    #[test]
    fn it_reads_a_bundled_directory_too() {
        let out = "List of available languages in \"/Users/x/.config/cosmic-capture-kit/tessdata/\" (1):\neng\n";
        assert_eq!(
            parse_lang_dir(out).as_deref(),
            Some("/Users/x/.config/cosmic-capture-kit/tessdata")
        );
    }

    /// No header, or a differently shaped one, must yield None so the caller can say
    /// something sensible instead of printing an empty path.
    #[test]
    fn a_missing_or_odd_header_yields_none() {
        assert_eq!(parse_lang_dir(""), None);
        assert_eq!(parse_lang_dir("eng\nosd\n"), None);
        assert_eq!(parse_lang_dir("List of available languages in (2):\n"), None);
    }
}
