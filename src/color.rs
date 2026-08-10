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
        let u = color.to_unit();
        match self {
            Self::Hex => format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b),
            Self::Rgb => format!("rgb({}, {}, {})", color.r, color.g, color.b),
            Self::Hsl => {
                let h = unit_to_hsl(u);
                format!(
                    "hsl({}, {}%, {}%)",
                    h[0].round() as i64,
                    (h[1] * 100.0).round() as i64,
                    (h[2] * 100.0).round() as i64
                )
            }
            Self::Hsv => {
                let h = unit_to_hsv(u);
                format!(
                    "hsv({}, {}%, {}%)",
                    h[0].round() as i64,
                    (h[1] * 100.0).round() as i64,
                    (h[2] * 100.0).round() as i64
                )
            }
            Self::Oklch => {
                let ok = xyz_to_oklab(linear_to_xyz(color.to_linear()));
                let (c, h) = ab_to_ch(ok[1], ok[2]);
                format!("oklch({:.1}% {:.3} {:.1})", ok[0] * 100.0, c, h)
            }
            Self::Cmyk => {
                let c = unit_to_cmyk(u);
                format!(
                    "cmyk({}%, {}%, {}%, {}%)",
                    (c[0] * 100.0).round() as i64,
                    (c[1] * 100.0).round() as i64,
                    (c[2] * 100.0).round() as i64,
                    (c[3] * 100.0).round() as i64
                )
            }
            Self::Lab => {
                let lab = xyz_to_lab(linear_to_xyz(color.to_linear()));
                format!("lab({:.1}% {:.1} {:.1})", lab[0], lab[1], lab[2])
            }
        }
    }

    /// Pure, unit-tested: parse a value written in this notation, or `None`.
    ///
    /// Deliberately TOLERANT, because a user edits these boxes by hand: the function
    /// name is optional, so are the parentheses, commas and spaces are interchangeable
    /// separators, and a `%` suffix is accepted. What is NOT tolerated is the wrong
    /// NUMBER of components: three numbers typed into the CMYK box is a typo, not a
    /// colour, and guessing would be the "reports the wrong colour" failure again.
    ///
    /// Out-of-gamut CIELAB / OKLCh values CLAMP into sRGB (module doc). Hue WRAPS, since
    /// 400 degrees is unambiguously 40.
    pub fn parse(self, s: &str) -> Option<Srgb> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if self == Self::Hex {
            return parse_hex(s);
        }
        let n = numbers(s, self.id())?;
        match self {
            Self::Hex => None,
            Self::Rgb => {
                let [r, g, b] = exactly3(&n)?;
                Some(Srgb::from_unit_clamped([
                    channel_255(r),
                    channel_255(g),
                    channel_255(b),
                ]))
            }
            Self::Hsl => {
                let [h, s, l] = exactly3(&n)?;
                Some(Srgb::from_unit_clamped(hsl_to_unit([
                    h.value,
                    fraction(s),
                    fraction(l),
                ])))
            }
            Self::Hsv => {
                let [h, s, v] = exactly3(&n)?;
                Some(Srgb::from_unit_clamped(hsv_to_unit([
                    h.value,
                    fraction(s),
                    fraction(v),
                ])))
            }
            Self::Oklch => {
                let [l, c, h] = exactly3(&n)?;
                // CSS allows `L` as `0..1` or as a percentage; `fraction` takes both.
                let (a, b) = ch_to_ab(c.value.max(0.0), h.value);
                Some(Srgb::from_linear_clamped(xyz_to_linear(oklab_to_xyz([
                    fraction(l).clamp(0.0, 1.0),
                    a,
                    b,
                ]))))
            }
            Self::Cmyk => {
                if n.len() != 4 {
                    return None;
                }
                Some(Srgb::from_unit_clamped(cmyk_to_unit([
                    fraction(n[0]),
                    fraction(n[1]),
                    fraction(n[2]),
                    fraction(n[3]),
                ])))
            }
            Self::Lab => {
                let [l, a, b] = exactly3(&n)?;
                // CIELAB's `L*` is already 0..100, so a `%` suffix means the same
                // number and the percent flag is ignored rather than rescaled.
                Some(Srgb::from_linear_clamped(xyz_to_linear(lab_to_xyz([
                    l.value.clamp(0.0, 100.0),
                    a.value,
                    b.value,
                ]))))
            }
        }
    }
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

/// Exactly three tokens, or `None`.
fn exactly3(n: &[Num]) -> Option<[Num; 3]> {
    match n {
        [a, b, c] => Some([*a, *b, *c]),
        _ => None,
    }
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

/// `#RGB`, `#RRGGBB`, and the same two without the `#`. The alpha forms (`#RGBA`,
/// `#RRGGBBAA`) are accepted and their alpha DISCARDED: the picker samples an opaque
/// screen pixel, so there is no alpha to carry, and refusing a pasted `#RRGGBBAA` would
/// only annoy.
fn parse_hex(s: &str) -> Option<Srgb> {
    let h = s.trim().trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 | 4 => {
            let nib = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|d| d * 17);
            Some(Srgb::new(nib(0)?, nib(1)?, nib(2)?))
        }
        6 | 8 => Some(Srgb::new(byte(0)?, byte(2)?, byte(4)?)),
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
        // Alpha is accepted and dropped: an opaque screen pixel has none to carry.
        assert_eq!(ColorFormat::Hex.parse("#FF8800CC"), Some(orange));
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
        assert_eq!(ColorFormat::Rgb.parse("rgb(255, 136, 0, 9)"), None, "four is a typo too");
        assert_eq!(ColorFormat::Cmyk.parse("cmyk(0%, 47%, 100%)"), None, "CMYK needs four");
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
