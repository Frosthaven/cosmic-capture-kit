//! The GNOME desktop profile (DRAGON-220): the wallpaper source is GNOME's
//! `org.gnome.desktop.background` gsettings key.
//!
//! GNOME-specific capture concerns slot in here as they land: the
//! `GnomeShellBackend` (DRAGON-100, GNOME Shell's org.gnome.Shell.Screenshot /
//! Mutter ScreenCast path) and the DRAGON-97 plain-windows-over-black overlay
//! fallback (GNOME has no per-window Wayland toplevel capture like COSMIC's).
//! Capture selection itself stays protocol-keyed in [`crate::platform::backend`].

use super::DesktopProfile;
use std::path::PathBuf;

/// The GNOME desktop profile.
pub struct GnomeProfile;

impl DesktopProfile for GnomeProfile {
    fn id(&self) -> &'static str {
        "gnome"
    }
    fn wallpaper_path(&self) -> Option<PathBuf> {
        gnome_wallpaper()
    }
}

/// GNOME: `gsettings get org.gnome.desktop.background picture-uri` (the -dark
/// variant when the interface prefers dark). Output shape: `'file:///path'`.
fn gnome_wallpaper() -> Option<PathBuf> {
    let get = |key: &str| -> Option<PathBuf> {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.background", key])
            .output()
            .ok()?;
        parse_gsettings_uri(&String::from_utf8_lossy(&out.stdout))
    };
    // Dark-preferring desktops use picture-uri-dark; try it first, fall back.
    get("picture-uri-dark").filter(|p| p.is_file()).or_else(|| get("picture-uri"))
}

/// Parse a `gsettings get` wallpaper value (`'file:///path'`) into a path: trim
/// whitespace, strip the surrounding single quotes gsettings prints, require the
/// `file://` scheme, and reject the empty path (an unset key prints `''`, and a
/// missing key prints `No such key …` — both yield `None`).
fn parse_gsettings_uri(raw: &str) -> Option<PathBuf> {
    let uri = raw.trim().trim_matches('\'');
    let path = uri.strip_prefix("file://")?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gsettings_uri_strips_quotes_and_scheme() {
        // The exact shape gsettings prints: single-quoted, file:// scheme, a
        // trailing newline from the command's stdout.
        assert_eq!(
            parse_gsettings_uri("'file:///home/user/wall.png'\n"),
            Some(PathBuf::from("/home/user/wall.png"))
        );
        // Spaces in the path survive.
        assert_eq!(
            parse_gsettings_uri("'file:///home/user/My Pictures/w.jpg'"),
            Some(PathBuf::from("/home/user/My Pictures/w.jpg"))
        );
    }

    #[test]
    fn parse_gsettings_uri_none_on_empty_or_no_key() {
        // Unset key → empty quoted string.
        assert_eq!(parse_gsettings_uri("''"), None);
        assert_eq!(parse_gsettings_uri("''\n"), None);
        // `file://` with nothing after it is treated as empty (rejected).
        assert_eq!(parse_gsettings_uri("'file://'"), None);
        // Missing key → gsettings prints an error line with no file:// scheme.
        assert_eq!(parse_gsettings_uri("No such key \"picture-uri-dark\"\n"), None);
        assert_eq!(parse_gsettings_uri(""), None);
    }
}
