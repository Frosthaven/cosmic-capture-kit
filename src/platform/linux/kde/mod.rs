//! The KDE Plasma desktop profile (DRAGON-220): the wallpaper source is the
//! desktop applet config's `Image=` entries.
//!
//! KWin advertises `zwlr-layer-shell`, so the layer-shell overlay path already
//! works there (DRAGON-97 treated KDE as verification-only). A future KDE
//! backdrop-blur protocol (KWin's own effect enrollment) would slot in here
//! alongside the config reader. Capture selection stays protocol-keyed in
//! [`crate::platform::backend`].

use super::DesktopProfile;
use std::path::PathBuf;

/// The KDE Plasma desktop profile.
pub struct KdeProfile;

impl DesktopProfile for KdeProfile {
    fn id(&self) -> &'static str {
        "kde"
    }
    fn wallpaper_path(&self) -> Option<PathBuf> {
        kde_wallpaper()
    }
}

/// KDE Plasma: the desktop applet config's wallpaper `Image=` entries.
fn kde_wallpaper() -> Option<PathBuf> {
    let path = dirs::config_dir()?.join("plasma-org.kde.plasma.desktop-appletsrc");
    let text = std::fs::read_to_string(path).ok()?;
    parse_appletsrc(&text)
}

/// The first `Image=` entry of an appletsrc body as a path: `file://` stripped
/// when present, empty values skipped. Pure over the file text (the disk read
/// stays in [`kde_wallpaper`]) so the unit tests exercise the REAL parser.
fn parse_appletsrc(text: &str) -> Option<PathBuf> {
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Image=") {
            let v = v.trim().strip_prefix("file://").unwrap_or(v.trim());
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appletsrc_reads_the_image_line() {
        // A realistic appletsrc section: the wallpaper is the `Image=` value, and
        // the `file://` scheme is stripped when present.
        let cfg = "\
[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///usr/share/wallpapers/Next/contents/images/1920x1080.png
SlidePaths=/usr/share/wallpapers/
";
        assert_eq!(
            parse_appletsrc(cfg),
            Some(PathBuf::from("/usr/share/wallpapers/Next/contents/images/1920x1080.png"))
        );
    }

    #[test]
    fn appletsrc_accepts_a_bare_path_and_skips_empties() {
        // A bare (non-file://) path is taken as-is.
        assert_eq!(
            parse_appletsrc("Image=/home/user/wall.jpg\n"),
            Some(PathBuf::from("/home/user/wall.jpg"))
        );
        // An empty Image= line is skipped; a later valid one wins.
        let cfg = "Image=\nOther=1\nImage=file:///a/b.png\n";
        assert_eq!(parse_appletsrc(cfg), Some(PathBuf::from("/a/b.png")));
        // No Image= line at all → None.
        assert_eq!(parse_appletsrc("Color=0,0,0\n"), None);
    }
}
