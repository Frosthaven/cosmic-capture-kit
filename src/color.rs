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

/// Pure, unit-tested: a colour's own HSV, `[hue degrees, saturation, value]`
/// (DRAGON-687). The plain forward conversion, published for the palette sorts' warmth
/// measure, which needs the hue and the saturation of a STORED colour rather than the
/// window's tracked triple ([`hsv_tracking`] answers a different question: what the
/// controls should show, with the previous hue surviving an achromatic colour). An
/// achromatic colour answers hue 0 and saturation 0, exactly as the internal conversion
/// always has.
pub fn srgb_to_hsv(color: Srgb) -> [f64; 3] {
    unit_to_hsv(color.to_unit())
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

// ── Colour HARMONIES (DRAGON-682) ────────────────────────────────────────────
//
// The compare panel's maths: given the colour the picker window is showing, the classic
// wheel relationships people reach for when they need a second colour that goes with the
// first. Pure, and here rather than in the panel's view because they are colour model,
// not layout, and because a harmony is exactly the kind of thing a test can pin exactly.
//
// **They rotate HUE in HSV, keeping saturation and value.** Two reasons, and the second is
// the one that would be re-argued: HSV is the model this window already thinks in (the
// gradient square's axes, the hue strip, the tracked `ColorPickerState::hsv`), so a
// harmony swatch is the same colour the hue strip would land on if you dragged it that
// far; and keeping S and V means every swatch in a card is as vivid and as bright as the
// colour it came from, which is what makes a harmony read as a SET. HSL rotation is the
// other common choice and produces slightly different swatches for the same angles; it is
// not more correct, and switching would move every card at once.
//
// **The hue is the same number in both models** (`hue_of` serves HSL and HSV alike), so
// the ANGLES below are the textbook ones whichever model a reader has in mind.

/// **Pure**, unit-tested: the MONOCHROMATIC ramp for a colour, as HSV `hsv` (DRAGON-682
/// item 24): [`MONOCHROME_STEPS`] segments of one hue, ordered DARK to LIGHT, with the base
/// colour itself at its natural place among them.
///
/// # The rule
///
/// 1. The base takes the SLOT its own value earns, `round(v * (n - 1))`, so a dark colour
///    sits near the dark end and a light one near the light end. That is the part a user
///    can predict by eye.
/// 2. The step is then whatever FITS on both sides: the smaller of the room below the base
///    and the room above it, per slot. So the ramp always stays inside `0..=1`, is evenly
///    spaced, and contains the base exactly, at full precision, rather than near it.
///
/// # Why not the obvious two
///
/// A ladder of FIXED values (0.25, 0.5, 0.75, 1.0) is what shipped first: it always spreads
/// nicely, but the base is not on it, so the card had to show the base separately and read
/// as one odd segment followed by an unrelated gradient. Fixed OFFSETS from the base
/// (`v ± 0.2`, `v ± 0.4`) put the base in the middle but collapse at the ends: a colour near
/// black or white clamps two or three steps onto the same value and the card shows duplicate
/// segments. Choosing the step from the room available is what avoids both, and it is why an
/// extreme base gives a ramp that reaches away from its own end rather than a shorter one.
///
/// Saturation and hue are the base's throughout: this is a ramp of shades and tints, and
/// desaturating the light end would make it a different harmony.
fn monochrome_ramp(hsv: [f64; 3]) -> Vec<Srgb> {
    let n = MONOCHROME_STEPS;
    let last = (n - 1) as f64;
    let v = hsv[2].clamp(0.0, 1.0);
    // The base's own slot, and therefore how many segments sit below and above it.
    let k = (v * last).round().clamp(0.0, last) as usize;
    let below = k as f64;
    let above = last - below;
    // The largest even step that keeps every segment inside `0..=1`. A base at either end
    // has room on one side only, and takes that side's step; a base with no room at all
    // (a one-segment ramp, which `MONOCHROME_STEPS` never produces) answers a flat ramp
    // rather than dividing by zero.
    let step = match (below > 0.0, above > 0.0) {
        (true, true) => (v / below).min((1.0 - v) / above),
        (true, false) => v / below,
        (false, true) => (1.0 - v) / above,
        (false, false) => 0.0,
    };
    (0..n)
        .map(|i| {
            let value = (v + (i as f64 - below) * step).clamp(0.0, 1.0);
            srgb_from_hsv([hsv[0], hsv[1], value])
        })
        .collect()
}

/// One colour HARMONY: a named relationship on the colour wheel (DRAGON-682).
///
/// The order of [`Self::ALL`] is the order the compare panel lists them in, and it runs
/// from the tightest relationship to the loosest: the one opposite colour, then its two
/// neighbours, then the sets that spread further round the wheel, then the one that does
/// not rotate at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Harmony {
    /// The colour directly opposite: one rotation of 180 degrees.
    Complementary,
    /// The two immediate neighbours, 30 degrees either side.
    Analogous,
    /// The three-way split of the wheel: 120 and 240 degrees.
    Triadic,
    /// The four-way split: 90, 180 and 270 degrees.
    Tetradic,
    /// No rotation at all: the same hue at an ordered ramp of VALUES.
    Monochromatic,
}

// SPLIT COMPLEMENTARY (150 and 210 degrees) lived here for one build and the owner cut the
// GROUP, not the maths: DRAGON-682 item 20 fixed the panel's list at exactly these five, in
// this order. Adding it back is two lines here and one variant above; nothing else in the
// panel is a list of harmonies.
//
// The names lost their "Colors" suffix in the same pass, and `Companion` became
// `Complementary`, which is the textbook name for the same 180-degree rotation.

/// How many segments [`Harmony::Monochromatic`]'s ramp holds, the base's own among them.
///
/// Five, which is the widest any other card is (tetradic's four rotations plus its base),
/// so the panel's bars stay a family rather than one long card among short ones.
const MONOCHROME_STEPS: usize = 5;

impl Harmony {
    /// Every harmony the compare panel shows, in panel order.
    /// Every harmony the panel shows, in PANEL ORDER, which is also this enum's own
    /// declaration order (DRAGON-682 items 20 and 29): the code and the screen list them the
    /// same way round, so neither can be read as the other's shuffle.
    ///
    /// The order is the owner's twice over: the five were fixed by item 20, and item 29
    /// moved Complementary to the front and Monochromatic to the back. It runs tightest
    /// relationship first and ends with the one that is not a rotation at all.
    pub const ALL: [Self; 5] = [
        Self::Complementary,
        Self::Analogous,
        Self::Triadic,
        Self::Tetradic,
        Self::Monochromatic,
    ];

    /// The group's title, as the panel prints it (DRAGON-682 item 20: the owner's exact
    /// names, with no "Colors" suffix on any of them).
    pub fn label(self) -> &'static str {
        match self {
            Self::Complementary => "Complementary",
            Self::Analogous => "Analogous",
            Self::Triadic => "Triadic",
            Self::Tetradic => "Tetradic",
            Self::Monochromatic => "Monochromatic",
        }
    }

    /// ONE very short sentence saying what this harmony IS, for the group heading's hover
    /// explainer (DRAGON-682 item 23).
    ///
    /// Plain language, no jargon beyond "hue", and US spelling like every other string in
    /// this app ("color wheel", not "colour wheel"). Short because it is a tooltip: it has
    /// to be readable in the moment a pointer rests, and a second sentence would be a
    /// paragraph nobody finishes.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Complementary => "Opposite hues on the color wheel.",
            Self::Analogous => "Neighboring hues on the color wheel.",
            Self::Triadic => "Three hues evenly spaced around the wheel.",
            Self::Tetradic => "Four hues evenly spaced around the wheel.",
            Self::Monochromatic => "One hue at different lightness levels.",
        }
    }

    /// A stable identifier for logs and tests. Never user-facing, so it can never move.
    ///
    /// No production caller today: the panel prints [`Self::label`] and the messages carry
    /// colours rather than harmonies. It stays because a harmony is exactly the kind of
    /// thing a log line will want to name the day one misbehaves, and because the tests
    /// below identify their cases with it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn id(self) -> &'static str {
        match self {
            Self::Complementary => "complementary",
            Self::Analogous => "analogous",
            Self::Triadic => "triadic",
            Self::Tetradic => "tetradic",
            Self::Monochromatic => "monochromatic",
        }
    }

    /// The hue rotations this harmony applies, in degrees, EXCLUDING the base's own zero.
    ///
    /// Empty for [`Self::Monochromatic`], which is the one relationship that is not a
    /// rotation; [`Self::swatches`] is where that difference is handled, so no caller has
    /// to know which is which.
    pub fn offsets(self) -> &'static [f64] {
        match self {
            Self::Complementary => &[180.0],
            Self::Analogous => &[-30.0, 30.0],
            Self::Triadic => &[120.0, 240.0],
            Self::Tetradic => &[90.0, 180.0, 270.0],
            Self::Monochromatic => &[],
        }
    }

    /// **Pure**, unit-tested: the card's swatches for `base`, the BASE FIRST and then the
    /// colours this harmony derives from it.
    ///
    /// The base leads every ROTATION card deliberately (the owner's ask: a card "shows
    /// swatches that include our current color and calculated companion color"). A harmony
    /// is a relationship, and a set of derived colours with nothing to relate them to is a
    /// row of colours the user has to hold the original in their head to read.
    ///
    /// **MONOCHROMATIC is the exception, and the asymmetry is deliberate** (DRAGON-682 item
    /// 24). It is not a set of rotations at all, it is one ORDERED RAMP, so the base belongs
    /// at its own place in the sequence rather than in front of it: leading with it put the
    /// chosen colour first and out of order, and the owner read the card as "the bright
    /// colour we chose and then a smooth gradient" with no relation between them. See
    /// [`monochrome_ramp`] for the rule. Do not unify the two conventions: for a rotation
    /// card the base leading IS the relationship, and for a ramp it breaks one.
    ///
    /// **The swatches carry no alpha of their own, and the PANEL draws them at the window's
    /// current one** (DRAGON-682 item 19). `Srgb` has no alpha field, so a harmony is a
    /// statement about hue alone and stays one; what changed is the presentation, which the
    /// owner asked for: a translucent current colour makes a translucent card, over the same
    /// checkerboard the history swatches use, so a harmony of a half transparent colour looks
    /// like what it is.
    ///
    /// It shipped OPAQUE for one build on the argument that a relationship holds at any
    /// transparency. That is true and was beside the point: the panel is showing you colours
    /// you might USE, and a preview that quietly drops the alpha is previewing something
    /// else. Everything downstream follows the same alpha: taking a swatch as the active
    /// colour takes that alpha with it, the copy spells it, and the tooltip shows it.
    pub fn swatches(self, base: Srgb) -> Vec<Srgb> {
        let hsv = unit_to_hsv(base.to_unit());
        if self == Self::Monochromatic {
            return monochrome_ramp(hsv);
        }
        let mut out = vec![base];
        out.extend(
            self.offsets()
                .iter()
                .map(|d| srgb_from_hsv([hsv[0] + d, hsv[1], hsv[2]])),
        );
        out
    }
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
    ///
    /// **No production caller since DRAGON-680**, and it stays because the alpha-blind
    /// question is a real one that this module should be able to answer: the picker's
    /// persisted history was its last user, and that now keeps the alpha
    /// (`color_picker::geom::Recent`). Its tests are the ones that pin the whole tolerant
    /// parsing surface, so deleting it would take that coverage with it.
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// Pure, unit-tested: the notation `steps` places away in [`Self::ALL`], wrapping at
    /// both ends (DRAGON-630, restored by DRAGON-680). The window's mode stepper walks the
    /// list with this, so the order the user cycles through is the one owner-chosen row
    /// order and nothing re-derives it.
    ///
    /// It lived here for DRAGON-630's first chevron pair, was deleted when the owner's
    /// review replaced that control with an index-selecting dropdown, and is back because
    /// DRAGON-680 reversed the dropdown: the mode control is a bare up/down chevron pair
    /// again. The walk is kept HERE, beside [`Self::ALL`], rather than in the picker's
    /// `geom`, because the ORDER being walked is this list's own property.
    pub fn cycled(self, steps: i32) -> Self {
        let at = Self::ALL.iter().position(|f| *f == self);
        // The wrap arithmetic is `keynav::step`, shared with the preview editor's toolbar
        // flyouts, so every keyboard-navigable list in the app wraps by one rule. What
        // stays here is the LIST being walked, which is this type's own property.
        match crate::keynav::step(at, steps, Self::ALL.len()) {
            Some(i) => Self::ALL[i],
            None => self,
        }
    }

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
/// The cell size on the HISTORY swatches (DRAGON-680 item 26).
///
/// FOUR, about half the shared cell, because the same board reads twice too coarse on a
/// swatch a fraction of the size: the owner's "the checkerboard size on the small swatches
/// needs to be zoomed out by about 2x. its good on the main swatch and transparency line
/// though". At 9 a 28px swatch holds barely three cells, which reads as two grey BLOCKS
/// rather than as the "transparent here" texture the board is; at 4 it holds seven.
///
/// It is a SECOND constant rather than a smaller shared one precisely because the owner
/// approved the other two at 9: the round swatch and the alpha strip are much larger, and
/// moving the shared value would have fixed one complaint by creating another.
const RECENT_CHECKER_CELL: u32 = 4;
const CHECKER_LIGHT: [u8; 3] = [190, 190, 190];
const CHECKER_DARK: [u8; 3] = [122, 122, 122];

/// The checker grey under pixel `(x, y)`, at the shared [`CHECKER_CELL`] size.
fn checker_at(x: u32, y: u32) -> [u8; 3] {
    checker_at_cell(x, y, CHECKER_CELL)
}

/// The checker grey under pixel `(x, y)` at an explicit cell size, for a raster whose
/// board has to be a different size from everybody else's (DRAGON-680 item 26).
fn checker_at_cell(x: u32, y: u32, cell: u32) -> [u8; 3] {
    let cell = cell.max(1);
    if ((x / cell) + (y / cell)).is_multiple_of(2) {
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
    (0.5 - rounded_rect_sd(x, y, w, h, r)).clamp(0.0, 1.0)
}

/// The SIGNED DISTANCE from pixel `(x, y)`'s centre to that rounded rectangle's edge:
/// negative inside, positive outside, in pixels.
///
/// Split out of [`rounded_rect_coverage`] by DRAGON-682, because a RING needs the distance
/// itself rather than one coverage value: the dotted placeholder outline is the difference
/// between the shape's coverage and the coverage of the same shape one pixel smaller, and
/// computing that from two independent evaluations is the same arithmetic twice.
fn rounded_rect_sd(x: u32, y: u32, w: u32, h: u32, r: f64) -> f64 {
    let r = r.max(0.0).min(w as f64 / 2.0).min(h as f64 / 2.0);
    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
    let (hw, hh) = (w as f64 / 2.0, h as f64 / 2.0);
    let qx = (px - hw).abs() - (hw - r);
    let qy = (py - hh).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
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

/// The rim's thickness as a fraction of the swatch's DIAMETER, so a disc rastered at any
/// resolution keeps the same visible hairline (DRAGON-680).
///
/// 1/32, which is the 1.5px the rim always was at the shipped 48pt disc. It has to be a
/// fraction rather than a constant now that the raster is built SUPERSAMPLED: a fixed 1.5
/// pixels in a 3x buffer would draw a rim a third of its intended weight, and the owner
/// would have reported the opposite problem.
const SWATCH_RIM_FRACTION: f64 = 1.0 / 32.0;

/// Pure, unit-tested: a round swatch of `color` at `alpha`, `d` x `d`, straight RGBA:
/// the colour over the checkerboard (so a translucent colour shows AS translucent),
/// inside an anti-aliased circular mask with a thin rim in `rim` so the disc holds its
/// shape over any window background. The rim is a parameter (DRAGON-630, the owner's
/// "subdued, not white/black") so the caller can hand in the live theme's subdued tone.
///
/// # The rim: this raster does NOT draw the visible edge (DRAGON-680)
///
/// The disc's silhouette is a QUAD RING stacked over this raster by the view
/// (`color_picker::geom::SWATCH_RING_W`), because the renderer anti-aliases a rounded quad
/// analytically at the display's real resolution and a raster cannot. Two raster-side
/// attempts at the owner's "super blocky around the rim" failed first, and
/// `SWATCH_RING_W`'s doc carries why; the short version is that a one-pixel coverage ramp
/// cannot hide a 24px-radius curve, and supersampling into iced's mip-less image sampler
/// decimates the feather instead of averaging it.
///
/// So what this function draws is the disc's INTERIOR, which is the part a quad cannot
/// express: the colour composited over the checkerboard at its own alpha. Its edge stops
/// `inset` pixels short of the buffer's own radius, so the raster's stepped boundary and
/// its ramp both end up UNDER the ring's opaque band and are never seen. It keeps painting
/// its own rim band in `rim` for the same reason it keeps the ramp: whatever peeks past the
/// ring's inner edge is then the same colour as the ring, so the join is invisible.
///
/// Every pixel is WRITTEN, including the fully transparent ones outside the disc, and those
/// carry the RIM's colour rather than zeroed black. That is the fringe guard: this buffer is
/// STRAIGHT alpha (like every raster in this module), and any filtered resample averages the
/// RGB of neighbouring texels regardless of their alpha, so transparent black outside the
/// edge would be averaged into it and draw a dark halo.
pub fn swatch_circle_rgba(color: Srgb, alpha: u8, d: u32, rim: [u8; 3], inset: f64) -> Vec<u8> {
    let mut out = vec![0u8; (d as usize) * (d as usize) * 4];
    let radius = d as f64 / 2.0;
    // Where the RASTER's own disc ends: inside the buffer's radius by the mask, so the
    // ring drawn over it covers this boundary completely.
    let outer = (radius - inset.max(0.0)).max(0.0);
    let af = alpha as f64 / 255.0;
    // The rim band, in THIS buffer's pixels: a constant share of the diameter, so the band
    // reads the same at any raster size.
    let rim_w = (d as f64 * SWATCH_RIM_FRACTION).max(1.0);
    for y in 0..d {
        for x in 0..d {
            let (dx, dy) = (x as f64 + 0.5 - radius, y as f64 + 0.5 - radius);
            let dist = (dx * dx + dy * dy).sqrt();
            // One-pixel anti-alias band at the raster's edge; fully outside is transparent,
            // but is still written in the rim's own colour (see the doc's fringe guard).
            let cover = (outer - dist + 0.5).clamp(0.0, 1.0);
            let under = checker_at(x, y);
            let mix = |c: u8, u: u8| -> f64 { c as f64 * af + u as f64 * (1.0 - af) };
            let mut px = [mix(color.r, under[0]), mix(color.g, under[1]), mix(color.b, under[2])];
            // The rim: the outermost band of the raster's disc, and everything beyond it.
            if dist >= outer - rim_w {
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

/// Pure, unit-tested: a HISTORY swatch, `w` x `h`, straight RGBA, `radius`-rounded
/// (DRAGON-680): the colour SPLIT down the middle, opaque on the left, at its real
/// `alpha` over the checkerboard on the right.
///
/// The owner's design, and the reason for it is that a translucent swatch alone is
/// ambiguous: a 30%-alpha red over a checkerboard is a pale pink-grey, and nothing on
/// screen says whether the entry is "pale pink" or "red, mostly transparent". Showing the
/// colour at full strength beside it answers that in one glance, and the checkerboard
/// answers "how transparent" the same way it does under the round current-colour swatch
/// and along the alpha strip. Same greys, so the three read as one vocabulary, but a
/// SMALLER cell ([`RECENT_CHECKER_CELL`]): the shared size reads twice too coarse on a
/// swatch this small, which is the owner's item 26.
///
/// **An OPAQUE entry is a flat fill of the colour**, on both halves, with the board
/// invisible behind it: the split is not drawn, because there is nothing to compare.
/// (Callers may skip this raster entirely for an opaque entry and paint the flat colour,
/// which is what the picker window does; the two agree by construction, since at
/// `alpha == 255` the mix below is the colour itself on both sides.)
///
/// The split is at the exact half, `x < w / 2`, so an odd width gives the extra column to
/// the transparent side. That is the right way round: the LEFT half is a reference the eye
/// only compares against, while the right half is the value being judged.
pub fn recent_swatch_rgba(color: Srgb, alpha: u8, w: u32, h: u32, radius: f64) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let af = alpha as f64 / 255.0;
    let half = w / 2;
    for y in 0..h {
        for x in 0..w {
            let cover = rounded_rect_coverage(x, y, w, h, radius);
            // Every pixel is written, the transparent corners included, and each carries
            // the colour it would have had if it were inside. Same fringe guard
            // `swatch_circle_rgba`'s doc spells out: this is a STRAIGHT-alpha buffer drawn
            // at a fixed logical size from a supersampled raster, and a filtered downscale
            // averages neighbouring RGB whatever their alpha, so a zeroed corner would
            // darken the rounded edge beside it.
            //
            // Left: the colour with no transparency at all. Right: the colour at its own
            // alpha over the board, at the HISTORY's own finer cell (DRAGON-680 item 26:
            // the shared cell reads twice too coarse at this size).
            let a = if x < half { 1.0 } else { af };
            let under = checker_at_cell(x, y, RECENT_CHECKER_CELL);
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

/// The dotted placeholder outline's dash period, in raster pixels: `on` then `off`.
///
/// Two and two, which reads as dots rather than as a dashed line at the sizes this draws
/// at (a 28pt swatch has room for seven of them along an edge) and is coarse enough to
/// survive being drawn at 1:1 on a 1x display.
const DOT_ON: u32 = 2;
const DOT_OFF: u32 = 2;

/// Pure, unit-tested: a 1px DOTTED rounded-rect outline in `ink`, `w` x `h`, straight RGBA
/// (DRAGON-682): the empty slots in the colour history, so the grid's full extent is
/// visible before it fills up.
///
/// **A raster because a quad cannot dash.** iced draws a container's border as a solid
/// signed-distance ring with no dash pattern, and the alternatives are worse: a ring of
/// little quads is a dozen widgets per empty slot (there can be eighteen of them), and a
/// baked image would freeze one theme's ink into an asset. This takes the colour as a
/// parameter and the caller hands it the live `theme::subdued`, so it follows the theme
/// exactly as the swatch rims and the slider hairlines do.
///
/// The dash phase is measured along whichever EDGE a pixel is nearest, which keeps the
/// dots evenly spaced along the straight runs where the eye reads them. At the corners the
/// two phases meet and a dot can land a pixel early or late; that is invisible at a 1px
/// stroke and is the price of not carrying an arc-length parameterisation for a
/// placeholder.
///
/// Everything outside the ring is transparent, and every pixel carries the INK's own
/// colour so a resample cannot fringe it (the same guard [`swatch_circle_rgba`] documents).
pub fn dotted_outline_rgba(w: u32, h: u32, radius: f64, ink: [u8; 3]) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let period = (DOT_ON + DOT_OFF).max(1);
    for y in 0..h {
        for x in 0..w {
            // The 1px ring: the shape's own coverage MINUS the coverage of the same shape
            // one pixel smaller, which is 1 for a pixel straddling the edge and 0 for one
            // safely inside or outside it.
            let sd = rounded_rect_sd(x, y, w, h, radius);
            let outer = (0.5 - sd).clamp(0.0, 1.0);
            let inner = (0.5 - (sd + 1.0)).clamp(0.0, 1.0);
            let ring = (outer - inner).clamp(0.0, 1.0);
            // The dash phase, measured along whichever pair of edges this pixel is nearer,
            // so the dots march evenly across a top and down a side.
            let dx = x.min(w.saturating_sub(1).saturating_sub(x));
            let dy = y.min(h.saturating_sub(1).saturating_sub(y));
            let t = if dy <= dx { x } else { y };
            let on = t % period < DOT_ON;
            let a = if on { ring } else { 0.0 };
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[
                ink[0],
                ink[1],
                ink[2],
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// A drop zone's dash, in pixels on and pixels off (DRAGON-682 item 41).
///
/// Far longer than [`DOT_ON`]'s two, because the shape is far bigger: the same two-on
/// two-off that reads as dots around a 28pt swatch reads as a fuzzy line around a 388pt
/// region. Eight and six is a dash you can see the rhythm of at arm's length.
const DASH_ON: u32 = 8;
const DASH_OFF: u32 = 6;

/// Pure, unit-tested: a DASHED rounded-rect outline in `ink`, `w` x `h`, straight RGBA
/// (DRAGON-682 item 41): the boundary of the drop zone a drag is currently over.
///
/// [`dotted_outline_rgba`]'s bigger sibling, and deliberately the same construction (the
/// same ring, the same edge-measured dash phase, the same fully-coloured transparent
/// pixels), so the two cannot diverge in how they look; what differs is the dash period and
/// the stroke WIDTH, since a hairline around a whole region disappears.
///
/// The ink is a parameter, as it is there, and the caller hands it the live accent, so the
/// highlight follows the user's accent colour and their light or dark theme.
pub fn dashed_outline_rgba(w: u32, h: u32, radius: f64, stroke: f64, ink: [u8; 3]) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let period = (DASH_ON + DASH_OFF).max(1);
    for y in 0..h {
        for x in 0..w {
            // The stroke-wide ring: the shape's coverage minus the coverage of the same
            // shape `stroke` pixels smaller.
            let sd = rounded_rect_sd(x, y, w, h, radius);
            let outer = (0.5 - sd).clamp(0.0, 1.0);
            let inner = (0.5 - (sd + stroke.max(1.0))).clamp(0.0, 1.0);
            let ring = (outer - inner).clamp(0.0, 1.0);
            let dx = x.min(w.saturating_sub(1).saturating_sub(x));
            let dy = y.min(h.saturating_sub(1).saturating_sub(y));
            let t = if dy <= dx { x } else { y };
            let on = t % period < DASH_ON;
            let a = if on { ring } else { 0.0 };
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[
                ink[0],
                ink[1],
                ink[2],
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// Pure, unit-tested: a plain CHECKERBOARD, `w` x `h`, straight RGBA, `radius`-rounded
/// (DRAGON-682 item 19): the board a translucent swatch bar is drawn over.
///
/// The TRANSPARENCY SLIDER's own cell size (DRAGON-682 item 26, the owner: "the
/// checkerboard size on the items in the tabs should be the same size we use in the
/// transparency slider"), which is the shared [`CHECKER_CELL`] that [`alpha_strip_rgba`]
/// draws with. Reading the same constant is what stops the two drifting; it shipped at the
/// history swatches' finer cell for one build, and the owner matched it to the strip
/// instead. The history swatches and the strip are both unchanged.
///
/// No colour at all, which is what makes it shareable: the colours are drawn OVER it as
/// translucent quads, one per segment, so one board serves every bar and every colour.
pub fn checkerboard_rgba(w: u32, h: u32, radius: f64) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h {
        for x in 0..w {
            let cover = rounded_rect_coverage(x, y, w, h, radius);
            let c = checker_at(x, y);
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&[c[0], c[1], c[2], (cover * 255.0).round() as u8]);
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
/// DRAGON-682: the colour HARMONIES the compare panel draws. What is pinned is the ANGLES
/// (a harmony is defined by them, so a wrong one is a wrong feature, not a wrong pixel) and
/// the properties that hold whatever the angles are.
#[cfg(test)]
mod harmony_tests {
    use super::*;

    /// A vivid orange, chromatic enough that its hue is unambiguous.
    const BASE: Srgb = Srgb { r: 255, g: 136, b: 0 };

    /// The hue of a colour, in degrees, for comparing what a rotation actually produced.
    fn hue(c: Srgb) -> f64 {
        unit_to_hsv(c.to_unit())[0]
    }

    /// The textbook angles, one harmony at a time. These ARE the feature: everything else
    /// here is a property that would hold for a wrong set just as well.
    #[test]
    fn every_harmony_rotates_by_its_own_textbook_angles() {
        assert_eq!(Harmony::Complementary.offsets(), &[180.0]);
        assert_eq!(Harmony::Analogous.offsets(), &[-30.0, 30.0]);
        assert_eq!(Harmony::Triadic.offsets(), &[120.0, 240.0]);
        assert_eq!(Harmony::Tetradic.offsets(), &[90.0, 180.0, 270.0]);
        assert!(Harmony::Monochromatic.offsets().is_empty(), "it does not rotate at all");
    }

    /// And the angles reach the COLOURS: every derived swatch really is its offset away
    /// from the base on the wheel. Checked in degrees rather than in bytes, because that is
    /// what the harmony means; a byte comparison would pass on a table of literals.
    #[test]
    fn the_derived_swatches_land_on_their_own_angles() {
        for h in Harmony::ALL {
            let swatches = h.swatches(BASE);
            for (offset, got) in h.offsets().iter().zip(swatches.iter().skip(1)) {
                let want = (hue(BASE) + offset).rem_euclid(360.0);
                let delta = (hue(*got) - want).abs().min(360.0 - (hue(*got) - want).abs());
                assert!(
                    delta < 1.5,
                    "{}: {offset} degrees landed on {} rather than {want}",
                    h.id(),
                    hue(*got)
                );
            }
        }
    }

    /// The BASE leads every ROTATION card (the owner's ask), and a card is never just the
    /// base. Monochromatic is deliberately not in this rule (item 24): it is a ramp, and
    /// the base sits at its own place inside it.
    #[test]
    fn every_rotation_card_leads_with_the_current_colour() {
        for h in Harmony::ALL.into_iter().filter(|h| *h != Harmony::Monochromatic) {
            let swatches = h.swatches(BASE);
            assert_eq!(swatches[0], BASE, "{}", h.id());
            assert!(swatches.len() >= 2, "{}: a card with nothing to compare", h.id());
            assert_eq!(swatches.len(), 1 + h.offsets().len(), "{}", h.id());
        }
        assert_eq!(Harmony::Monochromatic.swatches(BASE).len(), MONOCHROME_STEPS);
    }

    /// A rotation keeps the colour's SATURATION and VALUE: that is what makes a card read
    /// as one family rather than as five unrelated colours, and it is the whole reason
    /// these rotate in HSV.
    #[test]
    fn a_rotation_keeps_the_vividness_it_started_with() {
        let hsv = unit_to_hsv(BASE.to_unit());
        for h in Harmony::ALL.into_iter().filter(|h| *h != Harmony::Monochromatic) {
            for got in h.swatches(BASE).into_iter().skip(1) {
                let got = unit_to_hsv(got.to_unit());
                assert!((got[1] - hsv[1]).abs() < 0.02, "{}: saturation moved", h.id());
                assert!((got[2] - hsv[2]).abs() < 0.02, "{}: value moved", h.id());
            }
        }
    }

    /// The companion of the companion is the colour you started with. Pinned because it is
    /// the one relationship a user can check by eye, and a sign error in the rotation
    /// survives every other test here.
    #[test]
    fn the_companion_of_the_companion_comes_home() {
        let companion = Harmony::Complementary.swatches(BASE)[1];
        let back = Harmony::Complementary.swatches(companion)[1];
        for (a, b) in [(back.r, BASE.r), (back.g, BASE.g), (back.b, BASE.b)] {
            assert!(a.abs_diff(b) <= 2, "{back:?} is not {BASE:?} again");
        }
    }

    /// The values of a monochromatic card, in order, for the tests below.
    fn ramp(base: Srgb) -> Vec<f64> {
        Harmony::Monochromatic
            .swatches(base)
            .into_iter()
            .map(|c| unit_to_hsv(c.to_unit())[2])
            .collect()
    }

    /// MONOCHROMATIC is ONE ORDERED RAMP of the base's hue (DRAGON-682 item 24): same hue,
    /// same saturation, values climbing dark to light, no duplicates, and the BASE itself
    /// among the segments rather than in front of them.
    #[test]
    fn monochromatic_is_one_ordered_ramp_containing_its_base() {
        let swatches = Harmony::Monochromatic.swatches(BASE);
        assert_eq!(swatches.len(), MONOCHROME_STEPS);
        let base_hsv = unit_to_hsv(BASE.to_unit());
        for got in &swatches {
            let hsv = unit_to_hsv(got.to_unit());
            // A segment at value 0 is BLACK, which has no hue or saturation to compare:
            // `unit_to_hsv` answers 0 for both, honestly. The ramp reaching black at its
            // dark end is the feature, so the check is on the segments that have a colour.
            if hsv[2] <= f64::EPSILON {
                continue;
            }
            assert!((hsv[0] - base_hsv[0]).abs() < 1.5, "the hue moved to {}", hsv[0]);
            assert!((hsv[1] - base_hsv[1]).abs() < 0.02, "the saturation moved");
        }
        let values = ramp(BASE);
        assert!(
            values.windows(2).all(|w| w[1] > w[0] + 0.01),
            "the ramp must climb without repeating: {values:?}"
        );
        // The BASE is one of the segments, at its own value, not near it.
        assert!(
            values.iter().any(|v| (v - base_hsv[2]).abs() < 0.005),
            "{values:?} does not contain the base's own value {}",
            base_hsv[2]
        );
        // …and it sits where its value earns, not always first or always middle.
        let at = values
            .iter()
            .position(|v| (v - base_hsv[2]).abs() < 0.005)
            .expect("the base is in the ramp");
        let want = (base_hsv[2] * (MONOCHROME_STEPS - 1) as f64).round() as usize;
        assert_eq!(at, want, "the base sits at slot {at}, not the {want} its value earns");
    }

    /// The EDGES, which is where the two rules this replaced fell over: a base at black,
    /// at white, and in the middle. Every one must give a climbing ramp with no duplicate
    /// segments, inside the range, still containing its base.
    #[test]
    fn the_ramp_degrades_sensibly_at_black_white_and_grey() {
        for (name, base) in [
            ("black", Srgb::new(0, 0, 0)),
            ("white", Srgb::new(255, 255, 255)),
            ("mid grey", Srgb::new(128, 128, 128)),
            ("near black", Srgb::new(12, 0, 0)),
            ("near white", Srgb::new(255, 240, 240)),
        ] {
            let values = ramp(base);
            let v = unit_to_hsv(base.to_unit())[2];
            assert_eq!(values.len(), MONOCHROME_STEPS, "{name}");
            assert!(
                values.windows(2).all(|w| w[1] > w[0] + 0.01),
                "{name}: {values:?} repeats or falls"
            );
            assert!(values.iter().all(|v| (0.0..=1.0).contains(v)), "{name}: out of range");
            assert!(
                values.iter().any(|got| (got - v).abs() < 0.005),
                "{name}: {values:?} lost its own base value {v}"
            );
        }
        // Black starts the ramp and white ends it, which is what "dark to light with the
        // base at its own slot" means at the two extremes.
        let black = ramp(Srgb::new(0, 0, 0));
        assert!(black[0].abs() < 0.005, "black must be the first segment");
        let white = ramp(Srgb::new(255, 255, 255));
        assert!((white[MONOCHROME_STEPS - 1] - 1.0).abs() < 0.005, "white must be the last");
    }

    /// A GREY has no hue to rotate, so every rotation answers a grey: the harmonies are
    /// honest about it rather than inventing a colour. Monochromatic still works, which is
    /// the one that means anything there.
    #[test]
    fn an_achromatic_colour_has_no_hue_to_rotate() {
        let grey = Srgb::new(128, 128, 128);
        for h in Harmony::ALL.into_iter().filter(|h| *h != Harmony::Monochromatic) {
            for got in h.swatches(grey).into_iter().skip(1) {
                assert_eq!(got.r, got.g, "{}: a rotated grey is still grey", h.id());
                assert_eq!(got.g, got.b, "{}", h.id());
            }
        }
        let mono = Harmony::Monochromatic.swatches(grey);
        assert!(mono[0] != mono[MONOCHROME_STEPS - 1], "the ramp still spreads a grey");
    }

    /// The labels are USER COPY and the ids are not: one may change, the other may not,
    /// and neither may carry a dash the house style bans.
    #[test]
    fn the_names_are_distinct_and_house_style() {
        let mut ids: Vec<&str> = Harmony::ALL.iter().map(|h| h.id()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "two harmonies share an id");
        for h in Harmony::ALL {
            assert!(!h.label().is_empty(), "{}", h.id());
            for text in [h.label(), h.hint()] {
                assert!(!text.contains('\u{2014}'), "em dash in {}", h.id());
                assert!(!text.contains('\u{2013}'), "en dash in {}", h.id());
            }
            // The HINT is a tooltip, so it is one short sentence and says so (item 23).
            let hint = h.hint();
            assert!(hint.ends_with('.'), "{}: {hint:?} is not a sentence", h.id());
            assert_eq!(hint.matches('.').count(), 1, "{}: more than one sentence", h.id());
            assert!(hint.len() <= 48, "{}: {} characters is not VERY short", h.id(), hint.len());
            assert!(!hint.contains("colour"), "{}: US spelling, like the rest of the app", h.id());
        }
        // The owner's exact list, in the owner's order, with no "Colors" suffix anywhere
        // (items 20 and 29).
        assert_eq!(
            Harmony::ALL.map(|h| h.label()),
            ["Complementary", "Analogous", "Triadic", "Tetradic", "Monochromatic"]
        );
        for h in Harmony::ALL {
            assert!(!h.label().contains("Colors"), "{} kept its suffix", h.id());
        }
    }
}

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

    /// The stepper walks the owner's row order, wraps at both ends, and a full lap of
    /// either sign comes home. That is the whole contract of the ARROW KEYS on the mode
    /// activator (DRAGON-680, and DRAGON-630's first chevron pair).
    #[test]
    fn cycling_walks_the_owned_order_and_wraps() {
        assert_eq!(ColorFormat::Hex.cycled(1), ColorFormat::Rgb);
        assert_eq!(ColorFormat::Hex.cycled(-1), ColorFormat::Lab, "wraps backwards");
        assert_eq!(ColorFormat::Lab.cycled(1), ColorFormat::Hex, "wraps forwards");
        for f in ColorFormat::ALL {
            assert_eq!(f.cycled(ColorFormat::ALL.len() as i32), f, "a full lap is home");
            assert_eq!(f.cycled(0), f);
        }
    }

    /// One step up and one step down are inverses, everywhere in the list. ArrowUp and
    /// ArrowDown on the focused activator are one control with two directions, so a user
    /// who overshoots by one press and corrects with the other has to land exactly where
    /// they were.
    #[test]
    fn the_two_chevrons_undo_each_other() {
        for f in ColorFormat::ALL {
            assert_eq!(f.cycled(1).cycled(-1), f, "{}", f.id());
            assert_eq!(f.cycled(-1).cycled(1), f, "{}", f.id());
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
        let solid = swatch_circle_rgba(c, 255, d, [128, 128, 128], 0.0);
        assert_eq!(px(&solid, d, 0, 0)[3], 0, "corners are outside the circle");
        assert_eq!(px(&solid, d, d / 2, d / 2), [200, 10, 10, 255]);
        let seethrough = swatch_circle_rgba(c, 128, d, [128, 128, 128], 0.0);
        assert_ne!(
            px(&seethrough, d, d / 2, d / 2),
            [200, 10, 10, 255],
            "half alpha mixes the board in"
        );
    }

    /// DRAGON-680: the raster's own disc stops INSIDE the buffer, so the analytic ring the
    /// view stacks over it covers the raster's edge completely.
    ///
    /// This is the property the whole fix rests on, and it is invisible on screen when it
    /// is right (you see the ring) and invisible in the source when it is wrong (you see a
    /// staircase, exactly as the owner did twice). So it is pinned as pixels: the last ring
    /// of the buffer has to be fully transparent, and by at least the mask.
    #[test]
    fn the_raster_stops_inside_the_ring_that_masks_it() {
        let c = Srgb::new(200, 10, 10);
        let rim = [90u8, 90, 90];
        // The size the window really rasters it at, with the mask it really passes.
        let (d, inset) = (48u32, 1.0);
        let buf = swatch_circle_rgba(c, 255, d, rim, inset);
        // The INTERIOR is untouched: a solid colour, fully opaque.
        assert_eq!(px(&buf, d, d / 2, d / 2), [200, 10, 10, 255]);
        // The buffer's own outermost pixels, on the four axes where a circle reaches
        // furthest, are transparent: the raster ends before the ring's outer edge.
        for (x, y) in [(0, d / 2), (d - 1, d / 2), (d / 2, 0), (d / 2, d - 1)] {
            assert_eq!(
                px(&buf, d, x, y)[3],
                0,
                "({x}, {y}) is painted, so the raster reaches the ring's outer edge"
            );
        }
        // With NO mask the same pixels would be painted, or the check above would pass on
        // a raster that never had an edge there to begin with.
        let flush = swatch_circle_rgba(c, 255, d, rim, 0.0);
        assert!(px(&flush, d, 0, d / 2)[3] > 0, "a flush raster does reach the wall");
        // And the fringe guard: a transparent pixel carries the rim's own colour rather
        // than zeroed black, so any resample of this buffer cannot average a dark halo
        // into the edge.
        let corner = px(&buf, d, 0, 0);
        assert_eq!(corner[3], 0, "the corner is outside the disc");
        assert_eq!(&corner[..3], &rim[..], "a transparent pixel must carry the edge colour");
    }

    /// The mask really does eat into the disc rather than into the middle: a bigger mask
    /// leaves a smaller painted disc, and the interior is the same colour either way.
    #[test]
    fn the_mask_only_takes_from_the_edge() {
        let c = Srgb::new(200, 10, 10);
        let rim = [90u8, 90, 90];
        let painted = |inset: f64| -> usize {
            let buf = swatch_circle_rgba(c, 255, 48, rim, inset);
            (0..48u32)
                .flat_map(|y| (0..48u32).map(move |x| (x, y)))
                .filter(|(x, y)| px(&buf, 48, *x, *y)[3] > 0)
                .count()
        };
        assert!(painted(2.0) < painted(1.0), "a wider mask must paint less");
        assert!(painted(1.0) < painted(0.0));
        for inset in [0.0, 1.0, 2.0] {
            let buf = swatch_circle_rgba(c, 255, 48, rim, inset);
            assert_eq!(px(&buf, 48, 24, 24), [200, 10, 10, 255], "inset {inset} moved the fill");
        }
    }

    /// The rim band keeps its WEIGHT at any raster size: it is a fraction of the diameter,
    /// so the band under the ring reads the same whatever resolution the buffer is built
    /// at. (It is hidden by the ring in the shipped window; what it must not do is show a
    /// DIFFERENT colour peeking past the ring's inner edge.)
    #[test]
    fn the_rim_weight_follows_the_raster_size() {
        let c = Srgb::new(200, 10, 10);
        let rim = [90u8, 90, 90];
        // The share of a horizontal radius that is rim-coloured must match across sizes.
        let rim_share = |d: u32| -> f64 {
            let buf = swatch_circle_rgba(c, 255, d, rim, 0.0);
            let n = (0..d)
                .filter(|x| {
                    let p = px(&buf, d, *x, d / 2);
                    p[3] > 0 && p[..3] == rim[..]
                })
                .count();
            n as f64 / d as f64
        };
        let (small, large) = (rim_share(48), rim_share(144));
        assert!(small > 0.0 && large > 0.0, "both sizes draw a rim at all");
        assert!(
            (small - large).abs() < 0.05,
            "the rim is {small:.3} of the disc at 48px and {large:.3} at 144px"
        );
    }

    /// DRAGON-682 item 26: the panel's board is the TRANSPARENCY SLIDER's board, cell for
    /// cell, so the two cannot drift. Compared by sampling both rather than by reading the
    /// constant twice: what matters is that the same pixel is the same grey.
    #[test]
    fn the_panel_board_matches_the_transparency_sliders() {
        let (w, h) = (120u32, 28u32);
        let board = checkerboard_rgba(w, h, 0.0);
        // A fully OPAQUE alpha strip is its own board with the colour laid over it at full
        // strength, so its left edge (alpha 0) is pure board and is what to compare against.
        let strip = alpha_strip_rgba(Srgb::new(0, 0, 0), w, h, 0.0);
        for y in [0, 5, 9, 13, 20, 27] {
            assert_eq!(
                &px(&board, w, 0, y)[..3],
                &px(&strip, w, 0, y)[..3],
                "row {y}: the panel's board and the strip's differ"
            );
        }
        // …and it really is the COARSER cell, not the history swatches': one fine cell
        // along, the board has NOT flipped, which it would have on the old size.
        assert_eq!(
            px(&board, w, 0, 0)[..3],
            px(&board, w, RECENT_CHECKER_CELL, 0)[..3],
            "the board flipped at the history swatches' cell, so it is still the fine one"
        );
        assert_ne!(
            px(&board, w, 0, 0)[..3],
            px(&board, w, CHECKER_CELL, 0)[..3],
            "one SHARED cell along must be the other grey"
        );
        assert_eq!(
            px(&board, w, 0, 0)[..3],
            px(&board, w, 2 * CHECKER_CELL, 0)[..3],
            "two shared cells along is the same grey again"
        );
    }

    /// DRAGON-682: the empty history slot's DOTTED outline. What matters is that it is a
    /// ring (nothing in the middle), that it really dashes, and that it carries the ink it
    /// was handed rather than a baked colour.
    #[test]
    fn the_empty_slot_outline_is_a_dotted_ring() {
        let ink = [90u8, 90, 90];
        let (w, h) = (28u32, 28u32);
        let buf = dotted_outline_rgba(w, h, 6.0, ink);
        // The MIDDLE is empty: this is an outline, not a fill.
        for (x, y) in [(w / 2, h / 2), (w / 2, h / 2 + 3), (w / 3, h / 2)] {
            assert_eq!(px(&buf, w, x, y)[3], 0, "({x}, {y}) is inside the ring");
        }
        // The top edge really alternates: some pixels are painted and some are not.
        let top: Vec<u8> = (0..w).map(|x| px(&buf, w, x, 0)[3]).collect();
        assert!(top.iter().any(|a| *a > 0), "the outline never paints");
        assert!(top.contains(&0), "the outline is solid, not dotted");
        // …with the ink it was given, so the caller's theme colour is what shows.
        let lit = (0..w).map(|x| px(&buf, w, x, 0)).find(|p| p[3] > 0).expect("a lit dot");
        assert_eq!(&lit[..3], &ink[..], "the outline invented its own colour");
        // And a transparent pixel carries that ink too (the fringe guard every raster in
        // this module follows).
        assert_eq!(&px(&buf, w, w / 2, h / 2)[..3], &ink[..]);
    }

    /// The dash PERIOD is the same on the vertical runs as on the horizontal ones, which
    /// is what makes it read as one dotted outline rather than as two different dashes
    /// meeting at the corners.
    #[test]
    fn the_dots_march_at_one_period_on_both_axes() {
        let (w, h) = (28u32, 28u32);
        let buf = dotted_outline_rgba(w, h, 0.0, [90, 90, 90]);
        let lit_run = |vals: Vec<u8>| -> usize { vals.iter().filter(|a| **a > 0).count() };
        let top = lit_run((0..w).map(|x| px(&buf, w, x, 0)[3]).collect());
        let left = lit_run((0..h).map(|y| px(&buf, w, 0, y)[3]).collect());
        // Within one pixel: the two runs share a period but not a phase origin, because
        // each corner is where the x-phase and the y-phase meet (the function's doc says
        // so), and the tie there can hand one run a dot the other starts a pixel later.
        assert!(
            top.abs_diff(left) <= DOT_ON as usize,
            "the two runs paint {top} and {left} dots, which is more than a corner's worth"
        );
        // Roughly the on/off ratio, not every other pixel.
        let want = (w * DOT_ON / (DOT_ON + DOT_OFF)) as usize;
        assert!(top.abs_diff(want) <= 2, "{top} dots along {w}px, expected about {want}");
    }

    /// DRAGON-680's HISTORY swatch: the left half is the colour with no transparency and
    /// the right half is the colour at its real alpha over the checkerboard, so a
    /// translucent entry says both "which colour" and "how transparent" at once.
    #[test]
    fn a_translucent_history_swatch_splits_down_the_middle() {
        let c = Srgb::new(200, 10, 10);
        let (w, h) = (28u32, 28u32);
        let buf = recent_swatch_rgba(c, 128, w, h, 0.0);
        let left = px(&buf, w, w / 4, h / 2);
        let right = px(&buf, w, 3 * w / 4, h / 2);
        assert_eq!(left, [200, 10, 10, 255], "the left half is the colour itself");
        assert_ne!(right, left, "the right half shows the transparency");
        // And the right half really is the board mixing through, not a flat dimming: two
        // rows a checker cell apart differ, because the board's own squares differ. The
        // cell is the HISTORY's own (DRAGON-680 item 26), read from the constant so the
        // test cannot pass by accident on a different board.
        let a = px(&buf, w, 3 * w / 4, 1);
        let b = px(&buf, w, 3 * w / 4, 1 + RECENT_CHECKER_CELL);
        assert_ne!(a, b, "the checkerboard shows through the transparent half");
        // The history's board really is FINER than the shared one, or item 26 changed
        // nothing: a swatch this size holds about seven cells instead of three.
        const {
            assert!(
                RECENT_CHECKER_CELL * 2 <= CHECKER_CELL,
                "item 26: the history's board must be about half the shared one"
            )
        };
        assert!(w / RECENT_CHECKER_CELL >= 6, "the swatch holds too few cells to read");
    }

    /// An OPAQUE entry is a flat fill on both halves: nothing to compare, so no split is
    /// drawn and the swatch looks exactly as it did before alpha entered the history.
    #[test]
    fn an_opaque_history_swatch_has_no_visible_split() {
        let c = Srgb::new(200, 10, 10);
        let (w, h) = (28u32, 28u32);
        let buf = recent_swatch_rgba(c, 255, w, h, 0.0);
        for x in [1, w / 4, w / 2, 3 * w / 4, w - 2] {
            assert_eq!(
                px(&buf, w, x, h / 2),
                [200, 10, 10, 255],
                "x={x} is not the flat colour"
            );
        }
    }

    /// A FULLY transparent entry is pure checkerboard on the right and the pure colour on
    /// the left, which is the extreme the design has to survive: the swatch still says
    /// which colour it is.
    #[test]
    fn a_fully_transparent_entry_still_names_its_colour() {
        let c = Srgb::new(200, 10, 10);
        let (w, h) = (28u32, 28u32);
        let buf = recent_swatch_rgba(c, 0, w, h, 0.0);
        assert_eq!(px(&buf, w, w / 4, h / 2), [200, 10, 10, 255]);
        let right = px(&buf, w, 3 * w / 4, h / 2);
        assert!(
            right[0] == right[1] && right[1] == right[2],
            "a zero-alpha half is the neutral board, got {right:?}"
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
