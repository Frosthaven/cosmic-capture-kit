//! The COSMIC desktop profile (DRAGON-220): everything COSMIC-BRAND about a
//! session, gathered behind [`CosmicProfile`].
//!
//! What lives here is the config + quirk knowledge that is specific to the
//! System76 COSMIC desktop, not to a Wayland protocol:
//! - [`compositor`]: cctk toplevel enrichment (the Linux body of the
//!   [`crate::platform::compositor`] facade — toplevel-info enumeration +
//!   toplevel-management activation),
//! - [`theme`]: the `~/.config/cosmic` `com.system76.CosmicTheme.*` file readers
//!   (corner radii, window-hint colour, frosted-glass alpha map),
//! - [`wallpaper`]: `com.system76.CosmicBackground` RON reader,
//! - [`quirks`]: the `com.system76.CosmicSettings.WindowRules` tiling-exception
//!   writer that floats the preview window.
//!
//! Future COSMIC-only integrations (a native compositor-blur enrollment beyond
//! the current `window::Settings.blur` path, cosmic-config live watchers) slot in
//! here rather than sprinkling `is_cosmic()` checks through the portable core.

pub mod compositor;
pub mod quirks;
pub mod theme;
pub mod wallpaper;

use super::DesktopProfile;

/// The COSMIC desktop profile. Its wallpaper source is `cosmic-bg`'s RON config.
pub struct CosmicProfile;

impl DesktopProfile for CosmicProfile {
    fn id(&self) -> &'static str {
        "cosmic"
    }
    fn wallpaper_path(&self) -> Option<std::path::PathBuf> {
        wallpaper::cosmic_bg()
    }
}

/// Whether we're running under the COSMIC desktop (auto-tiling compositor). Read from
/// `XDG_CURRENT_DESKTOP` (a colon-separated list) / `XDG_SESSION_DESKTOP`, asking the
/// ONE detection core ([`super::detect_from`]: the same case-insensitive colon-split
/// match, COSMIC checked first) whether those vars resolve to the "cosmic" profile id.
pub fn is_cosmic() -> bool {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("XDG_SESSION_DESKTOP").ok();
    super::detect_from(current.as_deref(), session.as_deref()) == Some("cosmic")
}
