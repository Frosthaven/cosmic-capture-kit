//! The wlroots-family desktop profile (DRAGON-220): sway / Hyprland. The
//! wallpaper source is a best-effort parse of the common paper daemons'
//! configs (hyprpaper, sway's `output ... bg`).
//!
//! Modern wlroots ships `ext-image-copy-capture`, so the NATIVE capture backend
//! already works on these compositors via the protocol probe in
//! [`crate::platform::backend`] — this profile is only the config reader, not a
//! capture path. (Older wlroots exposed `wlr-screencopy`; the probe handles
//! both.)

use super::DesktopProfile;
use std::path::PathBuf;

/// The wlroots-family (sway / Hyprland) desktop profile.
pub struct WlrootsProfile;

impl DesktopProfile for WlrootsProfile {
    fn id(&self) -> &'static str {
        "wlroots"
    }
    fn wallpaper_path(&self) -> Option<PathBuf> {
        sway_hyprland()
    }
}

/// sway/hyprland: best-effort parse of hyprpaper.conf (`wallpaper = mon,path` /
/// `preload = path`) and the sway config (`output * bg <path> <mode>`).
fn sway_hyprland() -> Option<PathBuf> {
    let config = dirs::config_dir()?;
    if let Ok(text) = std::fs::read_to_string(config.join("hypr/hyprpaper.conf"))
        && let Some(p) = parse_hyprpaper(&text)
    {
        return Some(p);
    }
    if let Ok(text) = std::fs::read_to_string(config.join("sway/config"))
        && let Some(p) = parse_sway(&text)
    {
        return Some(p);
    }
    None
}

/// The first `wallpaper`/`preload` path of a hyprpaper.conf body, taking the
/// last comma field (`wallpaper = monitor,path`) and expanding a leading `~/`.
/// Pure over the file text (the disk read stays in [`sway_hyprland`]) so the
/// unit tests exercise the REAL parser.
fn parse_hyprpaper(text: &str) -> Option<PathBuf> {
    for line in text.lines() {
        let line = line.trim();
        for key in ["wallpaper", "preload"] {
            if let Some(v) = line.strip_prefix(key)
                && let Some(v) = v.trim_start().strip_prefix('=')
            {
                // `wallpaper = monitor,path` — the path is the last comma field.
                let path = v.rsplit(',').next().unwrap_or(v).trim();
                if !path.is_empty() {
                    return Some(PathBuf::from(shellexpand_home(path)));
                }
            }
        }
    }
    None
}

/// The first `output <which> bg <path> <mode>` path of a sway config body,
/// expanding a leading `~/`. Pure over the file text (the disk read stays in
/// [`sway_hyprland`]) so the unit tests exercise the REAL parser.
fn parse_sway(text: &str) -> Option<PathBuf> {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some("output")
            && let Some(_which) = it.next()
            && it.next() == Some("bg")
            && let Some(path) = it.next()
        {
            return Some(PathBuf::from(shellexpand_home(path)));
        }
    }
    None
}

fn shellexpand_home(p: &str) -> String {
    crate::util::expand_tilde(p).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprpaper_takes_the_path_field() {
        // `wallpaper = monitor,path` — the path is the last comma field.
        assert_eq!(
            parse_hyprpaper("wallpaper = DP-1,/home/u/wall.png\n"),
            Some(PathBuf::from("/home/u/wall.png"))
        );
        // `preload = path` (no monitor prefix) is also accepted.
        assert_eq!(
            parse_hyprpaper("preload = /pics/bg.jpg"),
            Some(PathBuf::from("/pics/bg.jpg"))
        );
        // A bare `wallpaper = ,path` (empty monitor) still yields the path.
        assert_eq!(
            parse_hyprpaper("wallpaper = ,/x/y.png"),
            Some(PathBuf::from("/x/y.png"))
        );
        // Nothing matching → None.
        assert_eq!(parse_hyprpaper("# a comment\nsplash = false\n"), None);
    }

    #[test]
    fn hyprpaper_expands_a_leading_tilde() {
        // The `~/` prefix expands via crate::util::expand_tilde.
        let home = dirs::home_dir().expect("home dir in test env");
        assert_eq!(
            parse_hyprpaper("preload = ~/Pictures/wall.png"),
            Some(home.join("Pictures/wall.png"))
        );
    }

    #[test]
    fn sway_reads_output_bg_line() {
        assert_eq!(
            parse_sway("output * bg /usr/share/backgrounds/x.png fill\n"),
            Some(PathBuf::from("/usr/share/backgrounds/x.png"))
        );
        assert_eq!(
            parse_sway("output HDMI-A-1 bg ~/w.jpg stretch"),
            Some(dirs::home_dir().expect("home").join("w.jpg"))
        );
        // A non-bg output line (e.g. resolution) is ignored.
        assert_eq!(parse_sway("output * resolution 1920x1080\n"), None);
        assert_eq!(parse_sway("bindsym $mod+Return exec term\n"), None);
    }
}
