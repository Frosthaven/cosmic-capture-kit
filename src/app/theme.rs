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
/// - Windows: the app theme via the registry (`AppsUseLightTheme`, 0 = dark) —
///   see `crate::platform::windows::appearance` (DRAGON-239). Without this,
///   Automatic / System Default fell to the arm below, which off COSMIC pinned
///   Windows to dark regardless of the OS setting.
pub(crate) fn system_is_dark() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::appearance::system_is_dark()
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::appearance::system_is_dark()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

/// Build the process-global theme for the given appearance settings and RETURN it
/// (the pure builder; [`apply_appearance`] wraps this in `set_theme`). Split out so
/// the SAME resolved theme — and thus its resolved accent — can be read headlessly
/// (the CLI diag / [`resolved_appearance_accent_rgba`]) where the runtime never
/// applies the global theme.
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
pub(crate) fn resolve_appearance_theme(
    use_system: bool,
    mode: u8,
    accent: Option<[f32; 3]>,
    roundness: u8,
    contrast_boost: bool,
) -> cosmic::Theme {
    // Automatic Contrast Boost (DRAGON-289): unify EVERY accent element — fills, lines,
    // outlines AND chrome text — on ONE colour. Under System Default the boost is forced
    // ON (its toggle is hidden), matching how the platform-native accent handles its own
    // contrast; when customizing, the persisted `contrast_boost` decides.
    let boost = contrast_boost || use_system;
    if use_system {
        // Platform-native "System Default" (DRAGON-239): Windows and macOS have no
        // COSMIC system theme to follow, so system-default resolves to an OS-native
        // accent + corner rounding (see [`native_system_default`]) over libcosmic's
        // built-in dark/light base — the same base the Linux arm below falls back to
        // off COSMIC, so the composed result is honest there. The dark/light choice
        // is the portable [`system_is_dark`] seam (Windows now the registry app
        // theme; macOS AppleInterfaceStyle). Linux keeps following the real COSMIC
        // system theme in the arm below, BYTE-IDENTICAL to before this ticket save
        // for the contrast-boost unify, which is a no-op when the accent already
        // passes 4:1.
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let (native_accent, native_roundness) = native_system_default();
            let dark = system_is_dark();
            let build = |acc: Option<[f32; 3]>| -> cosmic::cosmic_theme::Theme {
                let mut b = if dark { ThemeBuilder::dark() } else { ThemeBuilder::light() };
                if let Some([r, g, bl]) = acc {
                    b = b.accent(cosmic::cosmic_theme::palette::Srgb::new(r, g, bl));
                }
                b.corner_radii(CornerRadii::from(roundness_from_u8(native_roundness))).build()
            };
            let built = build(native_accent);
            let final_theme = apply_contrast_boost(built, boost, |acc| build(Some(acc)));
            return cosmic::Theme::custom(std::sync::Arc::new(final_theme));
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
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
            // Contrast boost (forced ON under System Default): unify fills onto the
            // contrast-corrected accent. No-op — and BYTE-IDENTICAL to before this
            // ticket — when the COSMIC accent already passes 4:1 (the boosted value
            // equals the base), which every default COSMIC accent does; only a
            // genuinely low-contrast custom accent triggers a rebuild from the SAME
            // COSMIC config the system theme reads (so mode/rounding are preserved).
            let base = t.cosmic().accent_color();
            let boosted = t.cosmic().accent_text_color();
            if boost && base != boosted {
                let builder = if dark {
                    ThemeBuilder::dark_config().ok().and_then(|c| ThemeBuilder::get_entry(&c).ok())
                } else {
                    ThemeBuilder::light_config().ok().and_then(|c| ThemeBuilder::get_entry(&c).ok())
                };
                if let Some(b) = builder {
                    let mut rebuilt = b
                        .accent(cosmic::cosmic_theme::palette::Srgb::new(
                            boosted.red,
                            boosted.green,
                            boosted.blue,
                        ))
                        .build();
                    rebuilt.accent_text = Some(rebuilt.accent.base);
                    return cosmic::Theme::custom(std::sync::Arc::new(rebuilt));
                }
            }
            return t;
        }
    }
    let dark = mode_wants_dark(mode);
    // Best-effort system-theme base; fall back to libcosmic's built-in default
    // (the expected path off a COSMIC desktop, not an error edge). `ThemeBuilder` is
    // `Clone`, so the two-pass boost below reuses this base without re-reading config.
    let base_builder = if dark {
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
    // DRAGON-255b: with NO manual accent override (`None`), "reset accent" resolves to
    // the OS-native accent instead of libcosmic's built-in default — so on Windows
    // clearing the accent matches the real system accent (registry), and on macOS the
    // built-in default (its native accent IS `None`). macOS is byte-identical (its
    // native accent is `None`, so `None` stays `None`); Linux has no
    // `native_system_default` and already composes on the COSMIC base accent, so it
    // keeps applying no override here — also byte-identical. Computed once (the closure
    // below runs up to twice) so the registry read happens at most once per resolve.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let effective_accent = accent.or_else(|| native_system_default().0);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let effective_accent = accent;
    // Build the theme with a substituted accent (mode + rounding preserved). Verify the
    // OUTPUT against the requested mode (DRAGON-144): cosmic-config returns a
    // DEFAULT-FILLED entry — which builds DARK — instead of an error when the theme
    // files don't exist (macOS/Windows always; a COSMIC system with no saved theme), so
    // the built-in fallback above never fired and Light mode silently rendered dark.
    // When the build disagrees, rebuild from libcosmic's built-in palette, which is what
    // those systems actually render; a healthy COSMIC config always agrees.
    let build = |acc: Option<[f32; 3]>| -> cosmic::cosmic_theme::Theme {
        let decorate = |mut b: ThemeBuilder| {
            if let Some([r, g, bl]) = acc {
                b = b.accent(cosmic::cosmic_theme::palette::Srgb::new(r, g, bl));
            }
            b.corner_radii(CornerRadii::from(roundness_from_u8(roundness)))
        };
        let mut built = decorate(base_builder.clone()).build();
        if built.is_dark != dark {
            built = decorate(if dark { ThemeBuilder::dark() } else { ThemeBuilder::light() }).build();
        }
        built
    };
    let built = build(effective_accent);
    let final_theme = apply_contrast_boost(built, boost, |acc| build(Some(acc)));
    cosmic::Theme::custom(std::sync::Arc::new(final_theme))
}

/// Apply the Automatic Contrast Boost policy to a freshly built theme (DRAGON-289), the
/// pure heart of the one-accent unify. `built` is the theme built with the PICKED
/// accent; `rebuild(rgb)` rebuilds the SAME base (mode/rounding preserved) with a
/// substituted accent.
///
/// - **Boost ON**: read `built`'s [`accent_text_color`](cosmic::cosmic_theme::Theme::accent_text_color)
///   — the contrast-corrected accent when the picked one fails a 4:1 test against the
///   surface, else the picked accent UNCHANGED — and rebuild so `accent.base`/hover/
///   pressed (every fill, line and outline) derive from it too. When it already passes
///   (corrected == picked) the rebuild is skipped (it would reproduce `built`).
/// - **Boost OFF**: keep the picked build untouched (fills stay the exact picked colour).
///
/// In BOTH cases `accent_text` is pinned to the FINAL `accent.base`, so chrome TEXT
/// (active nav links, tab titles) draws in exactly the same colour as the fills — the
/// split libcosmic normally keeps between `accent_text_color()` and `accent.base` can
/// never reappear. Boost off therefore forces text down to the raw picked colour even
/// when it is low-contrast; boost on lifts everything to the corrected colour.
fn apply_contrast_boost(
    built: cosmic::cosmic_theme::Theme,
    boost: bool,
    rebuild: impl FnOnce([f32; 3]) -> cosmic::cosmic_theme::Theme,
) -> cosmic::cosmic_theme::Theme {
    let mut theme = if boost {
        let base = built.accent_color();
        let boosted = built.accent_text_color();
        // Equal (exactly — both are `accent.base`) when the picked accent already passes
        // contrast, so `accent_text` was `None`; skip the rebuild in that case.
        if base == boosted {
            built
        } else {
            rebuild([boosted.red, boosted.green, boosted.blue])
        }
    } else {
        built
    };
    theme.accent_text = Some(theme.accent.base);
    theme
}

/// Build + apply the process-global theme for the current appearance settings, as a
/// `Task` the caller returns from `update` (or batches into `init`). The thin apply
/// wrapper over [`resolve_appearance_theme`] (which holds the whole build + its
/// portability contract) — behaviour is byte-identical to the pre-split function.
pub(crate) fn apply_appearance<M: Send + 'static>(
    use_system: bool,
    mode: u8,
    accent: Option<[f32; 3]>,
    roundness: u8,
    contrast_boost: bool,
) -> Task<cosmic::Action<M>> {
    cosmic::command::set_theme(resolve_appearance_theme(
        use_system,
        mode,
        accent,
        roundness,
        contrast_boost,
    ))
}

/// The RESOLVED appearance accent as opaque RGBA — the value the unset Active
/// window-border follows (`crate::decoration::accent_rgba`), computed from the
/// PERSISTED appearance settings via [`resolve_appearance_theme`]. Equal to
/// `accent(&cosmic::theme::active())` after [`apply_appearance`] has run (both read
/// the same resolved theme's accent), but runtime-independent, so a headless CLI
/// process — where the global theme is never applied — resolves the SAME colour the
/// running app draws. Used by the Windows composite diagnostic to verify the border;
/// the mac/Linux composite diags could adopt it, so it stays compiled everywhere.
/// (Windows AND Linux consume it now: the Windows composite diag + both the Windows
/// and Linux resident daemons tint their tray from it, so the boost drives the tray
/// in lockstep — DRAGON-289; only macOS, whose tray icon is template-tinted with no
/// accent, leaves it unused.)
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
pub(crate) fn resolved_appearance_accent_rgba() -> [u8; 4] {
    let p = crate::state::load();
    let theme = resolve_appearance_theme(
        p.appearance_use_system,
        p.appearance_mode.min(2),
        p.appearance_accent,
        p.appearance_roundness.min(2),
        p.appearance_contrast_boost,
    );
    color_to_rgba(accent(&theme))
}

/// An iced [`Color`] as opaque 8-bit RGBA (the `image::Rgba` byte order), clamped.
/// The shared accent→border-colour encoding.
pub(crate) fn color_to_rgba(c: Color) -> [u8; 4] {
    [
        (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}

/// The platform-native "System Default" appearance for OSes that have no COSMIC
/// system theme to follow (DRAGON-239): `(accent override, roundness byte)`, fed to
/// the `use_system` build in [`apply_appearance`]. The dark/light base is chosen
/// separately by [`system_is_dark`]; this picks only the accent + rounding.
///
/// - **Windows**: the OS accent ("trim") colour read from the registry (the closed
///   platform body `crate::platform::windows::appearance::accent_rgb`; `None` if it
///   can't be read, keeping libcosmic's default accent) + FULLY-round corners
///   (roundness byte 0) — fully-round is the app's System-Default look on every
///   non-COSMIC OS (COSMIC follows its own rounding config instead).
/// - **macOS**: the OS accent read from `NSColor.controlAccentColor`, converted to
///   sRGB (the closed platform body `crate::platform::mac::appearance::accent_rgb`;
///   when the accent is "Multicolor", the macOS default, it pins Apple's default blue
///   #047AFF, and `None` keeps libcosmic's default accent) + FULLY-round corners
///   (roundness byte 0).
///
/// Linux never calls this (it follows the real COSMIC theme in [`apply_appearance`]).
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn native_system_default() -> (Option<[f32; 3]>, u8) {
    #[cfg(target_os = "windows")]
    {
        (crate::platform::windows::appearance::accent_rgb(), 0)
    }
    #[cfg(target_os = "macos")]
    {
        (crate::platform::mac::appearance::accent_rgb(), 0)
    }
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

    /// The button token, but with each corner capped at `max` px. The quad
    /// renderer already clamps a radius to half the SHORTER axis, so the raw
    /// `xl` (160 under "round") reads as a clean pill on a SHORT-and-wide
    /// control (its half-height wins). It only balloons on a control that is
    /// TALL relative to its width — e.g. the capture toolbar's stacked
    /// kind+timer group, where the delay chip wraps below the kind trio — where
    /// half the taller axis becomes a near-square blob. Capping at the standard
    /// group half-height keeps every short group byte-identical (their clamp was
    /// already `max`) while taming the tall stacked one. "Slightly round"/
    /// "square" (xl = 8/2) fall through untouched, so the roundness preference is
    /// still honoured.
    pub(crate) fn xl_capped(&self, max: f32) -> [f32; 4] {
        self.xl.map(|r| r.min(max))
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
    // Windows (DRAGON-239): the active theme now follows the OS light/dark setting
    // (System Default resolves through `system_is_dark`), so the drop-shadow opacity
    // must track it too — the registry app-theme probe, exactly as macOS does above.
    // (Historically this arm returned a hardcoded `true`; correct only because the
    // Windows theme was itself pinned dark until this ticket.)
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::appearance::system_is_dark()
    }
    // Other: no COSMIC config dir, so the reader's `unwrap_or(true)` default — dark
    // — is the historical result.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        true
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
    // Windows (DRAGON-267): the Mica backdrop is the frosted-windows analog.
    #[cfg(windows)]
    {
        windows_glass_config()
    }
    // macOS (DRAGON-268): window vibrancy is the frosted-windows analog — winit's
    // `blur` enrolls an NSVisualEffectView and `platform::mac::window` clears the Metal
    // layer over it. Its own arm now, so the final `None` arm covers only the remaining
    // platforms (no frosted-windows support yet), keeping their opaque look unchanged.
    #[cfg(target_os = "macos")]
    {
        macos_glass_config()
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        None
    }
}

/// The frosted-surface opacity the Windows chrome paints at OVER the DWM Mica material
/// (DRAGON-267). Mica IS visible — the windows render alpha-composited (verified via a WGC
/// capture, which sees the DWM layer that a GDI BitBlt screenshot does not; that BitBlt
/// blindness is what earlier made this look like a no-op — DRAGON-275). `frost_color` paints
/// the chrome at this alpha over the Mica backdrop: **0.0 = the pure Mica material shows**
/// (heavily-blurred, desaturated desktop tint — the truest Mica look), higher values lay more
/// of the flat theme colour over it (0.85 was a restrained, very subtle glass). User picked
/// 0.0 for the full Mica effect (DRAGON-275).
#[cfg(windows)]
const MICA_SURFACE_ALPHA: f32 = 0.0;

/// The Windows frosted-windows config (DRAGON-267) — the Mica-backdrop equivalent of the
/// Linux COSMIC frosted-glass read. On Win11 22H2+ (where DWM's `DWMWA_SYSTEMBACKDROP_TYPE`
/// Mica material is supported) this returns `Some` with `frosted_windows` ON, so the SHARED
/// translucent-chrome painting (`frost_color`, the settings/preview chrome closures) and
/// [`glass_windows_enabled`] behave exactly like Linux's frosted glass; the native DWM
/// material itself is applied post-show by `platform::windows::window::apply_mica`. Below
/// 22H2 (Mica unsupported) it is `None` — a graceful no-op keeping today's opaque look.
/// Honors the SAME `CCK_NO_GLASS=1` kill-switch as the Linux reader so the frosted-windows
/// toggle is unified across platforms. `strength_ordinal` is unused off Linux (only the
/// COSMIC capture-glass reproduction consumes it), so a `0` placeholder.
#[cfg(windows)]
fn windows_glass_config() -> Option<GlassConfig> {
    if std::env::var_os("CCK_NO_GLASS").is_some_and(|v| v == "1") {
        return None;
    }
    if !crate::platform::windows::window::mica_supported() {
        return None;
    }
    Some(GlassConfig { strength_ordinal: 0, alpha: MICA_SURFACE_ALPHA, frosted_windows: true })
}

/// The frosted-surface opacity the macOS chrome paints at OVER the window vibrancy
/// (DRAGON-268). The vibrancy material comes from a winit-inserted NSVisualEffectView
/// behind a cleared Metal layer (`platform::mac::window::enable_window_vibrancy`), and
/// `frost_color` paints the page/chrome background at this alpha over it: 0.0 = the pure
/// vibrancy shows, higher values lay more of the flat theme colour over it.
///
/// Unlike Windows (where DWM Mica fills the whole window, so `MICA_SURFACE_ALPHA = 0.0`
/// reads as a solid frosted pane), macOS vibrancy behind a fully-transparent page reads as
/// a see-through hole, so the mac page needs a translucent theme tint painted over the
/// vibrancy to read as a frosted PANEL rather than clear glass. This value is that tint's
/// opacity; tune it up for a more solid pane, down for more of the raw desktop blur.
#[cfg(target_os = "macos")]
const MAC_SURFACE_ALPHA: f32 = 0.75;

/// The active theme's opaque background base as straight-alpha `[r, g, b, a]` u8 — the DARK
/// (or light) pane color the fullscreen backdrop fix paints an opaque NSWindow with, so the
/// page tint composites over a proper theme pane instead of the bright vibrancy no-backdrop
/// fallback (DRAGON-268 follow-up, Task 2). Reads the live `cosmic::theme::active()`.
#[cfg(target_os = "macos")]
pub(crate) fn background_base_rgba() -> [u8; 4] {
    let bg: Color = cosmic::theme::active().cosmic().background.base.into();
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [c(bg.r), c(bg.g), c(bg.b), 255]
}

/// The macOS frosted-windows config (DRAGON-268) — the window-vibrancy equivalent of the
/// Linux COSMIC frosted-glass read / the Windows Mica read. Vibrancy is available on our
/// whole macOS floor (`vibrancy_supported` is always `true`), so this returns `Some` with
/// `frosted_windows` ON unless the SAME `CCK_NO_GLASS=1` kill-switch the Linux/Windows
/// readers honor is set — so the frosted-windows toggle is unified across platforms. The
/// SHARED translucent-chrome painting (`frost_color`, the settings/preview chrome closures)
/// and [`glass_windows_enabled`] then behave exactly like Linux's frosted glass; the native
/// vibrancy itself is enrolled by winit's `blur` at window creation and revealed post-show
/// by `platform::mac::window::enable_window_vibrancy`. `strength_ordinal` is unused off
/// Linux (only the COSMIC capture-glass reproduction consumes it), so a `0` placeholder.
#[cfg(target_os = "macos")]
fn macos_glass_config() -> Option<GlassConfig> {
    if std::env::var_os("CCK_NO_GLASS").is_some_and(|v| v == "1") {
        return None;
    }
    if !crate::platform::mac::window::vibrancy_supported() {
        return None;
    }
    Some(GlassConfig { strength_ordinal: 0, alpha: MAC_SURFACE_ALPHA, frosted_windows: true })
}

/// Whether to enroll a fresh toplevel WINDOW in the frosted-windows material: the
/// user has frosted windows on ([`glass_config`]). Portable by the seam — `None` off
/// COSMIC (no v2 theme config) yields `false`, so the window opens un-enrolled and
/// fully opaque exactly as before.
///
/// On Linux this is the compositor's backdrop blur (winit `blur`); on macOS
/// (DRAGON-268) winit's `blur` flag is the window vibrancy (an NSVisualEffectView), so
/// this ALSO gates the mac `blur` — AND the mac `transparent` flag, so the Metal layer
/// is non-opaque enough for the vibrancy to show (`platform::mac::window` finishes the
/// job post-show).
// DRAGON-267: on Windows the settings/preview windows take their material from a DWM Mica
// backdrop, NOT winit's `blur` (a Windows `blur:true` is a legacy accent-policy blur-behind
// that competes with `DWMWA_SYSTEMBACKDROP_TYPE`), so both `blur:` call sites are
// `cfg(not(windows))` and this seam has no Windows caller — honestly gated as dead there
// while staying live on Linux/macOS.
#[cfg_attr(windows, allow(dead_code))]
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

/// The fill alpha of the settings window's **section/option cards** (DRAGON-279).
/// Distinct from [`MICA_SURFACE_ALPHA`] / [`frost_color`], which drive the WINDOW
/// base + nav rail to (near-)full transparency. Tuned to the Fluent card weight
/// (user reference: Win11 Settings "Bluetooth devices" card, dark, 2026-07-19): a
/// NEARLY-SOLID neutral panel with only a HINT of the backdrop bleeding through — not
/// a sheer veil. High alpha is deliberate: the card fill is the (neutral, dark-gray)
/// component base, so a near-opaque fill reads as a crisp lighter panel the way Win11
/// cards do, whereas a low alpha let the (more saturated than Win11 Mica) backdrop
/// dominate and the card read as ghostly tinted glass.
///
/// STRUCTURE VERDICT (2026-07-19, sampled from the reference): the Win11 card is ONE
/// UNIFORM fill — the section body AND the toggle/device rows all measure the SAME
/// The settings window's interior surfaces — item-row cards AND standard buttons —
/// paint the SAME material as the nav rail's active pill (user decision 2026-07-19,
/// "make it easy": one material for pills, rows, and buttons, so they always move
/// together): libcosmic's segmented-button active fill, `palette.neutral_5` at
/// these alphas. UNCONDITIONAL on every platform — the backdrop bleeds through
/// wherever one exists (Windows Mica, COSMIC frosted windows; mac blur when it
/// lands) and blends over the opaque window base where none does yet.
///
/// `PILL_ALPHA` (0.2) is exactly the nav pill's active alpha. Buttons rest at the
/// same 0.2 and bump on interaction so they still read as controls; a button
/// sitting INSIDE a row stacks its fill over the row's (0.2 over 0.2), which is
/// the Fluent layering the user hypothesized — heavier by construction, and it
/// tracks any retune of the shared material automatically.
pub(crate) const PILL_ALPHA: f32 = 0.2;
pub(crate) const PILL_HOVER_ALPHA: f32 = 0.3;
pub(crate) const PILL_PRESSED_ALPHA: f32 = 0.35;
pub(crate) const PILL_DISABLED_ALPHA: f32 = 0.1;

/// The shared pill material at `alpha`: `palette.neutral_5` — the exact token the
/// nav rail's active pill uses (libcosmic segmented_button active =
/// `neutral_5.with_alpha(0.2)`).
pub(crate) fn pill_fill(theme: &cosmic::Theme, alpha: f32) -> Color {
    let n = theme.cosmic().palette.neutral_5;
    Color::from_rgba(n.red, n.green, n.blue, alpha)
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
    fn xl_capped_tames_round_but_leaves_slightly_round_and_square() {
        // "round" (xl = 160) is capped down to the small-control ceiling so the
        // stacked capture-toolbar group stops ballooning into a blob.
        let round = Rounding::FALLBACK; // xl = [160; 4]
        assert_eq!(round.xl_capped(22.0), [22.0; 4]);
        // "slightly round" (xl = 8) and "square" (xl = 2) already sit under the
        // ceiling, so the roundness preference passes through untouched.
        let slightly = Rounding { xs: [2.0; 4], s: [8.0; 4], m: [8.0; 4], xl: [8.0; 4] };
        assert_eq!(slightly.xl_capped(22.0), [8.0; 4]);
        let square = Rounding { xs: [2.0; 4], s: [2.0; 4], m: [2.0; 4], xl: [2.0; 4] };
        assert_eq!(square.xl_capped(22.0), [2.0; 4]);
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

    // ── Platform-native System Default (DRAGON-239) ──────────────────────────
    // The per-platform selection fed to the `use_system` build: Windows =
    // slightly-round (1), macOS = fully-round (0). Runs on the OS it's compiled
    // for (Windows via `--no-default-features`); Linux never compiles the fn.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn native_system_default_selects_platform_roundness() {
        let (_accent, roundness) = native_system_default();
        #[cfg(target_os = "windows")]
        assert_eq!(roundness, 1, "Windows System Default = slightly round");
        #[cfg(target_os = "macos")]
        assert_eq!(roundness, 0, "macOS System Default = fully round");
    }

    #[test]
    fn color_to_rgba_encodes_opaque_bytes() {
        assert_eq!(color_to_rgba(Color::from_rgb(0.0, 120.0 / 255.0, 212.0 / 255.0)), [0, 120, 212, 255]);
        // Alpha is always forced to 255 (an opaque border) regardless of the Color's
        // own alpha; components round to the nearest byte. (The `.clamp` in the body
        // guards a hand-built out-of-range Color, which iced won't let us construct
        // here — its own debug asserts enforce 0..=1 — so we exercise valid inputs.)
        assert_eq!(color_to_rgba(Color::from_rgba(1.0, 0.5, 0.0, 0.3)), [255, 128, 0, 255]);
    }

    #[test]
    fn resolve_appearance_theme_routes_the_manual_accent() {
        // The border DEFAULT follows the resolved theme accent. A manual override
        // accent must flow into the built theme's accent; different manuals differ,
        // and each leans toward its dominant channel — robust to cosmic-theme's exact
        // accent derivation (we assert direction, not an exact byte). Boost OFF so the
        // fills stay the raw picked colour (boost could lift a low-contrast pick).
        let red = accent(&resolve_appearance_theme(false, 1, Some([0.90, 0.10, 0.10]), 0, false));
        let blue = accent(&resolve_appearance_theme(false, 1, Some([0.10, 0.10, 0.90]), 0, false));
        assert_ne!((red.r, red.g, red.b), (blue.r, blue.g, blue.b));
        assert!(red.r > red.b, "a red manual accent stays red-dominant: {red:?}");
        assert!(blue.b > blue.r, "a blue manual accent stays blue-dominant: {blue:?}");
    }

    // ── Automatic Contrast Boost (DRAGON-289) ────────────────────────────────
    // Force dark mode (1) so the derivation path is deterministic regardless of the
    // host's system light/dark preference.

    fn luma(c: Color) -> f32 {
        0.299 * c.r + 0.587 * c.g + 0.114 * c.b
    }

    #[test]
    fn contrast_boost_off_unifies_text_with_the_fill_accent() {
        // Boost OFF: chrome text (accent_text) is forced to EXACTLY the fill accent
        // (accent.base), so the historical libcosmic split can't show — even for a
        // deliberately low-contrast pick (dark red on a dark surface).
        let t = resolve_appearance_theme(false, 1, Some([0.25, 0.0, 0.0]), 0, false);
        let fill = accent(&t);
        let text = accent_text(&t);
        assert_eq!(
            (fill.r, fill.g, fill.b),
            (text.r, text.g, text.b),
            "boost off: text must equal the fill accent"
        );
    }

    #[test]
    fn contrast_boost_on_lifts_a_low_contrast_accent_and_unifies() {
        // A dark-red accent fails 4:1 against the dark surface, so boost ON lifts the
        // WHOLE accent (fills + text) to the brighter contrast-corrected variant, while
        // boost OFF leaves the fill at the raw dark pick. Both stay unified (text==fill).
        let dark_red = Some([0.25, 0.0, 0.0]);
        let off = resolve_appearance_theme(false, 1, dark_red, 0, false);
        let on = resolve_appearance_theme(false, 1, dark_red, 0, true);
        // Unified in both cases.
        for (label, th) in [("off", &off), ("on", &on)] {
            let f = accent(th);
            let x = accent_text(th);
            assert_eq!((f.r, f.g, f.b), (x.r, x.g, x.b), "{label}: text must equal fill");
        }
        // Boost lifted the fill to a brighter accent.
        assert!(
            luma(accent(&on)) > luma(accent(&off)),
            "boost on must brighten a low-contrast accent: on={:?} off={:?}",
            accent(&on),
            accent(&off),
        );
    }

    #[test]
    fn contrast_boost_leaves_a_high_contrast_accent_unchanged() {
        // A bright accent already passes 4:1 on the dark surface, so boost is a no-op:
        // the fill accent is (essentially) the same with the boost on or off, and stays
        // unified with the text either way.
        let bright = Some([0.85, 0.85, 0.90]);
        let off = accent(&resolve_appearance_theme(false, 1, bright, 0, false));
        let on = accent(&resolve_appearance_theme(false, 1, bright, 0, true));
        let d = (on.r - off.r).abs() + (on.g - off.g).abs() + (on.b - off.b).abs();
        assert!(d < 0.02, "boost must not shift a high-contrast accent: on={on:?} off={off:?}");
    }
}
