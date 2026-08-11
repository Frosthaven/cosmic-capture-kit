//! Pure colour model: the seven notations the colour picker offers, their
//! formatters, and their parsers (DRAGON-582).
//!
//! No widget, no platform, no `App`: an 8-bit sRGB triple goes in, a string comes out,
//! and the same string parses back. That is what lets the picker window edit ANY row and
//! re-derive every other row plus the swatch from one value, and it is why every rule
//! here is unit-tested on any host.
//!
//! # One conversion stack, not seven
//!
//! Everything hangs off two shared stages, so a fix to either reaches every notation:
//!
//! ```text
//! sRGB 8-bit  <->  sRGB 0..1  <->  LINEAR sRGB  <->  CIE XYZ (D65)  <->  CIELAB
//!                       |                                  |
//!                    HSL/HSV                             OKLab  <->  OKLCh
//!                     CMYK
//! ```
//!
//! * The transfer function is the **sRGB EOTF** (the piecewise 2.4 curve, not a plain
//!   2.2 gamma).
//! * The white point is **D65** throughout, for CIELAB and for OKLab alike. OKLab is
//!   reached through the SAME XYZ stage CIELAB uses (Ottosson's `M1` takes XYZ D65),
//!   rather than through the shortcut linear-sRGB-to-LMS matrix. The shortcut agrees to
//!   about 1e-4; having two would mean two things to fix.
//! * The maths runs in `f64`. Not precision for its own sake: an 8-bit round trip
//!   through five matrices has to come back to the SAME byte, and in `f32` the
//!   accumulated error puts some channels one step off.
//!
//! # CMYK here is device-agnostic, and that is not print-accurate
//!
//! [`ColorFormat::Cmyk`] is the naive `k = 1 - max(r, g, b)` separation every colour
//! picker shows. It is a reversible re-encoding of the same sRGB triple, NOT an
//! ICC-managed separation for any real press or printer: a true CMYK value depends on
//! the output profile (ink set, paper, total ink limit), which this app has no way to
//! know. Treat the row as "the numbers a design tool would show", never as plate values.
//! The round-trip test asserts only what the naive formula actually guarantees.
//!
//! # Out of gamut clamps, on purpose
//!
//! CIELAB and OKLCh can express colours sRGB cannot (`lab(90% -128 100)` has no sRGB
//! answer). A typed value that lands outside the cube is CLAMPED per channel to `0..1`
//! before it becomes a byte. The alternative — a wrapped or garbage swatch — is the
//! exact "reports the wrong colour" failure this tool must not have. Clamping is stated
//! here, done in one place ([`Srgb::from_unit_clamped`]), and tested.
//!
//! # Privacy
//!
//! A picked colour is the user's CONTENT. Nothing in this module logs, and no caller may
//! put a formatted value into the debug log. [`ColorFormat::id`] exists so a log line can
//! name the NOTATION without naming the colour.

/// An 8-bit sRGB colour: the app's one exchange type for a picked pixel.
///
/// Deliberately opaque. The picker samples a screen pixel, and a screen pixel has no
/// meaningful alpha to report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Srgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Srgb {
    /// Build from raw bytes.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The three channels as `0..1` reals, still sRGB-encoded (gamma applied).
    pub fn to_unit(self) -> [f64; 3] {
        [
            self.r as f64 / 255.0,
            self.g as f64 / 255.0,
            self.b as f64 / 255.0,
        ]
    }

    /// Pure, unit-tested: `0..1` sRGB-encoded reals back to bytes, CLAMPED into gamut
    /// and rounded half-up. THE one place a real becomes a byte, so the clamp cannot be
    /// forgotten at a call site (see the module doc on out-of-gamut values).
    pub fn from_unit_clamped(v: [f64; 3]) -> Self {
        let q = |x: f64| {
            if x.is_nan() {
                0
            } else {
                (x.clamp(0.0, 1.0) * 255.0).round() as u8
            }
        };
        Self { r: q(v[0]), g: q(v[1]), b: q(v[2]) }
    }

    /// Pure, unit-tested: LINEAR sRGB back to bytes, clamped into gamut. The exit of
    /// every colorimetric notation (CIELAB, OKLCh).
    pub fn from_linear_clamped(lin: [f64; 3]) -> Self {
        Self::from_unit_clamped([
            linear_to_srgb(lin[0]),
            linear_to_srgb(lin[1]),
            linear_to_srgb(lin[2]),
        ])
    }

    /// This colour in LINEAR sRGB (the sRGB EOTF applied).
    pub fn to_linear(self) -> [f64; 3] {
        let u = self.to_unit();
        [srgb_to_linear(u[0]), srgb_to_linear(u[1]), srgb_to_linear(u[2])]
    }

    /// The `#RRGGBB` spelling — the picker's primary value, so it has a direct name
    /// rather than going through [`ColorFormat::Hex`] at every call site.
    pub fn hex(self) -> String {
        ColorFormat::Hex.format(self)
    }

    /// Pure, unit-tested: the WCAG relative luminance of this colour (`0..1`).
    pub fn relative_luminance(self) -> f64 {
        let l = self.to_linear();
        0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
    }

    /// Pure, unit-tested: should text drawn ON this colour be BLACK (`true`) or WHITE?
    ///
    /// The overlay's hex label paints the picked colour as its own background, so its
    /// text has to stay legible over anything on screen. The threshold is the usual WCAG
    /// crossover: black-on-colour beats white-on-colour once relative luminance passes
    /// ~0.179, where the two contrast ratios (against 0 and against 1) meet.
    pub fn wants_dark_text(self) -> bool {
        self.relative_luminance() > 0.179
    }

    /// Pure, unit-tested: the WCAG 2.x contrast ratio between this colour and `other`,
    /// from `1.0` (identical) to `21.0` (black against white). Order does not matter.
    ///
    /// The number that says whether a pairing is legible, rather than whether it merely
    /// looks different. 4.5:1 is the AA bar for body text, which is what DRAGON-601 held
    /// the picker's hex label to and what DRAGON-607 holds on-accent button labels to.
    ///
    /// Both colours are treated as OPAQUE. A translucent ink over a fill is a different
    /// question (it needs the composite first), and this deliberately does not pretend to
    /// answer it.
    ///
    /// Dead outside `cfg(test)` today, and kept anyway: it is a MEASUREMENT that tests hold
    /// production to, not a step in any render path. Production picks its ink with
    /// [`Self::wants_dark_text`] (one crossover, one answer); this is how the suite checks
    /// that the chosen pairing is actually legible. It lives here rather than inside a test
    /// module because two of them use it now, `color::contrast_ratio_tests` and
    /// `app::theme::on_accent_ink_tests`, and DRAGON-607 was caused by exactly this kind of
    /// duplicated answer drifting apart. DRAGON-601 kept its copy private to one test
    /// module, which is why there was a second copy to begin with.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn contrast_ratio(self, other: Srgb) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

// ── The shared stages ────────────────────────────────────────────────────────

/// The sRGB EOTF: one sRGB-encoded channel (`0..1`) to LINEAR light.
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB OETF: one LINEAR channel back to sRGB encoding.
fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// LINEAR sRGB to CIE XYZ, D65 — the sRGB primaries matrix (IEC 61966-2-1).
fn linear_to_xyz(l: [f64; 3]) -> [f64; 3] {
    [
        0.412_456_4 * l[0] + 0.357_576_1 * l[1] + 0.180_437_5 * l[2],
        0.212_672_9 * l[0] + 0.715_152_2 * l[1] + 0.072_175_0 * l[2],
        0.019_333_9 * l[0] + 0.119_192_0 * l[1] + 0.950_304_1 * l[2],
    ]
}

/// CIE XYZ (D65) back to LINEAR sRGB. May land outside `0..1`; the caller clamps.
fn xyz_to_linear(x: [f64; 3]) -> [f64; 3] {
    [
        3.240_454_2 * x[0] - 1.537_138_5 * x[1] - 0.498_531_4 * x[2],
        -0.969_266_0 * x[0] + 1.876_010_8 * x[1] + 0.041_556_0 * x[2],
        0.055_643_4 * x[0] - 0.204_025_9 * x[1] + 1.057_225_2 * x[2],
    ]
}

/// The D65 white point in XYZ, normalised to `Y = 1` — CIELAB's reference white.
const WHITE_D65: [f64; 3] = [0.950_47, 1.0, 1.088_83];

// ── CIELAB ───────────────────────────────────────────────────────────────────

/// CIE XYZ (D65) to CIELAB `(L*, a*, b*)`, `L*` in `0..100`.
fn xyz_to_lab(x: [f64; 3]) -> [f64; 3] {
    const DELTA: f64 = 6.0 / 29.0;
    let f = |t: f64| {
        if t > DELTA * DELTA * DELTA {
            t.cbrt()
        } else {
            t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
        }
    };
    let fx = f(x[0] / WHITE_D65[0]);
    let fy = f(x[1] / WHITE_D65[1]);
    let fz = f(x[2] / WHITE_D65[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIELAB back to CIE XYZ (D65).
fn lab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    const DELTA: f64 = 6.0 / 29.0;
    let finv = |t: f64| {
        if t > DELTA {
            t * t * t
        } else {
            3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
        }
    };
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    [
        WHITE_D65[0] * finv(fx),
        WHITE_D65[1] * finv(fy),
        WHITE_D65[2] * finv(fz),
    ]
}

// ── OKLab / OKLCh ────────────────────────────────────────────────────────────
//
// Ottosson's `M1` (XYZ D65 to a cone-response LMS) and `M2` (the cube-rooted LMS to
// OKLab), taken through the SAME XYZ stage CIELAB uses (module doc).

/// CIE XYZ (D65) to OKLab `(L, a, b)`, `L` in `0..1`.
fn xyz_to_oklab(x: [f64; 3]) -> [f64; 3] {
    let l = 0.818_933_010_1 * x[0] + 0.361_866_742_4 * x[1] - 0.128_859_713_7 * x[2];
    let m = 0.032_984_543_6 * x[0] + 0.929_311_871_5 * x[1] + 0.036_145_638_7 * x[2];
    let s = 0.048_200_301_8 * x[0] + 0.264_366_269_1 * x[1] + 0.633_851_707_0 * x[2];
    // `f64::cbrt` is SIGNED, which matters: an out-of-gamut XYZ can push a cone
    // response negative, and a `powf(1/3)` there would be NaN.
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_,
        1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_,
        0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_,
    ]
}

/// OKLab back to CIE XYZ (D65).
fn oklab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    let l_ = lab[0] + 0.396_337_777_4 * lab[1] + 0.215_803_757_3 * lab[2];
    let m_ = lab[0] - 0.105_561_345_8 * lab[1] - 0.063_854_172_8 * lab[2];
    let s_ = lab[0] - 0.089_484_177_5 * lab[1] - 1.291_485_548_0 * lab[2];
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        1.227_013_851_1 * l - 0.557_799_980_7 * m + 0.281_256_149_0 * s,
        -0.040_580_178_4 * l + 1.112_256_869_6 * m - 0.071_676_678_7 * s,
        -0.076_381_284_5 * l - 0.421_481_978_4 * m + 1.586_163_220_4 * s,
    ]
}

/// A Lab-family `(a, b)` pair as polar `(chroma, hue degrees in 0..360)`.
fn ab_to_ch(a: f64, b: f64) -> (f64, f64) {
    let c = (a * a + b * b).sqrt();
    // A neutral has no hue: report 0 rather than whatever `atan2` makes of the noise,
    // so grey formats stably instead of flickering between 0 and 180.
    if c < 1e-9 {
        return (0.0, 0.0);
    }
    let mut h = b.atan2(a).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    (c, h)
}

/// Polar `(chroma, hue degrees)` back to a Lab-family `(a, b)` pair.
fn ch_to_ab(c: f64, h: f64) -> (f64, f64) {
    let r = h.to_radians();
    (c * r.cos(), c * r.sin())
}

/// `color` as OKLCh `[L in 0..1, C, H in degrees]` — the shared source for the
/// formatter and the component boxes (DRAGON-630), so the two cannot round differently.
fn oklch_of(color: Srgb) -> [f64; 3] {
    let ok = xyz_to_oklab(linear_to_xyz(color.to_linear()));
    let (c, h) = ab_to_ch(ok[1], ok[2]);
    [ok[0], c, h]
}

/// `color` as CIELAB `[L*, a*, b*]`, D65. Same sharing reason as [`oklch_of`].
fn lab_of(color: Srgb) -> [f64; 3] {
    xyz_to_lab(linear_to_xyz(color.to_linear()))
}

// ── HSL / HSV ────────────────────────────────────────────────────────────────

/// sRGB-encoded `0..1` to HSL `(h in 0..360, s in 0..1, l in 0..1)`.
fn unit_to_hsl(v: [f64; 3]) -> [f64; 3] {
    let (max, min) = (v[0].max(v[1]).max(v[2]), v[0].min(v[1]).min(v[2]));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-12 {
        return [0.0, 0.0, l];
    }
    [hue_of(v, max, d), d / (1.0 - (2.0 * l - 1.0).abs()), l]
}

/// sRGB-encoded `0..1` to HSV `(h in 0..360, s in 0..1, v in 0..1)`.
fn unit_to_hsv(v: [f64; 3]) -> [f64; 3] {
    let (max, min) = (v[0].max(v[1]).max(v[2]), v[0].min(v[1]).min(v[2]));
    let d = max - min;
    if d < 1e-12 {
        return [0.0, 0.0, max];
    }
    [hue_of(v, max, d), d / max, max]
}

/// The hue both HSL and HSV compute, in degrees.
fn hue_of(v: [f64; 3], max: f64, d: f64) -> f64 {
    let h = if max == v[0] {
        ((v[1] - v[2]) / d).rem_euclid(6.0)
    } else if max == v[1] {
        (v[2] - v[0]) / d + 2.0
    } else {
        (v[0] - v[1]) / d + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

/// HSL back to sRGB-encoded `0..1`.
fn hsl_to_unit(hsl: [f64; 3]) -> [f64; 3] {
    let (h, s, l) = (
        hsl[0].rem_euclid(360.0),
        hsl[1].clamp(0.0, 1.0),
        hsl[2].clamp(0.0, 1.0),
    );
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    sector(h, c, l - c / 2.0)
}

/// HSV back to sRGB-encoded `0..1`.
fn hsv_to_unit(hsv: [f64; 3]) -> [f64; 3] {
    let (h, s, v) = (
        hsv[0].rem_euclid(360.0),
        hsv[1].clamp(0.0, 1.0),
        hsv[2].clamp(0.0, 1.0),
    );
    let c = v * s;
    sector(h, c, v - c)
}

/// The shared HSL/HSV hue-sector expansion.
fn sector(h: f64, c: f64, m: f64) -> [f64; 3] {
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

// ── The picker window's HSV interaction model (DRAGON-630) ───────────────────

/// Pure, unit-tested: an HSV triple back to a colour. Hue wraps, S and V clamp.
pub fn srgb_from_hsv(hsv: [f64; 3]) -> Srgb {
    Srgb::from_unit_clamped(hsv_to_unit(hsv))
}

/// Pure, unit-tested: the HSV the window should TRACK after `color` changed under it
/// (DRAGON-630).
///
/// An achromatic colour has no hue of its own ([`hsv_components`] answers 0), and black
/// has no saturation either. Snapping the controls there would throw away the user's
/// aim: dragging Value to the floor and back would land on red every time, and sliding
/// Saturation to zero would park the hue slider at red too. So the previous hue survives
/// an achromatic colour, and the previous saturation survives black, which is what
/// every two-control HSV picker does. A chromatic colour answers its own exact HSV, so
/// the controls never drift from a colour that can speak for itself.
pub fn hsv_tracking(prev: [f64; 3], color: Srgb) -> [f64; 3] {
    let now = unit_to_hsv(color.to_unit());
    let h = if now[1] <= f64::EPSILON { prev[0] } else { now[0] };
    let s = if now[2] <= f64::EPSILON { prev[1] } else { now[1] };
    [h, s, now[2]]
}

// ── CMYK (naive, device-agnostic; see the module doc) ────────────────────────

/// sRGB-encoded `0..1` to naive CMYK, each `0..1`.
fn unit_to_cmyk(v: [f64; 3]) -> [f64; 4] {
    let k = 1.0 - v[0].max(v[1]).max(v[2]);
    if k >= 1.0 - 1e-12 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let d = 1.0 - k;
    [
        (1.0 - v[0] - k) / d,
        (1.0 - v[1] - k) / d,
        (1.0 - v[2] - k) / d,
        k,
    ]
}

/// Naive CMYK back to sRGB-encoded `0..1`.
fn cmyk_to_unit(c: [f64; 4]) -> [f64; 3] {
    let d = 1.0 - c[3].clamp(0.0, 1.0);
    [
        (1.0 - c[0].clamp(0.0, 1.0)) * d,
        (1.0 - c[1].clamp(0.0, 1.0)) * d,
        (1.0 - c[2].clamp(0.0, 1.0)) * d,
    ]
}

// ── The notations ────────────────────────────────────────────────────────────

/// One notation the colour picker offers as a labelled, editable, copyable row.
///
/// The ORDER of [`ColorFormat::ALL`] is the row order in the window, and it is the
/// owner's (DRAGON-582): the two colorimetric additions, CMYK and CIELAB, sit at the END
/// after OKLCh rather than being interleaved with the everyday ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ColorFormat {
    /// `#RRGGBB`. The picker's PRIMARY value: the overlay label shows it, and a pick
    /// copies it.
    Hex,
    /// `rgb(255, 136, 0)`.
    Rgb,
    /// `hsl(32, 100%, 50%)`.
    Hsl,
    /// `hsv(32, 100%, 100%)`.
    Hsv,
    /// `oklch(75.6% 0.176 60.7)` — CSS Color 4 spelling (space separated, `L` as a
    /// percentage, chroma absolute, hue in degrees).
    Oklch,
    /// `cmyk(0%, 47%, 100%, 0%)` — the naive DEVICE-AGNOSTIC separation, not an
    /// ICC-managed one (see the module doc before quoting these at a printer).
    Cmyk,
    /// `lab(70.2% 34.4 76.4)` — CIELAB, D65 white, CSS Color 4 spelling.
    Lab,
}

impl ColorFormat {
    /// Every notation, in the window's row order (the owner's, DRAGON-582).
    pub const ALL: [Self; 7] = [
        Self::Hex,
        Self::Rgb,
        Self::Hsl,
        Self::Hsv,
        Self::Oklch,
        Self::Cmyk,
        Self::Lab,
    ];

    /// The row's short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "HEX",
            Self::Rgb => "RGB",
            Self::Hsl => "HSL",
            Self::Hsv => "HSV",
            Self::Oklch => "OKLCH",
            Self::Cmyk => "CMYK",
            Self::Lab => "LAB",
        }
    }

    /// A stable identifier for logs and tests. Never user-facing, and never a colour
    /// VALUE: a picked colour is the user's content and must not reach the debug log,
    /// so this is what a log line names instead.
    pub fn id(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Rgb => "rgb",
            Self::Hsl => "hsl",
            Self::Hsv => "hsv",
            Self::Oklch => "oklch",
            Self::Cmyk => "cmyk",
            Self::Lab => "lab",
        }
    }

    /// Pure, unit-tested: `color` written in this notation.
    ///
    /// HEX is UPPERCASE (`#FF8800`). It is the value the overlay label shows at a glance
    /// over arbitrary screen pixels, and capitals read more distinctly there. Everything
    /// else follows CSS Color 4 spelling, so a copied value pastes into a stylesheet.
    pub fn format(self, color: Srgb) -> String {
        self.spell(color, None)
    }

    /// Pure, unit-tested: `color` written in this notation, carrying `alpha`
    /// (DRAGON-630).
    ///
    /// OPAQUE is spelled exactly as [`Self::format`] spells it, byte for byte, so every
    /// pre-alpha caller and clipboard string is unchanged. Anything translucent takes
    /// the notation's own alpha spelling: the CSS `-a` function names where paste-compat
    /// wants them (`rgba`, `hsla`, and `hsva` for symmetry), CSS Color 4's ` / a` suffix
    /// for the space-separated notations (`oklch`, `lab`), a fifth component for `cmyk`
    /// (which has no CSS home at all), and the 8-digit form for hex.
    pub fn format_with_alpha(self, color: Srgb, alpha: u8) -> String {
        if alpha == u8::MAX {
            return self.spell(color, None);
        }
        self.spell(color, Some(alpha))
    }

    /// The ONE spelling table behind [`Self::format`] and [`Self::format_with_alpha`]:
    /// every notation's opaque and alpha forms differ only where alpha itself appears,
    /// and sharing the table is what keeps the two from rounding differently.
    fn spell(self, color: Srgb, alpha: Option<u8>) -> String {
        let u = color.to_unit();
        let a = alpha.map(alpha_text);
        match self {
            Self::Hex => match alpha {
                None => format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b),
                Some(al) => {
                    format!("#{:02X}{:02X}{:02X}{:02X}", color.r, color.g, color.b, al)
                }
            },
            Self::Rgb => match &a {
                None => format!("rgb({}, {}, {})", color.r, color.g, color.b),
                Some(a) => format!("rgba({}, {}, {}, {a})", color.r, color.g, color.b),
            },
            Self::Hsl => {
                let h = unit_to_hsl(u);
                let (hh, s, l) = (
                    h[0].round() as i64,
                    (h[1] * 100.0).round() as i64,
                    (h[2] * 100.0).round() as i64,
                );
                match &a {
                    None => format!("hsl({hh}, {s}%, {l}%)"),
                    Some(a) => format!("hsla({hh}, {s}%, {l}%, {a})"),
                }
            }
            Self::Hsv => {
                let h = unit_to_hsv(u);
                let (hh, s, v) = (
                    h[0].round() as i64,
                    (h[1] * 100.0).round() as i64,
                    (h[2] * 100.0).round() as i64,
                );
                match &a {
                    None => format!("hsv({hh}, {s}%, {v}%)"),
                    Some(a) => format!("hsva({hh}, {s}%, {v}%, {a})"),
                }
            }
            Self::Oklch => {
                let ok = oklch_of(color);
                match &a {
                    None => format!("oklch({:.1}% {:.3} {:.1})", ok[0] * 100.0, ok[1], ok[2]),
                    Some(a) => {
                        format!("oklch({:.1}% {:.3} {:.1} / {a})", ok[0] * 100.0, ok[1], ok[2])
                    }
                }
            }
            Self::Cmyk => {
                let c = unit_to_cmyk(u);
                let (ci, m, y, k) = (
                    (c[0] * 100.0).round() as i64,
                    (c[1] * 100.0).round() as i64,
                    (c[2] * 100.0).round() as i64,
                    (c[3] * 100.0).round() as i64,
                );
                match &a {
                    None => format!("cmyk({ci}%, {m}%, {y}%, {k}%)"),
                    Some(a) => format!("cmyk({ci}%, {m}%, {y}%, {k}%, {a})"),
                }
            }
            Self::Lab => {
                let lab = lab_of(color);
                match &a {
                    None => format!("lab({:.1}% {:.1} {:.1})", lab[0], lab[1], lab[2]),
                    Some(a) => format!("lab({:.1}% {:.1} {:.1} / {a})", lab[0], lab[1], lab[2]),
                }
            }
        }
    }

    /// Pure, unit-tested: parse a value written in this notation, or `None`. The
    /// alpha-blind form of [`Self::parse_with_alpha`]: an alpha component is ACCEPTED
    /// and discarded, so a pasted `rgba(…)` still loads its colour.
    pub fn parse(self, s: &str) -> Option<Srgb> {
        self.parse_with_alpha(s).map(|(c, _)| c)
    }

    /// Pure, unit-tested: parse a value written in this notation, with its alpha, or
    /// `None`. A value with no alpha component reads as OPAQUE (`255`).
    ///
    /// Deliberately TOLERANT, because a user edits these boxes by hand: the function
    /// name is optional (with or without the CSS `a` suffix, so `rgba(…)` and `rgb(…)`
    /// both load), so are the parentheses, commas, spaces and CSS Color 4's `/` are
    /// interchangeable separators, and a `%` suffix is accepted. What is NOT tolerated
    /// is the wrong NUMBER of components: two numbers typed into the RGB box is a typo,
    /// not a colour, and guessing would be the "reports the wrong colour" failure again.
    /// Since DRAGON-629 each notation takes its component count OR one more (the alpha),
    /// so "four is a typo" stopped being true of `rgb()` when `rgba()` became legal.
    ///
    /// Out-of-gamut CIELAB / OKLCh values CLAMP into sRGB (module doc). Hue WRAPS, since
    /// 400 degrees is unambiguously 40. Alpha follows CSS: `0..1`, a `%` means a
    /// percentage, and an out-of-range value clamps.
    pub fn parse_with_alpha(self, s: &str) -> Option<(Srgb, u8)> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if self == Self::Hex {
            return parse_hex_alpha(s);
        }
        // The `a`-suffixed name first: `numbers` only strips a name it matches whole, so
        // trying `rgb` against `rgba(…)` would leave the `a(` glued to the first token.
        let n = numbers(s, &format!("{}a", self.id())).or_else(|| numbers(s, self.id()))?;
        match self {
            Self::Hex => None,
            Self::Rgb => {
                let ([r, g, b], alpha) = three_plus_alpha(&n)?;
                Some((
                    Srgb::from_unit_clamped([channel_255(r), channel_255(g), channel_255(b)]),
                    alpha_byte(alpha),
                ))
            }
            Self::Hsl => {
                let ([h, s, l], alpha) = three_plus_alpha(&n)?;
                Some((
                    Srgb::from_unit_clamped(hsl_to_unit([h.value, fraction(s), fraction(l)])),
                    alpha_byte(alpha),
                ))
            }
            Self::Hsv => {
                let ([h, s, v], alpha) = three_plus_alpha(&n)?;
                Some((
                    Srgb::from_unit_clamped(hsv_to_unit([h.value, fraction(s), fraction(v)])),
                    alpha_byte(alpha),
                ))
            }
            Self::Oklch => {
                let ([l, c, h], alpha) = three_plus_alpha(&n)?;
                // CSS allows `L` as `0..1` or as a percentage; `fraction` takes both.
                let (a, b) = ch_to_ab(c.value.max(0.0), h.value);
                Some((
                    Srgb::from_linear_clamped(xyz_to_linear(oklab_to_xyz([
                        fraction(l).clamp(0.0, 1.0),
                        a,
                        b,
                    ]))),
                    alpha_byte(alpha),
                ))
            }
            Self::Cmyk => {
                if n.len() != 4 && n.len() != 5 {
                    return None;
                }
                Some((
                    Srgb::from_unit_clamped(cmyk_to_unit([
                        fraction(n[0]),
                        fraction(n[1]),
                        fraction(n[2]),
                        fraction(n[3]),
                    ])),
                    alpha_byte(n.get(4).copied()),
                ))
            }
            Self::Lab => {
                let ([l, a, b], alpha) = three_plus_alpha(&n)?;
                // CIELAB's `L*` is already 0..100, so a `%` suffix means the same
                // number and the percent flag is ignored rather than rescaled.
                Some((
                    Srgb::from_linear_clamped(xyz_to_linear(lab_to_xyz([
                        l.value.clamp(0.0, 100.0),
                        a.value,
                        b.value,
                    ]))),
                    alpha_byte(alpha),
                ))
            }
        }
    }

    /// Pure, unit-tested: the notation whose [`Self::id`] this is, for reading the
    /// persisted mode back (DRAGON-630). `None` for junk, so a hand-edited config falls
    /// back to the caller's default rather than guessing.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    // `cycled(steps)` briefly lived here for a chevron-pair mode stepper; the owner's
    // review replaced that control with a dropdown that selects by index, so the
    // wrapping walk had no caller left and was removed rather than allowed to rot.

    /// The labels of this notation's own component boxes, in box order, WITHOUT the
    /// alpha box (every mode shows one, and the caller appends its "A").
    ///
    /// Hex wears the same R/G/B letters as RGB (the owner's call): its boxes hold hex
    /// PAIRS (`FF`) where RGB's hold decimals, and the content tells them apart. It
    /// briefly held one wide whole-spelling box; the owner asked for split channels.
    pub fn component_labels(self) -> &'static [&'static str] {
        match self {
            Self::Hex | Self::Rgb => &["R", "G", "B"],
            Self::Hsl => &["H", "S", "L"],
            Self::Hsv => &["H", "S", "V"],
            Self::Oklch => &["L", "C", "H"],
            Self::Cmyk => &["C", "M", "Y", "K"],
            Self::Lab => &["L", "a", "b"],
        }
    }

    /// Pure, unit-tested: the canonical text of ONE component box (DRAGON-630).
    ///
    /// `idx` counts [`Self::component_labels`], and any index past them is the ALPHA
    /// box, so the caller's box loop cannot go wrong. The numbers agree with
    /// [`Self::format`]'s own tokens digit for digit (a test tokenizes one against the
    /// other), and the `%` stays OFF: a box's unit is fixed and its label carries it,
    /// so the box holds only the number. Hex boxes hold hex PAIRS, alpha included: the
    /// mode's own dialect, in every one of its boxes.
    pub fn component_text(self, color: Srgb, alpha: u8, idx: usize) -> String {
        if idx >= self.component_labels().len() {
            // The alpha box speaks its mode's dialect: hex digits in hex mode, the CSS
            // 0..1 fraction everywhere else.
            return match self {
                Self::Hex => format!("{alpha:02X}"),
                _ => alpha_text(alpha),
            };
        }
        let u = color.to_unit();
        let percent = |f: f64| ((f * 100.0).round() as i64).to_string();
        match self {
            Self::Hex => format!("{:02X}", [color.r, color.g, color.b][idx.min(2)]),
            Self::Rgb => [color.r, color.g, color.b][idx.min(2)].to_string(),
            Self::Hsl => {
                let h = unit_to_hsl(u);
                match idx {
                    0 => (h[0].round() as i64).to_string(),
                    i => percent(h[i.min(2)]),
                }
            }
            Self::Hsv => {
                let h = unit_to_hsv(u);
                match idx {
                    0 => (h[0].round() as i64).to_string(),
                    i => percent(h[i.min(2)]),
                }
            }
            Self::Oklch => {
                let ok = oklch_of(color);
                match idx {
                    0 => format!("{:.1}", ok[0] * 100.0),
                    1 => format!("{:.3}", ok[1]),
                    _ => format!("{:.1}", ok[2]),
                }
            }
            Self::Cmyk => percent(unit_to_cmyk(u)[idx.min(3)]),
            Self::Lab => format!("{:.1}", lab_of(color)[idx.min(2)]),
        }
    }

    /// Pure, unit-tested: the colour and alpha after ONE component box is edited to
    /// `text` (DRAGON-630). `None` while the text does not parse, leaving the colour
    /// untouched, so the caller keeps showing the draft exactly as the old full-string
    /// rows did.
    ///
    /// A box's unit is the unit it DISPLAYS ([`Self::component_text`]): the S box shows
    /// `50` for 50%, so `50` typed there means 50% (a `%` suffix is accepted and means
    /// the same thing), where the free-form string parser reads a bare `50` as an
    /// already-fractional value. Hue wraps; everything else clamps into its own range.
    pub fn with_component(
        self,
        color: Srgb,
        alpha: u8,
        idx: usize,
        text: &str,
    ) -> Option<(Srgb, u8)> {
        if self == Self::Hex {
            // Hex boxes hold hex PAIRS, one channel each, alpha included: the mode's
            // own dialect end to end. (This mode briefly wore one whole-spelling box;
            // the owner asked for split channels.)
            let v = parse_hex_pair(text)?;
            if idx >= self.component_labels().len() {
                return Some((color, v));
            }
            let mut ch = [color.r, color.g, color.b];
            ch[idx.min(2)] = v;
            return Some((Srgb::new(ch[0], ch[1], ch[2]), alpha));
        }
        let n = parse_component_token(text)?;
        if idx >= self.component_labels().len() {
            // The alpha box, shared by every mode: `0..1`, or a percentage.
            return Some((color, alpha_byte(Some(n))));
        }
        // A percent-unit box: `%` or bare, the number means the same thing.
        let percent_unit = (n.value / 100.0).clamp(0.0, 1.0);
        match self {
            Self::Hex => None,
            Self::Rgb => {
                let v = if n.percent { n.value * 255.0 / 100.0 } else { n.value };
                let mut ch = [color.r, color.g, color.b];
                ch[idx.min(2)] = v.round().clamp(0.0, 255.0) as u8;
                Some((Srgb::new(ch[0], ch[1], ch[2]), alpha))
            }
            Self::Hsl => {
                let mut h = unit_to_hsl(color.to_unit());
                match idx {
                    0 => h[0] = n.value.rem_euclid(360.0),
                    i => h[i.min(2)] = percent_unit,
                }
                Some((Srgb::from_unit_clamped(hsl_to_unit(h)), alpha))
            }
            Self::Hsv => {
                let mut h = unit_to_hsv(color.to_unit());
                match idx {
                    0 => h[0] = n.value.rem_euclid(360.0),
                    i => h[i.min(2)] = percent_unit,
                }
                Some((Srgb::from_unit_clamped(hsv_to_unit(h)), alpha))
            }
            Self::Oklch => {
                let mut ok = oklch_of(color);
                match idx {
                    0 => ok[0] = percent_unit,
                    1 => ok[1] = n.value.max(0.0),
                    _ => ok[2] = n.value.rem_euclid(360.0),
                }
                let (a2, b2) = ch_to_ab(ok[1], ok[2]);
                Some((
                    Srgb::from_linear_clamped(xyz_to_linear(oklab_to_xyz([ok[0], a2, b2]))),
                    alpha,
                ))
            }
            Self::Cmyk => {
                let mut c4 = unit_to_cmyk(color.to_unit());
                c4[idx.min(3)] = percent_unit;
                Some((Srgb::from_unit_clamped(cmyk_to_unit(c4)), alpha))
            }
            Self::Lab => {
                let mut lab = lab_of(color);
                match idx {
                    0 => lab[0] = n.value.clamp(0.0, 100.0),
                    i => lab[i.min(2)] = n.value,
                }
                Some((Srgb::from_linear_clamped(xyz_to_linear(lab_to_xyz(lab))), alpha))
            }
        }
    }
}

// ── Gradient rasters (DRAGON-630) ────────────────────────────────────────────
//
// The straight-RGBA gradients the picker window's `widgets::color_field` fields draw,
// pure and here rather than in the widget because the widget is deliberately
// colour-agnostic (the owner wants it reusable) and these are colour through and
// through. Any future tenant of `color_field` that wants a standard gradient takes
// them from here.

/// The checkerboard the translucent rasters sit on: cell size in pixels, and the two
/// greys. Mid greys on purpose, so the board reads as "transparent here" on a light
/// and a dark theme alike. Nine-pixel cells: the owner sized them by eye at rev 3
/// ("about 50% too small" at the original six).
const CHECKER_CELL: u32 = 9;
const CHECKER_LIGHT: [u8; 3] = [190, 190, 190];
const CHECKER_DARK: [u8; 3] = [122, 122, 122];

/// The checker grey under pixel `(x, y)`.
fn checker_at(x: u32, y: u32) -> [u8; 3] {
    if ((x / CHECKER_CELL) + (y / CHECKER_CELL)).is_multiple_of(2) {
        CHECKER_LIGHT
    } else {
        CHECKER_DARK
    }
}

/// Anti-aliased coverage of pixel `(x, y)` inside a `w` x `h` ROUNDED rectangle with
/// corner radius `r` (DRAGON-630): `1.0` in the body and along the straight edges, a
/// soft one-pixel ramp around the corner arcs, `0.0` outside them. The standard
/// rounded-rect signed-distance evaluation, so the straight edges stay fully opaque
/// (the naive distance-to-inner-box fades them, which reads as a blurry border).
///
/// This is how the window's gradient rasters follow the app's "Edge rounding" setting:
/// the radius handed in is the theme's swatch token, and a zero radius is exact
/// square corners, byte-identical to the un-rounded rasters.
fn rounded_rect_coverage(x: u32, y: u32, w: u32, h: u32, r: f64) -> f64 {
    if r <= 0.0 {
        return 1.0;
    }
    let r = r.min(w as f64 / 2.0).min(h as f64 / 2.0);
    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
    let (hw, hh) = (w as f64 / 2.0, h as f64 / 2.0);
    let qx = (px - hw).abs() - (hw - r);
    let qy = (py - hh).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let sd = outside + qx.max(qy).min(0.0) - r;
    (0.5 - sd).clamp(0.0, 1.0)
}

/// Pure, unit-tested: the saturation/value square for `hue`, `w` x `h`, straight RGBA,
/// with `radius`-rounded corners ([`rounded_rect_coverage`]; 0 is square).
///
/// The classic layout: saturation runs left (0) to right (1), value runs top (1) to
/// bottom (0), so the top-left is white, the top-right is the pure hue, and the whole
/// bottom edge is black. A one-pixel axis still spans its whole range (the divisor is
/// `extent - 1`), so the corners are exact at any size.
pub fn sv_square_rgba(hue: f64, w: u32, h: u32, radius: f64) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let span = |i: u32, extent: u32| -> f64 {
        if extent <= 1 { 0.0 } else { i as f64 / (extent - 1) as f64 }
    };
    for y in 0..h {
        let v = 1.0 - span(y, h);
        for x in 0..w {
            let cover = rounded_rect_coverage(x, y, w, h, radius);
            if cover <= 0.0 {
                continue;
            }
            let s = span(x, w);
            let c = srgb_from_hsv([hue, s, v]);
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[c.r, c.g, c.b, (cover * 255.0).round() as u8]);
        }
    }
    out
}

/// Pure, unit-tested: the hue strip, `w` x `h`, straight RGBA, `radius`-rounded: the
/// full hue circle left to right at full saturation and value, both ends red (0 and
/// 360 are the same hue).
pub fn hue_strip_rgba(w: u32, h: u32, radius: f64) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for x in 0..w {
        let hue = if w <= 1 { 0.0 } else { 360.0 * x as f64 / (w - 1) as f64 };
        let c = srgb_from_hsv([hue.min(360.0), 1.0, 1.0]);
        for y in 0..h {
            let cover = rounded_rect_coverage(x, y, w, h, radius);
            if cover <= 0.0 {
                continue;
            }
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[c.r, c.g, c.b, (cover * 255.0).round() as u8]);
        }
    }
    out
}

/// Pure, unit-tested: the alpha strip for `color`, `w` x `h`, straight RGBA,
/// `radius`-rounded: `color` composited over the checkerboard, transparent at the left
/// through opaque at the right, so the marker's position IS the alpha.
pub fn alpha_strip_rgba(color: Srgb, w: u32, h: u32, radius: f64) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h {
        for x in 0..w {
            let cover = rounded_rect_coverage(x, y, w, h, radius);
            if cover <= 0.0 {
                continue;
            }
            let a = if w <= 1 { 0.0 } else { x as f64 / (w - 1) as f64 };
            let under = checker_at(x, y);
            let mix = |c: u8, u: u8| -> u8 {
                (c as f64 * a + u as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8
            };
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[
                mix(color.r, under[0]),
                mix(color.g, under[1]),
                mix(color.b, under[2]),
                (cover * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// Pure, unit-tested: a round swatch of `color` at `alpha`, `d` x `d`, straight RGBA:
/// the colour over the checkerboard (so a translucent colour shows AS translucent),
/// inside an anti-aliased circular mask with a thin rim in `rim` so the disc holds its
/// shape over any window background. The rim is a parameter (DRAGON-630, the owner's
/// "subdued, not white/black") so the caller can hand in the live theme's subdued tone.
pub fn swatch_circle_rgba(color: Srgb, alpha: u8, d: u32, rim: [u8; 3]) -> Vec<u8> {
    let mut out = vec![0u8; (d as usize) * (d as usize) * 4];
    let radius = d as f64 / 2.0;
    let af = alpha as f64 / 255.0;
    for y in 0..d {
        for x in 0..d {
            let (dx, dy) = (x as f64 + 0.5 - radius, y as f64 + 0.5 - radius);
            let dist = (dx * dx + dy * dy).sqrt();
            // One-pixel anti-alias band at the rim; fully outside stays transparent.
            let cover = (radius - dist + 0.5).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let under = checker_at(x, y);
            let mix = |c: u8, u: u8| -> f64 { c as f64 * af + u as f64 * (1.0 - af) };
            let mut px = [mix(color.r, under[0]), mix(color.g, under[1]), mix(color.b, under[2])];
            // The rim: the outermost ~1px of the disc.
            if dist >= radius - 1.5 {
                px = [rim[0] as f64, rim[1] as f64, rim[2] as f64];
            }
            let idx = ((y as usize) * (d as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[
                px[0].round().clamp(0.0, 255.0) as u8,
                px[1].round().clamp(0.0, 255.0) as u8,
                px[2].round().clamp(0.0, 255.0) as u8,
                (cover * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// One parsed number token: its value, and whether it carried a `%`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Num {
    value: f64,
    percent: bool,
}

/// A token as a `0..1` FRACTION: a percentage divided by 100, a bare number taken as
/// already fractional (so `hsl(0, 1, 0.5)` and `hsl(0, 100%, 50%)` agree).
fn fraction(n: Num) -> f64 {
    if n.percent { n.value / 100.0 } else { n.value }
}

/// An `rgb()` channel as a `0..1` fraction: `0..255` normally, `0..100%` when written
/// as a percentage (both are CSS-legal and they scale differently).
fn channel_255(n: Num) -> f64 {
    if n.percent { n.value / 100.0 } else { n.value / 255.0 }
}

/// Three tokens plus an optional fourth (the alpha), or `None` (DRAGON-630).
fn three_plus_alpha(n: &[Num]) -> Option<([Num; 3], Option<Num>)> {
    match n {
        [a, b, c] => Some(([*a, *b, *c], None)),
        [a, b, c, d] => Some(([*a, *b, *c], Some(*d))),
        _ => None,
    }
}

/// An optional alpha token as a byte (DRAGON-630): absent is OPAQUE, a `%` is a
/// percentage, a bare number is CSS's `0..1` fraction, and out of range clamps (CSS's
/// own rule, so a pasted `rgba(…, 9)` reads as opaque rather than refusing).
fn alpha_byte(n: Option<Num>) -> u8 {
    let Some(n) = n else { return u8::MAX };
    let f = if n.percent { n.value / 100.0 } else { n.value };
    (f.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Pure, unit-tested: an alpha byte as the `0..1` text every alpha spelling and the A
/// box share (DRAGON-630). Three decimals, trailing zeros trimmed, so all 256 bytes
/// round-trip (`0.502` is 128 again) and the common ends read plainly (`0`, `0.5`, `1`).
pub fn alpha_text(alpha: u8) -> String {
    let t = format!("{:.3}", alpha as f64 / 255.0);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// ONE number for a component box (DRAGON-630): a value with an optional `%`, nothing
/// else. Runs through the same tokenizer the full-string parser uses, so the same
/// digits are legal in both.
fn parse_component_token(s: &str) -> Option<Num> {
    let mut n = numbers(s, "")?;
    (n.len() == 1).then(|| n.remove(0))
}

/// One or two hex digits as a byte, for the hex mode's channel boxes (DRAGON-630).
/// A single digit reads as itself (`F` is 0x0F): the box is mid-edit more often than
/// not, and the nibble-doubling shorthand belongs to the whole `#RGB` spelling, not to
/// a channel field.
fn parse_hex_pair(s: &str) -> Option<u8> {
    let t = s.trim();
    if !(1..=2).contains(&t.len()) {
        return None;
    }
    u8::from_str_radix(t, 16).ok()
}

/// Pull the numeric tokens out of `s`, having stripped an optional leading function
/// name (`name`, case-insensitive), optional parentheses, and any mixture of commas,
/// whitespace and CSS Color 4's `/` separator. `None` if any non-numeric junk is
/// present, so a mistyped notation is refused rather than half-read.
fn numbers(s: &str, name: &str) -> Option<Vec<Num>> {
    let mut body = s.trim();
    if body
        .get(..name.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(name))
    {
        body = body[name.len()..].trim_start();
    }
    let body = body.trim_start_matches('(').trim_end_matches(')').trim();
    let mut out = Vec::new();
    for tok in body.split([',', ' ', '\t', '/']).filter(|t| !t.is_empty()) {
        let (digits, percent) = match tok.strip_suffix('%') {
            Some(d) => (d, true),
            None => (tok, false),
        };
        let value: f64 = digits.parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        out.push(Num { value, percent });
    }
    (!out.is_empty()).then_some(out)
}

/// `#RGB`, `#RRGGBB`, their alpha forms (`#RGBA`, `#RRGGBBAA`) and all four without the
/// `#`. No alpha digits read as OPAQUE. This used to DISCARD the alpha digits (a pick is
/// an opaque screen pixel, so there was nothing to carry them into); DRAGON-630 gave the
/// window an alpha of its own, so they land there now instead.
fn parse_hex_alpha(s: &str) -> Option<(Srgb, u8)> {
    let h = s.trim().trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let nib = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|d| d * 17);
    match h.len() {
        3 => Some((Srgb::new(nib(0)?, nib(1)?, nib(2)?), u8::MAX)),
        4 => Some((Srgb::new(nib(0)?, nib(1)?, nib(2)?), nib(3)?)),
        6 => Some((Srgb::new(byte(0)?, byte(2)?, byte(4)?), u8::MAX)),
        8 => Some((Srgb::new(byte(0)?, byte(2)?, byte(4)?), byte(6)?)),
        _ => None,
    }
}

#[cfg(test)]
fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    /// The transfer function is an exact inverse pair on both sides of its knee, and
    /// hits the published anchors.
    ///
    /// The KNEE itself is the one place the pair is only approximately inverse, and that
    /// is the sRGB standard's own doing rather than a bug here: the published constants
    /// `0.04045` and `0.0031308` do not satisfy `0.04045 / 12.92 == 0.0031308` exactly,
    /// so a value sitting on the boundary can go out through the linear branch and come
    /// back through the power branch. The mismatch is under 1e-6, i.e. a thousandth of
    /// one 8-bit step, which is why every implementation lives with it. Asserted at that
    /// size so a real regression would still show.
    #[test]
    fn the_srgb_eotf_round_trips() {
        for c in [0.0, 0.01, 0.03, 0.05, 0.5, 0.9, 1.0] {
            assert!(close(linear_to_srgb(srgb_to_linear(c)), c, 1e-12), "{c}");
        }
        let knee = 0.040_45;
        assert!(close(linear_to_srgb(srgb_to_linear(knee)), knee, 1e-6), "the knee");
        assert!(close(srgb_to_linear(0.5), 0.214_041_1, 1e-6));
        assert!(close(srgb_to_linear(1.0), 1.0, 1e-12));
        assert!(close(srgb_to_linear(0.0), 0.0, 1e-12));
    }

    /// sRGB white IS the D65 white point, which is what lets ONE XYZ stage serve both
    /// CIELAB and OKLab (module doc). The tolerance is the published matrix's own
    /// rounding, not slack.
    #[test]
    fn srgb_white_is_d65() {
        let w = linear_to_xyz([1.0, 1.0, 1.0]);
        for i in 0..3 {
            assert!(close(w[i], WHITE_D65[i], 1e-3), "channel {i}: {w:?}");
        }
    }

    /// XYZ is an inverse pair with linear sRGB.
    #[test]
    fn xyz_round_trips_with_linear_srgb() {
        for l in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.2, 0.5, 0.8],
            [1.0, 0.0, 0.0],
        ] {
            let back = xyz_to_linear(linear_to_xyz(l));
            for i in 0..3 {
                assert!(close(back[i], l[i], 1e-6), "{l:?} -> {back:?}");
            }
        }
    }

    /// CIELAB known values (D65): black L*0, white L*100 with a neutral a*/b*, mid grey
    /// near L*53.6, and sRGB red at the textbook `53.24 / 80.09 / 67.20`.
    #[test]
    fn cielab_known_values() {
        let lab = |c: Srgb| xyz_to_lab(linear_to_xyz(c.to_linear()));
        let black = lab(Srgb::new(0, 0, 0));
        assert!(close(black[0], 0.0, 1e-6), "black L* {}", black[0]);
        assert!(close(black[1], 0.0, 1e-6) && close(black[2], 0.0, 1e-6));
        let white = lab(Srgb::new(255, 255, 255));
        assert!(close(white[0], 100.0, 1e-3), "white L* {}", white[0]);
        assert!(close(white[1], 0.0, 0.02) && close(white[2], 0.0, 0.02), "white ab {white:?}");
        let grey = lab(Srgb::new(128, 128, 128));
        assert!(close(grey[0], 53.585, 0.05), "mid grey L* {}", grey[0]);
        let red = lab(Srgb::new(255, 0, 0));
        assert!(
            close(red[0], 53.24, 0.05) && close(red[1], 80.09, 0.15) && close(red[2], 67.20, 0.15),
            "sRGB red {red:?}"
        );
    }

    /// OKLab known values: white is L=1 and neutral, black is 0, and sRGB red is
    /// Ottosson's published `L~0.6280, C~0.2577, h~29.23`.
    #[test]
    fn oklab_known_values() {
        let ok = |c: Srgb| xyz_to_oklab(linear_to_xyz(c.to_linear()));
        let white = ok(Srgb::new(255, 255, 255));
        assert!(close(white[0], 1.0, 1e-3), "white L {}", white[0]);
        assert!(close(white[1], 0.0, 2e-3) && close(white[2], 0.0, 2e-3), "white ab {white:?}");
        assert!(close(ok(Srgb::new(0, 0, 0))[0], 0.0, 1e-9));
        let red = ok(Srgb::new(255, 0, 0));
        let (c, h) = ab_to_ch(red[1], red[2]);
        assert!(close(red[0], 0.6280, 3e-3), "red L {}", red[0]);
        assert!(close(c, 0.2577, 3e-3), "red C {c}");
        assert!(close(h, 29.23, 0.3), "red h {h}");
    }

    /// A neutral reports hue 0 rather than atan2 noise, so grey formats stably.
    #[test]
    fn a_neutral_has_no_hue() {
        assert_eq!(ab_to_ch(0.0, 0.0), (0.0, 0.0));
        assert_eq!(ab_to_ch(1e-12, -1e-12), (0.0, 0.0));
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    /// Every notation round-trips an arbitrary 8-bit colour back to (near) the SAME
    /// bytes.
    ///
    /// This is the contract the picker window rests on: editing one row re-derives the
    /// others, so a lossy notation would drift the colour every time a box was touched.
    /// HSL / HSV / CMYK print WHOLE percentages, which cannot name every 8-bit triple
    /// exactly, so those three are held to the tolerance their own printed precision
    /// allows and the exact claim is made only where it is true.
    #[test]
    fn every_notation_round_trips() {
        let samples = [
            Srgb::new(0, 0, 0),
            Srgb::new(255, 255, 255),
            Srgb::new(128, 128, 128),
            Srgb::new(255, 0, 0),
            Srgb::new(0, 255, 0),
            Srgb::new(0, 0, 255),
            Srgb::new(255, 136, 0),
            Srgb::new(18, 52, 86),
            Srgb::new(1, 2, 3),
            Srgb::new(254, 253, 252),
        ];
        for c in samples {
            for f in ColorFormat::ALL {
                let s = f.format(c);
                let back = f
                    .parse(&s)
                    .unwrap_or_else(|| panic!("{} did not parse its own {s:?}", f.id()));
                let tol: i32 = match f {
                    // Whole-percent notations: see the doc above.
                    ColorFormat::Hsl | ColorFormat::Hsv | ColorFormat::Cmyk => 4,
                    // One decimal of L* / chroma / hue is ample for 8 bits.
                    ColorFormat::Oklch | ColorFormat::Lab => 1,
                    ColorFormat::Hex | ColorFormat::Rgb => 0,
                };
                let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
                assert!(
                    d(back.r, c.r) <= tol && d(back.g, c.g) <= tol && d(back.b, c.b) <= tol,
                    "{} round trip {c:?} -> {s:?} -> {back:?} (tolerance {tol})",
                    f.id()
                );
            }
        }
    }

    /// The exact strings for every notation that can be written down by hand, so a
    /// change to a spelling is a deliberate act rather than a side effect.
    #[test]
    fn the_printed_spellings_are_pinned() {
        let c = Srgb::new(255, 136, 0);
        assert_eq!(ColorFormat::Hex.format(c), "#FF8800");
        assert_eq!(ColorFormat::Rgb.format(c), "rgb(255, 136, 0)");
        assert_eq!(ColorFormat::Hsl.format(c), "hsl(32, 100%, 50%)");
        assert_eq!(ColorFormat::Hsv.format(c), "hsv(32, 100%, 100%)");
        assert_eq!(ColorFormat::Cmyk.format(c), "cmyk(0%, 47%, 100%, 0%)");
        assert_eq!(ColorFormat::Hex.format(Srgb::new(0, 0, 0)), "#000000");
        assert_eq!(ColorFormat::Hex.format(Srgb::new(255, 255, 255)), "#FFFFFF");
        assert_eq!(ColorFormat::Cmyk.format(Srgb::new(0, 0, 0)), "cmyk(0%, 0%, 0%, 100%)");
        assert_eq!(Srgb::new(18, 52, 86).hex(), "#123456");
    }

    /// The two colorimetric rows print their PUBLISHED values, checked numerically
    /// rather than as a string (their last digit is matrix rounding, and pinning a
    /// literal there would make an honest matrix correction look like a regression).
    #[test]
    fn the_colorimetric_rows_print_published_values() {
        let read = |s: &str, name: &str| {
            numbers(s, name)
                .expect("its own output parses")
                .iter()
                .map(|n| n.value)
                .collect::<Vec<_>>()
        };
        let red_lab = read(&ColorFormat::Lab.format(Srgb::new(255, 0, 0)), "lab");
        assert!(
            close(red_lab[0], 53.24, 0.1) && close(red_lab[1], 80.09, 0.2) && close(red_lab[2], 67.20, 0.2),
            "sRGB red in CIELAB: {red_lab:?}"
        );
        let red_ok = read(&ColorFormat::Oklch.format(Srgb::new(255, 0, 0)), "oklch");
        assert!(
            close(red_ok[0], 62.80, 0.3) && close(red_ok[1], 0.2577, 0.003) && close(red_ok[2], 29.23, 0.3),
            "sRGB red in OKLCh: {red_ok:?}"
        );
        // White and black anchor both scales at their ends.
        let white_lab = read(&ColorFormat::Lab.format(Srgb::new(255, 255, 255)), "lab");
        assert!(close(white_lab[0], 100.0, 0.1), "white L*: {white_lab:?}");
        let black_ok = read(&ColorFormat::Oklch.format(Srgb::new(0, 0, 0)), "oklch");
        assert!(close(black_ok[0], 0.0, 0.05), "black L: {black_ok:?}");
        // A neutral has zero chroma in both, which is what makes grey read as grey.
        let grey_ok = read(&ColorFormat::Oklch.format(Srgb::new(128, 128, 128)), "oklch");
        assert!(close(grey_ok[1], 0.0, 0.002), "mid grey chroma: {grey_ok:?}");
    }

    /// The row order is the owner's: the two colorimetric additions come LAST.
    #[test]
    fn the_row_order_is_the_owners() {
        assert_eq!(
            ColorFormat::ALL.map(|f| f.id()),
            ["hex", "rgb", "hsl", "hsv", "oklch", "cmyk", "lab"]
        );
        assert_eq!(ColorFormat::ALL[0], ColorFormat::Hex, "HEX leads: it is what a pick copies");
        // Labels are distinct, so no two rows can read the same.
        let mut labels = ColorFormat::ALL.map(|f| f.label()).to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), ColorFormat::ALL.len());
    }

    /// Spellings a user will actually type all parse.
    #[test]
    fn the_parsers_are_tolerant_of_hand_typing() {
        let orange = Srgb::new(255, 136, 0);
        for s in ["#FF8800", "ff8800", "#ff8800", "#F80", "f80"] {
            assert_eq!(ColorFormat::Hex.parse(s), Some(orange), "{s}");
        }
        // The alpha-blind parse still accepts alpha digits and drops them.
        assert_eq!(ColorFormat::Hex.parse("#FF8800CC"), Some(orange));
        // The alpha-aware parse carries them (DRAGON-630).
        assert_eq!(ColorFormat::Hex.parse_with_alpha("#FF8800CC"), Some((orange, 0xCC)));
        assert_eq!(ColorFormat::Hex.parse_with_alpha("#FF8800"), Some((orange, 255)));
        for s in ["rgb(255, 136, 0)", "255 136 0", "rgb(255,136,0)", "  RGB( 255 , 136 , 0 ) "] {
            assert_eq!(ColorFormat::Rgb.parse(s), Some(orange), "{s}");
        }
        assert_eq!(ColorFormat::Rgb.parse("rgb(100%, 0%, 0%)"), Some(Srgb::new(255, 0, 0)));
        assert_eq!(
            ColorFormat::Hsl.parse("hsl(0, 1, 0.5)"),
            ColorFormat::Hsl.parse("hsl(0, 100%, 50%)"),
            "a bare fraction and a percentage mean the same thing"
        );
        assert_eq!(
            ColorFormat::Hsl.parse("hsl(400, 100%, 50%)"),
            ColorFormat::Hsl.parse("hsl(40, 100%, 50%)"),
            "hue wraps"
        );
    }

    /// Junk and the wrong component COUNT are refused rather than half-read.
    #[test]
    fn malformed_values_are_refused() {
        assert_eq!(ColorFormat::Hex.parse(""), None);
        assert_eq!(ColorFormat::Hex.parse("#12345"), None);
        assert_eq!(ColorFormat::Hex.parse("#GGGGGG"), None);
        assert_eq!(ColorFormat::Rgb.parse("rgb(255, 136)"), None, "two components is a typo");
        // DRAGON-630 flipped the old "four is a typo" pin: a fourth component is the
        // ALPHA now, read by CSS's own rule (out of range clamps to opaque).
        assert_eq!(ColorFormat::Rgb.parse("rgb(255, 136, 0, 9)"), Some(Srgb::new(255, 136, 0)));
        assert_eq!(ColorFormat::Rgb.parse("rgb(255, 136, 0, 9, 9)"), None, "five IS a typo");
        assert_eq!(ColorFormat::Cmyk.parse("cmyk(0%, 47%, 100%)"), None, "CMYK needs four");
        assert_eq!(ColorFormat::Cmyk.parse("cmyk(0, 0, 0, 0, 1, 1)"), None, "or five with alpha");
        assert_eq!(ColorFormat::Rgb.parse("rgb(a, b, c)"), None);
        assert_eq!(ColorFormat::Lab.parse("lab(nan 0 0)"), None);
        assert_eq!(ColorFormat::Lab.parse("lab(inf 0 0)"), None);
        for f in ColorFormat::ALL {
            assert_eq!(f.parse("   "), None, "{}", f.id());
        }
    }

    /// OUT OF GAMUT CLAMPS (module doc). CIELAB and OKLCh can name colours sRGB cannot;
    /// the answer is the nearest per-channel sRGB value, never a wrapped or garbage byte.
    #[test]
    fn out_of_gamut_values_clamp_into_srgb() {
        let c = ColorFormat::Lab.parse("lab(90% -128 100)").expect("parses");
        assert!(c.g > 200, "an impossibly saturated green clamps bright, not dark: {c:?}");
        let c = ColorFormat::Oklch.parse("oklch(70% 0.9 29)").expect("parses");
        assert_eq!(c.g.min(c.b), 0, "the negative channels clamp to zero, not wrap: {c:?}");
        assert_eq!(ColorFormat::Lab.parse("lab(400% 0 0)"), Some(Srgb::new(255, 255, 255)));
        assert_eq!(ColorFormat::Lab.parse("lab(-50% 0 0)"), Some(Srgb::new(0, 0, 0)));
        assert_eq!(ColorFormat::Oklch.parse("oklch(300% 0 0)"), Some(Srgb::new(255, 255, 255)));
        assert_eq!(ColorFormat::Rgb.parse("rgb(999, -20, 0)"), Some(Srgb::new(255, 0, 0)));
    }

    /// CMYK is the NAIVE separation, and this says exactly what that guarantees: a
    /// reversible re-encoding of the sRGB triple to its printed precision, nothing about
    /// any real press.
    #[test]
    fn naive_cmyk_is_a_reversible_re_encoding_only() {
        assert_eq!(ColorFormat::Cmyk.format(Srgb::new(255, 0, 0)), "cmyk(0%, 100%, 100%, 0%)");
        assert_eq!(ColorFormat::Cmyk.format(Srgb::new(255, 255, 255)), "cmyk(0%, 0%, 0%, 0%)");
        assert_eq!(ColorFormat::Cmyk.format(Srgb::new(0, 0, 0)), "cmyk(0%, 0%, 0%, 100%)");
        // Mid grey uses no chroma ink at all, only black: the naive formula's signature.
        assert_eq!(ColorFormat::Cmyk.format(Srgb::new(128, 128, 128)), "cmyk(0%, 0%, 0%, 50%)");
        for c in [Srgb::new(10, 200, 90), Srgb::new(200, 30, 30)] {
            let back = ColorFormat::Cmyk
                .parse(&ColorFormat::Cmyk.format(c))
                .expect("parses");
            let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
            assert!(d(back.r, c.r) <= 4 && d(back.g, c.g) <= 4 && d(back.b, c.b) <= 4, "{c:?} -> {back:?}");
        }
    }

    /// Label text drawn ON the picked colour flips at the WCAG crossover, so the
    /// overlay's hex chip stays legible over anything on screen.
    #[test]
    fn text_on_a_swatch_flips_with_luminance() {
        assert!(Srgb::new(255, 255, 255).wants_dark_text(), "black text on white");
        assert!(!Srgb::new(0, 0, 0).wants_dark_text(), "white text on black");
        assert!(Srgb::new(255, 255, 0).wants_dark_text(), "yellow is a light surface");
        assert!(!Srgb::new(0, 0, 255).wants_dark_text(), "pure blue is a dark surface");
        assert!(Srgb::new(128, 128, 128).wants_dark_text());
        assert!(!Srgb::new(90, 90, 90).wants_dark_text());
    }
}

/// DRAGON-630: the alpha spellings, and the promise that OPAQUE stays byte-identical to
/// the pre-alpha formatter, which is what keeps every existing clipboard string and
/// persisted recent unchanged.
#[cfg(test)]
mod alpha_format_tests {
    use super::*;

    /// Opaque is `format`, byte for byte, in every notation.
    #[test]
    fn opaque_spells_exactly_as_before() {
        for c in [Srgb::new(255, 136, 0), Srgb::new(0, 0, 0), Srgb::new(18, 52, 86)] {
            for f in ColorFormat::ALL {
                assert_eq!(f.format_with_alpha(c, 255), f.format(c), "{}", f.id());
            }
        }
    }

    /// Every alpha spelling parses back to the same colour AND the same alpha, in its
    /// own notation. The round trip is the feature: a copied value must paste back.
    #[test]
    fn alpha_spellings_round_trip() {
        let c = Srgb::new(255, 136, 0);
        for a in [0u8, 1, 51, 128, 204, 254] {
            for f in ColorFormat::ALL {
                let s = f.format_with_alpha(c, a);
                let (back, ba) = f.parse_with_alpha(&s).unwrap_or_else(|| {
                    panic!("{} did not re-parse its own alpha spelling {s:?}", f.id())
                });
                assert_eq!(ba, a, "{}: {s:?}", f.id());
                // The colour survives to the notation's own round-trip precision; the
                // exact-byte notations must be exact.
                if matches!(f, ColorFormat::Hex | ColorFormat::Rgb) {
                    assert_eq!(back, c, "{}: {s:?}", f.id());
                }
            }
        }
    }

    /// The concrete spellings, pinned once so a future edit cannot quietly reshape what
    /// users paste into stylesheets.
    #[test]
    fn the_alpha_spellings_are_the_css_ones() {
        let c = Srgb::new(255, 136, 0);
        assert_eq!(ColorFormat::Hex.format_with_alpha(c, 0xCC), "#FF8800CC");
        assert_eq!(ColorFormat::Rgb.format_with_alpha(c, 204), "rgba(255, 136, 0, 0.8)");
        assert_eq!(ColorFormat::Hsl.format_with_alpha(c, 204), "hsla(32, 100%, 50%, 0.8)");
        assert_eq!(ColorFormat::Hsv.format_with_alpha(c, 204), "hsva(32, 100%, 100%, 0.8)");
        assert!(ColorFormat::Oklch.format_with_alpha(c, 204).ends_with(" / 0.8)"));
        assert!(ColorFormat::Lab.format_with_alpha(c, 204).ends_with(" / 0.8)"));
        assert_eq!(
            ColorFormat::Cmyk.format_with_alpha(c, 204),
            "cmyk(0%, 47%, 100%, 0%, 0.8)"
        );
    }

    /// All 256 alpha bytes survive the text round trip: three decimals is enough, and
    /// fewer was not (two decimals collapses 256 bytes onto 101 spellings).
    #[test]
    fn every_alpha_byte_survives_its_text() {
        for a in 0..=255u8 {
            let t = alpha_text(a);
            let f: f64 = t.parse().expect("alpha_text is a plain number");
            assert_eq!((f * 255.0).round() as u8, a, "{a} -> {t}");
        }
        assert_eq!(alpha_text(255), "1");
        assert_eq!(alpha_text(0), "0");
    }

    /// A pasted `rgba()` loads into the alpha-blind parse too: tolerance did not narrow.
    #[test]
    fn the_a_suffixed_names_parse() {
        let orange = Srgb::new(255, 136, 0);
        assert_eq!(ColorFormat::Rgb.parse("rgba(255, 136, 0, 0.5)"), Some(orange));
        assert_eq!(
            ColorFormat::Hsl.parse_with_alpha("hsla(32, 100%, 50%, 50%)"),
            ColorFormat::Hsl.parse_with_alpha("hsl(32, 100%, 50%, 0.5)"),
            "a percentage alpha and its fraction agree"
        );
    }
}

/// DRAGON-630: the persisted mode's plumbing, `from_id` and the stepper's `cycled`.
#[cfg(test)]
mod mode_tests {
    use super::*;

    /// Every notation round-trips through its own id, and junk falls out, so a
    /// hand-edited config cannot invent a mode.
    #[test]
    fn ids_round_trip_and_junk_declines() {
        for f in ColorFormat::ALL {
            assert_eq!(ColorFormat::from_id(f.id()), Some(f));
        }
        for junk in ["", "HEX", "rgba", "colour", " hex "] {
            assert_eq!(ColorFormat::from_id(junk), None, "{junk:?}");
        }
    }

}

/// DRAGON-630: the per-component boxes. What is pinned is the agreement between what a
/// box SHOWS and what typing that same text back does: display and edit must be one
/// vocabulary, or the boxes rewrite values under the user.
#[cfg(test)]
mod component_box_tests {
    use super::*;

    /// Each notation's box texts are exactly the numeric tokens of its own formatted
    /// string (hex aside, whose boxes are the spelling's digit PAIRS). One source of
    /// numbers.
    #[test]
    fn box_texts_agree_with_the_formatter() {
        let c = Srgb::new(255, 136, 0);
        for f in ColorFormat::ALL {
            if f == ColorFormat::Hex {
                for (i, pair) in ["FF", "88", "00"].into_iter().enumerate() {
                    assert_eq!(f.component_text(c, 255, i), pair, "hex box {i}");
                }
                continue;
            }
            let spelled = f.format(c);
            let tokens: Vec<String> = spelled
                .trim_start_matches(|ch: char| ch != '(')
                .trim_start_matches('(')
                .trim_end_matches(')')
                .split([',', ' '])
                .filter(|t| !t.is_empty())
                .map(|t| t.trim_end_matches('%').to_string())
                .collect();
            let labels = f.component_labels();
            assert_eq!(tokens.len(), labels.len(), "{}: {spelled}", f.id());
            for (i, tok) in tokens.iter().enumerate() {
                assert_eq!(&f.component_text(c, 255, i), tok, "{} box {i}", f.id());
            }
        }
    }

    /// Typing a box's own canonical text back settles IMMEDIATELY: after one
    /// application the displayed text is a fixed point, so a user who clicks into a box
    /// and commits it unchanged can never start a value walking. Exact identity is
    /// asserted where the display is coarser than the byte grid (hex, RGB); the
    /// colorimetric boxes display FINER than 8-bit quantization (OKLCH's three decimals
    /// name more values than the byte cube holds), so there the fixed point is the
    /// honest contract and byte-identity would be pinning luck.
    #[test]
    fn retyping_the_shown_value_settles_at_once() {
        let c = Srgb::new(200, 90, 30);
        for f in ColorFormat::ALL {
            let boxes = f.component_labels().len();
            for i in 0..boxes {
                let shown = f.component_text(c, 255, i);
                let (c1, a) = f
                    .with_component(c, 255, i, &shown)
                    .unwrap_or_else(|| panic!("{} box {i} refused its own text {shown:?}", f.id()));
                assert_eq!(a, 255, "{} box {i}", f.id());
                if matches!(f, ColorFormat::Hex | ColorFormat::Rgb) {
                    assert_eq!(c1, c, "{} box {i} moved an exact value", f.id());
                }
                let t1 = f.component_text(c1, 255, i);
                let (c2, _) = f
                    .with_component(c1, 255, i, &t1)
                    .unwrap_or_else(|| panic!("{} box {i} refused its own text {t1:?}", f.id()));
                assert_eq!(
                    f.component_text(c2, 255, i),
                    t1,
                    "{} box {i} keeps walking under its own value",
                    f.id()
                );
            }
        }
    }

    /// The box unit is the DISPLAYED unit: `50` in a percent box means 50%, with or
    /// without the suffix, where the free-string parser reads bare numbers as fractions.
    #[test]
    fn percent_boxes_read_bare_numbers_as_percentages() {
        let c = Srgb::new(255, 0, 0); // hsl(0, 100%, 50%)
        let (dimmed, _) = ColorFormat::Hsl.with_component(c, 255, 2, "25").expect("parses");
        let (dimmed_pct, _) =
            ColorFormat::Hsl.with_component(c, 255, 2, "25%").expect("parses");
        assert_eq!(dimmed, dimmed_pct, "bare and suffixed agree in a box");
        assert_eq!(ColorFormat::Hsl.component_text(dimmed, 255, 2), "25");
    }

    /// The alpha box: every mode has it one index past its own labels, it reads 0..1 or
    /// a percentage, and it changes ONLY the alpha. Hex is exercised separately: its
    /// alpha box speaks hex digits like the rest of its boxes.
    #[test]
    fn the_alpha_box_is_shared_and_only_moves_alpha() {
        let c = Srgb::new(10, 20, 30);
        for f in ColorFormat::ALL {
            if f == ColorFormat::Hex {
                continue;
            }
            let idx = f.component_labels().len();
            assert_eq!(f.component_text(c, 128, idx), "0.502");
            let (back, a) = f.with_component(c, 255, idx, "0.5").expect("parses");
            assert_eq!(back, c, "{}: the colour must not move", f.id());
            assert_eq!(a, 128, "{}", f.id());
            let (_, a) = f.with_component(c, 255, idx, "50%").expect("parses");
            assert_eq!(a, 128, "{}: a percentage means the same", f.id());
        }
    }

    /// The hex boxes are the spelling's digit PAIRS, one channel each, alpha included
    /// (DRAGON-630, the owner's split; labels stay the single letters). A channel edit
    /// moves ONLY its channel, the alpha box only the alpha, and a lone digit reads as
    /// itself rather than doubling.
    #[test]
    fn the_hex_boxes_hold_one_channel_each() {
        let c = Srgb::new(255, 136, 0);
        assert_eq!(ColorFormat::Hex.component_text(c, 204, 3), "CC", "the alpha box is hex too");
        assert_eq!(
            ColorFormat::Hex.with_component(c, 204, 1, "0A"),
            Some((Srgb::new(255, 10, 0), 204)),
            "a channel edit keeps the alpha"
        );
        assert_eq!(
            ColorFormat::Hex.with_component(c, 255, 3, "80"),
            Some((c, 0x80)),
            "an alpha edit keeps the colour"
        );
        assert_eq!(
            ColorFormat::Hex.with_component(c, 255, 0, "f"),
            Some((Srgb::new(0x0F, 136, 0), 255)),
            "one digit is itself, not the nibble-doubled shorthand"
        );
        assert_eq!(ColorFormat::Hex.with_component(c, 255, 0, "GG"), None, "not hex");
        assert_eq!(ColorFormat::Hex.with_component(c, 255, 0, "123"), None, "too long");
    }

    /// Junk keeps the colour: `None`, never a guess, so a half-typed box shows its draft
    /// and changes nothing, the same contract the full-string rows had. (`zz`, not
    /// `abc`: three hex DIGITS are a legal colour in the hex box, which is the point of
    /// that box being the whole spelling.)
    #[test]
    fn junk_in_a_box_declines() {
        let c = Srgb::new(1, 2, 3);
        for f in ColorFormat::ALL {
            for text in ["", "  ", "zz", "1 2", "nan", "inf"] {
                assert_eq!(f.with_component(c, 255, 0, text), None, "{} {text:?}", f.id());
            }
        }
    }
}

/// DRAGON-630: the HSV the window tracks. The rule under test is the survival of the
/// user's AIM through the degenerate colours where HSV itself goes silent.
#[cfg(test)]
mod hsv_tracking_tests {
    use super::*;

    /// A chromatic colour speaks for itself: tracking is exact, whatever came before.
    #[test]
    fn a_chromatic_color_answers_its_own_hsv() {
        let got = hsv_tracking([123.0, 0.5, 0.5], Srgb::new(255, 0, 0));
        assert_eq!(got[0], 0.0);
        assert!((got[1] - 1.0).abs() < 1e-9 && (got[2] - 1.0).abs() < 1e-9);
    }

    /// White and grey have no hue: the previous hue survives, so the hue slider holds
    /// still while the square is dragged through the achromatic edge.
    #[test]
    fn achromatic_keeps_the_previous_hue() {
        for c in [Srgb::new(255, 255, 255), Srgb::new(128, 128, 128)] {
            let got = hsv_tracking([210.0, 0.8, 0.9], c);
            assert_eq!(got[0], 210.0, "{c:?}");
            assert_eq!(got[1], 0.0, "{c:?}: saturation really is zero here");
        }
    }

    /// Black has neither hue nor saturation: both survive, so dragging Value to the
    /// floor and back lands where the user was, not on red.
    #[test]
    fn black_keeps_hue_and_saturation() {
        let got = hsv_tracking([210.0, 0.8, 0.9], Srgb::new(0, 0, 0));
        assert_eq!(got[0], 210.0);
        assert_eq!(got[1], 0.8);
        assert_eq!(got[2], 0.0);
    }

    /// And the round trip the square depends on: HSV out, colour in, HSV back.
    #[test]
    fn hsv_round_trips_through_a_color() {
        let hsv = [200.0, 0.5, 0.75];
        let back = hsv_tracking(hsv, srgb_from_hsv(hsv));
        assert!((back[0] - hsv[0]).abs() < 1.0, "{back:?}");
        assert!((back[1] - hsv[1]).abs() < 0.01, "{back:?}");
        assert!((back[2] - hsv[2]).abs() < 0.01, "{back:?}");
    }
}

/// DRAGON-630: the gradient rasters. Corners and ends are pinned exactly, because they
/// are the positions the marker maps to values through, and an off-by-one there is a
/// picker that cannot reach a pure colour.
#[cfg(test)]
mod gradient_raster_tests {
    use super::*;

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y as usize) * (w as usize) + x as usize) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// The SV square's three teaching corners: white, the pure hue, black. Exact, at an
    /// arbitrary odd size, because the span divisor is `extent - 1`.
    #[test]
    fn the_sv_square_corners_are_exact() {
        let (w, h) = (37, 23);
        let buf = sv_square_rgba(0.0, w, h, 0.0);
        assert_eq!(px(&buf, w, 0, 0), [255, 255, 255, 255], "top-left is white");
        assert_eq!(px(&buf, w, w - 1, 0), [255, 0, 0, 255], "top-right is the pure hue");
        assert_eq!(px(&buf, w, 0, h - 1), [0, 0, 0, 255], "bottom-left is black");
        assert_eq!(px(&buf, w, w - 1, h - 1), [0, 0, 0, 255], "bottom-right is black");
    }

    /// Both ends of the hue strip are red: 0 and 360 are one hue, so the marker can
    /// never fall into a gap between them.
    #[test]
    fn the_hue_strip_closes_its_circle() {
        let (w, h) = (256, 4);
        let buf = hue_strip_rgba(w, h, 0.0);
        assert_eq!(px(&buf, w, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(&buf, w, w - 1, 0), [255, 0, 0, 255]);
        // And the middle is not red, so the strip actually travelled the circle.
        assert_ne!(px(&buf, w, w / 2, 0), [255, 0, 0, 255]);
    }

    /// The alpha strip: the left edge is pure checkerboard, the right edge is the pure
    /// colour, and everything is opaque (the CHECKER says "transparent", the buffer
    /// itself is not).
    #[test]
    fn the_alpha_strip_runs_checker_to_color() {
        let c = Srgb::new(10, 200, 30);
        let (w, h) = (128, 8);
        let buf = alpha_strip_rgba(c, w, h, 0.0);
        let left = px(&buf, w, 0, 0);
        assert_eq!(&left[..3], &CHECKER_LIGHT, "the left edge is the board");
        assert_eq!(px(&buf, w, w - 1, 0), [10, 200, 30, 255], "the right edge is the colour");
        for x in [0, w / 2, w - 1] {
            assert_eq!(px(&buf, w, x, 0)[3], 255, "the strip itself is opaque");
        }
    }

    /// DRAGON-630: the rounded corners the appearance setting asks of the square and
    /// the tracks. A radius empties the corner pixel, keeps the centre and the STRAIGHT
    /// edge midpoints fully opaque (the naive distance-to-inner-box fade blurs the
    /// whole border, which is the bug the SDF form avoids), and a zero radius is
    /// byte-identical to square.
    #[test]
    fn a_radius_rounds_only_the_corners() {
        let (w, h) = (64, 32);
        let r = 8.0;
        let buf = sv_square_rgba(0.0, w, h, r);
        assert_eq!(px(&buf, w, 0, 0)[3], 0, "the corner pixel is outside the arc");
        assert_eq!(px(&buf, w, w / 2, 0)[3], 255, "the top edge midpoint is opaque");
        assert_eq!(px(&buf, w, 0, h / 2)[3], 255, "the left edge midpoint is opaque");
        assert_eq!(px(&buf, w, w / 2, h / 2)[3], 255, "the body is opaque");
        let square = sv_square_rgba(0.0, w, h, 0.0);
        assert_eq!(px(&square, w, 0, 0)[3], 255, "zero radius keeps square corners");
        // The strips take the same mask.
        let hue = hue_strip_rgba(64, 20, 8.0);
        assert_eq!(px(&hue, 64, 0, 0)[3], 0);
        assert_eq!(px(&hue, 64, 32, 10)[3], 255);
        let alpha = alpha_strip_rgba(Srgb::new(9, 9, 9), 64, 20, 8.0);
        assert_eq!(px(&alpha, 64, 63, 0)[3], 0);
    }

    /// The swatch disc: transparent corners (it is round), the colour in the middle,
    /// and a translucent colour shows the board through itself.
    #[test]
    fn the_swatch_circle_is_round_and_honest_about_alpha() {
        let c = Srgb::new(200, 10, 10);
        let d = 32;
        let solid = swatch_circle_rgba(c, 255, d, [128, 128, 128]);
        assert_eq!(px(&solid, d, 0, 0)[3], 0, "corners are outside the circle");
        assert_eq!(px(&solid, d, d / 2, d / 2), [200, 10, 10, 255]);
        let seethrough = swatch_circle_rgba(c, 128, d, [128, 128, 128]);
        assert_ne!(
            px(&seethrough, d, d / 2, d / 2),
            [200, 10, 10, 255],
            "half alpha mixes the board in"
        );
    }
}

/// [`Srgb::contrast_ratio`], the number every legibility claim in this app is settled by
/// (DRAGON-607). Pinned at the ends, at the WCAG crossover, and at the accent the owner
/// actually runs, so a future edit cannot quietly move what "readable" means.
#[cfg(test)]
mod contrast_ratio_tests {
    use super::Srgb;

    const WHITE: Srgb = Srgb { r: 255, g: 255, b: 255 };
    const BLACK: Srgb = Srgb { r: 0, g: 0, b: 0 };

    /// The two fixed points of the WCAG formula. If either of these moves, the formula
    /// itself is wrong, not the colours being tested with it.
    #[test]
    fn the_ends_of_the_scale_are_exact() {
        assert!((BLACK.contrast_ratio(WHITE) - 21.0).abs() < 0.01, "black on white is 21:1");
        assert!((WHITE.contrast_ratio(WHITE) - 1.0).abs() < 0.001, "a colour on itself is 1:1");
    }

    /// Contrast is symmetric: it describes a PAIR, not a direction. Worth pinning because
    /// the naive implementation subtracts in a fixed order and silently returns numbers
    /// below 1.0 when the arguments arrive the other way round.
    #[test]
    fn the_ratio_does_not_care_which_way_round_it_is_asked() {
        let purple = Srgb { r: 151, g: 125, b: 236 };
        assert!((purple.contrast_ratio(BLACK) - BLACK.contrast_ratio(purple)).abs() < 1e-9);
    }

    /// The crossover `wants_dark_text` uses really is the point where the better ink
    /// changes hands: on either side of it, the colour that function names must be the
    /// one with the HIGHER measured ratio. This ties the two functions together, so a
    /// future edit that moves the 0.179 threshold without moving the maths fails here.
    #[test]
    fn the_flip_always_names_the_higher_contrast_ink() {
        let cases = [
            Srgb { r: 255, g: 255, b: 255 },
            Srgb { r: 0, g: 0, b: 0 },
            Srgb { r: 128, g: 128, b: 128 },
            Srgb { r: 151, g: 125, b: 236 },
            Srgb { r: 255, g: 255, b: 0 },
            Srgb { r: 0, g: 0, b: 255 },
            // Straddling the crossover from both sides: ~0.179 relative luminance is
            // roughly a 0.46 grey in sRGB, so these two sit either side of the flip.
            Srgb { r: 110, g: 110, b: 110 },
            Srgb { r: 125, g: 125, b: 125 },
        ];
        for c in cases {
            let (chosen, other) =
                if c.wants_dark_text() { (BLACK, WHITE) } else { (WHITE, BLACK) };
            assert!(
                c.contrast_ratio(chosen) >= c.contrast_ratio(other),
                "{c:?} chose the ink that reads at {:.2}:1 over the one at {:.2}:1",
                c.contrast_ratio(chosen),
                c.contrast_ratio(other),
            );
        }
    }

    /// The owner's real accent (DRAGON-607), read from their COSMIC config: a purple that
    /// sits ABOVE the crossover, so black is the correct ink and white is not merely a
    /// style difference but a genuine legibility failure. These two numbers are why the
    /// inconsistency the ticket reports is a bug and not a preference.
    #[test]
    fn the_owners_purple_accent_needs_black_ink_to_clear_aa() {
        let purple = Srgb { r: 151, g: 125, b: 236 };
        assert!(purple.wants_dark_text(), "the owner's accent is a LIGHT surface");
        let black = purple.contrast_ratio(BLACK);
        let white = purple.contrast_ratio(WHITE);
        assert!(black >= 4.5, "black on the owner's accent reads at only {black:.2}:1");
        assert!(white < 4.5, "white on the owner's accent should FAIL AA, got {white:.2}:1");
        assert!(black > white, "black must be the better of the two");
    }
}
