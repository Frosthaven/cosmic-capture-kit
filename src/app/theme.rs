//! COSMIC theme / config readers and shared theme-derived colours.
//!
//! THE appearance seam (DRAGON-117): the user's COSMIC preferences — accent
//! colour and rounding rule (cosmic-settings → round / slightly round /
//! square) — are read HERE, once, and every custom style closure / canvas
//! draw in the app goes through the helpers below ([`rounding`], [`accent`],
//! [`on_accent`], …) instead of reaching into `theme.cosmic()` conventions
//! independently. Off COSMIC (or when a setting can't be found) the helpers
//! degrade to the documented fallbacks, which reproduce the app's historical
//! hardcoded look exactly — never something unstyled.
//!
//! Also reads the active COSMIC theme files on disk (corner radii, window hint
//! colours, wallpaper path) for the paths that run without a `&Theme` (capture
//! compositing, the CLI), and exposes blended colour helpers that adapt to
//! dark/light themes without relying on alpha (which button widgets can clobber
//! for icons).

use cosmic::Theme;
use cosmic::Task;
use cosmic::iced::Color;

// ── The appearance-override seam (DRAGON-139) ────────────────────────────────
// The user can override the system theme's mode / accent / rounding (Settings →
// General → Appearance, when "Use System Settings" is OFF). These helpers build
// and apply the resulting process-global theme, and are PORTABLE by construction
// (see the doc on `apply_appearance` — no COSMIC-desktop assumption).

use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::cosmic_theme::{CornerRadii, Roundness, ThemeBuilder};

/// The persisted roundness byte (0 round / 1 slightly / 2 square) as a cosmic
/// [`Roundness`]. Any out-of-range value degrades to `Round` (the default look).
pub(crate) fn roundness_from_u8(n: u8) -> Roundness {
    match n {
        1 => Roundness::SlightlyRound,
        2 => Roundness::Square,
        _ => Roundness::Round,
    }
}

/// Resolve whether the persisted appearance MODE byte (0 automatic / 1 dark /
/// 2 light) wants a dark base right now. `automatic` defers to [`system_is_dark`]
/// (read at apply time); dark/light force the base regardless of the system.
pub(crate) fn mode_wants_dark(mode: u8) -> bool {
    match mode {
        1 => true,
        2 => false,
        _ => system_is_dark(),
    }
}

/// Whether the SYSTEM is currently in dark mode — the ONE portable dark/light
/// probe this feature uses (for Mode = Automatic and the base-theme fallback).
///
/// Portability seam:
/// - Linux / default: cosmic-config's `ThemeMode` read directly (the same source
///   libcosmic's `system_preference()` uses) — NEVER the process theme, which
///   reflects our own override once one is applied.
/// - macOS: `AppleInterfaceStyle` via `NSUserDefaults` (libcosmic has no mac
///   dark signal, so Automatic would otherwise be stuck dark) — see
///   `crate::platform::mac::appearance`.
/// - Windows: future; falls to the default arm for now.
pub(crate) fn system_is_dark() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::appearance::system_is_dark()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // MUST read the system's own mode, never the process theme: once an
        // override theme is applied, `cosmic::theme::active()` IS the override, so
        // Automatic would re-read its own output (switching Light → Automatic
        // stayed light). Mirror libcosmic's `system_preference()` instead:
        // cosmic-config's ThemeMode, defaulting to dark exactly as libcosmic does
        // (also the fallback on a non-COSMIC system with no config).
        use cosmic::cosmic_theme::ThemeMode;
        ThemeMode::config()
            .ok()
            .and_then(|cfg| ThemeMode::is_dark(&cfg).ok())
            .unwrap_or(true)
    }
}

/// Build + apply the process-global theme for the current appearance settings,
/// as a `Task` the caller returns from `update` (or batches into `init`).
///
/// **Portability contract (do not regress with a Linux-only read):** when
/// `use_system` is ON we simply follow `cosmic::theme::system_preference()`.
/// When OFF, the override BASE is the system COSMIC `ThemeBuilder` config when it
/// exists on disk (a COSMIC desktop), and libcosmic's built-in `dark()`/`light()`
/// default otherwise (macOS / Windows / a COSMIC desktop with no config) — that
/// built-in default is exactly what libcosmic renders on those platforms, so the
/// accent/roundness overrides compose onto the same base the user actually sees.
/// The dark/light choice comes from [`mode_wants_dark`] (the portable
/// [`system_is_dark`] seam for Automatic), never a raw cosmic-config read here.
pub(crate) fn apply_appearance<M: Send + 'static>(
    use_system: bool,
    mode: u8,
    accent: Option<[f32; 3]>,
    roundness: u8,
) -> Task<cosmic::Action<M>> {
    if use_system {
        // NOT system_preference(): that reads cosmic-config's ThemeMode, which
        // doesn't exist off COSMIC (macOS: always dark, ignoring the real
        // AppleInterfaceStyle). Route the dark/light pick through the portable
        // [`system_is_dark`] seam instead — on Linux ThemeMode is exactly what
        // that seam reads, so the result is identical there (DRAGON-144).
        let dark = system_is_dark();
        let t = if dark { cosmic::theme::system_dark() } else { cosmic::theme::system_light() };
        // Off COSMIC even the "system" themes are default-filled cosmic-config
        // entries that BUILD DARK (the same failure the override path verifies
        // against below), so check the output and fall back to libcosmic's
        // built-in palette; on a healthy COSMIC config the output always
        // agrees and the real system theme passes through untouched.
        let t = if t.cosmic().is_dark == dark {
            t
        } else {
            cosmic::Theme::custom(std::sync::Arc::new(
                if dark { ThemeBuilder::dark() } else { ThemeBuilder::light() }.build(),
            ))
        };
        return cosmic::command::set_theme(t);
    }
    let dark = mode_wants_dark(mode);
    // Best-effort system-theme base; fall back to libcosmic's built-in default
    // (the expected path off a COSMIC desktop, not an error edge).
    let builder = if dark {
        ThemeBuilder::dark_config()
            .ok()
            .and_then(|c| ThemeBuilder::get_entry(&c).ok())
            .unwrap_or_else(ThemeBuilder::dark)
    } else {
        ThemeBuilder::light_config()
            .ok()
            .and_then(|c| ThemeBuilder::get_entry(&c).ok())
            .unwrap_or_else(ThemeBuilder::light)
    };
    let decorate = |mut b: ThemeBuilder| {
        if let Some([r, g, bl]) = accent {
            b = b.accent(cosmic::cosmic_theme::palette::Srgb::new(r, g, bl));
        }
        b.corner_radii(CornerRadii::from(roundness_from_u8(roundness)))
    };
    let mut built = decorate(builder).build();
    // Verify the OUTPUT against the requested mode (DRAGON-144): cosmic-config
    // returns a DEFAULT-FILLED entry — which builds DARK — instead of an error
    // when the theme files don't exist (macOS/Windows always; a COSMIC system
    // with no saved theme), so the built-in fallback above never fired and
    // Light mode silently rendered dark. When the build disagrees, rebuild from
    // libcosmic's built-in palette, which is what those systems actually render;
    // a healthy COSMIC config always agrees and is untouched.
    if built.is_dark != dark {
        built = decorate(if dark { ThemeBuilder::dark() } else { ThemeBuilder::light() }).build();
    }
    cosmic::command::set_theme(cosmic::Theme::custom(std::sync::Arc::new(built)))
}

// ── The rounding seam ────────────────────────────────────────────────────────

/// The user's COSMIC rounding rule, as the active theme's corner-radius tokens.
/// cosmic-settings' three choices map to (xs / s / m / xl): round =
/// 4 / 8 / 16 / 160, slightly round = 2 / 8 / 8 / 8, square = 2 / 2 / 2 / 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rounding {
    /// Extra-small: chips, swatches, small marks.
    pub(crate) xs: [f32; 4],
    /// Small: menus/popovers, panels, cards, content strips.
    pub(crate) s: [f32; 4],
    /// Medium: large surfaces (dialog cards, overlay pills/toasts).
    pub(crate) m: [f32; 4],
    /// Extra-large: BUTTONS, button groups, and their border rings — the token
    /// libcosmic gives standard/icon buttons and segmented controls, so custom
    /// button chrome rounds exactly like the framework buttons inside it. Only
    /// "round" pushes it past `s` (to 160, i.e. a capsule — the quad renderer
    /// clamps the radius to half the widget, so it's safe to pass through; cap
    /// it manually for canvas paths).
    pub(crate) xl: [f32; 4],
}

impl Rounding {
    /// The non-COSMIC fallback: libcosmic's built-in tokens (the "round"
    /// preset) — off COSMIC the app styles exactly like default COSMIC.
    pub(crate) const FALLBACK: Rounding =
        Rounding { xs: [4.0; 4], s: [8.0; 4], m: [16.0; 4], xl: [160.0; 4] };

    /// The small token as a scalar (the tokens are uniform in practice) — for
    /// canvas draws that take one radius.
    pub(crate) fn s1(&self) -> f32 {
        self.s[0]
    }

    /// The button token as a scalar — for the segmented pairs' OUTER corners
    /// (inner corners are 0, like libcosmic's segmented controls). Quad
    /// rendering clamps it to half the segment, so "round" reads as a capsule.
    pub(crate) fn xl1(&self) -> f32 {
        self.xl[0]
    }

    /// The rounding cosmic-comp draws on WINDOW corners: `radius_s + 4` (when
    /// `radius_s >= 4`), per component — the same rule as [`window_radius`],
    /// applied to the live theme. CSD window containers use this so their
    /// corners meet the compositor's.
    pub(crate) fn window(&self) -> [f32; 4] {
        self.s.map(window_rule)
    }
}

/// The user's rounding preference, from the active theme. Off COSMIC the
/// theme carries libcosmic's built-in tokens = [`Rounding::FALLBACK`].
pub(crate) fn rounding(theme: &Theme) -> Rounding {
    let radii = &theme.cosmic().corner_radii;
    Rounding {
        xs: radii.radius_xs,
        s: radii.radius_s,
        m: radii.radius_m,
        xl: radii.radius_xl,
    }
}

// ── The accent seam ──────────────────────────────────────────────────────────

/// The user's accent colour preference (cosmic-settings → accent). Off COSMIC
/// this is libcosmic's built-in accent — the lavender the app always showed.
pub(crate) fn accent(theme: &Theme) -> Color {
    theme.cosmic().accent_color().into()
}

/// The foreground drawn ON an accent fill (glyphs on selected segments).
pub(crate) fn on_accent(theme: &Theme) -> Color {
    theme.cosmic().on_accent_color().into()
}

/// The accent variant tuned for TEXT/icons on the plain background.
pub(crate) fn accent_text(theme: &Theme) -> Color {
    theme.cosmic().accent_text_color().into()
}

// ── Shared segmented-pair styling ────────────────────────────────────────────

/// The ONE segmented-toggle segment style — the capture toolbar's
/// scanner/image/video kind pair AND the preview's pointer/pan and
/// pointer/razor pairs all render through here, so they can never drift
/// apart again. Active = accent fill (dimmed on hover) with the on-accent
/// glyph; inactive = the group's divider fill with the group background's own
/// colour as the glyph (an embossed look that also pops over the stronger
/// hover fill). Only the pair's outer corners round, at the button token.
pub(crate) fn segment_style(
    t: &Theme,
    active: bool,
    hovered: bool,
    round_left: bool,
    round_right: bool,
) -> cosmic::widget::button::Style {
    let c = t.cosmic();
    let r = rounding(t).xl1();
    let rl = if round_left { r } else { 0.0 };
    let rr = if round_right { r } else { 0.0 };
    let bg = if active {
        let mut a = accent(t);
        if hovered {
            a.a = 0.8;
        }
        a
    } else if hovered {
        // A fixed blend toward the foreground, strong enough to separate from
        // BOTH the group base and the segment's resting divider fill (in light
        // mode those sit within a few percent of each other, which washed the
        // hover out entirely).
        state_mix(t, 0.35)
    } else {
        c.background.component.divider.into()
    };
    let icon_color = if active {
        on_accent(t)
    } else {
        c.background.component.base.into()
    };
    cosmic::widget::button::Style {
        background: Some(cosmic::iced::Background::Color(bg)),
        border_radius: [rl, rr, rr, rl].into(),
        icon_color: Some(icon_color),
        ..cosmic::widget::button::Style::new()
    }
}

// ── Shared chrome constants (deliberately NOT theme-following) ───────────────

/// Modal-backdrop scrim behind in-app dialogs (overwrite / reset / mic test).
pub(crate) const SCRIM: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.55 };

/// The record/stop red family — semantic "recording" colour, deliberately not
/// the accent. Live chip, its hover, and the darker paused/countdown pair.
pub(crate) const RECORD: Color = Color { r: 0.85, g: 0.20, b: 0.20, a: 1.0 };
/// [`RECORD`], brightened on hover.
pub(crate) const RECORD_HOVER: Color = Color { r: 0.95, g: 0.30, b: 0.30, a: 1.0 };
/// The darker not-live red (paused recording, pre-capture countdown).
pub(crate) const RECORD_DIM: Color = Color { r: 0.52, g: 0.11, b: 0.11, a: 1.0 };
/// [`RECORD_DIM`], brightened on hover.
pub(crate) const RECORD_DIM_HOVER: Color = Color { r: 0.66, g: 0.15, b: 0.15, a: 1.0 };

// ── COSMIC config readers (Linux bodies in platform::linux::cosmic::theme) ───
// DRAGON-220: the raw `~/.config/cosmic` reading BODIES for the helpers below
// live in the COSMIC profile (`platform::linux::cosmic::theme`); the wrappers
// here keep every caller's path + signature stable and delegate on Linux, while
// preserving TODAY's off-COSMIC values on macOS/Windows byte-for-byte (the values
// the disk reads already produced there, since the COSMIC config dir never
// exists). `GlassConfig`, `Rounding`, `window_rule`, the PURE file-format parse
// helpers (`read_f32_after`, the glass alpha-map parsers), and the
// libcosmic-config appearance API stay defined here — the parsers are portable
// math kept compiled + unit-tested on every OS (the `crate::glass` pattern),
// even though only the Linux profile reader consumes them non-test.

/// First float after `key` in `text` (parses cosmic's RON-ish config), up to the
/// next comma or newline.
// Pure parse, portable + unit-tested on every OS; only the Linux COSMIC profile
// reader consumes it non-test — the same cfg_attr(dead_code) pattern as glass.rs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn read_f32_after(text: &str, key: &str) -> Option<f32> {
    let pos = text.find(key)?;
    let after = &text[pos + key.len()..];
    let end = after.find([',', '\n']).unwrap_or(after.len());
    after[..end].trim().trim_start_matches('(').trim().parse().ok()
}

/// cosmic-comp rounds window corners at radius_s + 4 (when radius_s >= 4), which
/// is more aggressive than radius_s alone — match it (see window.rs render_elements).
/// `pub(crate)`: the COSMIC theme reader (`platform::linux::cosmic::theme::window_radius`)
/// applies the same rule to its disk-read radius (DRAGON-220).
pub(crate) fn window_rule(r: f32) -> f32 {
    if r < 4.0 { r } else { r + 4.0 }
}

/// Logical corner radius cosmic-comp draws on windows (theme `radius_s`, first
/// component, through [`window_rule`]). Window captures get the same rounding.
/// The disk-reading twin of [`Rounding::window`], for the capture paths that
/// have no `&Theme`; falls back to [`Rounding::FALLBACK`].
pub(crate) fn window_radius() -> f32 {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::cosmic::theme::window_radius()
    }
    // Off COSMIC the corner_radii file never exists, so the reader fell straight
    // to the fallback radius through the rule — reproduce that exactly.
    #[cfg(not(target_os = "linux"))]
    {
        window_rule(Rounding::FALLBACK.s[0])
    }
}

/// Whether the active cosmic theme is dark — drives the drop-shadow opacity
/// (cosmic uses 0.45 in dark mode, 0.35 in light).
pub(crate) fn theme_is_dark() -> bool {
    // macOS: the COSMIC config file never exists, so the Linux read would pin this
    // to its always-true default; the system appearance probe is the honest
    // equivalent of "the system theme is dark" there (DRAGON-144).
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::appearance::system_is_dark()
    }
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::cosmic::theme::theme_is_dark()
    }
    // Other (e.g. a future Windows build): no COSMIC config dir, so the reader's
    // `unwrap_or(true)` default — dark — is the historical result.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        true
    }
}

/// The active-window hint colour: `window_hint` if set, else the accent colour
/// (matching cosmic-comp's `active_window_hint`). RGBA, opaque. Falls back to the
/// cosmic default lavender. Read for the Linux system-accent default of the
/// reconstructed Active window-capture border (`crate::decoration::accent_rgba`,
/// DRAGON-191) and for the resident tray icon tint.
pub(crate) fn active_hint_color() -> [u8; 4] {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::cosmic::theme::active_hint_color()
    }
    // Off COSMIC no theme dir is read, so the reader fell through to the cosmic
    // default lavender — the historical value.
    #[cfg(not(target_os = "linux"))]
    {
        [151, 125, 236, 255]
    }
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

/// The glass ordinal → `alpha_map` field names, in `BlurStrength` order (0..=13).
/// Matches cosmic-theme's `AlphaMap`/`BlurStrength::try_from` (theme.rs:1671/1696
/// @ rev 96a8204): `blurred_alpha(frosted)` selects the field at the strength's
/// ordinal. Read by name (not position) so a reordered RON never mis-maps.
// Pure table, portable + unit-tested on every OS; only the Linux COSMIC profile
// reader consumes it non-test — the same cfg_attr(dead_code) pattern as glass.rs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ALPHA_MAP_FIELDS: [&str; 14] = [
    "extremely_low",
    "extremely_low_2",
    "very_low",
    "very_low_2",
    "low",
    "low_2",
    "medium",
    "medium_2",
    "high",
    "high_2",
    "very_high",
    "very_high_2",
    "extremely_high",
    "extremely_high_2",
];

/// The user's frosted-glass preferences for the ACTIVE theme mode, read from
/// disk. `strength_ordinal` is the `frosted` `BlurStrength` (0..=13);
/// `alpha` is the `alpha_map` entry that strength selects (cosmic-theme's
/// `blurred_alpha`), i.e. the surface opacity a frosted window paints at;
/// `frosted_windows` is the user's global "frost normal windows" toggle.
///
/// The TYPE stays here (portable — `frost_color` and every chrome closure holds
/// an `Option<GlassConfig>`); the disk READER that fills it from the COSMIC v2
/// theme config lives in `platform::linux::cosmic::theme` (DRAGON-220).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GlassConfig {
    pub(crate) strength_ordinal: u8,
    pub(crate) alpha: f32,
    pub(crate) frosted_windows: bool,
}

/// The `BlurStrength` variant name → its ordinal (0..=13), or `None` for an
/// unknown token. Mirrors cosmic-theme's `BlurStrength::try_from(u8)`.
/// `pub(crate)`: the COSMIC v2 reader (`platform::linux::cosmic::theme`) parses
/// its `frosted` file through this.
// Pure parse, portable + unit-tested on every OS; only the Linux COSMIC profile
// reader consumes it non-test — the same cfg_attr(dead_code) pattern as glass.rs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn blur_strength_ordinal(name: &str) -> Option<u8> {
    Some(match name.trim() {
        "ExtremelyLow" => 0,
        "ExtremelyLow2" => 1,
        "VeryLow" => 2,
        "VeryLow2" => 3,
        "Low" => 4,
        "Low2" => 5,
        "Medium" => 6,
        "Medium2" => 7,
        "High" => 8,
        "High2" => 9,
        "VeryHigh" => 10,
        "VeryHigh2" => 11,
        "ExtremelyHigh" => 12,
        "ExtremelyHigh2" => 13,
        _ => return None,
    })
}

/// The `f32` value of a NAMED field in an `alpha_map` RON body (one `field: v,`
/// per line). Matches on the exact `field:` prefix so `low` never captures
/// `very_low`/`extremely_low`/`low_2`.
// Pure parse, portable + unit-tested on every OS; only the Linux COSMIC profile
// reader consumes it non-test — the same cfg_attr(dead_code) pattern as glass.rs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn alpha_map_field(text: &str, field: &str) -> Option<f32> {
    text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(field)?.strip_prefix(':')?;
        rest.trim().trim_end_matches(',').trim().parse().ok()
    })
}

/// The surface alpha for a glass `strength_ordinal`, read by field name from an
/// `alpha_map` RON body — cosmic-theme's `blurred_alpha(frosted)`, off disk.
/// `pub(crate)`: the COSMIC v2 reader (`platform::linux::cosmic::theme`) maps
/// the strength it read to the alpha through this.
// Pure parse, portable + unit-tested on every OS; only the Linux COSMIC profile
// reader consumes it non-test — the same cfg_attr(dead_code) pattern as glass.rs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn alpha_for_strength(alpha_map: &str, ordinal: u8) -> Option<f32> {
    let field = ALPHA_MAP_FIELDS.get(ordinal as usize)?;
    alpha_map_field(alpha_map, field)
}

/// The user's frosted-glass config for the active theme mode, or `None` off
/// COSMIC (no config dir) or when the v2 `alpha_map` can't be read/parsed (an
/// older schema, or a non-COSMIC desktop) — the callers treat `None` as "no
/// glass, fully opaque, today's look". Read ONCE at launch (theme is fixed for a
/// one-shot session), like the wallpaper/rounding scene reads. The COSMIC reader
/// (v2 dir scan, the file reads, the `CCK_NO_GLASS` kill-switch) lives in the
/// COSMIC profile and parses through the helpers above; off Linux there is no v2
/// theme config, so this is `None` exactly as before (DRAGON-220).
pub(crate) fn glass_config() -> Option<GlassConfig> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::cosmic::theme::glass_config()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Whether to enroll a fresh toplevel WINDOW in the compositor's backdrop blur:
/// the user has frosted windows on ([`glass_config`]). Portable by the seam —
/// `None` off COSMIC / macOS (no v2 theme config) yields `false`, so the window
/// opens un-enrolled and fully opaque exactly as before. (The winit `blur` flag
/// is macOS window vibrancy on mac, a separate effect tracked under DRAGON-166;
/// gating here keeps that off.)
pub(crate) fn glass_windows_enabled() -> bool {
    glass_config().is_some_and(|g| g.frosted_windows)
}

/// Overlay the frosted-glass surface alpha onto an opaque base colour, when the
/// user has frosted windows on. `glass` is the launch-time [`glass_config`] read;
/// `None` or `frosted_windows == false` returns the colour untouched (today's
/// fully-opaque look). Mirrors cosmic-theme's `transparent_bg` derivation
/// (`blurred_alpha` applied to the container base), which our pinned libcosmic
/// doesn't do. DRAGON-139: called on whatever base theme is live, so a user
/// appearance override frosts its OWN surface colours.
pub(crate) fn frost_color(mut c: Color, glass: Option<GlassConfig>) -> Color {
    if let Some(g) = glass
        && g.frosted_windows
    {
        c.a = g.alpha;
    }
    c
}

/// The desktop wallpaper image path, wherever this session keeps it (the
/// detection ladder in [`crate::wallpaper::detect`]: cosmic-bg → GNOME → KDE →
/// sway/hyprland). Used as the window-picker background and the capture scene's
/// wallpaper layer; `None` degrades those gracefully.
pub(crate) fn wallpaper_path() -> Option<std::path::PathBuf> {
    crate::wallpaper::detect()
}

// ── Theme-derived colour helpers ─────────────────────────────────────────────
// (Originally in style.rs; moved here so all theme/colour concerns live together.)

/// Blend the foreground (on-background) colour toward the background by `t`
/// (0.0 = full foreground, 1.0 = background).
fn toward_bg(theme: &Theme, t: f32) -> Color {
    let cosmic = theme.cosmic();
    let on: Color = cosmic.on_bg_color().into();
    let bg: Color = cosmic.background.base.into();
    Color::from_rgb(
        on.r * (1.0 - t) + bg.r * t,
        on.g * (1.0 - t) + bg.g * t,
        on.b * (1.0 - t) + bg.b * t,
    )
}

/// A very faint, adaptive tone — e.g. the reset icon at rest.
pub(crate) fn subdued(theme: &Theme) -> Color {
    toward_bg(theme, 0.78)
}

/// Unified toolbar toggle-icon states: `Off` is a subtle wash, `On` renders accent
/// (or white over a meter fill). Toggles work in every mode, so there is no
/// disabled state — the subdued colour alone carries on/off.
pub(crate) const MIX_OFF: f32 = 0.40;

/// Blend the group background toward its foreground by `amount` — the shared
/// dimming primitive behind every toolbar toggle state.
pub(crate) fn state_mix(t: &Theme, amount: f32) -> Color {
    let c = t.cosmic();
    let b = c.background.component.base;
    let o = c.background.component.on;
    let mix = |x: f32, y: f32| x + (y - x) * amount;
    Color::from_rgb(mix(b.red, o.red), mix(b.green, o.green), mix(b.blue, o.blue))
}

// ── Canonical semantic palette ───────────────────────────────────────────────
// The single source of the success / warning / error (green / amber / red) colours
// used everywhere in the app: status captions, health-check icons + nav tint, the
// audio level meter, and the mic test. Tuned for the dark COSMIC default; the helper
// functions deepen them for legibility on light themes. Use the CONSTANTS for fills /
// meters (drawn where no `&Theme` is handy) and the FUNCTIONS for text / icons.

/// Canonical success colour (green).
pub(crate) const SUCCESS: Color = Color { r: 0.36, g: 0.80, b: 0.45, a: 1.0 };
/// Canonical warning colour (amber).
pub(crate) const WARNING: Color = Color { r: 0.97, g: 0.73, b: 0.28, a: 1.0 };
/// Canonical error/danger colour (red).
pub(crate) const DANGER: Color = Color { r: 0.93, g: 0.36, b: 0.34, a: 1.0 };

/// Whether the active theme's background is light — so the semantic colours need
/// deepening to stay legible as text (the bright variants wash out on near-white).
fn bg_is_light(theme: &Theme) -> bool {
    let bg: Color = theme.cosmic().background.base.into();
    0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b >= 0.5
}

/// Success colour for text/icons (green) — [`SUCCESS`], deepened on light themes.
pub(crate) fn success(theme: &Theme) -> Color {
    if bg_is_light(theme) {
        Color::from_rgb(0.16, 0.52, 0.30)
    } else {
        SUCCESS
    }
}

/// Warning colour for text/icons (amber) — [`WARNING`], deepened on light themes.
pub(crate) fn warning(theme: &Theme) -> Color {
    if bg_is_light(theme) {
        Color::from_rgb(0.72, 0.42, 0.0)
    } else {
        WARNING
    }
}

/// Error/danger colour for text/icons (red) — [`DANGER`], deepened on light themes.
pub(crate) fn danger(theme: &Theme) -> Color {
    if bg_is_light(theme) {
        Color::from_rgb(0.78, 0.15, 0.12)
    } else {
        DANGER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_fallback_matches_libcosmic_builtin_tokens() {
        // The documented non-COSMIC degradation: libcosmic's built-in theme
        // must carry exactly the historical hardcoded radii. If libcosmic ever
        // changes its defaults, this catches the silent appearance shift.
        let d = cosmic::cosmic_theme::CornerRadii::default();
        let fb = Rounding::FALLBACK;
        assert_eq!(fb.xs, d.radius_xs);
        assert_eq!(fb.s, d.radius_s);
        assert_eq!(fb.m, d.radius_m);
        assert_eq!(fb.xl, d.radius_xl);
    }

    #[test]
    fn window_rule_adds_four_only_at_or_above_four() {
        assert_eq!(window_rule(0.0), 0.0);
        assert_eq!(window_rule(2.0), 2.0); // square preset stays subtle
        assert_eq!(window_rule(3.9), 3.9);
        assert_eq!(window_rule(4.0), 8.0);
        assert_eq!(window_rule(8.0), 12.0); // round preset = cosmic-comp's 12
    }

    #[test]
    fn rounding_window_applies_the_rule_per_component() {
        let r = Rounding { s: [8.0, 2.0, 4.0, 0.0], ..Rounding::FALLBACK };
        assert_eq!(r.window(), [12.0, 2.0, 8.0, 0.0]);
    }

    // ── Appearance-override mappings (DRAGON-139) ────────────────────────────

    #[test]
    fn roundness_u8_maps_to_cosmic_roundness_and_radii() {
        // The three exposed choices plus the out-of-range degrade-to-Round.
        assert_eq!(roundness_from_u8(0), Roundness::Round);
        assert_eq!(roundness_from_u8(1), Roundness::SlightlyRound);
        assert_eq!(roundness_from_u8(2), Roundness::Square);
        assert_eq!(roundness_from_u8(9), Roundness::Round);
        // Round = the default corner radii (radius_m 16); Square flattens to 2.
        assert_eq!(CornerRadii::from(roundness_from_u8(0)), CornerRadii::default());
        assert_eq!(CornerRadii::from(roundness_from_u8(2)).radius_m, [2.0; 4]);
        assert_eq!(CornerRadii::from(roundness_from_u8(1)).radius_m, [8.0; 4]);
    }

    #[test]
    fn mode_u8_forces_dark_or_light_base() {
        // Dark (1) and Light (2) are absolute regardless of the system probe.
        assert!(mode_wants_dark(1));
        assert!(!mode_wants_dark(2));
        // Automatic (0) and out-of-range defer to the system probe — we only assert
        // it agrees with the seam (whatever the seam returns in this environment).
        assert_eq!(mode_wants_dark(0), system_is_dark());
        assert_eq!(mode_wants_dark(7), system_is_dark());
    }

    // ── Frosted-glass reader (DRAGON-217) ────────────────────────────────────
    // The COSMIC v2 DISK reader moved to `platform::linux::cosmic::theme`
    // (DRAGON-220); the pure parse helpers it feeds stay here (portable, the
    // `crate::glass` pattern) so these tests run on every OS.

    #[test]
    fn read_f32_after_parses_first_float_up_to_delimiter() {
        // The corner-radii / accent scan: the first float after the key, stopping
        // at a comma or newline, tolerating a leading `(` (RON tuple colours).
        assert_eq!(read_f32_after("radius_s: 8.0,", "radius_s:"), Some(8.0));
        assert_eq!(read_f32_after("red: (0.59,", "red:"), Some(0.59));
        assert_eq!(read_f32_after("green: 0.49\n", "green:"), Some(0.49));
        assert_eq!(read_f32_after("nope: 1.0", "radius_s:"), None);
    }

    // The live Dark v2/alpha_map on the dev machine (2026-07-15) — a fixture so
    // the field→alpha mapping is pinned to a real file shape.
    const LIVE_ALPHA_MAP: &str = "(
    extremely_low: 1.0,
    extremely_low_2: 0.97692,
    very_low: 0.95385003,
    very_low_2: 0.93076,
    low: 0.90769005,
    low_2: 0.88461,
    medium: 0.86154,
    medium_2: 0.83846,
    high: 0.81538004,
    high_2: 0.79231,
    very_high: 0.76023,
    very_high_2: 0.74615,
    extremely_high: 0.72308004,
    extremely_high_2: 0.70000005,
)";

    #[test]
    fn blur_strength_names_map_to_cosmic_ordinals() {
        // Every BlurStrength variant → its cosmic-theme ordinal (try_from(u8)).
        assert_eq!(blur_strength_ordinal("ExtremelyLow"), Some(0));
        assert_eq!(blur_strength_ordinal("VeryLow"), Some(2));
        assert_eq!(blur_strength_ordinal("Medium"), Some(6));
        assert_eq!(blur_strength_ordinal("ExtremelyHigh2"), Some(13));
        // Trailing newline (files carry one) is tolerated; junk is rejected.
        assert_eq!(blur_strength_ordinal("VeryLow\n"), Some(2));
        assert_eq!(blur_strength_ordinal("Frosted"), None);
    }

    #[test]
    fn alpha_map_field_matches_exact_name_not_prefix() {
        // `low` must not capture `very_low`/`extremely_low`/`low_2` — the whole
        // point of matching on the `field:` prefix rather than a substring find.
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "low"), Some(0.90769005));
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "low_2"), Some(0.88461));
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "very_low"), Some(0.95385003));
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "extremely_low"), Some(1.0));
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "extremely_high_2"), Some(0.70000005));
        assert_eq!(alpha_map_field(LIVE_ALPHA_MAP, "nonexistent"), None);
    }

    #[test]
    fn strength_ordinal_selects_the_alpha_map_field() {
        // The live machine's frosted=VeryLow (ordinal 2) → very_low ≈ 0.95385.
        assert_eq!(alpha_for_strength(LIVE_ALPHA_MAP, 2), Some(0.95385003));
        assert_eq!(alpha_for_strength(LIVE_ALPHA_MAP, 0), Some(1.0)); // extremely_low
        assert_eq!(alpha_for_strength(LIVE_ALPHA_MAP, 6), Some(0.86154)); // medium
        assert_eq!(alpha_for_strength(LIVE_ALPHA_MAP, 13), Some(0.70000005));
        // Out-of-range ordinal (never produced by blur_strength_ordinal) → None.
        assert_eq!(alpha_for_strength(LIVE_ALPHA_MAP, 14), None);
    }

    #[test]
    fn alpha_field_names_cover_every_strength_in_order() {
        // The name table has one entry per BlurStrength ordinal, so the reader
        // maps all 14 strengths; each name resolves against the live fixture.
        assert_eq!(ALPHA_MAP_FIELDS.len(), 14);
        for (i, name) in ALPHA_MAP_FIELDS.iter().enumerate() {
            assert_eq!(
                alpha_for_strength(LIVE_ALPHA_MAP, i as u8),
                alpha_map_field(LIVE_ALPHA_MAP, name),
                "field {name} at ordinal {i}",
            );
        }
    }

    #[test]
    fn frost_color_gates_on_the_toggle_and_replaces_only_alpha() {
        let opaque = Color::from_rgba(0.1, 0.2, 0.3, 1.0);
        // No config (off COSMIC) → untouched.
        assert_eq!(frost_color(opaque, None), opaque);
        // frosted_windows off → untouched even with an alpha present.
        let off = Some(GlassConfig { strength_ordinal: 2, alpha: 0.9, frosted_windows: false });
        assert_eq!(frost_color(opaque, off), opaque);
        // frosted_windows on → alpha replaced, rgb preserved.
        let on = Some(GlassConfig { strength_ordinal: 2, alpha: 0.95385, frosted_windows: true });
        let frosted = frost_color(opaque, on);
        assert_eq!((frosted.r, frosted.g, frosted.b), (0.1, 0.2, 0.3));
        assert_eq!(frosted.a, 0.95385);
    }

    #[test]
    fn persisted_accent_builds_a_matching_srgb() {
        // A persisted [r,g,b] round-trips into the palette Srgb the builder takes,
        // so the applied accent is exactly the stored colour (no gamma surprises).
        let rgb = [0.13_f32, 0.52, 0.94];
        let srgb = cosmic::cosmic_theme::palette::Srgb::new(rgb[0], rgb[1], rgb[2]);
        assert_eq!([srgb.red, srgb.green, srgb.blue], rgb);
    }
}
