//! The SAVED PALETTES' own store (DRAGON-687): `palettes.toml`, beside the config.
//!
//! # Why a separate file
//!
//! The owner's rule: "we dont want factory resets to impact saved palettes." A factory
//! reset rewrites `config.toml` with the defaults (`app/persist.rs`'s `apply_persisted`),
//! and while the palettes lived inside it (one merge window, never a shipped release) the
//! reset would have taken a user's collected colours with it. Palettes are user CONTENT
//! the way captures are, not configuration, so they live in their own file that no reset
//! path writes.
//!
//! # Where the file is, and why that is not a new decision
//!
//! [`palettes_path`] is `app_config_dir()/palettes.toml`: the SAME
//! [`crate::util::app_config_dir`] that places `config.toml`, so every property of the
//! config's location is inherited rather than restated:
//!
//! * **test isolation** (DRAGON-424): `util::is_dev_process` routes every cargo-run
//!   process into the sandbox dir before any platform path is consulted, so a test run
//!   can never read or write the developer's real palettes, exactly as
//!   `tests/log_isolation.rs` proves for the config and the debug log;
//! * **the uniform `~/.config` location** (XDG-respecting on Linux), including the
//!   one-shot macOS/Windows DIRECTORY migration, which moves the whole app dir and takes
//!   this file along with it.
//!
//! # Concurrency
//!
//! The same story as the config, deliberately no better and no worse: a bare
//! `fs::write`, last writer wins (`store::save`'s own contract, stated at
//! `note_encoder_auto_hint`). The practical writer set is even smaller than the
//! config's: at most ONE colour picker window exists at a time (DRAGON-613), and it is
//! the only thing that writes palettes.
//!
//! # The default, and how deletion sticks
//!
//! An ABSENT file answers the [`default_saved_palettes`] starter palettes (Catppuccin
//! Latte and Mocha, the owner's pick, trimmed from all four flavours). The default is
//! never eagerly written: it applies afresh on
//! every load until the user's first palette mutation saves the file, and from then on
//! the FILE is the truth, empty included. TOML serializes an empty entry list as a
//! present `saved_palettes = []` key (the config's own `covermark_prefs = []` is the
//! long-standing proof), so a user who deletes every group keeps an empty file that
//! answers empty forever; `deleting_every_palette_sticks_across_the_round_trip` pins it.
//!
//! # Migration from the config era
//!
//! [`palettes_from`] is the whole rule, pure and pinned: a PRESENT file wins over
//! everything; with the file absent, a config that still carries the legacy
//! `saved_palettes` key (even empty: a config-era deletion stays a deletion) migrates
//! its list INTO the file immediately, the RON migration's own "write at once so it
//! happens exactly once" pattern, because the legacy key is `skip_serializing` and the
//! next config save drops it; only a load with neither answers the default.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::schema::SavedPalette;

/// The palettes file, beside the config (see the module doc for everything the shared
/// directory decision inherits).
pub fn palettes_path() -> Option<PathBuf> {
    Some(crate::util::app_config_dir()?.join("palettes.toml"))
}

/// The file's whole schema: one key, so the file reads as what it is. Container-level
/// `#[serde(default)]` for the same belt the config wears: an absent key loads as the
/// field default instead of failing the file.
#[derive(Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct PalettesFile {
    /// The groups, in the user's own order (drag-sortable, so ORDER is state).
    #[serde(default)]
    pub saved_palettes: Vec<SavedPalette>,
}

/// The starter palettes a fresh install opens with: **Catppuccin Latte and Mocha**
/// (the owner's second pick, DRAGON-687's lost-release round: "lets make the default
/// palettes latte and mocha, no others"; all four flavours shipped first, and the
/// Frappé / Macchiato constants went WITH the trim rather than lingering dead, their
/// values one `git log -p` away). Light-to-dark order, each with its 26 swatches in
/// the published order.
///
/// Values from <https://catppuccin.com/palette/> (each flavor's hex values, Rosewater
/// through Crust). Only the swatch VALUES are Catppuccin's: their palette is
/// MIT-licensed content (github.com/catppuccin/catppuccin), carried here the way the
/// vendored lucide icons carry their provenance, as a comment at the definition. The
/// groups are ordinary user palettes once loaded: renameable, editable, deletable, and
/// a deletion of any or all of them sticks (see the module doc). The default only ever
/// answers an ABSENT file, so an existing palettes.toml (the four-flavour set
/// included, wherever a save materialized it) is untouched by the trim, which is the
/// wanted behaviour, not a migration gap.
pub fn default_saved_palettes() -> Vec<SavedPalette> {
    // Each flavour, Rosewater / Flamingo / Pink / Mauve / Red / Maroon / Peach / Yellow /
    // Green / Teal / Sky / Sapphire / Blue / Lavender / Text / Subtext1 / Subtext0 /
    // Overlay2 / Overlay1 / Overlay0 / Surface2 / Surface1 / Surface0 / Base / Mantle /
    // Crust, in exactly that published order.
    const LATTE: [&str; 26] = [
        "#DC8A78", "#DD7878", "#EA76CB", "#8839EF", "#D20F39", "#E64553", "#FE640B",
        "#DF8E1D", "#40A02B", "#179299", "#04A5E5", "#209FB5", "#1E66F5", "#7287FD",
        "#4C4F69", "#5C5F77", "#6C6F85", "#7C7F93", "#8C8FA1", "#9CA0B0", "#ACB0BE",
        "#BCC0CC", "#CCD0DA", "#EFF1F5", "#E6E9EF", "#DCE0E8",
    ];
    const MOCHA: [&str; 26] = [
        "#F5E0DC", "#F2CDCD", "#F5C2E7", "#CBA6F7", "#F38BA8", "#EBA0AC", "#FAB387",
        "#F9E2AF", "#A6E3A1", "#94E2D5", "#89DCEB", "#74C7EC", "#89B4FA", "#B4BEFE",
        "#CDD6F4", "#BAC2DE", "#A6ADC8", "#9399B2", "#7F849C", "#6C7086", "#585B70",
        "#45475A", "#313244", "#1E1E2E", "#181825", "#11111B",
    ];
    let group = |name: &str, colors: &[&str; 26]| SavedPalette {
        name: name.to_string(),
        colors: colors.iter().map(|s| s.to_string()).collect(),
    };
    vec![group("Catppuccin Latte", &LATTE), group("Catppuccin Mocha", &MOCHA)]
}

/// **Pure**, unit-tested: THE load rule (see the module doc's default and migration
/// sections). `file` is what `palettes.toml` held (`None` = absent or unreadable),
/// `legacy` what the config's retired key held (`None` = key absent). Returns the list
/// to use and whether a MIGRATION WRITE must happen now (true exactly when a legacy list
/// is adopted, because the config's next save drops the key it came from).
pub fn palettes_from(
    file: Option<PalettesFile>,
    legacy: Option<&[SavedPalette]>,
) -> (Vec<SavedPalette>, bool) {
    if let Some(file) = file {
        // A present file is the truth, empty included: deletion sticks.
        return (file.saved_palettes, false);
    }
    if let Some(legacy) = legacy {
        // The config era's list, even empty (a deletion there stays one). Written to the
        // file NOW, before any config save can drop the key it lives in.
        return (legacy.to_vec(), true);
    }
    (default_saved_palettes(), false)
}

/// Move a config-era legacy list into the palettes file, once, if the file does not
/// exist yet: the write half of [`palettes_from`]'s migration arm, callable on its own.
///
/// It exists for the writer the picker never meets (DRAGON-687's duplicate-guard audit,
/// an adjacent find): `store::save` drops the legacy key on EVERY config save because
/// the field is `skip_serializing`, and some processes save the config without ever
/// loading palettes (the resident daemons' setting writes, `note_encoder_auto_hint`'s
/// load-modify-save from a recording worker). A config save from one of those, landing
/// before any picker-shaped process had run [`load_palettes`], would have dropped the
/// only copy of a config-era list. `store::load` calls this whenever it sees the legacy
/// key, so the list reaches the file before any such save can orphan it. Idempotent and
/// cheap: a present file returns immediately, and configs without the key (every config
/// after its first post-era save) never get here at all.
pub fn migrate_legacy_palettes(legacy: &[SavedPalette]) {
    let exists = palettes_path().is_some_and(|p| p.exists());
    if exists {
        return;
    }
    save_palettes(legacy);
}

/// Load the saved palettes, migrating a config-era list into the file the first time
/// (see [`palettes_from`] for the rule this applies).
pub fn load_palettes(legacy: Option<&[SavedPalette]>) -> Vec<SavedPalette> {
    let file = palettes_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<PalettesFile>(&s).ok());
    let (list, migrate) = palettes_from(file, legacy);
    if migrate {
        save_palettes(&list);
    }
    list
}

/// Write the saved palettes: `store::save`'s exact shape (create the dir, serialize,
/// bare write, last writer wins), aimed at the palettes file.
pub fn save_palettes(list: &[SavedPalette]) {
    let Some(path) = palettes_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = PalettesFile { saved_palettes: list.to_vec() };
    if let Ok(s) = toml::to_string(&file) {
        let _ = std::fs::write(path, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, colors: &[&str]) -> SavedPalette {
        SavedPalette {
            name: name.to_string(),
            colors: colors.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The starter set is Latte and Mocha, whole and light-to-dark (the owner's trim;
    /// it was all four flavours first), every group 26 swatches from its own Rosewater
    /// to its own Crust, every value parseable by the app's own colour reader, and no
    /// value shared between the two flavours' entries at the same position (a paste
    /// slip in one table would most likely duplicate a neighbour's).
    #[test]
    fn the_default_is_latte_and_mocha_in_order() {
        let d = default_saved_palettes();
        assert_eq!(
            d.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["Catppuccin Latte", "Catppuccin Mocha"],
            "light to dark, the brand's own spellings"
        );
        for g in &d {
            assert_eq!(g.colors.len(), 26, "{}", g.name);
            for c in &g.colors {
                assert!(
                    crate::app::color_picker::geom::Recent::parse(c).is_some(),
                    "{}: {c} must parse",
                    g.name
                );
            }
        }
        // The published landmarks: each flavour's Rosewater leads and its Crust closes.
        let ends: Vec<(&str, &str)> = d
            .iter()
            .map(|g| (g.colors.first().unwrap().as_str(), g.colors.last().unwrap().as_str()))
            .collect();
        assert_eq!(ends, vec![("#DC8A78", "#DCE0E8"), ("#F5E0DC", "#11111B")]);
        // Positionally distinct across flavours: 26 slots, two distinct values each.
        for slot in 0..26 {
            let mut vals: Vec<&str> = d.iter().map(|g| g.colors[slot].as_str()).collect();
            vals.sort_unstable();
            vals.dedup();
            assert_eq!(vals.len(), 2, "slot {slot} repeats a value across flavours");
        }
    }

    /// THE load rule, all three arms: a present file wins (empty included, no write), a
    /// legacy config list migrates WITH a write (empty included: a config-era deletion
    /// stays one), and only neither answers the default (no write, so the default keeps
    /// applying until the user's first save).
    #[test]
    fn the_load_rule_prefers_file_then_legacy_then_default() {
        let mine = vec![entry("Mine", &["#010203"])];
        // A present file is the truth...
        let (list, write) =
            palettes_from(Some(PalettesFile { saved_palettes: mine.clone() }), None);
        assert_eq!(list, mine);
        assert!(!write);
        // ...even empty: deletion sticks over both the legacy key and the default.
        let (list, write) = palettes_from(
            Some(PalettesFile::default()),
            Some(std::slice::from_ref(&entry("Legacy", &["#040506"]))),
        );
        assert!(list.is_empty(), "an empty FILE beats a legacy list and the default");
        assert!(!write);
        // The config era's list migrates, with the write that makes it once.
        let legacy = vec![entry("QA", &["#0A0B0C"])];
        let (list, write) = palettes_from(None, Some(&legacy));
        assert_eq!(list, legacy, "the owner's QA palettes survive the move");
        assert!(write, "and are written to the file before the config drops the key");
        // A config-era deletion migrates AS a deletion, not as a fresh install.
        let (list, write) = palettes_from(None, Some(&[]));
        assert!(list.is_empty());
        assert!(write);
        // Neither: the starter palette, not yet written.
        let (list, write) = palettes_from(None, None);
        assert_eq!(list, default_saved_palettes());
        assert!(!write);
    }

    /// Deletion sticks at the FILE boundary: an emptied list serializes as a PRESENT
    /// `saved_palettes = []` key (the config's `covermark_prefs = []` behaviour, pinned
    /// here for this file so a toml change cannot silently turn an empty file back into
    /// an absent one), and the round trip keeps answering empty over the default.
    #[test]
    fn deleting_every_palette_sticks_across_the_round_trip() {
        let text = toml::to_string(&PalettesFile::default()).expect("serialize");
        assert!(
            text.contains("saved_palettes = []"),
            "an empty list must serialize as a PRESENT key, or the default would \
             resurrect on the next load: {text}"
        );
        let back: PalettesFile = toml::from_str(&text).expect("parse back");
        let (list, write) = palettes_from(Some(back), None);
        assert!(list.is_empty(), "still deleted after the round trip");
        assert!(!write);
    }

    /// The file round-trips names, colours (alpha spellings included) and ORDER, and the
    /// tolerant colour read the app applies drops junk entries without taking the group.
    #[test]
    fn the_file_round_trips_and_junk_colours_drop_at_the_app_boundary() {
        let file = PalettesFile {
            saved_palettes: vec![
                entry("Sunset", &["#FF8800", "#FF880080"]),
                entry("Empty", &[]),
            ],
        };
        let text = toml::to_string(&file).expect("serialize");
        let back: PalettesFile = toml::from_str(&text).expect("parse back");
        assert_eq!(back, file, "names, colours and order survive");
        // A hand-edited colour that no longer parses is dropped by the app's read
        // (`geom::Recent::parse`), never a reason to fail the file.
        let junk: PalettesFile =
            toml::from_str("[[saved_palettes]]\nname = \"Mixed\"\ncolors = [\"#010203\", \"nope\"]\n")
                .expect("junk colours must not fail the file");
        let kept: Vec<_> = junk.saved_palettes[0]
            .colors
            .iter()
            .filter_map(|s| crate::app::color_picker::geom::Recent::parse(s))
            .collect();
        assert_eq!(kept.len(), 1);
    }

    /// A factory reset cannot reach this file: the config's serialized defaults carry no
    /// palettes key at all (the legacy field is `skip_serializing`), so the reset's
    /// config write has nothing palette-shaped in it, and no reset path calls
    /// [`save_palettes`] (`apply_persisted` no longer touches palettes; this is the
    /// store-side half of that guarantee).
    #[test]
    fn a_factory_reset_writes_no_palette_key_into_the_config() {
        let text = toml::to_string(&super::super::store::defaults()).expect("serialize");
        assert!(
            !text.contains("saved_palettes"),
            "the config must never carry the legacy key again: {text}"
        );
    }
}
