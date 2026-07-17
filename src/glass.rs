//! Frosted-glass reproduction for RECONSTRUCTED capture scenes (DRAGON-218).
//!
//! Single-window and windows-only captures re-render their backdrop (the
//! wallpaper file / the windows already composited below) instead of grabbing
//! the live screen, so the compositor's frosted-glass blur is missing: a
//! translucent window that reads as glass live would reveal a SHARP backdrop in
//! the capture. This module reproduces the effect in image space — blur (+
//! grain) the backdrop within the window's rounded footprint before the window
//! is composited over it, so its preserved alpha reveals a frosted backdrop
//! like the live compositor.
//!
//! Fidelity model (cosmic-comp @ 139faf98):
//! - cosmic-comp runs a dual-Kawase blur; its per-strength band table is
//!   `BLUR_PARAMS` (src/backend/render/wayland/blur_effect.rs:47-80). We port
//!   that table verbatim ([`blur_params`]) and collapse each entry to ONE
//!   Gaussian-ish sigma for `image`'s `fast_blur`: every Kawase downsample
//!   level doubles the effective sample footprint, so `passes` half-resolution
//!   passes at pixel `offset` spread roughly like sigma ≈ offset · 2^passes
//!   ([`sigma_for_strength`]). A faithful Kawase port would replace only
//!   [`blur_backdrop`] — the footprint masking, grain, and gating stay as-is.
//! - The shader adds `NOISE = 0.03` grain over the blurred backdrop
//!   (clipped_surface.frag); [`apply_grain`] reproduces it with a deterministic
//!   per-pixel hash (no RNG dependency, stable under tests).
//! - The strength index is `theme.frosted as u8 + 1` (render/mod.rs:730),
//!   clamped to the table — [`sigma_for_strength`] takes the RAW `frosted`
//!   ordinal (0..=13, [`crate::app::theme::GlassConfig::strength_ordinal`]) and
//!   applies that shift itself.
//!
//! Everything here is pure image-in/image-out math (portable, unit-tested);
//! only the Linux composers in `screenshot.rs` call it today, gated on the
//! glass reader ([`crate::app::theme::glass_config`]) returning Some with
//! `frosted_windows` — which is `None` off COSMIC/macOS, keeping every other
//! platform's output byte-identical.

// Only the Linux composers call into this module today; the math itself is
// portable (and unit-tested on every platform), so keep it compiled rather
// than cfg-ing the module out — macOS glass reproduction (DRAGON-166 umbrella)
// would call these same seams.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use image::RgbaImage;

/// One band of cosmic-comp's dual-Kawase parameter table: how many
/// half-resolution passes to run and the per-pass sample offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlurBand {
    pub(crate) passes: u32,
    pub(crate) offset: f32,
}

/// cosmic-comp's `MAX_STEPS` (blur_effect.rs:38): the strength table length.
const MAX_STEPS: usize = 15;

/// Port of cosmic-comp's `BLUR_PARAMS` table generation (blur_effect.rs:47-80
/// @ 139faf98), kept as the same loop rather than a hand-copied value table so
/// it can't drift by transcription error. Each band (min offset, max offset)
/// contributes `ceil(diff/sum · MAX_STEPS)` steps at `passes = band_index + 1`,
/// with the final band truncated so the table is exactly [`MAX_STEPS`] long.
pub(crate) fn blur_params() -> Vec<BlurBand> {
    let mut params = Vec::new();
    let mut remaining_steps = MAX_STEPS as isize;
    // (min offset, max offset); cosmic-comp's third field (extended_radius) only
    // pads its intermediate buffers against edge artifacts — irrelevant here.
    let offsets = [(1.0f32, 2.0f32), (2.0, 3.0), (2.0, 5.0), (3.0, 8.0)];
    let sum = offsets.iter().map(|(min, max)| *max - *min).sum::<f32>();
    for (i, (min, max)) in offsets.into_iter().enumerate() {
        let mut iter_num = ((max - min) / sum * (MAX_STEPS as f32)).ceil() as usize;
        remaining_steps -= iter_num as isize;
        if remaining_steps < 0 {
            iter_num = iter_num.saturating_add_signed(remaining_steps);
        }
        let diff = max - min;
        for j in 1..=iter_num {
            params.push(BlurBand {
                passes: (i + 1) as u32,
                offset: min + (diff / iter_num as f32) * j as f32,
            });
        }
    }
    params
}

/// The Gaussian-equivalent sigma (physical px) for a theme `frosted` strength
/// ordinal (0..=13). cosmic-comp indexes `BLUR_PARAMS[strength.min(MAX_STEPS-1)]`
/// with `strength = frosted as u8 + 1` (render/mod.rs:730); each Kawase
/// downsample doubles the effective sample footprint, so the single-pass
/// approximation is sigma ≈ offset · 2^passes.
pub(crate) fn sigma_for_strength(strength_ordinal: u8) -> f32 {
    let params = blur_params();
    let idx = (strength_ordinal as usize + 1).min(MAX_STEPS - 1);
    let band = params[idx];
    band.offset * (1u32 << band.passes) as f32
}

/// How close a pixel's alpha byte must sit to the theme's blurred-alpha byte to
/// count as "painted at the frosted surface opacity" (± this many 8-bit steps).
/// The empirical separation (DRAGON-218 follow-up) is ~7 steps between a frosted
/// libcosmic window and a user-transparent terminal, so a tolerance of 2 splits
/// them with wide margin on both sides.
const FROSTED_ALPHA_TOL: u8 = 2;

/// The minimum fraction of a captured window's pixels that must sit at the
/// theme's blurred-alpha for the window to read as a frosted libcosmic surface.
/// Frosted apps paint their whole backdrop at that alpha (measured ~85% of the
/// window); an incidental sliver at that exact byte in some other window stays
/// well under this floor.
const FROSTED_MIN_FRAC: f32 = 0.10;

/// Whether a captured window's own alpha says it is a FROSTED libcosmic surface
/// (so its capture should be frosted) rather than a window the user merely made
/// translucent (which the live compositor never blurs).
///
/// The signal (DRAGON-218 follow-up — no client-visible "has a blur region" bit
/// exists on cosmic-comp today, so we infer it from the pixels): a frosted
/// libcosmic window paints its backdrop surface at the theme's `blurred_alpha`
/// constant, an unusual value like 0.9137 for VeryLow. The compositor renders a
/// SINGLE toplevel's buffer WITHOUT the backdrop blur (that's the whole reason
/// DRAGON-218 reproduces it), so that painted alpha survives verbatim in the
/// captured buffer. A window the user set to, say, 0.94 opacity plateaus at a
/// different byte and is left sharp.
///
/// `blurred_alpha` is the launch-time [`crate::app::theme::GlassConfig::alpha`]
/// (the surface opacity the active strength selects). Returns `false` for an
/// essentially-opaque window (nothing to reveal) and for any window whose alpha
/// plateau lands away from `blurred_alpha`.
///
/// Failure modes (documented, accepted for the interim): a frosted libcosmic
/// window whose visible content is mostly opaque widgets can fall under
/// [`FROSTED_MIN_FRAC`] and stay sharp (but then there is little glass to reveal
/// anyway); a non-libcosmic window a user coincidentally set to the theme's exact
/// `blurred_alpha` would be frosted (astronomically unlikely for the specific
/// constant; `CCK_NO_GLASS=1` is the global escape hatch either way).
pub(crate) fn looks_frosted(img: &RgbaImage, blurred_alpha: f32) -> bool {
    if img.width() == 0 || img.height() == 0 {
        return false;
    }
    let target = (blurred_alpha.clamp(0.0, 1.0) * 255.0).round() as i32;
    let (lo, hi) = (target - FROSTED_ALPHA_TOL as i32, target + FROSTED_ALPHA_TOL as i32);
    let mut near = 0u64;
    for p in img.pixels() {
        let a = p.0[3] as i32;
        if a >= lo && a <= hi {
            near += 1;
        }
    }
    let total = img.width() as u64 * img.height() as u64;
    near as f32 / total as f32 >= FROSTED_MIN_FRAC
}

/// The grain amplitude cosmic-comp's shader mixes over the blurred backdrop
/// (`NOISE` in clipped_surface.frag).
const GRAIN: f32 = 0.03;

/// A cheap deterministic per-pixel hash → [0, 1). Wang-style integer mix over
/// the pixel coordinates: stable across runs (testable) and free of any RNG
/// dependency; visually indistinguishable from the shader's `fract(sin(...))`
/// white noise at 3% amplitude.
fn hash01(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    (h >> 8) as f32 / ((1u32 << 24) as f32)
}

/// Mix ±[`GRAIN`] white noise into the RGB channels (alpha untouched) — the
/// shader's grain, in image space.
fn apply_grain(img: &mut RgbaImage) {
    for (x, y, p) in img.enumerate_pixels_mut() {
        let n = (hash01(x, y) - 0.5) * 2.0 * GRAIN * 255.0;
        for c in &mut p.0[..3] {
            *c = (*c as f32 + n).clamp(0.0, 255.0) as u8;
        }
    }
}

/// Frost the backdrop under a window: blur `canvas` (+ grain) WITHIN the
/// window's rounded footprint — the rect at (`x`, `y`) sized `w`×`h` (physical
/// px, canvas coordinates), corners rounded at `radius` — leaving everything
/// outside the footprint untouched (the compositor only blurs BEHIND the
/// window; padding margins and shadow halos stay sharp). Call it on the
/// backdrop right before compositing the translucent window over it.
///
/// The blur is computed on a crop padded by 3·sigma so pixels just inside the
/// footprint edge blend the same neighbourhood they would in a full-canvas
/// blur (no edge darkening at the footprint boundary). Off-canvas parts of the
/// footprint are clipped away.
pub(crate) fn frost_region(
    canvas: &mut RgbaImage,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    radius: u32,
    sigma: f32,
) {
    if w == 0 || h == 0 || sigma <= 0.0 {
        return;
    }
    let (cw, ch) = (canvas.width() as i64, canvas.height() as i64);
    // Footprint clipped to the canvas.
    let fx0 = x.max(0);
    let fy0 = y.max(0);
    let fx1 = (x + w as i64).min(cw);
    let fy1 = (y + h as i64).min(ch);
    if fx1 <= fx0 || fy1 <= fy0 {
        return;
    }
    // Blur-source crop: the clipped footprint padded by 3·sigma (clipped again).
    let pad = (sigma * 3.0).ceil() as i64;
    let bx0 = (fx0 - pad).max(0);
    let by0 = (fy0 - pad).max(0);
    let bx1 = (fx1 + pad).min(cw);
    let by1 = (fy1 + pad).min(ch);
    let crop = image::imageops::crop_imm(
        canvas,
        bx0 as u32,
        by0 as u32,
        (bx1 - bx0) as u32,
        (by1 - by0) as u32,
    )
    .to_image();
    let mut blurred = blur_backdrop(&crop, sigma);
    apply_grain(&mut blurred);
    // The rounded-footprint mask, in FULL footprint coordinates (so a corner
    // clipped off-canvas keeps its curve where it re-enters).
    let rf = radius.min(w / 2).min(h / 2) as f32;
    for py in fy0..fy1 {
        for px in fx0..fx1 {
            // Coverage of this pixel inside the rounded rect (1 inside, 0
            // outside, 1px anti-aliased edge) — same maths as
            // `compose::round_corners`, evaluated over the footprint.
            let lx = (px - x) as f32 + 0.5;
            let ly = (py - y) as f32 + 0.5;
            let dx = (rf - lx).max(lx - (w as f32 - rf)).max(0.0);
            let dy = (rf - ly).max(ly - (h as f32 - rf)).max(0.0);
            let cov = if dx > 0.0 && dy > 0.0 {
                (rf - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if cov <= 0.0 {
                continue;
            }
            let b = *blurred.get_pixel((px - bx0) as u32, (py - by0) as u32);
            let dst = canvas.get_pixel_mut(px as u32, py as u32);
            if cov >= 1.0 {
                *dst = b;
            } else {
                for i in 0..4 {
                    dst[i] = (b[i] as f32 * cov + dst[i] as f32 * (1.0 - cov)).round() as u8;
                }
            }
        }
    }
}

/// The blur seam: ONE call approximating cosmic-comp's dual-Kawase output for
/// the [`sigma_for_strength`] sigma. `fast_blur` (the box-blur approximation
/// `compose::with_shadow` already leans on) — a faithful Kawase port would
/// replace exactly this function.
fn blur_backdrop(img: &RgbaImage, sigma: f32) -> RgbaImage {
    image::imageops::fast_blur(img, sigma)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_params_table_matches_cosmic_comp_shape() {
        // MAX_STEPS entries; passes ascend band by band exactly as cosmic-comp
        // computes them: 2 steps at 1 pass, 2 at 2, 5 at 3, 6 at 4 (the last
        // band truncated from ceil(7.5)=8 by the remaining-steps clamp).
        let p = blur_params();
        assert_eq!(p.len(), 15);
        let passes: Vec<u32> = p.iter().map(|b| b.passes).collect();
        assert_eq!(passes, [1, 1, 2, 2, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4]);
        // Band boundary offsets land on the band maxima.
        assert!((p[1].offset - 2.0).abs() < 1e-5);
        assert!((p[3].offset - 3.0).abs() < 1e-5);
        assert!((p[8].offset - 5.0).abs() < 1e-5);
        assert!((p[14].offset - 8.0).abs() < 1e-5);
        // First entries of each band interpolate up from the band minimum.
        assert!((p[0].offset - 1.5).abs() < 1e-5);
        assert!((p[2].offset - 2.5).abs() < 1e-5);
        assert!((p[4].offset - 2.6).abs() < 1e-5);
        assert!((p[9].offset - (3.0 + 5.0 / 6.0)).abs() < 1e-5);
    }

    #[test]
    fn sigma_maps_strength_through_the_plus_one_index() {
        // strength index = ordinal + 1 (render/mod.rs:730), sigma = offset · 2^passes.
        let p = blur_params();
        // The live machine's VeryLow (ordinal 2) → params[3] = (2 passes, 3.0).
        assert!((sigma_for_strength(2) - 3.0 * 4.0).abs() < 1e-4);
        // Weakest (ExtremelyLow, ordinal 0) → params[1] = (1, 2.0) → 4.
        assert!((sigma_for_strength(0) - 2.0 * 2.0).abs() < 1e-4);
        // Strongest (ExtremelyHigh2, ordinal 13) clamps to params[14] = (4, 8.0) → 128.
        assert!((sigma_for_strength(13) - 8.0 * 16.0).abs() < 1e-4);
        // Monotone non-decreasing across the whole strength range.
        let sig: Vec<f32> = (0..=13).map(sigma_for_strength).collect();
        assert!(sig.windows(2).all(|w| w[1] >= w[0]), "{sig:?}");
        // And consistent with the raw table.
        assert!(
            (sigma_for_strength(5) - p[6].offset * (1 << p[6].passes) as f32).abs() < 1e-4
        );
    }

    #[test]
    fn grain_hash_is_deterministic_and_uniformish() {
        assert_eq!(hash01(3, 7), hash01(3, 7));
        assert_ne!(hash01(3, 7), hash01(7, 3));
        let vals: Vec<f32> = (0..1000).map(|i| hash01(i % 40, i / 40)).collect();
        assert!(vals.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        assert!((mean - 0.5).abs() < 0.05, "mean {mean}");
    }

    #[test]
    fn frost_region_blurs_only_inside_the_rounded_footprint() {
        // A hard black/white edge: after frosting a centered footprint, pixels
        // inside are mixed (blurred), pixels outside remain untouched.
        let mut img = RgbaImage::from_fn(80, 80, |x, _| {
            if x < 40 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([255, 255, 255, 255]) }
        });
        let before = img.clone();
        frost_region(&mut img, 20, 20, 40, 40, 8, 4.0);
        // Inside, on the edge: blurred to a mid tone (grain is only ±3%).
        let inside = img.get_pixel(40, 40);
        assert!(inside[0] > 40 && inside[0] < 215, "edge should blur, got {}", inside[0]);
        // Outside the footprint: byte-identical.
        assert_eq!(img.get_pixel(10, 40), before.get_pixel(10, 40));
        assert_eq!(img.get_pixel(70, 40), before.get_pixel(70, 40));
        assert_eq!(img.get_pixel(40, 10), before.get_pixel(40, 10));
        // The footprint corner (outside the rounded radius) is also untouched:
        // the pixel at the very corner of the rect lies outside the r=8 curve.
        assert_eq!(img.get_pixel(20, 20), before.get_pixel(20, 20));
    }

    #[test]
    fn frost_region_clips_offcanvas_footprints_safely() {
        let mut img = RgbaImage::from_pixel(30, 30, image::Rgba([128, 128, 128, 255]));
        // Footprint hanging off every edge — must not panic, and must touch
        // only the on-canvas overlap.
        frost_region(&mut img, -10, -10, 100, 100, 12, 3.0);
        frost_region(&mut img, 25, 25, 40, 40, 4, 2.0);
        // Fully off-canvas / degenerate inputs are no-ops.
        let before = img.clone();
        frost_region(&mut img, -50, 0, 20, 20, 4, 2.0);
        frost_region(&mut img, 0, 0, 0, 10, 4, 2.0);
        frost_region(&mut img, 0, 0, 10, 10, 4, 0.0);
        assert_eq!(img, before);
    }

    #[test]
    fn looks_frosted_splits_frosted_libcosmic_from_user_transparency() {
        // The live evidence (DRAGON-218 follow-up, 2026-07-15): a frosted VeryLow
        // theme's blurred_alpha ≈ 0.91385 → byte 233. COSMIC Files (frosted
        // libcosmic) plateaus at byte 233 over ~85% of the window; wezterm (user
        // transparency) plateaus at byte 240 (0.9412). Reproduce both shapes.
        let blurred_alpha = 0.91385_f32; // byte 233
        // Frosted: 85% at the theme alpha byte, 15% opaque widgets.
        let frosted = RgbaImage::from_fn(100, 100, |_, y| {
            if y < 85 { image::Rgba([50, 50, 50, 233]) } else { image::Rgba([50, 50, 50, 255]) }
        });
        assert!(looks_frosted(&frosted, blurred_alpha));
        // wezterm: 83% at byte 240 (7 steps off), 17% opaque — no plateau at the
        // theme alpha, so it stays sharp.
        let user_transparent = RgbaImage::from_fn(100, 100, |_, y| {
            if y < 83 { image::Rgba([10, 10, 10, 240]) } else { image::Rgba([200, 200, 200, 255]) }
        });
        assert!(!looks_frosted(&user_transparent, blurred_alpha));
    }

    #[test]
    fn looks_frosted_rejects_opaque_and_empty_and_sub_threshold() {
        let blurred_alpha = 0.91385_f32; // byte 233
        // Fully opaque window: nothing to reveal.
        let opaque = RgbaImage::from_pixel(60, 60, image::Rgba([120, 120, 120, 255]));
        assert!(!looks_frosted(&opaque, blurred_alpha));
        // Only a 5% sliver at the theme alpha (below FROSTED_MIN_FRAC = 10%).
        let sliver = RgbaImage::from_fn(100, 100, |_, y| {
            if y < 5 { image::Rgba([0, 0, 0, 233]) } else { image::Rgba([0, 0, 0, 255]) }
        });
        assert!(!looks_frosted(&sliver, blurred_alpha));
        // Degenerate image → false (never panics).
        assert!(!looks_frosted(&RgbaImage::new(0, 0), blurred_alpha));
        // The ±2-step tolerance accepts 231..=235 but not 236/230.
        let edge_hi = RgbaImage::from_pixel(40, 40, image::Rgba([0, 0, 0, 235]));
        assert!(looks_frosted(&edge_hi, blurred_alpha));
        let edge_out = RgbaImage::from_pixel(40, 40, image::Rgba([0, 0, 0, 236]));
        assert!(!looks_frosted(&edge_out, blurred_alpha));
    }

    #[test]
    fn grain_amplitude_stays_within_three_percent() {
        let mut img = RgbaImage::from_pixel(64, 64, image::Rgba([128, 128, 128, 255]));
        apply_grain(&mut img);
        let max_dev = img.pixels().map(|p| (p[0] as i32 - 128).unsigned_abs()).max().unwrap();
        // ±0.03 · 255 ≈ ±7.65 → deviations stay ≤ 8, and SOME deviation exists.
        assert!(max_dev <= 8, "max deviation {max_dev}");
        assert!(max_dev >= 1, "grain should actually perturb pixels");
        // Alpha untouched.
        assert!(img.pixels().all(|p| p[3] == 255));
    }
}
