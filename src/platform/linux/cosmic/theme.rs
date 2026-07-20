//! COSMIC theme config-file readers: the raw `~/.config/cosmic`
//! `com.system76.CosmicTheme.*` scans (corner radii, window-hint colour, the
//! frosted-glass v2 config) that back the appearance seam on the paths that run
//! without a live `&Theme` (capture compositing, the CLI). The Linux bodies
//! behind the thin wrappers in [`crate::app::theme`] — that module keeps the
//! `GlassConfig` TYPE, the `Rounding` tokens + `window_rule`, the PURE
//! file-format parse helpers (`read_f32_after`, `blur_strength_ordinal`,
//! `alpha_for_strength` — portable, unit-tested on every OS, the `crate::glass`
//! pattern), the libcosmic-config appearance API, and the cross-OS wrappers that
//! delegate here on Linux and fall back to today's off-COSMIC values elsewhere.
//! Only the disk-READING bodies live here; they parse through those helpers.

use crate::app::theme::{
    GlassConfig, Rounding, alpha_for_strength, blur_strength_ordinal, read_f32_after, window_rule,
};

/// Path to the active cosmic theme's `v1/` config dir (Dark or Light).
pub(crate) fn cosmic_theme_dir() -> Option<std::path::PathBuf> {
    let base = dirs::config_dir()?.join("cosmic");
    let dark = std::fs::read_to_string(base.join("com.system76.CosmicTheme.Mode/v1/is_dark"))
        .map(|s| s.trim() != "false")
        .unwrap_or(true);
    let name = if dark {
        "com.system76.CosmicTheme.Dark"
    } else {
        "com.system76.CosmicTheme.Light"
    };
    Some(base.join(name).join("v1"))
}

/// Logical corner radius cosmic-comp draws on windows (theme `radius_s`, first
/// component, through [`window_rule`](crate::app::theme)). Window captures get the
/// same rounding. The disk-reading twin of `Rounding::window`, for the capture
/// paths that have no `&Theme`; falls back to `Rounding::FALLBACK`.
pub(crate) fn window_radius() -> f32 {
    let r = cosmic_theme_dir()
        .and_then(|d| std::fs::read_to_string(d.join("corner_radii")).ok())
        .and_then(|t| read_f32_after(&t, "radius_s:"))
        .unwrap_or(Rounding::FALLBACK.s[0]);
    window_rule(r)
}

/// Whether the active cosmic theme is dark — drives the drop-shadow opacity
/// (cosmic uses 0.45 in dark mode, 0.35 in light).
pub(crate) fn theme_is_dark() -> bool {
    dirs::config_dir()
        .and_then(|d| {
            std::fs::read_to_string(
                d.join("cosmic/com.system76.CosmicTheme.Mode/v1/is_dark"),
            )
            .ok()
        })
        .map(|s| s.trim() != "false")
        .unwrap_or(true)
}

// ── Frosted-glass ("liquid glass") config ────────────────────────────────────
// COSMIC's frosted windows (cosmic-settings → Appearance → Style): the
// compositor blurs the backdrop behind an opted-in surface, and the theme paints
// its surfaces translucent so the blur shows through. Our PINNED libcosmic
// (4657b6a) predates the theme's `frosted`/`alpha_map` fields, so we read them
// straight off the active theme's v2 config on disk and supply the alpha
// ourselves (see `frost_color`). Enrollment is done separately (the two toplevel
// windows set `window::Settings.blur`; DRAGON-217). Ticket DRAGON-218 reuses this
// reader to reproduce the glass inside single-window captures.

/// The active cosmic theme's `v2/` config dir (Dark or Light). The v2 twin of
/// [`cosmic_theme_dir`] — the `frosted`/`alpha_map`/`frosted_windows` keys moved
/// to v2 (the schema recently bumped v1→v2). `None` off COSMIC (no config dir).
fn cosmic_theme_v2_dir() -> Option<std::path::PathBuf> {
    let base = dirs::config_dir()?.join("cosmic");
    let dark = std::fs::read_to_string(base.join("com.system76.CosmicTheme.Mode/v1/is_dark"))
        .map(|s| s.trim() != "false")
        .unwrap_or(true);
    let name = if dark {
        "com.system76.CosmicTheme.Dark"
    } else {
        "com.system76.CosmicTheme.Light"
    };
    Some(base.join(name).join("v2"))
}

/// The user's frosted-glass config for the active theme mode, or `None` off
/// COSMIC (no config dir) or when the v2 `alpha_map` can't be read/parsed (an
/// older schema, or a non-COSMIC desktop) — the callers treat `None` as "no
/// glass, fully opaque, today's look". Read ONCE at launch (theme is fixed for a
/// one-shot session), like the wallpaper/rounding scene reads.
///
/// A missing/unknown `frosted` strength degrades to `Medium` (cosmic-theme's
/// `BlurStrength::default`); a missing `frosted_windows` degrades to `false`
/// (glass off) — either way the reader stays `Some` on a healthy COSMIC config.
pub(crate) fn glass_config() -> Option<GlassConfig> {
    // Kill-switch: `CCK_NO_GLASS=1` disables every frosted-glass behavior (window
    // enrollment, translucent chrome, capture glass) as if the theme had frosting
    // off — the `--test glass-shot` harness's A/B lever, and an escape hatch.
    if std::env::var_os("CCK_NO_GLASS").is_some_and(|v| v == "1") {
        return None;
    }
    let dir = cosmic_theme_v2_dir()?;
    let alpha_map = std::fs::read_to_string(dir.join("alpha_map")).ok()?;
    let strength_ordinal = std::fs::read_to_string(dir.join("frosted"))
        .ok()
        .and_then(|s| blur_strength_ordinal(&s))
        .unwrap_or(6); // Medium
    let alpha = alpha_for_strength(&alpha_map, strength_ordinal)?;
    let frosted_windows = std::fs::read_to_string(dir.join("frosted_windows"))
        .map(|s| s.trim() == "true")
        .unwrap_or(false);
    Some(GlassConfig { strength_ordinal, alpha, frosted_windows })
}
