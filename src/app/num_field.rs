//! A numeric setting paired with the live text buffer its settings num-input
//! widget edits. The widget displays `text`; on each keystroke the handler
//! parses + validates the raw input and, when it's valid, stores the parsed
//! `value`. The `text` buffer always mirrors exactly what the user typed.
//! Programmatic value changes go through `set_value`, which re-syncs `text`.

use std::ops::RangeBounds;
use std::str::FromStr;

pub(crate) struct NumField<T> {
    pub value: T,
    pub text: String,
}

impl<T: ToString + Copy> NumField<T> {
    /// Build from a value, seeding the text buffer to match it.
    pub fn new(value: T) -> Self {
        Self { text: value.to_string(), value }
    }

    /// Set the value programmatically and re-sync the text buffer to it (used
    /// when a value changes from somewhere other than the text field, e.g. a
    /// reset or auto-calibration).
    pub fn set_value(&mut self, value: T) {
        self.value = value;
        self.text = value.to_string();
    }

    /// Store raw user input in the text buffer without reparsing it into the
    /// value (the value is updated separately by the caller's own clamp/parse).
    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    /// Apply user input from the text field: always mirror the raw input into
    /// `text`; if it parses and falls within `valid`, store the parsed value.
    /// Returns `true` iff the value was updated, so the caller can persist only
    /// then (matching the original per-handler `if let Ok(n) = … { save }`).
    pub fn edit<R: RangeBounds<T>>(&mut self, text: String, valid: R) -> bool
    where
        T: FromStr + PartialOrd,
    {
        let applied = match text.trim().parse::<T>() {
            Ok(n) if valid.contains(&n) => {
                self.value = n;
                true
            }
            _ => false,
        };
        self.text = text;
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_seeds_text_from_value() {
        let f = NumField::new(42u32);
        assert_eq!(f.value, 42);
        assert_eq!(f.text, "42");
    }

    #[test]
    fn new_seeds_text_for_negative_i32() {
        let f = NumField::new(-7i32);
        assert_eq!(f.value, -7);
        assert_eq!(f.text, "-7");
    }

    #[test]
    fn set_value_updates_value_and_mirrors_text() {
        let mut f = NumField::new(1u32);
        f.set_value(99);
        assert_eq!(f.value, 99);
        assert_eq!(f.text, "99");
    }

    #[test]
    fn set_text_changes_text_only_value_unchanged() {
        let mut f = NumField::new(5u32);
        f.set_text("123".to_string());
        assert_eq!(f.text, "123");
        assert_eq!(f.value, 5); // value untouched by set_text
    }

    #[test]
    fn edit_in_range_stores_value_and_text_returns_true() {
        let mut f = NumField::new(0u32);
        assert!(f.edit("50".to_string(), 0..=100));
        assert_eq!(f.value, 50);
        assert_eq!(f.text, "50");
    }

    #[test]
    fn edit_in_range_trims_whitespace_for_parsing_but_keeps_raw_text() {
        let mut f = NumField::new(0u32);
        assert!(f.edit("  7 ".to_string(), 0..=100));
        assert_eq!(f.value, 7);
        assert_eq!(f.text, "  7 "); // raw input preserved verbatim
    }

    #[test]
    fn edit_above_range_keeps_old_value_but_stores_text_returns_false() {
        let mut f = NumField::new(10u32);
        assert!(!f.edit("500".to_string(), 0..=100));
        assert_eq!(f.value, 10); // old value retained
        assert_eq!(f.text, "500"); // raw input still mirrored
    }

    #[test]
    fn edit_below_range_keeps_old_value_returns_false() {
        let mut f = NumField::new(-2i32);
        assert!(!f.edit("-50".to_string(), -10..=10));
        assert_eq!(f.value, -2);
        assert_eq!(f.text, "-50");
    }

    #[test]
    fn edit_in_range_negative_i32_stores_value() {
        let mut f = NumField::new(0i32);
        assert!(f.edit("-5".to_string(), -10..=10));
        assert_eq!(f.value, -5);
        assert_eq!(f.text, "-5");
    }

    #[test]
    fn edit_unparseable_keeps_value_stores_text_returns_false() {
        let mut f = NumField::new(7u32);
        assert!(!f.edit("abc".to_string(), 0..=100));
        assert_eq!(f.value, 7);
        assert_eq!(f.text, "abc");
    }

    #[test]
    fn edit_empty_string_is_unparseable_and_keeps_value() {
        let mut f = NumField::new(7u32);
        assert!(!f.edit(String::new(), 0..=100));
        assert_eq!(f.value, 7);
        assert_eq!(f.text, "");
    }

    #[test]
    fn edit_negative_into_u32_is_unparseable() {
        // u32 can't parse a negative literal -> unparseable, not out-of-range.
        let mut f = NumField::new(3u32);
        assert!(!f.edit("-1".to_string(), 0..=100));
        assert_eq!(f.value, 3);
        assert_eq!(f.text, "-1");
    }

    #[test]
    fn edit_at_range_bounds_is_inclusive() {
        let mut f = NumField::new(5u32);
        assert!(f.edit("100".to_string(), 0..=100)); // upper bound inclusive
        assert_eq!(f.value, 100);
        assert!(f.edit("0".to_string(), 0..=100)); // lower bound inclusive
        assert_eq!(f.value, 0);
    }
}
