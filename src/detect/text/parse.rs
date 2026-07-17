//! Tesseract TSV parsing + the token heuristics behind it: turn the raw OCR rows into
//! per-word boxes in reading order, trimming misread control glyphs at line edges and
//! merging the two polarity passes.

use super::TextWord;
use crate::detect::luma;

/// Parse tesseract TSV into per-word boxes in reading order. Level-4 rows close the
/// current line; level-5 rows are the words (confidence-gated). Each line's edges are
/// trimmed of suspicious control-glyph tokens (see [`is_suspicious`]) before it's kept.
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_tsv_words(
    tsv: &str,
    rx: i32,
    ry: i32,
    sx: f32,
    sy: f32,
    skew: f32,
    center: (f32, f32),
    conf_thresh: f32,
) -> Vec<TextWord> {
    let mut words = Vec::new();
    let mut line_id: u32 = 0;
    let mut cur: Vec<TextWord> = Vec::new();
    // Map an OCR-image corner back to global logical coords: rotate by +skew about the
    // deskew centre (undoing the straightening), then scale + offset. Identity for skew=0.
    let (s, co) = skew.sin_cos();
    let (cx, cy) = center;
    let map = |ox: f32, oy: f32| -> (i32, i32) {
        let (dx, dy) = (ox - cx, oy - cy);
        let rxc = cx + dx * co - dy * s;
        let ryc = cy + dx * s + dy * co;
        (rx + (rxc / sx).round() as i32, ry + (ryc / sy).round() as i32)
    };
    for (i, row) in tsv.lines().enumerate() {
        if i == 0 {
            continue; // header
        }
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 {
            continue;
        }
        match c[0].parse::<i32>().unwrap_or(0) {
            4 => flush_line(&mut cur, &mut line_id, &mut words),
            5 => {
                let conf = c[10].parse::<f32>().unwrap_or(0.0);
                let text = normalize_token(c[11].trim());
                // Keep words at/above the threshold, plus a low-confidence rescue
                // (~0.4× the threshold) for clearly word-like tokens (≥4 letters) —
                // tesseract reports low confidence on blurry photos even when correct.
                let keep = conf >= conf_thresh || (conf >= conf_thresh * 0.4 && is_wordy(&text));
                if text.is_empty() || !keep {
                    continue;
                }
                let l = c[6].parse::<f32>().unwrap_or(0.0);
                let t = c[7].parse::<f32>().unwrap_or(0.0);
                let w = c[8].parse::<f32>().unwrap_or(0.0);
                let h = c[9].parse::<f32>().unwrap_or(0.0);
                let poly = [
                    map(l, t),
                    map(l + w, t),
                    map(l + w, t + h),
                    map(l, t + h),
                ];
                cur.push(TextWord {
                    rect: luma::quad_bbox(&poly),
                    poly,
                    text,
                    line: 0, // assigned in flush_line
                });
            }
            _ => {}
        }
    }
    flush_line(&mut cur, &mut line_id, &mut words);
    words
}

/// Merge two polarity passes into one reading-order word list: drop words read twice
/// (a box whose centre lies inside an already-kept box), then order top-to-bottom /
/// left-to-right and re-derive line ids by row.
pub(super) fn merge_words(mut words: Vec<TextWord>, more: Vec<TextWord>) -> Vec<TextWord> {
    words.extend(more);
    if words.is_empty() {
        return words;
    }
    let mut hs: Vec<i32> = words.iter().map(|w| w.rect.3).collect();
    hs.sort_unstable();
    let line_h = hs[hs.len() / 2].max(1);
    let row_tol = (line_h * 6 / 10).max(1);

    words.sort_by_key(|w| w.rect.1);
    let mut kept: Vec<TextWord> = Vec::new();
    'outer: for w in words {
        let (cx, cy) = (w.rect.0 + w.rect.2 / 2, w.rect.1 + w.rect.3 / 2);
        for k in &kept {
            if cx >= k.rect.0 && cx < k.rect.0 + k.rect.2 && cy >= k.rect.1 && cy < k.rect.1 + k.rect.3
            {
                continue 'outer; // same text already kept from the other pass
            }
        }
        kept.push(w);
    }

    // Group consecutive (by top) words into rows, sort each row left-to-right.
    kept.sort_by_key(|w| w.rect.1);
    let mut result: Vec<TextWord> = Vec::with_capacity(kept.len());
    let mut line_id = 0u32;
    let mut i = 0;
    while i < kept.len() {
        let top = kept[i].rect.1;
        let mut j = i;
        while j < kept.len() && kept[j].rect.1 - top <= row_tol {
            j += 1;
        }
        let mut row: Vec<TextWord> = kept[i..j].to_vec();
        row.sort_by_key(|w| w.rect.0);
        for mut w in row {
            w.line = line_id;
            result.push(w);
        }
        line_id += 1;
        i = j;
    }
    result
}

/// Recover the capital "I" that tesseract reads as a vertical bar `|` (identical
/// sans-serif glyphs): a standalone `|`, or a leading `|` before a contraction
/// apostrophe (`|'ve` → `I've`, `|'m` → `I'm`).
fn normalize_token(t: &str) -> String {
    if t == "|" {
        return "I".to_string();
    }
    let mut chars = t.chars();
    if chars.next() == Some('|') && matches!(chars.next(), Some('\'') | Some('\u{2019}')) {
        return format!("I{}", &t[1..]);
    }
    t.to_string()
}

/// Trim suspicious control-glyph tokens off both ends of a line, then emit the rest
/// tagged with `line_id` (bumped only for a non-empty line).
fn flush_line(cur: &mut Vec<TextWord>, line_id: &mut u32, out: &mut Vec<TextWord>) {
    while cur.first().is_some_and(|w| is_suspicious(&w.text)) {
        cur.remove(0);
    }
    while cur.last().is_some_and(|w| is_suspicious(&w.text)) {
        cur.pop();
    }
    if cur.is_empty() {
        return;
    }
    for mut w in cur.drain(..) {
        w.line = *line_id;
        out.push(w);
    }
    *line_id += 1;
}

/// Whether a token is a lone misread letter (radio "O", checkmark "v", "L", …) — edge
/// chrome to trim from a line's ends. Standalone punctuation (a dash, slash, …) and
/// emoticons are content, so they're NOT flagged. Only applied at line *edges*, so
/// mid-line single letters (`Mouse X Sensitivity`) and longer tokens are always kept.
fn is_suspicious(text: &str) -> bool {
    if is_emoticon(text) {
        return false;
    }
    let alnum: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
    matches!(alnum.as_slice(), [c] if c.is_alphabetic() && !matches!(c, 'a' | 'A' | 'I'))
}

/// A common text emoticon — kept as content even though it's mostly punctuation.
fn is_emoticon(text: &str) -> bool {
    matches!(
        text,
        ":)" | ":-)"
            | ":("
            | ":-("
            | ";)"
            | ";-)"
            | ":D"
            | ":-D"
            | ":P"
            | ":-P"
            | ":p"
            | ":O"
            | ":o"
            | "<3"
            | "</3"
            | ":/"
            | ":|"
            | "=)"
            | "=("
            | ":')"
            | "^^"
            | "^_^"
            | ":3"
            | "xD"
            | "XD"
            | ">:("
    )
}

/// A substantial word-like token (≥4 chars, mostly letters) — eligible for the
/// low-confidence rescue, unlike short tokens / numbers / symbols which need full
/// confidence to survive.
fn is_wordy(text: &str) -> bool {
    let total = text.chars().count();
    total >= 4 && text.chars().filter(|c| c.is_alphabetic()).count() * 4 >= total * 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn tw(x: i32, y: i32, w: i32, h: i32, text: &str) -> TextWord {
        TextWord {
            rect: (x, y, w, h),
            poly: [(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
            text: text.to_string(),
            line: 0,
        }
    }

    #[rstest]
    #[case("|", "I")] // lone vertical bar -> capital I
    #[case("|'ve", "I've")] // bar + ASCII apostrophe contraction
    #[case("|'m", "I'm")]
    #[case("|\u{2019}ve", "I\u{2019}ve")] // bar + curly apostrophe
    #[case("hello", "hello")] // ordinary token untouched
    #[case("|abc", "|abc")] // bar not followed by an apostrophe -> unchanged
    #[case("", "")] // empty -> empty
    #[case("a|b", "a|b")] // bar not at the start -> unchanged
    fn normalize_token_cases(#[case] input: &str, #[case] want: &str) {
        assert_eq!(normalize_token(input), want);
    }

    #[rstest]
    #[case(":)", true)]
    #[case(":-)", true)]
    #[case("<3", true)]
    #[case("xD", true)]
    #[case(">:(", true)]
    #[case("^_^", true)]
    #[case("hello", false)]
    #[case(":", false)]
    #[case("", false)]
    #[case(":-D ", false)] // trailing space -> not an exact match
    fn is_emoticon_cases(#[case] input: &str, #[case] want: bool) {
        assert_eq!(is_emoticon(input), want);
    }

    #[rstest]
    #[case("O", true)] // lone misread letter
    #[case("v", true)]
    #[case("L", true)]
    #[case("X", true)]
    #[case("a", false)] // 'a' is a real one-letter word
    #[case("A", false)]
    #[case("I", false)] // 'I' is a real one-letter word
    #[case("1", false)] // a lone digit is not alphabetic -> not suspicious
    #[case("Mouse", false)] // multi-letter token
    #[case(":)", false)] // emoticons are content, not chrome
    #[case("-", false)] // standalone punctuation (no alnum) is content
    #[case("", false)]
    fn is_suspicious_cases(#[case] input: &str, #[case] want: bool) {
        assert_eq!(is_suspicious(input), want);
    }

    #[rstest]
    #[case("Mouse", true)] // 5 letters, all alphabetic
    #[case("test", true)] // exactly 4 chars, all letters
    #[case("Hello!", true)] // 5/6 alphabetic (>= 75%)
    #[case("abc", false)] // < 4 chars
    #[case("ab12", false)] // only 50% letters
    #[case("a1b2", false)]
    #[case("abcd1", true)] // 4/5 = 80% letters
    #[case("", false)]
    fn is_wordy_cases(#[case] input: &str, #[case] want: bool) {
        assert_eq!(is_wordy(input), want);
    }

    #[test]
    fn merge_words_empty_is_empty() {
        assert!(merge_words(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn merge_words_drops_overlapping_duplicate_from_other_pass() {
        // The second pass re-read the same word at a slightly shifted box; its centre
        // falls inside the first pass's box, so it's dropped.
        let pass1 = vec![tw(0, 0, 10, 10, "hello")];
        let pass2 = vec![tw(2, 2, 10, 10, "hello")];
        let merged = merge_words(pass1, pass2);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "hello");
    }

    #[test]
    fn merge_words_keeps_non_overlapping_words() {
        let pass1 = vec![tw(0, 0, 10, 10, "left")];
        let pass2 = vec![tw(100, 0, 10, 10, "right")];
        let merged = merge_words(pass1, pass2);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_words_orders_reading_order_and_assigns_line_ids() {
        // Supplied out of order and split across the two passes; expect top-to-bottom,
        // left-to-right with line ids re-derived per row.
        let words = vec![tw(20, 0, 10, 10, "b"), tw(0, 100, 10, 10, "c")];
        let more = vec![tw(0, 0, 10, 10, "a")];
        let merged = merge_words(words, more);
        let order: Vec<&str> = merged.iter().map(|w| w.text.as_str()).collect();
        let lines: Vec<u32> = merged.iter().map(|w| w.line).collect();
        assert_eq!(order, vec!["a", "b", "c"]);
        assert_eq!(lines, vec![0, 0, 1]); // a,b share the top row; c is the next line
    }
}
