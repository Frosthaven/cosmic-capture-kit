//! Keyboard navigation over a LIST or a GRID of choices (DRAGON-680): which entry an arrow
//! key lands on, and what wrapping means at each end.
//!
//! Pure arithmetic over indices. No widget, no message, no platform: a caller hands in
//! where the highlight is and which way the user pressed, and gets back where it goes.
//!
//! # Why this is its own module
//!
//! Two unrelated parts of the app already navigate a list with the arrow keys, and a third
//! was about to invent a third copy of the same three lines:
//!
//! * the preview editor's toolbar FLYOUTS (`preview::edit::FlyoutNav`), where the arrows
//!   walk the covermark picker, the colour palette, the text size and font dropdowns and
//!   the upload destination list;
//! * the colour picker window's mode ACTIVATOR, where up and down step the notation while
//!   the control has focus, with or without its menu open;
//! * the colour picker window's colour HISTORY, which is a two-dimensional grid rather
//!   than a list, and needs the row-aware rules [`grid_step`] states.
//!
//! The owner asked for the second one to be reusable ("make this reusable because we have
//! other dropdowns that can eventually use this keyboard behavior"), which is the whole
//! reason the arithmetic left the picker: a dropdown that adopts arrow navigation later
//! should get the wrap rules by CALLING them, not by copying them.
//!
//! # The rules, stated once
//!
//! * **Both ends wrap.** A list you can walk off the end of is a list that punishes a key
//!   held one press too long, and every keyboard-navigable control in this app already
//!   wrapped before this module existed.
//! * **"Nothing highlighted" enters from the end you pressed FROM**: a forward step enters
//!   at the first entry and a backward step at the last, so both directions reach a list
//!   with one press.
//! * **An empty list answers 0 and changes nothing**, so a caller never has to special-case
//!   it before asking.
//!
//! # What this module does NOT decide
//!
//! Moving a highlight is all of it. What SPACE or ENTER then does on the highlighted entry
//! belongs to the caller, and the two grids in the colour picker window deliberately answer
//! that differently: the colour HISTORY applies its swatch, the harmony PANEL copies its
//! swatch (DRAGON-682 items 7 and 32, the owner's choice). That asymmetry is stated and
//! tested once, in `app::color_picker::geom::accept_action`, whose doc carries the reasoning.
//! **Do not "unify" the two by moving an accept rule in here**: navigation is shared because
//! the arithmetic really is the same, and the meaning of an entry is not.

use crate::shortcuts::Direction;

/// **Pure**, unit-tested: the highlighted index after stepping `delta` places through a
/// list of `len` entries, wrapping at both ends.
///
/// `current` is `None` when nothing is highlighted yet; a forward step then enters at the
/// first entry and a backward one at the last. `None` comes back only for an EMPTY list,
/// where there is nothing to highlight and the caller should leave its state alone.
///
/// `delta` is a signed step count rather than a bool because the flyouts pass `-1` / `+1`
/// from a key and nothing stops a future caller passing a page-sized jump; `rem_euclid`
/// keeps any magnitude, positive or negative, inside the list.
pub fn step(current: Option<usize>, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let n = len as i32;
    // From "nothing highlighted", a forward step lands on 0 and a backward one on the last
    // entry: the base is one BEFORE the first for a forward press, and the first itself for
    // a backward press, so the arithmetic below produces both without a second branch.
    let base = current.map(|s| s as i32).unwrap_or(if delta >= 0 { -1 } else { 0 });
    Some(((base + delta).rem_euclid(n)) as usize)
}

/// **Pure**, unit-tested: the highlighted index after an arrow key in a GRID of `len`
/// entries laid out `per_row` to a row, in reading order (left to right, top to bottom).
///
/// The colour picker's history is the first tenant: two rows of nine, with a last row that
/// is usually partial.
///
/// * **Left and Right walk the READING ORDER**, not the row: right from the end of a row
///   continues at the start of the next one, and the two ends of the whole grid wrap into
///   each other. That is what makes the arrows agree with the order the eye reads, and it
///   is also what a one-row grid degenerates to, exactly [`step`].
/// * **Up and Down move a whole ROW, staying in the column**, and wrap within that column
///   rather than into a neighbouring one. A column that has no entry in the row being moved
///   to (the partial last row) keeps the highlight where it is rather than sliding sideways
///   to the nearest one: sideways movement belongs to the sideways keys.
///
/// `current` past the end (a list that shrank under the highlight) is treated as the first
/// entry, and an empty grid answers 0.
pub fn grid_step(current: usize, dir: Direction, len: usize, per_row: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let per_row = per_row.max(1);
    let current = if current < len { current } else { 0 };
    match dir {
        Direction::Left => (current + len - 1) % len,
        Direction::Right => (current + 1) % len,
        Direction::Down => {
            let below = current + per_row;
            // Off the bottom (or into a gap in a partial last row): back to the TOP of this
            // column, which is the wrap the vertical keys own.
            if below < len { below } else { current % per_row }
        }
        Direction::Up => {
            if current >= per_row {
                current - per_row
            } else {
                // Wrap to the LOWEST entry in this column, which is not always the last
                // row: a partial last row leaves some columns ending one row higher.
                let col = current % per_row;
                let mut at = col;
                while at + per_row < len {
                    at += per_row;
                }
                at
            }
        }
    }
}

/// **Pure**, unit-tested: the highlighted cell after an arrow key in a RAGGED grid, where
/// every row has its own length (DRAGON-682).
///
/// `rows` is each row's entry count, in display order; `at` is `(row, column)`. The colour
/// picker's compare panel is the first tenant: a column of harmony cards whose swatch
/// counts differ (a companion card holds two, a tetradic card four), which is exactly the
/// shape [`grid_step`]'s rectangular rules cannot express.
///
/// * **Left and Right walk the READING ORDER through the rows**, so right from the end of
///   one card continues at the start of the next and the two ends of the whole panel wrap
///   into each other. That makes one repeated key sweep everything, which is the only way
///   to reach a swatch when you do not know which card it is in.
/// * **Up and Down move a whole ROW**, wrapping top to bottom, and CLAMP the column into
///   the row they land on rather than skipping a short row. A short card is still a place
///   the cursor can be, and skipping it would make some swatches unreachable from above.
///
/// An empty `rows`, or one with no entries at all, answers `(0, 0)`: there is nothing to
/// highlight and the caller has nothing to draw either way.
pub fn ragged_step(at: (usize, usize), dir: Direction, rows: &[usize]) -> (usize, usize) {
    let live: Vec<usize> = rows.to_vec();
    if live.iter().all(|n| *n == 0) {
        return (0, 0);
    }
    // A cursor left over from a shorter or longer list re-enters at the first cell.
    let row = if at.0 < live.len() && live[at.0] > 0 { at.0 } else { first_row(&live) };
    let col = at.1.min(live[row].saturating_sub(1));
    match dir {
        Direction::Right => {
            if col + 1 < live[row] {
                (row, col + 1)
            } else {
                let next = next_row(&live, row, 1);
                (next, 0)
            }
        }
        Direction::Left => {
            if col > 0 {
                (row, col - 1)
            } else {
                let prev = next_row(&live, row, -1);
                (prev, live[prev] - 1)
            }
        }
        Direction::Down => {
            let next = next_row(&live, row, 1);
            (next, col.min(live[next] - 1))
        }
        Direction::Up => {
            let prev = next_row(&live, row, -1);
            (prev, col.min(live[prev] - 1))
        }
    }
}

/// The first row that has any entries at all.
fn first_row(rows: &[usize]) -> usize {
    rows.iter().position(|n| *n > 0).unwrap_or(0)
}

/// The next non-empty row `delta` steps away, wrapping. Empty rows are SKIPPED rather than
/// landed on: a card with no swatches is not a place a cursor can sit, and stopping there
/// would make the arrow key look broken.
fn next_row(rows: &[usize], from: usize, delta: i32) -> usize {
    let n = rows.len();
    let mut at = from;
    for _ in 0..n {
        at = ((at as i32 + delta).rem_euclid(n as i32)) as usize;
        if rows[at] > 0 {
            return at;
        }
    }
    from
}

#[cfg(test)]
mod step_tests {
    use super::*;

    /// The ordinary case, both directions, and the wrap at each end. This is the
    /// behaviour the preview's flyouts have always had and that `FlyoutNav::nav` now
    /// delegates here for.
    #[test]
    fn stepping_walks_and_wraps() {
        assert_eq!(step(Some(0), 1, 4), Some(1));
        assert_eq!(step(Some(3), 1, 4), Some(0), "forward off the end wraps");
        assert_eq!(step(Some(0), -1, 4), Some(3), "backward off the start wraps");
        assert_eq!(step(Some(2), -1, 4), Some(1));
    }

    /// From "nothing highlighted" each direction enters from its own end, so one press
    /// reaches the list whichever way the user pressed.
    #[test]
    fn an_empty_highlight_enters_from_the_pressed_end() {
        assert_eq!(step(None, 1, 5), Some(0), "forward enters at the first");
        assert_eq!(step(None, -1, 5), Some(4), "backward enters at the last");
    }

    /// A step of any size stays inside the list, and a full lap comes home. The flyouts
    /// only ever pass ±1, but nothing in the signature says so and `rem_euclid` is what
    /// makes the promise hold for the rest.
    #[test]
    fn any_magnitude_stays_inside_the_list() {
        for delta in [-9, -4, -1, 0, 1, 4, 9] {
            let got = step(Some(2), delta, 4).expect("a non-empty list always answers");
            assert!(got < 4, "delta {delta} left the list at {got}");
        }
        assert_eq!(step(Some(2), 4, 4), Some(2), "a full lap is home");
        assert_eq!(step(Some(2), -4, 4), Some(2));
    }

    /// A ONE-entry list cannot move, and an EMPTY one answers nothing rather than a index
    /// the caller would have to bounds-check.
    #[test]
    fn degenerate_lists_are_safe() {
        assert_eq!(step(Some(0), 1, 1), Some(0));
        assert_eq!(step(None, 1, 1), Some(0));
        assert_eq!(step(Some(0), 1, 0), None);
        assert_eq!(step(None, -1, 0), None);
    }
}

#[cfg(test)]
mod grid_step_tests {
    use super::*;

    /// The colour picker's own grid: two FULL rows of nine.
    const PER_ROW: usize = 9;

    /// Left and Right walk the reading order across the row boundary, and the two ends of
    /// the grid wrap into each other.
    #[test]
    fn sideways_walks_the_reading_order() {
        assert_eq!(grid_step(0, Direction::Right, 18, PER_ROW), 1);
        assert_eq!(grid_step(8, Direction::Right, 18, PER_ROW), 9, "into the next row");
        assert_eq!(grid_step(9, Direction::Left, 18, PER_ROW), 8, "and back up it");
        assert_eq!(grid_step(17, Direction::Right, 18, PER_ROW), 0, "the far end wraps");
        assert_eq!(grid_step(0, Direction::Left, 18, PER_ROW), 17);
    }

    /// Up and Down move a whole row and stay in their column, wrapping within it.
    #[test]
    fn vertical_moves_a_row_and_stays_in_its_column() {
        assert_eq!(grid_step(2, Direction::Down, 18, PER_ROW), 11);
        assert_eq!(grid_step(11, Direction::Up, 18, PER_ROW), 2);
        assert_eq!(grid_step(11, Direction::Down, 18, PER_ROW), 2, "off the bottom wraps up");
        assert_eq!(grid_step(2, Direction::Up, 18, PER_ROW), 11, "off the top wraps down");
    }

    /// A PARTIAL last row, which is what the history looks like until eighteen colours have
    /// been picked: thirteen entries, so the second row holds four (indices 9..12).
    ///
    /// The column that has no entry below it must not slide sideways into a neighbour's:
    /// the highlight stays put, and Left/Right is how you reach the other column.
    #[test]
    fn a_partial_last_row_never_slides_sideways() {
        let len = 13;
        assert_eq!(grid_step(2, Direction::Down, len, PER_ROW), 11, "column 2 has a row below");
        assert_eq!(grid_step(5, Direction::Down, len, PER_ROW), 5, "column 5 does not");
        assert_eq!(grid_step(5, Direction::Up, len, PER_ROW), 5, "and cannot wrap into one");
        assert_eq!(grid_step(11, Direction::Up, len, PER_ROW), 2);
        assert_eq!(grid_step(2, Direction::Up, len, PER_ROW), 11, "the wrap finds the last row");
        // And the reading order still crosses the ragged end cleanly.
        assert_eq!(grid_step(12, Direction::Right, len, PER_ROW), 0);
        assert_eq!(grid_step(0, Direction::Left, len, PER_ROW), 12);
    }

    /// Every move from every position lands INSIDE the grid, at every length a real
    /// history can have. The exhaustive check is the point: the partial-row arithmetic is
    /// exactly the kind that is right for the cases someone thought of.
    #[test]
    fn no_move_ever_leaves_the_grid() {
        for len in 1..=18usize {
            for at in 0..len {
                for dir in
                    [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
                {
                    let got = grid_step(at, dir, len, PER_ROW);
                    assert!(got < len, "len {len}, {at} {dir:?} -> {got}");
                }
            }
        }
    }

    /// Degenerate inputs answer 0 rather than panicking on a modulo by zero or an index
    /// past the end: a grid can be empty for a frame while the history loads, and a
    /// highlight can outlive the list it pointed into.
    #[test]
    fn degenerate_grids_are_safe() {
        assert_eq!(grid_step(3, Direction::Right, 0, PER_ROW), 0);
        assert_eq!(grid_step(99, Direction::Right, 4, PER_ROW), 1, "a stale index re-enters at 0");
        // A zero-wide row cannot divide, so it is read as ONE per row, which makes the
        // grid a vertical list: Down moves to the next entry rather than panicking.
        assert_eq!(grid_step(0, Direction::Down, 4, 0), 1);
        assert_eq!(grid_step(3, Direction::Down, 4, 0), 0, "and wraps at the end of it");
    }

    /// A ONE-ROW grid is exactly a list: the vertical keys cannot move, and the sideways
    /// ones behave like [`step`]. Worth pinning because the picker's history IS one row
    /// until the tenth colour is picked.
    #[test]
    fn a_single_row_grid_degenerates_to_a_list() {
        for at in 0..5 {
            assert_eq!(grid_step(at, Direction::Down, 5, PER_ROW), at);
            assert_eq!(grid_step(at, Direction::Up, 5, PER_ROW), at);
            assert_eq!(
                Some(grid_step(at, Direction::Right, 5, PER_ROW)),
                step(Some(at), 1, 5),
                "sideways in one row is the list step"
            );
        }
    }
}

#[cfg(test)]
mod ragged_step_tests {
    use super::*;

    /// The colour picker's compare panel, near enough: cards of two, three, three, three,
    /// four and five swatches.
    const CARDS: [usize; 6] = [2, 3, 3, 3, 4, 5];

    /// Left and Right walk the reading order across card boundaries, and the two ends of
    /// the whole panel wrap into each other.
    #[test]
    fn sideways_walks_the_reading_order_across_cards() {
        assert_eq!(ragged_step((0, 0), Direction::Right, &CARDS), (0, 1));
        assert_eq!(ragged_step((0, 1), Direction::Right, &CARDS), (1, 0), "into the next card");
        assert_eq!(ragged_step((1, 0), Direction::Left, &CARDS), (0, 1), "and back up it");
        let last = (CARDS.len() - 1, CARDS[CARDS.len() - 1] - 1);
        assert_eq!(ragged_step(last, Direction::Right, &CARDS), (0, 0), "the far end wraps");
        assert_eq!(ragged_step((0, 0), Direction::Left, &CARDS), last);
    }

    /// Up and Down move a whole card and CLAMP into it, so a short card is still a place
    /// the cursor can land rather than one it skips over.
    #[test]
    fn vertical_moves_a_card_and_clamps_into_it() {
        assert_eq!(ragged_step((5, 4), Direction::Up, &CARDS), (4, 3), "clamped into a shorter card");
        assert_eq!(ragged_step((4, 3), Direction::Up, &CARDS), (3, 2));
        assert_eq!(ragged_step((0, 1), Direction::Down, &CARDS), (1, 1), "the column survives");
        assert_eq!(ragged_step((0, 1), Direction::Up, &CARDS), (5, 1), "the top wraps to the last");
        assert_eq!(ragged_step((5, 0), Direction::Down, &CARDS), (0, 0));
    }

    /// Every move from every cell lands on a real cell, at a spread of card shapes. The
    /// exhaustive sweep is the point: ragged rows are exactly where an off-by-one hides.
    #[test]
    fn no_move_ever_leaves_the_panel() {
        for rows in [&CARDS[..], &[1], &[5, 1], &[1, 5], &[2, 2, 2]] {
            for (r, len) in rows.iter().enumerate() {
                for c in 0..*len {
                    for dir in
                        [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
                    {
                        let (nr, nc) = ragged_step((r, c), dir, rows);
                        assert!(nr < rows.len(), "{rows:?} ({r},{c}) {dir:?} -> row {nr}");
                        assert!(nc < rows[nr], "{rows:?} ({r},{c}) {dir:?} -> col {nc}");
                    }
                }
            }
        }
    }

    /// An EMPTY card is skipped rather than landed on, in both directions, because a card
    /// with no swatches is not a place a cursor can sit.
    #[test]
    fn an_empty_card_is_skipped() {
        let rows = [2, 0, 3];
        assert_eq!(ragged_step((0, 1), Direction::Right, &rows), (2, 0));
        assert_eq!(ragged_step((2, 0), Direction::Left, &rows), (0, 1));
        assert_eq!(ragged_step((0, 0), Direction::Down, &rows), (2, 0));
        assert_eq!(ragged_step((2, 0), Direction::Up, &rows), (0, 0));
    }

    /// Degenerate input answers the first cell rather than panicking: the panel can be
    /// empty for a frame, and a cursor can outlive the list it pointed into.
    #[test]
    fn degenerate_panels_are_safe() {
        assert_eq!(ragged_step((0, 0), Direction::Right, &[]), (0, 0));
        assert_eq!(ragged_step((0, 0), Direction::Down, &[0, 0]), (0, 0));
        // A stale cursor is first NORMALISED into the panel (the first row, its own last
        // column) and then stepped, so one press both rescues it and moves it.
        assert_eq!(ragged_step((9, 9), Direction::Right, &CARDS), (1, 0));
        assert_eq!(ragged_step((9, 9), Direction::Left, &CARDS), (0, 0));
    }
}
