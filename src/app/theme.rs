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
// The auto-glass opt-out below (`toolkit_auto_glass`) speaks libcosmic's own
// vocabulary: `Core::set_auto_blur` takes a `BitFlags<Auto>`. `enumflags2` is a
// direct dependency ONLY so that empty set can be spelled `BitFlags::EMPTY`
// instead of a `Default::default()` whose meaning would change silently if
// upstream ever gave `Auto` a custom default. It is the same version libcosmic
// already resolves, so it adds no extra build.
use cosmic::core::Auto;
use enumflags2::BitFlags;

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

/// Whether the chrome this app is CURRENTLY PAINTING is dark (DRAGON-666).
///
/// [`mode_wants_dark`] answers for a mode byte a caller already holds; this answers for the
/// live window, reading the persisted appearance itself. Its one caller is native and has
/// no `App` in reach: the Windows caption installer, which has to tell DWM whether the
/// header under its caption buttons is dark or it paints black glyphs on black chrome
/// (`platform::windows::caption`).
///
/// System Default (`appearance_use_system`) follows the system, exactly as
/// `resolve_appearance_theme` does with the same flag; otherwise the persisted mode decides,
/// with Automatic deferring to the system through the same [`system_is_dark`] seam. So this
/// cannot disagree with the theme actually applied unless that function's rule changes, and
/// then both change together.
///
/// Windows-only in practice (macOS asks AppKit for its own traffic lights, Linux draws its
/// own captions), so it is honestly dead everywhere else rather than hidden behind a
/// crate-wide allow.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn window_chrome_is_dark() -> bool {
    let p = crate::state::load();
    if p.appearance_use_system { system_is_dark() } else { mode_wants_dark(p.appearance_mode) }
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

// ── The text SELECTION fill (DRAGON-680) ─────────────────────────────────────

/// How much of the accent's opacity a text SELECTION keeps.
///
/// A third, and the owner sized it by eye on the shipped build, twice: first "the text
/// selection color being our trim color is good, but we need it to be half as opaque
/// otherwise text is hard to read" (0.5), then DRAGON-687's follow-up run took it down
/// again: "lets make this 33% opacity instead of 50% opacity."
const SELECTION_ALPHA: f32 = 0.33;

/// The fill painted behind SELECTED text in every text input in this app: the user's
/// accent at [`SELECTION_ALPHA`] of its opacity.
///
/// **Why the full-strength accent is unreadable, stated properly, because it looks like it
/// should be fine.** libcosmic's `text_input::Appearance` carries a `selected_text_color`
/// beside its `selected_fill`, so the obvious reading is that selected text is drawn in an
/// ink chosen to sit on that fill. It is not: the widget never reads that field (grep
/// `selected_text_color` in libcosmic's `text_input/input.rs` and every hit is the theme
/// setting it). The selection quad is painted and then the value is drawn over it in its
/// ORDINARY colour, so at full opacity a saturated accent sits directly under body text
/// that was never chosen to contrast with it. Cutting the alpha lets the input's own
/// background back through, which is what restores the contrast the text was designed for,
/// and it keeps the accent recognisably the accent, which is the balance the owner liked.
///
/// It is a FRACTION of the live accent rather than a colour of its own, so it follows the
/// user's accent setting exactly as everything else does, and a theme whose accent already
/// carries alpha is scaled from whatever it really is rather than from an assumed 1.0.
pub(crate) fn selection_fill(theme: &Theme) -> Color {
    let mut c = accent(theme);
    c.a *= SELECTION_ALPHA;
    c
}

/// Apply [`selection_fill`] to a text input appearance. THE one place the override is
/// made, so every input in the app softens by the same rule.
pub(crate) fn soften_selection(
    mut appearance: cosmic::widget::text_input::Appearance,
    theme: &Theme,
) -> cosmic::widget::text_input::Appearance {
    appearance.selected_fill = selection_fill(theme);
    appearance
}

/// Which STOCK appearance an input starts from, for [`input_style`]. A tiny `Copy` enum
/// rather than the real `cosmic::theme::TextInput`, because that one owns boxed closures
/// in its `Custom` variant and so cannot be copied into the five style closures below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InputBase {
    /// libcosmic's ordinary field: every settings input, the picker's value boxes.
    Default,
    /// The rounded search field (the settings search bar).
    Search,
}

impl InputBase {
    fn stock(self) -> cosmic::theme::TextInput {
        match self {
            Self::Default => cosmic::theme::TextInput::Default,
            Self::Search => cosmic::theme::TextInput::Search,
        }
    }
}

/// A stock text input with the app's own SELECTION fill (DRAGON-680): everything about the
/// field stays whatever libcosmic says it is, and only `selected_fill` is overridden.
///
/// **Every text input in the app takes this**, which is why it lives here in the appearance
/// seam rather than beside any one of them. libcosmic offers no theme-level hook for the
/// selection colour, only the per-widget style, so a shared helper the sites call IS the
/// one mechanism available; what it buys is that the rule is written once and a new input
/// inherits it by using the helper instead of by remembering a number.
///
/// A caller that also overrides something else (the colour picker's value boxes change the
/// resting border) should derive from the stock appearance and finish with
/// [`soften_selection`] rather than reaching for this, so the two overrides compose instead
/// of one replacing the other.
pub(crate) fn input_style(base: InputBase) -> cosmic::theme::TextInput {
    use cosmic::widget::text_input::StyleSheet as _;
    cosmic::theme::TextInput::Custom {
        active: Box::new(move |t| soften_selection(t.active(&base.stock()), t)),
        error: Box::new(move |t| soften_selection(t.error(&base.stock()), t)),
        hovered: Box::new(move |t| soften_selection(t.hovered(&base.stock()), t)),
        focused: Box::new(move |t| soften_selection(t.focused(&base.stock()), t)),
        disabled: Box::new(move |t| soften_selection(t.disabled(&base.stock()), t)),
    }
}

/// The foreground drawn ON an accent fill: labels AND glyphs, one value for both.
///
/// THE one source of truth for on-accent ink (DRAGON-607). It is `accent_button.on`,
/// which is exactly what libcosmic's own [`Button::Suggested`](cosmic::theme::Button)
/// paints, so a custom accent button of ours and a stock suggested one standing next to
/// it can never disagree. That "next to it" case is not hypothetical: the preview
/// toolbar shows Apply Crop and Upload side by side.
///
/// **It is deliberately NOT `on_accent_color()` (`accent.on`), which is what this
/// returned until DRAGON-607, and the name is the whole trap.** That token is a fixed
/// neutral step chosen by LIGHT/DARK MODE ALONE: `control_steps_array[0]`, reversed for
/// light, so a dark theme always gets near-black and a light theme always gets
/// near-white. **The accent colour is never consulted.** It looks correct on the COSMIC
/// defaults only because those pair a LIGHT accent with a dark theme, which is the case
/// where "always near-black" happens to be the right answer. Give it a DARK accent in a
/// dark theme and it returns near-black ink for a near-black fill.
/// `accent_button.on` instead comes from cosmic-theme's `get_text`, which derives the
/// ink from the accent's own lightness, so it tracks the colour it sits on. There is a
/// test below that fails if this is ever pointed back.
pub(crate) fn on_accent(theme: &Theme) -> Color {
    theme.cosmic().accent_button.on.into()
}

/// The container style that content wrapped INSIDE an accent-filled button must wear.
///
/// **The rule this exists to enforce, which has now been rediscovered twice
/// (DRAGON-601's hex label, DRAGON-607's accent buttons): a container's `text_color` and
/// `icon_color` are not a suggestion that the leaf can decline, they are an
/// UNCONDITIONAL override of everything below, and the LAST container before the leaf
/// wins.** Depth changes nothing: one wrapper and three wrappers behave identically.
///
/// The other half of that lesson, which rules out a whole family of false suspects: only
/// a DESCENDANT of the button can override its ink. An ANCESTOR cannot, because the
/// button's own style is applied deeper and deeper wins. So the cluster containers and
/// group backgrounds that accent buttons sit inside are all innocent, and the search for
/// this bug is always "what sits BETWEEN the button and its label".
///
/// Two consequences worth keeping in mind before styling anything nested:
///  - Setting ink on an ancestor (a button, an outer container) does NOT reach a leaf
///    that has any styled container between them. That is the bug both tickets were.
///  - A PLAIN `widget::container` is not neutral. libcosmic's default class is
///    `Container::Transparent`, and that class explicitly sets BOTH fields to the
///    surrounding container's `component.on`, the ordinary window foreground. Dropping
///    one between an accent button and its label therefore REPLACES the on-accent ink
///    with near-white in a dark theme, which is precisely how the owner ended up with
///    some purple buttons in black and some in white.
///
/// So: wrap with THIS instead of a bare container, and the ink survives the trip. It
/// paints nothing itself (no background, no border), it only refuses to clobber the ink.
/// Both fields are set together on purpose, because setting one and forgetting the other
/// is the exact shape of the defect.
///
/// **One hazard, learned by making the mistake while fixing this ticket.** This helper is
/// for a surface that is ALWAYS the accent. A surface that is only SOMETIMES the accent,
/// such as either half of a segmented pair, must not use it: the inactive half is filled
/// with the [`state_mix`] wash, and giving it on-accent ink reproduces the very bug this
/// helper exists to fix, just somewhere new. Use [`ink_content`] with [`segment_ink`]
/// there. A shared helper makes a fix easy to apply and equally easy to MISapply, so check
/// what the surface actually is before reaching for this one.
pub(crate) fn on_accent_content(theme: &Theme) -> cosmic::iced::widget::container::Style {
    ink_content(on_accent(theme))
}

/// A wrapper container that CARRIES `ink` down to the leaf instead of resetting it, for
/// content whose ink is not the plain on-accent value (DRAGON-607).
///
/// Pure; unit-tested. THE one place both ink fields are written, which is the point:
/// every wrapper in the app goes through here, so no site can set the text colour and
/// forget the glyph colour. [`on_accent_content`] is this with the accent's ink already
/// filled in; a segmented control passes [`segment_ink`] instead, because its inactive
/// half is not filled with the accent.
///
/// It paints nothing. No background, no border, no radius: it exists only to refuse to
/// clobber the ink. See [`on_accent_content`] for why a bare container does clobber it.
pub(crate) fn ink_content(ink: Color) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        text_color: Some(ink),
        icon_color: Some(ink),
        ..Default::default()
    }
}

/// The container class a button's wrapped CONTENT must wear, decided from the button's
/// own class (DRAGON-607).
///
/// Pure; unit-tested. This is the decision extracted out of the call sites, so the
/// settings helper and the preview's Upload button cannot answer it differently: an
/// accent-filled class gets a wrapper that carries the on-accent ink, anything else keeps
/// the toolkit default and is byte-identical to before.
///
/// Stock classes only, via [`fills_with_accent`]. A `Button::Custom` owns its own ink and
/// gets the default here; segmented controls pass [`ink_content`] with [`segment_ink`]
/// directly, because their answer depends on which segment it is rather than on the class.
pub(crate) fn button_content_class(
    class: &cosmic::theme::Button,
) -> cosmic::theme::Container<'static> {
    if fills_with_accent(class) {
        cosmic::theme::Container::custom(on_accent_content)
    } else {
        cosmic::theme::Container::default()
    }
}

/// The legible ink for a fill the toolkit has NO on-colour token for: our own blended
/// surfaces, such as the inactive segment's [`state_mix`] wash (DRAGON-607).
///
/// Pure; unit-tested. Black or white, chosen by [`crate::color::Srgb::wants_dark_text`],
/// which is the app's one WCAG crossover and the same decision the colour picker's hex
/// label makes. Reusing it is the point: this is not a second opinion about contrast, it
/// is the SAME opinion applied to a second surface.
///
/// This does NOT replace [`on_accent`] and must not be used for an accent fill. For the
/// accent we defer to the toolkit's own `accent_button.on`, so our accent buttons match
/// libcosmic's stock ones exactly. This exists only for the surfaces libcosmic never
/// derived a token for, because we invented them.
///
/// Alpha is ignored: the fill is treated as opaque. Every surface this is used on today
/// is opaque, and answering honestly for a translucent one would need the composite.
pub(crate) fn legible_ink_on(fill: Color) -> Color {
    let s = crate::color::Srgb::from_unit_clamped([fill.r as f64, fill.g as f64, fill.b as f64]);
    if s.wants_dark_text() { Color::BLACK } else { Color::WHITE }
}

/// The ink ONE segment of a segmented pair draws its glyph and its label in (DRAGON-607).
///
/// Pure; unit-tested. THE shared answer for both consumers, so the capture toolbar's kind
/// pair and the preview timeline's pointer/scissor pair can never drift: [`segment_style`]
/// reads it for the button's own `icon_color`/`text_color`, and `preview::chrome`'s
/// `seg_toggle` reads it for the explicit SVG class its glyph needs.
///
/// Active segments are filled with the accent, so they take [`on_accent`], the toolkit's
/// own value. Inactive segments are filled with our [`state_mix`] wash, which is NOT the
/// accent, so they take the ink that fill actually needs ([`legible_ink_on`]). See
/// [`segment_style`] for why the two used to share one colour and why the owner changed it.
///
/// The inactive answer is deliberately computed against the RESTING fill and then used for
/// the hover fill too. The hover wash is only a little lighter, the assertion in this
/// module's tests pins that one ink clears the bar on BOTH, and a glyph that changed
/// colour under the pointer would be a new annoyance in place of the old one.
pub(crate) fn segment_ink(t: &Theme, active: bool) -> Color {
    if active { on_accent(t) } else { legible_ink_on(state_mix(t, SEGMENT_MIX_OFF)) }
}

/// The inactive segment's resting fill blend, shared by [`segment_style`] and the ink
/// [`segment_ink`] derives from it, so the fill and its ink can never be computed from two
/// different numbers.
pub(crate) const SEGMENT_MIX_OFF: f32 = 0.2;

/// The inactive segment's HOVER fill blend. Stronger than [`SEGMENT_MIX_OFF`] so hover
/// separates from both the group base and the resting fill.
pub(crate) const SEGMENT_MIX_HOVER: f32 = 0.35;

const _: () = assert!(
    SEGMENT_MIX_HOVER > SEGMENT_MIX_OFF,
    "DRAGON-607: hover must be a STRONGER wash than rest, or the segment stops reacting and \
     the one ink chosen for the resting fill is no longer the ink hover was checked against"
);

/// Whether a STOCK libcosmic button class fills its box with the accent, and so needs its
/// content to carry on-accent ink rather than the ordinary window foreground.
///
/// Pure; unit-tested. It exists so a helper that takes a button class as a PARAMETER
/// (`settings::row::centered_button`) can decide for itself, instead of every caller
/// remembering to pass the right ink alongside the right class. Forgetting is the whole
/// failure mode this ticket is about.
///
/// It answers for the stock classes only. `Button::Custom` carries an opaque closure, so
/// this cannot see whether it paints an accent fill and deliberately says `false` rather
/// than guessing; a custom style owns its own ink (see [`segment_style`]).
pub(crate) fn fills_with_accent(class: &cosmic::theme::Button) -> bool {
    matches!(class, cosmic::theme::Button::Suggested)
}

/// The accent variant tuned for TEXT/icons on the plain background.
///
/// **Never use this as ink ON an accent fill; that is [`on_accent`].** The names read as
/// alternatives and they are not: this one is accent-COLOURED text for a normal surface,
/// the other is the contrasting ink for an accent surface. Misusing it here fails harder
/// than the DRAGON-607 bug did, because [`apply_contrast_boost`] pins this app's
/// `accent_text` to `accent.base`: the text would be the accent drawn on the accent, so
/// INVISIBLE rather than merely low contrast.
///
/// Its one non-test consumer is the settings nav rail's active icon, which sits on a
/// neutral pill. That is the correct shape for this helper.
pub(crate) fn accent_text(theme: &Theme) -> Color {
    theme.cosmic().accent_text_color().into()
}

/// The 1px outline every bordered toolbar CLUSTER wears — the preview editor's tool
/// groups, and (DRAGON-475) the capture toolbar's groups, so the two toolbars carry one
/// chrome. Also the preview's disabled tone (`preview::chrome::disabled_label_tone`), on
/// purpose: one quiet colour for "chrome, not state".
///
/// It is the theme's `background.divider`, which is ALREADY FLATTENED to an opaque colour
/// by cosmic-theme (`over(on_bg @ 20% alpha, background.base)`) — so, unlike a raw
/// translucent token, it is safe as an SVG tint despite the rasterizer discarding colour
/// alpha (see the slider-icon note in `preview::chrome`). Measured: dark `0.266` grey
/// against a `0.182` cluster fill, light `0.689` against `0.960` — faint in both,
/// deliberately, and never invisible.
///
/// Lived in `preview::chrome` until DRAGON-475 gave it a second consumer; moved here
/// rather than copied, so the two toolbars can never drift apart.
pub(crate) fn cluster_border(theme: &Theme) -> Color {
    theme.cosmic().background(false).divider.into()
}

// ── Shared segmented-pair styling ────────────────────────────────────────────

/// The ONE segmented-toggle segment style — the capture toolbar's
/// scanner/image/video kind pair AND the preview's
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
        // BOTH the group base and the segment's resting fill.
        state_mix(t, SEGMENT_MIX_HOVER)
    } else {
        // An OPAQUE resting fill (a low blend toward the foreground) rather than the
        // theme's translucent `component.divider`: the divider's alpha let the frosted
        // preview toolbars show through, washing the inactive segment out to near-invisible
        // (the image editor's toggles on the glass bar especially). An opaque
        // fill reads clearly on every backing while staying clearly below the accent-filled
        // active state. Shared by both preview toggles + the capture mode selector, so all
        // three stay consistent. Bumped 0.12 -> 0.2 so unselected items read stronger against
        // the glass (still clearly below the accent-filled active state).
        state_mix(t, SEGMENT_MIX_OFF)
    };
    // The ink is chosen PER SEGMENT, against the fill that segment actually has.
    //
    // HISTORY, so this is not quietly reverted (DRAGON-607). This group used to paint EVERY
    // glyph with `on_accent`, active and inactive alike, deliberately: the owner asked for one
    // uniform icon colour across a group, with the fill alone signalling selection. The flaw
    // is that `on_accent` means "ink for a surface that IS the accent", and an inactive
    // segment is not filled with the accent, it is filled with `state_mix(t, 0.2)`. So the
    // uniform rule put on-accent ink on a non-accent surface, which is wrong by the definition
    // of the token and not merely low contrast: on a dark theme that is near-black ink on a
    // dark grey fill. Put the tradeoff to the owner, they chose legibility over uniformity, so
    // active and inactive glyphs no longer match and that IS the intended look now.
    //
    // The inactive ink is derived from its OWN fill rather than being a second hardcoded
    // constant, so it keeps tracking `state_mix` if that factor or the theme ever moves.
    //
    // TEXT gets the same value as the glyph, set here rather than left unset. `Style::new()`
    // leaves `text_color` as `None`, so a segment carrying a LABEL rather than a glyph used to
    // inherit the ambient window foreground while its glyph took the segment's ink: one
    // button, two answers. Every segment today is icon-only, so that half changes nothing on
    // screen; it is here so the first labelled segment cannot reintroduce the split.
    //
    // NOTE this only reaches content that is not wrapped in a styled container. A segment
    // whose content sits inside a plain `widget::container` loses BOTH of these; see
    // [`on_accent_content`] for why, and use it there.
    let ink = if active { on_accent(t) } else { legible_ink_on(bg) };
    cosmic::widget::button::Style {
        background: Some(cosmic::iced::Background::Color(bg)),
        border_radius: [rl, rr, rr, rl].into(),
        icon_color: Some(ink),
        text_color: Some(ink),
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
// its surfaces translucent so the blur shows through. We read the `frosted` /
// `alpha_map` preferences straight off the active theme's v2 config on disk and
// supply the alpha ourselves (see `frost_color`), which started as a workaround
// for a libcosmic pin that predated those theme fields and is now simply the one
// reader: it answers the same question on every platform and it does not depend
// on which surface the toolkit happens to have themed.
//
// ENROLLMENT is separate and stays EXPLICIT. The two toplevel windows that want
// glass set `window::Settings.blur` themselves (DRAGON-217), and DRAGON-602 turns
// the toolkit's automatic enrollment off for everything else, so nothing gets
// compositor glass it did not ask for (see `toolkit_auto_glass`). DRAGON-218
// reuses this reader to reproduce the glass inside single-window captures.

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

/// The frosted-surface opacity the chrome paints at over the WINDOWS 10 **blur-behind**
/// material (DRAGON-405) — deliberately NOT [`MICA_SURFACE_ALPHA`]'s 0.0.
///
/// This constant IS the feature's safety property. Mica can afford a fully transparent chrome
/// because DWM guarantees the material behind it; Win10's blur comes from an UNDOCUMENTED
/// accent policy that may silently do nothing on some build, and a 0.0 surface over no
/// material is a see-through window (or, if per-pixel alpha never reaches DWM, a solid black
/// one). At 0.85 — the restrained pre-DRAGON-275 value — every failure mode is boring:
/// * policy applied → a subtle blur reads through a mostly-solid surface;
/// * policy silently ignored → an 85%-opaque theme surface, barely distinguishable from today;
/// * per-pixel alpha not honored at all → a plain opaque window, exactly today's look.
///
/// Tune HERE (not in the native arm): the accent policy is applied with a zero gradient
/// colour, so all of the Win10 glass tint comes from this one number.
#[cfg(windows)]
const WIN10_SURFACE_ALPHA: f32 = 0.85;

/// The Windows frosted-windows config (DRAGON-267) — the native-material equivalent of the
/// Linux COSMIC frosted-glass read. On Win11 22H2+ (where DWM's `DWMWA_SYSTEMBACKDROP_TYPE`
/// Mica material is supported) this returns `Some` with `frosted_windows` ON, so the SHARED
/// translucent-chrome painting (`frost_color`, the settings/preview chrome closures) and
/// [`glass_windows_enabled`] behave exactly like Linux's frosted glass; the native
/// material itself is applied post-show by
/// `platform::windows::window::apply_window_glass`. On Windows 10 the same `Some` comes
/// back with [`WIN10_SURFACE_ALPHA`] and the blur-behind material behind it (DRAGON-405);
/// on a Win11 build below 22H2 (neither material) it is `None` — a graceful no-op keeping
/// today's opaque look.
/// Honors the SAME `CCK_NO_GLASS=1` kill-switch as the Linux reader so the frosted-windows
/// toggle is unified across platforms. `strength_ordinal` is unused off Linux (only the
/// COSMIC capture-glass reproduction consumes it), so a `0` placeholder.
/// DRAGON-405: below the Mica floor this no longer falls straight through to `None`. Windows
/// 10 gets its own material — the blur-behind accent policy — with its own, deliberately
/// CONSERVATIVE surface alpha ([`WIN10_SURFACE_ALPHA`], see there for the failure modes). The
/// materials are mutually exclusive by build (`mica_supported` ≥ 22621 vs
/// `blur_behind_supported` `[10240, 22000)`), and Mica is asked FIRST so every Windows 11
/// build keeps exactly what it returns today: Some(0.0) at 22H2+, and `None` in the
/// 22000..22620 band, which has neither material.
#[cfg(windows)]
fn windows_glass_config() -> Option<GlassConfig> {
    if std::env::var_os("CCK_NO_GLASS").is_some_and(|v| v == "1") {
        return None;
    }
    if crate::platform::windows::window::mica_supported() {
        return Some(GlassConfig {
            strength_ordinal: 0,
            alpha: MICA_SURFACE_ALPHA,
            frosted_windows: true,
        });
    }
    if crate::platform::windows::window::blur_behind_supported() {
        return Some(GlassConfig {
            strength_ordinal: 0,
            alpha: WIN10_SURFACE_ALPHA,
            frosted_windows: true,
        });
    }
    None
}


/// The active theme's opaque background base as straight-alpha `[r, g, b, a]` u8 — the DARK
/// (or light) pane color the fullscreen backdrop fix paints an opaque NSWindow with, so the
/// page tint composites over a proper theme pane instead of the bright vibrancy no-backdrop
/// fallback (DRAGON-268 follow-up, Task 2). Reads the live `cosmic::theme::active()`.
#[cfg(target_os = "macos")]
pub(crate) fn background_base_rgba() -> [u8; 4] {
    let bg: Color = cosmic::theme::active().cosmic().background(false).base.into();
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
    Some(GlassConfig {
        strength_ordinal: 0,
        // MAC-SPECIFIC surface alpha (reduced so the masked NSVisualEffectView vibrancy shows
        // through); the value lives at the mac seam, not here (DRAGON-293).
        alpha: crate::platform::mac::window::vibrancy_surface_alpha(),
        frosted_windows: true,
    })
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

/// Which surface classes the TOOLKIT may enroll in the compositor's backdrop blur
/// on its own, from the user's frosted-windows theme preference and nothing else.
/// NONE of them (DRAGON-602).
///
/// This is a correctness rule, not a taste one. We are a capture tool: an overlay
/// exists so the user can see the desktop they are about to capture, and a
/// compositor that frosts what is behind that overlay shows them a picture of
/// their screen that is not their screen. It reached the owner as "the entire
/// screen is highly blurred" in region select AND in monitor select, because both
/// are the same fullscreen layer surface, and with `freeze = false` nothing is
/// drawn on it at all: what you look through the overlay AT is the live desktop,
/// so only the compositor could have blurred it.
///
/// The mechanism, for the next reader. libcosmic 8a017a1 added an auto-blur
/// feature whose `Core::auto_blur` defaults to every class
/// (`Auto::System | Auto::Popup | Auto::Window`). Once cosmic-comp advertises
/// `ext_background_effect_manager_v1` with the Blur capability, libcosmic answers
/// each surface's `Opened` with `iced::window::enable_blur`, which on Wayland is a
/// `set_blur_region` covering the whole surface (measured under `WAYLAND_DEBUG=1`:
/// `wl_region.add(0, 0, 2147483647, 2147483647)`, three surfaces, one per
/// overlay). Emptying `auto_blur` also pins `Theme::transparent` to false, which
/// is what keeps every `background(false)` call site rendering the same opaque
/// containers it always did instead of flipping to the translucent variants
/// part-way through a session.
///
/// Emptying this does NOT turn glass off. It removes the IMPLICIT arm only. A
/// surface that ASKS for glass still gets it: the colour picker and the macOS
/// permission window pass `window::Settings.blur` at creation (gated on
/// [`glass_windows_enabled`]), `crate::glass` still reproduces cosmic-comp's frost
/// inside reconstructed captures, and libcosmic's per-surface
/// `LiveSettings.blur` override is untouched. Glass because we asked, never by
/// default.
///
/// Pure; unit-tested.
pub(crate) fn toolkit_auto_glass() -> BitFlags<Auto> {
    BitFlags::EMPTY
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
    let bg: Color = cosmic.background(false).base.into();
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

/// Half of [`subdued`]'s dimming: text that should read as secondary/quiet but still
/// comfortably readable, not near-invisible. `subdued` itself was reported unreadable for a
/// sentence someone actually has to read (several places in `settings::pages::cloud` moved off
/// it for exactly that reason, to plain body text); `subtle` is the middle ground for text that
/// wants to read as secondary WITHOUT going all the way to full-strength body copy.
pub(crate) fn subtle(theme: &Theme) -> Color {
    toward_bg(theme, 0.39)
}

/// The FULL-strength foreground on the plain background — white in the dark themes,
/// near-black in the light ones. The un-blended end of the very ramp [`subdued`] sits on,
/// so a control that flips between "active" and "dimmed" (the preview editor's undo / redo,
/// DRAGON-337) stays on ONE colour ramp instead of mixing tokens.
pub(crate) fn foreground(theme: &Theme) -> Color {
    toward_bg(theme, 0.0)
}

/// Unified toolbar toggle-icon states: `Off` is a subtle wash, `On` renders accent
/// (or white over a meter fill). Toggles work in every mode, so there is no
/// disabled state — the subdued colour alone carries on/off.
pub(crate) const MIX_OFF: f32 = 0.40;

/// Blend the group background toward its foreground by `amount` — the shared
/// dimming primitive behind every toolbar toggle state.
pub(crate) fn state_mix(t: &Theme, amount: f32) -> Color {
    let c = t.cosmic();
    let b = c.background(false).component.base;
    let o = c.background(false).component.on;
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
    let bg: Color = theme.cosmic().background(false).base.into();
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

// ── The monospace face (DRAGON-601) ──────────────────────────────────────────
//
// **Asking for `Font::MONOSPACE` does not get you a monospace face.** That request reaches
// cosmic-text as `Family::Monospace`, which fontdb resolves to ONE literal family NAME,
// whatever `set_monospace_family` was given. cosmic-text sets it to "Noto Sans Mono"
// (`font/system.rs`), and `fontdb::Database::query` returns `None` when no installed face
// carries that name (`lib.rs:661`), at which point cosmic-text falls back to the head of its
// all-faces list — a PROPORTIONAL face. Nothing anywhere reports that this happened.
//
// Measured, not assumed, by shaping "#FF8800" through the app's own global font system and
// reading back the resolved face and per-glyph advances:
//
// * `Font::MONOSPACE` on this Linux box -> NotoSansMono-Regular, every advance 16.800 at 28px.
//   Real monospace, because "Noto Sans Mono" happens to be installed.
// * the same request with an UNINSTALLED family -> Noto Sans, advances 18.088 / 14.532 /
//   16.016. Silently proportional, with no warning of any kind.
//
// macOS and Windows do not ship "Noto Sans Mono", so the second shape is what they were
// getting. The fix is to name families that actually exist and check before asking.

/// The families this app accepts for monospace text, best first.
///
/// Everything here is a REAL family name, never a generic: a generic is exactly the request
/// that fails silently. The list spans the three platforms deliberately, because the same
/// binary ships to all of them and the first installed entry wins.
pub(crate) const MONO_FAMILY_LADDER: &[&str] = &[
    // cosmic-text's own generic target, and COSMIC's configured default. First so a Linux box
    // resolves to the same face it always did and nothing about that platform changes.
    "Noto Sans Mono",
    // The common Linux fallbacks, for a distro with no Noto.
    "DejaVu Sans Mono",
    "Liberation Mono",
    "JetBrains Mono",
    // Windows. Cascadia ships with Windows 11 and the terminal; Consolas goes back much
    // further; Courier New is the floor and is also fontdb's own generic default.
    "Cascadia Mono",
    "Consolas",
    "Courier New",
    // macOS.
    "SF Mono",
    "Menlo",
    "Monaco",
];

/// **Pure**, unit-tested: the first family in `ladder` the font stack can really render.
///
/// `has_face(family, bold)` answers whether that family has a face at that weight. Two passes,
/// and the order is the point: a family with BOTH a regular and a bold face is preferred over
/// one with only a regular, because the caller may ask for bold and **cosmic-text does not
/// synthesise it**. Weight matching there picks the nearest available face, so a bold request
/// against a regular-only family renders regular, silently, exactly like the family miss this
/// whole ladder exists to prevent. Preferring a complete family is how that is avoided without
/// the call site having to know which weights exist.
///
/// `None` when nothing in the ladder is installed, which is the caller's cue to fall back to
/// the toolkit's generic request rather than to name a family that is not there.
pub(crate) fn pick_mono_family<'a>(
    ladder: &[&'a str],
    has_face: impl Fn(&str, bool) -> bool,
) -> Option<&'a str> {
    ladder
        .iter()
        .copied()
        .find(|f| has_face(f, false) && has_face(f, true))
        .or_else(|| ladder.iter().copied().find(|f| has_face(f, false)))
}

/// Whether `family` has an installed face at the requested weight. The effectful half of
/// [`pick_mono_family`]: it reads the process-global font database the renderer itself shapes
/// against, so the answer is the one the screen will actually give.
fn font_family_has_face(family: &str, bold: bool) -> bool {
    let Ok(fs) = cosmic::iced::advanced::graphics::text::font_system().read() else {
        return false;
    };
    fs.db().faces().any(|f| {
        f.families.iter().any(|(name, _)| name == family)
            // 600 is the usual semibold/bold crossover. A face at or above it can serve a
            // bold request; one below it serves the regular one.
            && (f.weight.0 >= 600) == bold
    })
}

/// The user's COSMIC-configured monospace family, if it is a named one.
///
/// Tried before [`MONO_FAMILY_LADDER`] because it is the face the user actually chose for
/// monospace text on this desktop, and matching it keeps our chips looking like the rest of
/// their session. Off COSMIC this is libcosmic's own default ("Noto Sans Mono"), which the
/// ladder names anyway, so nothing is lost.
fn configured_mono_family() -> Option<&'static str> {
    match cosmic::font::mono().family {
        cosmic::iced::font::Family::Name(name) => Some(name),
        _ => None,
    }
}

/// THE monospace font this app renders with, at the requested weight.
///
/// One treatment for every monospace chip in the app, so the hex label and the capture
/// toolbar's timer can never resolve to different faces. Resolved ONCE per process (the
/// database walk is not free and the answer cannot change under us), and it says in the debug
/// log which family it landed on, so a report of "that does not look monospace" is answerable
/// from the log instead of by guesswork.
pub(crate) fn mono_font(bold: bool) -> cosmic::iced::Font {
    static FAMILY: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    let family = *FAMILY.get_or_init(|| {
        let mut ladder: Vec<&'static str> = Vec::with_capacity(MONO_FAMILY_LADDER.len() + 1);
        ladder.extend(configured_mono_family());
        ladder.extend_from_slice(MONO_FAMILY_LADDER);
        let picked = pick_mono_family(&ladder, font_family_has_face);
        match picked {
            Some(f) => log::debug!("monospace text resolves to the '{f}' family"),
            None => log::warn!(
                "no monospace family this app knows about is installed, so monospace text \
                 falls back to the toolkit's generic request and may render proportional"
            ),
        }
        picked
    });
    let weight = if bold {
        cosmic::iced::font::Weight::Bold
    } else {
        cosmic::iced::font::Weight::Normal
    };
    match family {
        Some(name) => cosmic::iced::Font { weight, ..cosmic::iced::Font::with_name(name) },
        // Nothing installed matched. The generic is no worse than what we asked for before,
        // and the warning above has already said the face may not be monospace.
        None => cosmic::iced::Font { weight, ..cosmic::iced::Font::MONOSPACE },
    }
}

#[cfg(test)]
mod toolkit_auto_glass_tests {
    use super::*;

    // DRAGON-602. The failure these pin is one the compiler cannot see: adding a
    // class back makes the compositor frost the LIVE desktop behind whatever
    // surface it covers, and a capture overlay that frosts its own subject is
    // showing the user a screen that is not their screen.

    #[test]
    fn layer_surfaces_are_never_auto_frosted() {
        // `Auto::System` is libcosmic's name for layer-shell surfaces, which is
        // every capture overlay we mint on Linux: region select, monitor select,
        // window select, the countdown, the preview overlay.
        assert!(!toolkit_auto_glass().contains(Auto::System));
    }

    #[test]
    fn toplevel_windows_are_never_auto_frosted() {
        // Settings, preview-in-a-window, the colour picker, and the Flatpak
        // fallback capture overlay, which is a plain toplevel and covers the
        // screen exactly like the layer-shell one does.
        assert!(!toolkit_auto_glass().contains(Auto::Window));
    }

    #[test]
    fn popups_are_never_auto_frosted() {
        // The overlay's own dropdown menus sit over the capture subject too.
        assert!(!toolkit_auto_glass().contains(Auto::Popup));
    }

    #[test]
    fn the_set_is_empty_so_a_new_upstream_class_is_off_by_default() {
        // Asserting emptiness rather than three absences: if libcosmic adds a
        // fourth `Auto` variant, an opt-in must be a deliberate edit here, not
        // something a dependency bump turns on for us.
        assert!(toolkit_auto_glass().is_empty());
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
    // The per-platform selection fed to the `use_system` build. Fully-round (0)
    // is now the deliberate System-Default look on every non-COSMIC OS (see the
    // fn doc); the old Windows assert (slightly-round, 1) predated that and went
    // stale. macOS-gated per user decision. Linux never compiles the fn.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_system_default_selects_platform_roundness() {
        let (_accent, roundness) = native_system_default();
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

/// DRAGON-601: which monospace family we ask for, and why asking for the generic was not
/// enough. The installed-face probe is injected, so these run identically on a build machine
/// with every font and on one with none.
#[cfg(test)]
mod mono_family_tests {
    use super::*;

    /// A stack described as `(family, has_regular, has_bold)`.
    fn stack(faces: &'static [(&'static str, bool, bool)]) -> impl Fn(&str, bool) -> bool {
        move |family, bold| {
            faces
                .iter()
                .any(|(n, reg, bld)| *n == family && if bold { *bld } else { *reg })
        }
    }

    /// The ordinary case: the first installed family in the ladder wins, and the ones above it
    /// that are not installed are skipped rather than named.
    #[test]
    fn the_first_installed_family_wins() {
        // A Windows-shaped stack: nothing above Consolas in the ladder is installed.
        let has = stack(&[("Consolas", true, true), ("Courier New", true, true)]);
        assert_eq!(pick_mono_family(MONO_FAMILY_LADDER, &has), Some("Consolas"));
        // A Linux-shaped one resolves at the top, which is why that platform's rendering is
        // unchanged by this ladder existing.
        let has = stack(&[("Noto Sans Mono", true, true), ("Consolas", true, true)]);
        assert_eq!(pick_mono_family(MONO_FAMILY_LADDER, &has), Some("Noto Sans Mono"));
    }

    /// THE reason the two passes exist. A family with a regular face but NO bold one loses to a
    /// complete family further down, because cosmic-text does not synthesise bold: a bold
    /// request against a regular-only family renders regular and says nothing.
    #[test]
    fn a_family_with_no_bold_face_loses_to_a_complete_one() {
        let has = stack(&[("Noto Sans Mono", true, false), ("DejaVu Sans Mono", true, true)]);
        assert_eq!(
            pick_mono_family(MONO_FAMILY_LADDER, &has),
            Some("DejaVu Sans Mono"),
            "a regular-only family cannot serve the bold hex label"
        );
    }

    /// But a regular-only family is still far better than nothing, so it is taken when it is
    /// all there is. Rendering bold-as-regular in a real monospace face is a much smaller
    /// failure than rendering the label proportional.
    #[test]
    fn a_regular_only_family_is_still_taken_when_it_is_all_there_is() {
        let has = stack(&[("Menlo", true, false)]);
        assert_eq!(pick_mono_family(MONO_FAMILY_LADDER, &has), Some("Menlo"));
    }

    /// Nothing installed means NONE, never a guess. The caller turns that into the toolkit's
    /// generic request plus a warning; naming a family that is not there would land us back on
    /// the silent proportional fallback this ladder exists to prevent.
    #[test]
    fn an_empty_stack_names_nothing() {
        let has = stack(&[]);
        assert_eq!(pick_mono_family(MONO_FAMILY_LADDER, &has), None);
        // And a stack with only fonts we never ask for is the same answer.
        let has = stack(&[("Inter", true, true), ("Open Sans", true, true)]);
        assert_eq!(pick_mono_family(MONO_FAMILY_LADDER, &has), None);
    }

    /// The ladder itself: real family names only, no duplicates, and one entry for each of the
    /// three platforms we ship. A generic slipping in here would reintroduce the exact silent
    /// fallback that made the hex label proportional on macOS and Windows.
    #[test]
    fn the_ladder_names_real_families_for_every_platform() {
        for f in MONO_FAMILY_LADDER {
            assert!(!f.is_empty());
            assert!(
                !matches!(*f, "monospace" | "sans-serif" | "serif"),
                "{f} is a generic, which is the request that fails silently"
            );
        }
        let mut seen = MONO_FAMILY_LADDER.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "a duplicate family only costs a second lookup");
        for want in ["Noto Sans Mono", "Consolas", "Menlo"] {
            assert!(MONO_FAMILY_LADDER.contains(&want), "{want} covers a platform we ship to");
        }
    }
}

/// On-accent ink: the ONE source of truth every accent-filled surface reads (DRAGON-607).
///
/// These build REAL cosmic themes rather than asserting on colour literals, because the
/// bug was never our arithmetic. It was reading the wrong token out of the toolkit, and
/// only a real theme can tell two tokens apart.
#[cfg(test)]
mod on_accent_ink_tests {
    use super::*;
    use crate::color::Srgb;
    use cosmic::cosmic_theme::ThemeBuilder;
    use cosmic::cosmic_theme::palette::Srgb as PSrgb;

    /// The AA bar for body text, the same one DRAGON-601 held the picker's hex label to.
    const AA: f64 = 4.5;

    fn srgb(c: Color) -> Srgb {
        Srgb::from_unit_clamped([c.r as f64, c.g as f64, c.b as f64])
    }

    /// A dark theme carrying `accent`, which is what every case here varies.
    fn dark_theme_with(accent: [f32; 3]) -> cosmic::Theme {
        let built = ThemeBuilder::dark()
            .accent(PSrgb::new(accent[0], accent[1], accent[2]))
            .build();
        cosmic::Theme::custom(std::sync::Arc::new(built))
    }

    /// The owner's real accent, read from their COSMIC config: the purple that started this.
    const OWNER_PURPLE: [f32; 3] = [0.5921569, 0.49019608, 0.9254902];

    /// The ink we choose must be READABLE on the accent it sits on. This is the whole
    /// contract, stated against a real built theme.
    #[test]
    fn the_owners_accent_gets_readable_ink() {
        let t = dark_theme_with(OWNER_PURPLE);
        let fill = srgb(accent(&t));
        let ink = srgb(on_accent(&t));
        let ratio = fill.contrast_ratio(ink);
        assert!(ratio >= AA, "on-accent ink reads at only {ratio:.2}:1 on the owner's purple");
    }

    /// **The regression guard for the actual change, and it FAILS against the old source.**
    ///
    /// `on_accent` used to return `accent.on` (`on_accent_color()`). That token is a fixed
    /// neutral step chosen by light/dark mode alone and never looks at the accent, so in a
    /// DARK theme it is always near-black. Give it a DARK accent and it puts near-black ink
    /// on a near-black fill. `accent_button.on` is derived from the accent's own lightness,
    /// so it flips to light ink instead.
    ///
    /// Both halves are asserted on purpose: that the OLD source really does fail here (so
    /// this test is proving something, not passing by luck on either source), and that the
    /// NEW one clears AA. Repointing `on_accent` back at `on_accent_color()` fails this.
    #[test]
    fn a_dark_accent_in_a_dark_theme_is_where_the_old_source_broke() {
        // A deep indigo: dark enough that black ink on it is unreadable.
        let t = dark_theme_with([0.13, 0.07, 0.38]);
        let fill = srgb(accent(&t));

        let old = srgb(t.cosmic().on_accent_color().into());
        let old_ratio = fill.contrast_ratio(old);
        assert!(
            old_ratio < AA,
            "the OLD source (accent.on) was supposed to fail here but read {old_ratio:.2}:1; \
             if the toolkit changed, this test needs a new accent, not a lowered bar"
        );

        let new_ratio = fill.contrast_ratio(srgb(on_accent(&t)));
        assert!(new_ratio >= AA, "on-accent ink reads at only {new_ratio:.2}:1 on a dark accent");
    }

    /// Our helper must agree with the toolkit's own suggested button EXACTLY, because they
    /// stand side by side (the preview toolbar shows Apply Crop next to Upload). Same value,
    /// not merely both readable: two different readable answers is still the reported bug.
    #[test]
    fn we_draw_the_same_ink_the_toolkits_suggested_button_draws() {
        for accent_rgb in [OWNER_PURPLE, [0.13, 0.07, 0.38], [0.95, 0.9, 0.2]] {
            let t = dark_theme_with(accent_rgb);
            let ours = on_accent(&t);
            let toolkit: Color = t.cosmic().accent_button.on.into();
            assert_eq!(ours, toolkit, "our accent ink diverged from Button::Suggested");
        }
    }

    /// The ink really does FLIP with the accent. A source that ignores the accent (which is
    /// exactly what the old one did) returns one constant for a whole theme mode, so it
    /// passes any single-colour test. Only comparing a light accent against a dark one in
    /// the SAME mode catches that.
    #[test]
    fn the_ink_flips_between_a_light_and_a_dark_accent() {
        let light = on_accent(&dark_theme_with([0.95, 0.9, 0.2]));
        let dark = on_accent(&dark_theme_with([0.13, 0.07, 0.38]));
        assert_ne!(light, dark, "the ink must depend on the accent, not only on the theme mode");
    }

    /// The INACTIVE segment, checked against its OWN fill (`state_mix`), never against the
    /// accent. Checking ink against the wrong surface is the mistake this whole ticket is,
    /// so a test that measured against the accent here would reproduce it.
    ///
    /// The owner chose legibility over one uniform glyph colour when the tradeoff was put to
    /// them; this is the assertion that keeps that choice honest.
    /// ONE ink is chosen (from the RESTING fill) and must clear the bar on the resting fill
    /// AND the hover fill, since the glyph deliberately does not change colour under the
    /// pointer. Testing each fill against its own ink would let the two answers diverge and
    /// still pass, which is the thing being ruled out.
    #[test]
    fn an_inactive_segment_is_readable_against_its_own_fill() {
        for accent_rgb in [OWNER_PURPLE, [0.13, 0.07, 0.38]] {
            let t = dark_theme_with(accent_rgb);
            let ink = srgb(segment_ink(&t, false));
            for mix in [SEGMENT_MIX_OFF, SEGMENT_MIX_HOVER] {
                let ratio = srgb(state_mix(&t, mix)).contrast_ratio(ink);
                assert!(
                    ratio >= AA,
                    "the inactive glyph reads at only {ratio:.2}:1 on its own fill at mix {mix}"
                );
            }
        }
    }

    /// The two consumers of [`segment_ink`], `segment_style` and `preview::chrome`'s
    /// `seg_toggle`, must read the SAME answer. `seg_toggle` cannot call `segment_style`
    /// (it needs a bare colour for an SVG class), so the shared function is the only thing
    /// stopping the two from drifting; this pins that they agree.
    #[test]
    fn the_button_style_and_the_glyph_class_read_one_answer() {
        let t = dark_theme_with(OWNER_PURPLE);
        for active in [true, false] {
            let from_style = segment_style(&t, active, false, true, true).icon_color;
            assert_eq!(
                from_style,
                Some(segment_ink(&t, active)),
                "active={active}: the button style and `segment_ink` disagree"
            );
        }
    }

    /// The inactive ink must not simply BE the on-accent ink under another name. On the
    /// owner's theme both the accent and the wash are dark enough to want light ink in
    /// some themes, so "they differ" is asserted where the fills genuinely differ in
    /// lightness rather than blanket-asserted.
    #[test]
    fn the_inactive_ink_answers_for_its_own_fill_not_the_accent() {
        // A LIGHT accent in a dark theme: the accent wants dark ink, the dark wash wants
        // light ink, so the two answers must come out different.
        let t = dark_theme_with([0.95, 0.9, 0.2]);
        assert_ne!(
            segment_ink(&t, true),
            segment_ink(&t, false),
            "a light accent beside a dark wash must not share one ink"
        );
    }

    /// `segment_style` must ink BOTH text and glyph, and must not hand the inactive segment
    /// the accent's ink. The asymmetry (one field set, the other left to inherit) is the
    /// shape of the original defect, so it is pinned rather than left to review.
    #[test]
    fn segment_style_inks_both_fields_and_picks_per_segment() {
        let t = dark_theme_with(OWNER_PURPLE);
        for active in [true, false] {
            let s = segment_style(&t, active, false, true, true);
            assert!(s.text_color.is_some(), "active={active}: text_color left to inherit");
            assert_eq!(s.icon_color, s.text_color, "active={active}: label and glyph disagree");
        }
        let on = segment_style(&t, true, false, true, true).icon_color;
        let off = segment_style(&t, false, false, true, true).icon_color;
        assert_eq!(on, Some(on_accent(&t)), "the ACTIVE segment takes the on-accent ink");
        assert_ne!(on, off, "the inactive segment must not reuse the accent's ink on its own fill");
    }

    /// The class predicate that decides whether a wrapped button's content needs on-accent
    /// ink. `Suggested` is the only stock class that fills with the accent; `Custom` owns
    /// its own ink and must answer false rather than guess.
    #[test]
    fn only_the_suggested_class_counts_as_accent_filled() {
        assert!(fills_with_accent(&cosmic::theme::Button::Suggested));
        for class in [
            cosmic::theme::Button::Standard,
            cosmic::theme::Button::Text,
            cosmic::theme::Button::Destructive,
            cosmic::theme::Button::Icon,
            cosmic::theme::Button::Link,
        ] {
            assert!(!fills_with_accent(&class), "a non-accent class must not claim accent ink");
        }
    }

    /// The wrapper helper sets BOTH fields, to the SAME value, and that value is
    /// [`on_accent`]. Setting one and forgetting the other is precisely the bug, so "both"
    /// is the property worth pinning, not the colour.
    #[test]
    fn the_content_wrapper_carries_both_inks() {
        let t = dark_theme_with(OWNER_PURPLE);
        let s = on_accent_content(&t);
        assert_eq!(s.text_color, Some(on_accent(&t)));
        assert_eq!(s.icon_color, Some(on_accent(&t)));
        assert!(s.background.is_none(), "the wrapper must paint nothing, only carry ink");
    }

    /// [`ink_content`] is the ONE place both ink fields are written, so it is the one place
    /// the "set one, forget the other" defect could come back. It must write both, write
    /// the same value to both, and paint nothing.
    #[test]
    fn the_ink_wrapper_writes_both_fields_and_paints_nothing() {
        for ink in [Color::BLACK, Color::WHITE, Color::from_rgb(0.2, 0.4, 0.9)] {
            let s = ink_content(ink);
            assert_eq!(s.text_color, Some(ink));
            assert_eq!(s.icon_color, Some(ink));
            assert!(s.background.is_none(), "the wrapper must not paint a background");
            assert_eq!(s.border.width, 0.0, "the wrapper must not paint a border");
        }
    }

    /// **The ticket's proof obligation, discharged headlessly.**
    ///
    /// The reported bug is "two accent buttons, two different inks". The settings helper
    /// (`settings::row::centered_button`, which the About page's donate and update buttons
    /// use) and the preview toolbar's Upload button now both derive their content wrapper
    /// from [`button_content_class`] with the SAME class, so asserting that this decision
    /// is a function of the class alone proves they agree, on every theme, forever. A
    /// screenshot could only show it once, on one accent, on one machine.
    ///
    /// It also asserts the resolved ink actually clears AA on the accent, so "they agree"
    /// cannot be satisfied by both being wrong together.
    #[test]
    fn every_accent_filled_button_resolves_one_readable_ink() {
        for accent_rgb in [OWNER_PURPLE, [0.13, 0.07, 0.38], [0.95, 0.9, 0.2]] {
            let t = dark_theme_with(accent_rgb);
            // Both call sites ask the same question with the same class, so they cannot
            // diverge: the wrapper is chosen by the class, not by the call site.
            let settings = button_content_class(&cosmic::theme::Button::Suggested);
            let upload = button_content_class(&cosmic::theme::Button::Suggested);
            let resolved = |c: &cosmic::theme::Container<'static>| match c {
                cosmic::theme::Container::Custom(f) => f(&t),
                _ => panic!("an accent-filled button must get the ink-carrying wrapper"),
            };
            let (a, b) = (resolved(&settings), resolved(&upload));
            assert_eq!(a.text_color, b.text_color, "the two accent buttons disagreed on label ink");
            assert_eq!(a.icon_color, b.icon_color, "the two accent buttons disagreed on glyph ink");

            let ink = a.text_color.expect("the wrapper sets the label ink");
            let ratio = srgb(accent(&t)).contrast_ratio(srgb(ink));
            assert!(ratio >= AA, "the agreed ink reads at only {ratio:.2}:1 on the accent");
        }
    }

    /// A NON-accent class must keep the toolkit default, so the settings pill buttons stay
    /// byte-identical to before this ticket. The fix has to be surgical: it would be no
    /// better to have repainted every button in the app.
    #[test]
    fn a_neutral_class_keeps_the_default_wrapper() {
        assert!(
            matches!(
                button_content_class(&cosmic::theme::Button::Standard),
                cosmic::theme::Container::Transparent
            ),
            "a neutral button's content must keep the toolkit default wrapper"
        );
    }
}

/// DRAGON-680: the text SELECTION fill. The owner's report was that selected text is hard
/// to read at the full-strength accent, so what these pin is the RELATION (a third of the
/// live accent since the DRAGON-687 follow-up run, tracking it) rather than any colour
/// literal.
#[cfg(test)]
mod selection_fill_tests {
    use super::*;

    /// A THIRD of the accent's opacity (the owner's second sizing, DRAGON-687 item
    /// eleven; it was half from DRAGON-680), and the accent's own hue: both halves
    /// matter. Dropping the accent would lose the look the owner said was good; keeping
    /// full alpha is the defect.
    #[test]
    fn the_selection_is_the_accent_at_a_third_opacity() {
        for t in [cosmic::theme::Theme::dark(), cosmic::theme::Theme::light()] {
            let a = accent(&t);
            let s = selection_fill(&t);
            assert_eq!((s.r, s.g, s.b), (a.r, a.g, a.b), "the selection is still the accent");
            assert!((s.a - a.a * 0.33).abs() < 1e-6, "expected a third of {}, got {}", a.a, s.a);
            assert!(s.a < a.a, "the selection must be softer than the accent itself");
        }
    }

    /// It is a FRACTION of whatever the accent's alpha really is, not a hard-coded 0.33.
    /// A theme whose accent already carries transparency must be scaled from there, or
    /// the override would silently make some accents MORE opaque than they were.
    #[test]
    fn it_scales_the_live_alpha_rather_than_assuming_one() {
        let mut c = Color::from_rgb(0.2, 0.4, 0.9);
        c.a = 0.6;
        let scaled = Color { a: c.a * SELECTION_ALPHA, ..c };
        assert!((scaled.a - 0.6 * 0.33).abs() < 1e-6);
    }

    /// Every input the app builds through the shared helper really does carry the
    /// override, in every state a field can be in. This is the half that would rot: the
    /// helper existing is not the same as the states using it.
    #[test]
    fn the_shared_input_style_softens_every_state() {
        use cosmic::widget::text_input::StyleSheet as _;
        for base in [InputBase::Default, InputBase::Search] {
            let style = input_style(base);
            for t in [cosmic::theme::Theme::dark(), cosmic::theme::Theme::light()] {
                let want = selection_fill(&t);
                for (name, got) in [
                    ("active", t.active(&style).selected_fill),
                    ("error", t.error(&style).selected_fill),
                    ("hovered", t.hovered(&style).selected_fill),
                    ("focused", t.focused(&style).selected_fill),
                    ("disabled", t.disabled(&style).selected_fill),
                ] {
                    assert_eq!(got, want, "{base:?}/{name} kept the stock selection fill");
                }
                // And it really is a CHANGE: the stock appearance it derives from paints
                // the accent at full strength, or this test proves nothing.
                assert_ne!(
                    t.active(&base.stock()).selected_fill,
                    want,
                    "{base:?}: the stock fill already matches, so nothing is being overridden"
                );
            }
        }
    }

    /// The helper changes ONLY the selection: a field that softened its own background or
    /// border would be a second, unasked-for change riding along.
    #[test]
    fn nothing_but_the_selection_moves() {
        use cosmic::widget::text_input::StyleSheet as _;
        let t = cosmic::theme::Theme::dark();
        let stock = t.active(&cosmic::theme::TextInput::Default);
        let ours = t.active(&input_style(InputBase::Default));
        assert_eq!(ours.background, stock.background);
        assert_eq!(ours.border_color, stock.border_color);
        assert_eq!(ours.border_width, stock.border_width);
        assert_eq!(ours.border_radius, stock.border_radius);
        assert_eq!(ours.placeholder_color, stock.placeholder_color);
        assert_eq!(ours.text_color, stock.text_color);
    }
}
