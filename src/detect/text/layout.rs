//! Layout reconstruction: turn a reading-order run of word boxes back into text,
//! recovering word spaces, aligned-column gaps, line breaks, and paragraph breaks from
//! the geometry alone.

use super::TextWord;

/// Inserted where a line has a wide horizontal gap (aligned columns).
const COLUMN_GAP: &str = "     "; // 5 spaces

/// Join a reading-order slice of words into text, reconstructing layout from the word
/// boxes: a space between words on a line, five spaces where the horizontal gap is much
/// wider than a space (aligned columns), a newline at a line break, and a blank line at
/// a paragraph / block break.
pub fn join_words(words: &[TextWord]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let pct = |mut v: Vec<f32>, num: usize, den: usize| -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(v[(v.len() * num / den).min(v.len() - 1)])
    };
    let line_h = pct(words.iter().map(|w| w.rect.3 as f32).collect(), 1, 2).unwrap_or(1.0).max(1.0);

    let hgap = |a: &TextWord, b: &TextWord| (b.rect.0 - (a.rect.0 + a.rect.2)) as f32;
    let pitch = |a: &TextWord, b: &TextWord| (b.rect.1 - a.rect.1) as f32; // line top-to-top
    // Two words sit on the same visual row if their boxes overlap vertically by more
    // than half the shorter one — independent of tesseract's line ids, which split
    // horizontally-separated items on one row (e.g. a banner) into different "lines".
    let same_row = |a: &TextWord, b: &TextWord| {
        let overlap = (a.rect.1 + a.rect.3).min(b.rect.1 + b.rect.3) - a.rect.1.max(b.rect.1);
        overlap * 2 > a.rect.3.min(b.rect.3).max(1)
    };

    // Horizontal: a column gap is several normal word-spaces wide. Use the 25th
    // percentile as the normal-space baseline so it isn't skewed when most same-row
    // gaps are themselves big column gaps (e.g. a banner with two items).
    let base_h = pct(
        words
            .windows(2)
            .filter(|p| same_row(&p[0], &p[1]))
            .map(|p| hgap(&p[0], &p[1]))
            .collect(),
        1,
        4,
    )
    .unwrap_or(0.0);
    let col_thresh = (base_h * 3.0).max(line_h * 1.2);

    // Vertical: use the *tightest* line spacing (25th percentile pitch) as the single-
    // line baseline so paragraph gaps stand out even when most paragraphs are one line
    // (a bullet list). A blank line roughly doubles the pitch.
    let pitches: Vec<f32> = words
        .windows(2)
        .filter(|p| !same_row(&p[0], &p[1]))
        .map(|p| pitch(&p[0], &p[1]))
        .filter(|&v| v > 0.0)
        .collect();
    let base_pitch = pct(pitches, 1, 4).unwrap_or(line_h * 1.4);
    let para_thresh = (base_pitch * 1.6).max(line_h * 1.8);

    let mut out = String::new();
    out.push_str(&words[0].text);
    for pair in words.windows(2) {
        let (prev, w) = (&pair[0], &pair[1]);
        if !same_row(prev, w) {
            out.push_str(if pitch(prev, w) > para_thresh { "\n\n" } else { "\n" });
        } else if hgap(prev, w) > col_thresh {
            out.push_str(COLUMN_GAP);
        } else {
            out.push(' ');
        }
        out.push_str(&w.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `TextWord` from a rect (poly = the rect corners, line = 0).
    fn tw(x: i32, y: i32, w: i32, h: i32, text: &str) -> TextWord {
        TextWord {
            rect: (x, y, w, h),
            poly: [(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
            text: text.to_string(),
            line: 0,
        }
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(join_words(&[]), "");
    }

    #[test]
    fn single_space_between_close_words() {
        let words = [tw(0, 0, 10, 10, "a"), tw(12, 0, 10, 10, "b")];
        assert_eq!(join_words(&words), "a b");
    }

    #[test]
    fn wide_gap_becomes_column_gap() {
        let words = [
            tw(0, 0, 10, 10, "a"),
            tw(12, 0, 10, 10, "b"),
            tw(200, 0, 10, 10, "c"),
        ];
        assert_eq!(join_words(&words), format!("a b{COLUMN_GAP}c"));
    }

    #[test]
    fn next_row_breaks_line() {
        let words = [tw(0, 0, 10, 10, "a"), tw(0, 30, 10, 10, "b")];
        assert_eq!(join_words(&words), "a\nb");
    }
}
