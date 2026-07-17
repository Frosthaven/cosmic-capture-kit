//! The per-DESKTOP profile layer (DRAGON-220).
//!
//! Linux is not one desktop: COSMIC, GNOME, KDE Plasma, and the wlroots family
//! (sway/Hyprland) each keep their wallpaper, theme, and window-behavior config
//! in their own place, with their own quirks. This module is the PROFILE axis —
//! one config/quirk owner per desktop ([`cosmic`], [`gnome`], [`kde`],
//! [`wlroots`]) behind the [`DesktopProfile`] seam, plus the fixed-order
//! [`PROFILES`] registry the wallpaper ladder walks.
//!
//! This axis is deliberately SEPARATE from capture. Capture is judged by
//! PROTOCOL, never by desktop name (DRAGON-93: "judge compositors by protocol,
//! not name") — the screencopy vs portal choice lives in [`crate::platform::backend`]
//! and is keyed on which Wayland protocols the compositor actually advertises, so
//! a wlroots compositor that ships `ext-image-copy-capture` gets the native
//! backend regardless of which profile (if any) matches here. The profiles only
//! own the per-desktop CONFIG readers (where is the wallpaper file, what is the
//! theme) and behavior tweaks (COSMIC tiling exceptions), never the capture path.
//!
//! Note on the other files in this directory: `native/` (screencopy/screenshot),
//! `portal/` (screencast/pipewire/pixfmt), `autostart.rs`, `tray.rs`, and
//! `daemon.rs` are NOT declared here. They are `#[path]`-mounted at their stable
//! legacy module paths from `main.rs` and `platform/mod.rs` (so
//! `crate::screencopy`, `platform::screencast`, `platform::linux_autostart`,
//! etc. keep resolving unchanged); re-declaring them here would double-mount
//! them. This module only owns the DESKTOP-profile submodules + registry.

pub mod cosmic;
pub mod gnome;
pub mod kde;
pub mod wlroots;

/// One Linux desktop's profile: its stable id and how it locates the wallpaper
/// file. Extended per-desktop concern (theme readers, tiling quirks) live in the
/// concrete profile modules; this seam is the common denominator the desktop-
/// agnostic paths (the wallpaper ladder) iterate over.
pub trait DesktopProfile {
    /// A stable machine id for the desktop (`"cosmic"`, `"gnome"`, `"kde"`,
    /// `"wlroots"`) — the key [`detected`] returns and the registry-order tests
    /// assert on. `allow(dead_code)`: consumed by [`detected`] (itself new,
    /// not-yet-wired API this ticket) and the tests, not yet by the runtime.
    #[allow(dead_code)]
    fn id(&self) -> &'static str;
    /// This desktop's current wallpaper image path, if it can be read from the
    /// desktop's own config, else `None` (the caller falls through to the next
    /// profile / degrades gracefully). No is-a-file guarantee here — the ladder
    /// applies the final presence filter once.
    fn wallpaper_path(&self) -> Option<std::path::PathBuf>;
}

/// The desktop profiles in the FIXED wallpaper-ladder order: COSMIC, GNOME, KDE,
/// wlroots. [`crate::wallpaper::detect`] walks this in order, taking the first
/// profile that yields a readable wallpaper path — the same precedence the old
/// `cosmic_bg().or_else(gnome).or_else(kde).or_else(sway_hyprland)` chain had.
pub const PROFILES: [&'static dyn DesktopProfile; 4] = [
    &cosmic::CosmicProfile,
    &gnome::GnomeProfile,
    &kde::KdeProfile,
    &wlroots::WlrootsProfile,
];

/// The profile matching the current session's desktop (from the environment), or
/// `None` when nothing recognizable is set. NEW API (DRAGON-220): no existing
/// call site uses it yet — it exists for future desktop-keyed decisions and for
/// the detection tests. A thin environment reader over the pure [`detect_from`]
/// core, so the mapping logic stays unit-testable without touching env vars.
/// `allow(dead_code)` until a future ticket wires a desktop-keyed decision to it.
#[allow(dead_code)]
pub fn detected() -> Option<&'static dyn DesktopProfile> {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("XDG_SESSION_DESKTOP").ok();
    let id = detect_from(current.as_deref(), session.as_deref())?;
    PROFILES.iter().copied().find(|p| p.id() == id)
}

/// Map the two desktop env vars to a profile id, purely. `XDG_CURRENT_DESKTOP` is
/// a colon-separated list; both vars are matched case-insensitively per entry.
/// THE one desktop-detection core: [`crate::platform::linux::cosmic::is_cosmic`]
/// derives its answer from this ("cosmic" checked first), and [`detected`] maps
/// the returned id onto the registry. Precedence follows the registry order:
/// COSMIC, then GNOME, then KDE/Plasma, then the wlroots compositors
/// (sway/Hyprland). `None` for anything else.
pub fn detect_from(
    xdg_current_desktop: Option<&str>,
    xdg_session_desktop: Option<&str>,
) -> Option<&'static str> {
    // Any colon-split token of either var equals `name` (case-insensitive).
    let has = |name: &str| -> bool {
        [xdg_current_desktop, xdg_session_desktop]
            .into_iter()
            .flatten()
            .any(|v| v.split(':').any(|d| d.eq_ignore_ascii_case(name)))
    };
    if has("COSMIC") {
        return Some("cosmic");
    }
    if has("GNOME") {
        return Some("gnome");
    }
    if has("KDE") || has("plasma") {
        return Some("kde");
    }
    if has("sway") || has("Hyprland") {
        return Some("wlroots");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_registry_order_is_fixed() {
        // The wallpaper ladder depends on this exact order (cosmic, gnome, kde,
        // wlroots) — a reorder would silently change wallpaper precedence.
        let ids: Vec<&str> = PROFILES.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["cosmic", "gnome", "kde", "wlroots"]);
    }

    #[test]
    fn detect_from_maps_each_desktop() {
        // Current-desktop hits.
        assert_eq!(detect_from(Some("COSMIC"), None), Some("cosmic"));
        assert_eq!(detect_from(Some("GNOME"), None), Some("gnome"));
        assert_eq!(detect_from(Some("KDE"), None), Some("kde"));
        assert_eq!(detect_from(Some("sway"), None), Some("wlroots"));
        assert_eq!(detect_from(Some("Hyprland"), None), Some("wlroots"));
        // Unknown / unset.
        assert_eq!(detect_from(Some("XFCE"), None), None);
        assert_eq!(detect_from(None, None), None);
    }

    #[test]
    fn detect_from_is_case_insensitive_and_reads_session_var() {
        // Case-insensitive per the eq_ignore_ascii_case match.
        assert_eq!(detect_from(Some("cosmic"), None), Some("cosmic"));
        assert_eq!(detect_from(Some("gnome"), None), Some("gnome"));
        assert_eq!(detect_from(Some("HYPRLAND"), None), Some("wlroots"));
        // KDE Plasma sessions expose "plasma" rather than "KDE" in some vars.
        assert_eq!(detect_from(Some("plasma"), None), Some("kde"));
        assert_eq!(detect_from(Some("PLASMA"), None), Some("kde"));
        // Falls back to the session var when current-desktop misses.
        assert_eq!(detect_from(None, Some("gnome")), Some("gnome"));
        assert_eq!(detect_from(Some("XFCE"), Some("cosmic")), Some("cosmic"));
    }

    #[test]
    fn detect_from_handles_colon_lists() {
        // XDG_CURRENT_DESKTOP is a colon-separated list; any entry can match.
        assert_eq!(detect_from(Some("ubuntu:GNOME"), None), Some("gnome"));
        assert_eq!(detect_from(Some("COSMIC:GNOME"), None), Some("cosmic")); // order precedence
        assert_eq!(detect_from(Some("pop:COSMIC"), None), Some("cosmic"));
        assert_eq!(detect_from(Some("KDE:plasma"), None), Some("kde"));
    }
}
