//! COSMIC wallpaper source: the `cosmic-bg` RON config reader. The COSMIC arm of
//! the [`crate::wallpaper::detect`] ladder (via [`super::CosmicProfile`]).

use std::path::PathBuf;

/// cosmic-bg: RON files under `~/.config/cosmic/com.system76.CosmicBackground/v1`
/// — the shared `all` entry, else any per-output entry (same-on-all off).
pub fn cosmic_bg() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("cosmic/com.system76.CosmicBackground/v1");
    if let Ok(text) = std::fs::read_to_string(dir.join("all"))
        && let Some(p) = parse_cosmic_bg(&text)
    {
        return Some(p);
    }
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        if entry.file_name().to_string_lossy().starts_with("output.")
            && let Ok(text) = std::fs::read_to_string(entry.path())
            && let Some(p) = parse_cosmic_bg(&text)
        {
            return Some(p);
        }
    }
    None
}

/// The first `Path("…")` value of a cosmic-bg RON entry body (the wallpaper
/// `source`), or `None` for non-file sources (gradients/colours). Pure over the
/// file text (the disk reads stay in [`cosmic_bg`]) so the unit tests exercise
/// the REAL parser.
fn parse_cosmic_bg(text: &str) -> Option<PathBuf> {
    let i = text.find("Path(\"")?;
    let rest = &text[i + 6..];
    Some(PathBuf::from(&rest[..rest.find('"')?]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosmic_bg_extracts_the_path_string() {
        // A realistic cosmic-bg `all` / `output.*` RON body: the wallpaper is the
        // first `Path("…")` after the entry's `source:`.
        let ron = r#"(
    output: "all",
    source: Path("/home/user/Pictures/wall.png"),
    filter_by_theme: true,
    rotation_frequency: 300,
)"#;
        assert_eq!(parse_cosmic_bg(ron), Some(PathBuf::from("/home/user/Pictures/wall.png")));
    }

    #[test]
    fn cosmic_bg_parse_takes_first_path_and_handles_spaces() {
        // Paths with spaces survive (the scan is delimited by the closing quote),
        // and only the FIRST Path("…") is taken.
        let ron = r#"( source: Path("/a/My Wallpapers/x.jpg"), fallback: Path("/b/y.png") )"#;
        assert_eq!(parse_cosmic_bg(ron), Some(PathBuf::from("/a/My Wallpapers/x.jpg")));
    }

    #[test]
    fn cosmic_bg_parse_none_without_a_path_entry() {
        // A gradient/color background (no Path) yields None so the ladder falls
        // through to the next profile.
        let ron = r#"( output: "all", source: Color(([0.1, 0.2, 0.3])) )"#;
        assert_eq!(parse_cosmic_bg(ron), None);
        assert_eq!(parse_cosmic_bg(""), None);
    }
}
