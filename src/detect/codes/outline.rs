//! The rxing-based detection and the orientation-following outline geometry.
//!
//! `rxing`'s decoder reports only sparse points (three QR finder centres, the two
//! endpoints of a 1D scan line, …), so the bulk of this module reconstructs each code's
//! true four-corner footprint from those points plus the luma buffer: QR quads from
//! finder/alignment patterns, and 1D barcodes from the measured bar axis + flooded stripe
//! region. The `rxing` crate types stay confined to this module.

use crate::detect::luma;

/// Run rxing's multi-format detector (TryHarder on) over the region's luma buffer.
pub(super) fn decode(lum: &[u8], iw: u32, ih: u32) -> Vec<rxing::RXingResult> {
    let mut hints = rxing::DecodeHints {
        TryHarder: Some(true),
        ..Default::default()
    };
    rxing::helpers::detect_multiple_in_luma_with_hints(lum.to_vec(), iw, ih, &mut hints)
        .unwrap_or_default()
}

/// Whether a barcode format is a 1D (linear) symbology — rxing reports only the two
/// endpoints of the scan line for these, so we estimate the bar height ourselves.
fn is_1d(f: &rxing::BarcodeFormat) -> bool {
    use rxing::BarcodeFormat::*;
    matches!(
        f,
        CODABAR
            | CODE_39
            | CODE_93
            | CODE_128
            | EAN_8
            | EAN_13
            | ITF
            | RSS_14
            | RSS_EXPANDED
            | TELEPEN
            | UPC_A
            | UPC_E
            | UPC_EAN_EXTENSION
            | DXFilmEdge
    )
}

/// Whether a format is a QR variant — rxing reports the three finder-pattern centres
/// (inset from the symbol edge), which we reconstruct into a full quad.
pub(super) fn is_qr(f: &rxing::BarcodeFormat) -> bool {
    use rxing::BarcodeFormat::*;
    matches!(f, QR_CODE | MICRO_QR_CODE | RECTANGULAR_MICRO_QR_CODE)
}

/// Build the orientation-following 4-corner outline (global logical coords) for a
/// detected code from its rxing result points + the luma buffer it was decoded from.
/// `sx,sy` map buffer px → logical px.
#[allow(clippy::too_many_arguments)]
pub(super) fn code_poly(
    format: &rxing::BarcodeFormat,
    pts: &[rxing::Point],
    lum: &[u8],
    iw: u32,
    ih: u32,
    rx: i32,
    ry: i32,
    sx: f32,
    sy: f32,
) -> [(i32, i32); 4] {
    // 1D barcodes report only the two endpoints of a single scan line (which may sit
    // anywhere in the bar height, often near the bottom). Measure the true bar extent
    // from the image in buffer space, then map to logical coords.
    if is_1d(format) || pts.len() == 2 {
        let p0 = (pts[0].x, pts[0].y);
        let p1 = (pts[pts.len() - 1].x, pts[pts.len() - 1].y);
        let quad = scanline_quad(p0, p1, lum, iw, ih);
        return quad.map(|(x, y)| {
            (
                (rx as f32 + x / sx).round() as i32,
                (ry as f32 + y / sy).round() as i32,
            )
        });
    }
    // 2D codes: map the points to logical coords and build the quad from real corners.
    let g = |p: &rxing::Point| (rx as f32 + p.x / sx, ry as f32 + p.y / sy);
    let gp: Vec<(f32, f32)> = pts.iter().map(g).collect();
    let quad = if is_qr(format) && gp.len() >= 4 {
        // 3 finder centres + the bottom-right alignment pattern: four real, perspective-
        // following corners. Grow outward since they sit a few modules inside the edge.
        expand_quad(order_quad(&gp[..4]), 1.16)
    } else if is_qr(format) && gp.len() == 3 {
        qr_quad(gp[0], gp[1], gp[2])
    } else if gp.len() == 4 {
        // Data Matrix / Aztec / PDF417 report the true symbol corners — no expansion.
        order_quad(&gp)
    } else if gp.len() == 3 {
        qr_quad(gp[0], gp[1], gp[2])
    } else {
        bbox_quad(&gp)
    };
    quad.map(|(x, y)| (x.round() as i32, y.round() as i32))
}

/// Estimate a 1D barcode's true scan axis (the direction across the bars) from the
/// bar-edge gradients sampled along rxing's scan line. The bars are parallel stripes, so
/// their luma gradient points across them, i.e. along the scan axis; averaging those
/// gradient directions (in doubled angle, so dark→light and light→dark edges reinforce
/// rather than cancel) recovers the barcode's rotation even though rxing's own endpoints
/// are horizontal. Returns a unit vector oriented roughly `p0`→`p1`; falls back to the
/// rxing direction when there isn't enough edge signal.
fn bar_axis(lum: &[u8], iw: u32, ih: u32, p0: (f32, f32), p1: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len = dx.hypot(dy).max(1.0);
    let (u0x, u0y) = (dx / len, dy / len);
    let (n0x, n0y) = (-u0y, u0x); // perpendicular to rxing's scan line
    // Sample Sobel over a 2D band centred on the scan line — not just the single scan
    // row, which can graze the barcode's top/bottom edge and bias the estimate. Across
    // the band the many interior bar edges (gradient = scan axis) outweigh the two long
    // edges, so the doubled-angle average lands on the true rotation.
    let band = (len * 0.3).clamp(12.0, 240.0);
    let along = (len / 160.0).clamp(1.0, 4.0);
    let perp = (band / 24.0).clamp(1.0, 6.0);
    let (mut s2, mut c2) = (0.0f32, 0.0f32);
    let mut t = 0.0;
    while t <= len {
        let (bx, by) = (p0.0 + u0x * t, p0.1 + u0y * t);
        let mut o = -band;
        while o <= band {
            let cx = (bx + n0x * o).round() as i32;
            let cy = (by + n0y * o).round() as i32;
            // 3x3 Sobel; a bar edge's gradient points across the bars (= scan axis).
            let (gx, gy) = luma::sobel_at(lum, iw, ih, cx, cy);
            let mag = gx.hypot(gy);
            if mag >= 40.0 {
                let a = gy.atan2(gx);
                c2 += mag * (2.0 * a).cos();
                s2 += mag * (2.0 * a).sin();
            }
            o += perp;
        }
        t += along;
    }
    luma::doubled_angle_average(s2, c2, (u0x, u0y))
}

/// Dominant bar axis (across the bars) from the gradients of the already-mapped barcode
/// cells. Sampling only inside the region avoids the off-barcode pollution that biases
/// the initial band estimate, so curved/steep real-world symbols get a much truer angle.
/// Doubled-angle averaging makes opposite edges reinforce; oriented to match `fallback`.
fn region_axis(
    lum: &[u8],
    iw: u32,
    ih: u32,
    region: &[(f32, f32)],
    fallback: (f32, f32),
) -> (f32, f32) {
    let (mut s2, mut c2) = (0.0f32, 0.0f32);
    for &(fx, fy) in region {
        let (cx, cy) = (fx.round() as i32, fy.round() as i32);
        let (gx, gy) = luma::sobel_at(lum, iw, ih, cx, cy);
        let mag = gx.hypot(gy);
        if mag >= 40.0 {
            let a = gy.atan2(gx);
            c2 += mag * (2.0 * a).cos();
            s2 += mag * (2.0 * a).sin();
        }
    }
    luma::doubled_angle_average(s2, c2, fallback)
}

/// Oriented rectangle around a 1D barcode. rxing only reports the two endpoints of one
/// scan line, which doesn't capture the symbol's rotation or extent (and grazes its
/// edge on partial reads). Instead we recover the bar axis from the edge gradients, then
/// flood the connected striped region outward from the scan line and bound *that* along
/// the axis — so the box traces the barcode's real footprint, including curved/steep
/// symbols. Falls back to walking out from the scan line when the region can't be mapped.
fn scanline_quad(p0: (f32, f32), p1: (f32, f32), lum: &[u8], iw: u32, ih: u32) -> [(f32, f32); 4] {
    // Rough scan axis from a band around rxing's scan line — good enough to seed the
    // region grow, but polluted on real photos by off-barcode pixels (the band straddles
    // the can/label/background).
    let (ux0, uy0) = bar_axis(lum, iw, ih, p0, p1);

    // Map the connected striped region from the scan-line seed.
    let mut region = map_barcode_region(lum, iw, ih, p0, p1, (ux0, uy0));
    // Refine the axis from gradients *inside the mapped region only* (no off-barcode
    // pollution), then re-map along it for a tighter footprint.
    let (ux, uy) = if region.len() >= 24 {
        let a = region_axis(lum, iw, ih, &region, (ux0, uy0));
        let r2 = map_barcode_region(lum, iw, ih, p0, p1, a);
        if r2.len() >= 24 {
            region = r2;
        }
        a
    } else {
        (ux0, uy0)
    };
    let (nx, ny) = (-uy, ux); // unit perpendicular (runs along the bars)

    // Bound the mapped region, oriented along the refined bar axis.
    if region.len() >= 24 {
        let (mut umin, mut umax) = (f32::MAX, f32::MIN);
        let (mut vmin, mut vmax) = (f32::MAX, f32::MIN);
        for &(x, y) in &region {
            let (pu, pv) = (x * ux + y * uy, x * nx + y * ny);
            umin = umin.min(pu);
            umax = umax.max(pu);
            vmin = vmin.min(pv);
            vmax = vmax.max(pv);
        }
        // A small margin so the outline sits just outside the mapped bars.
        let mu = ((umax - umin) * 0.02).clamp(1.0, 6.0);
        let mv = ((vmax - vmin) * 0.04).clamp(1.0, 6.0);
        let (umin, umax, vmin, vmax) = (umin - mu, umax + mu, vmin - mv, vmax + mv);
        // Reconstruct the four corners from (along-axis, across-axis) back to buffer px.
        let pt = |pu: f32, pv: f32| (pu * ux + pv * nx, pu * uy + pv * ny);
        return [
            pt(umin, vmin),
            pt(umax, vmin),
            pt(umax, vmax),
            pt(umin, vmax),
        ];
    }

    // Fallback (low contrast / region too small): re-seed the endpoints onto the bar
    // axis, extend to the guard bars, then walk perpendicular until the bars fade.
    let m = ((p0.0 + p1.0) * 0.5, (p0.1 + p1.1) * 0.5);
    let proj = |p: (f32, f32)| {
        let d = (p.0 - m.0) * ux + (p.1 - m.1) * uy;
        (m.0 + ux * d, m.1 + uy * d)
    };
    let (p0, p1) = (proj(p0), proj(p1));
    let module = min_run_along(lum, iw, ih, p0, p1).max(1.0);
    let gap = (module * 8.0).clamp(6.0, 40.0);
    let q0 = extend_endpoint(lum, iw, ih, p0, (-ux, -uy), gap);
    let q1 = extend_endpoint(lum, iw, ih, p1, (ux, uy), gap);
    let len = ((q1.0 - q0.0).hypot(q1.1 - q0.1)).max(1.0);
    let reference = transitions_along(lum, iw, ih, q0, q1);
    let threshold = (reference as f32 * 0.5).max(6.0);
    let cap = (iw + ih) as f32;
    let reach = |sign: f32| -> f32 {
        let mut last = 0.0;
        let mut misses = 0;
        let mut k = 1.0;
        while k <= cap {
            let a = (q0.0 + nx * sign * k, q0.1 + ny * sign * k);
            let b = (q1.0 + nx * sign * k, q1.1 + ny * sign * k);
            if transitions_along(lum, iw, ih, a, b) as f32 >= threshold {
                last = k;
                misses = 0;
            } else {
                misses += 1;
                if misses >= 3 {
                    break;
                }
            }
            k += 1.0;
        }
        last
    };
    let (mut up, mut down) = (reach(-1.0), reach(1.0));
    if up + down < 8.0 {
        let hh = (len * 0.18).clamp(12.0, len * 0.5);
        up = hh;
        down = hh;
    } else {
        up += 2.0;
        down += 2.0;
    }
    [
        (q0.0 - nx * up, q0.1 - ny * up),
        (q1.0 - nx * up, q1.1 - ny * up),
        (q1.0 + nx * down, q1.1 + ny * down),
        (q0.0 + nx * down, q0.1 + ny * down),
    ]
}

/// Flood the connected striped region of a barcode outward from the scan-line seed.
/// "Striped" = a short window along the bar axis crosses many dark↔light transitions,
/// which holds throughout a barcode (even between bars) but not in the quiet zone, digit
/// row, or background — so the grow stays on this one symbol, stops at its true edges,
/// and follows curvature. Returns the covered cell centres (buffer px). 8-connected on a
/// grid sized to the module width; a hard cap guards against runaway growth.
fn map_barcode_region(
    lum: &[u8],
    iw: u32,
    ih: u32,
    p0: (f32, f32),
    p1: (f32, f32),
    u: (f32, f32),
) -> Vec<(f32, f32)> {
    let (ux, uy) = u;
    let module = min_run_along(lum, iw, ih, p0, p1).max(1.0);
    let scan_len = ((p1.0 - p0.0).hypot(p1.1 - p0.1)).max(1.0);
    // The test window must span many bars so wide bars/spaces don't dip it below
    // threshold mid-symbol (which would stall the grow). Size it to the scan length.
    let half = (scan_len * 0.10).clamp(module * 10.0, scan_len * 0.4);
    let step = (module * 1.5).clamp(2.0, 8.0); // grid resolution
    let density = |c: (f32, f32)| -> u32 {
        let a = (c.0 - ux * half, c.1 - uy * half);
        let b = (c.0 + ux * half, c.1 + uy * half);
        transitions_along(lum, iw, ih, a, b)
    };
    let seed = ((p0.0 + p1.0) * 0.5, (p0.1 + p1.1) * 0.5);
    let reference = density(seed).max(4);
    let thresh = (reference as f32 * 0.4).max(3.0);
    let key = |c: (f32, f32)| ((c.0 / step).round() as i32, (c.1 / step).round() as i32);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut stack = vec![seed];
    seen.insert(key(seed));
    while let Some(c) = stack.pop() {
        if (density(c) as f32) < thresh {
            continue;
        }
        out.push(c);
        if out.len() > 100_000 {
            break; // safety
        }
        for (dx, dy) in [
            (step, 0.0),
            (-step, 0.0),
            (0.0, step),
            (0.0, -step),
            (step, step),
            (step, -step),
            (-step, step),
            (-step, -step),
        ] {
            let nb = (c.0 + dx, c.1 + dy);
            if nb.0 < 0.0 || nb.1 < 0.0 || nb.0 >= iw as f32 || nb.1 >= ih as f32 {
                continue;
            }
            if seen.insert(key(nb)) {
                stack.push(nb);
            }
        }
    }
    out
}

/// Length (px) of the narrowest same-colour run sampled along `a`→`b` — an estimate of
/// the barcode's one-module width, used to size the quiet-zone gap.
fn min_run_along(lum: &[u8], iw: u32, ih: u32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let steps = dx.hypot(dy).round() as i32;
    if steps <= 0 {
        return 1.0;
    }
    let (mut min_run, mut run, mut dark, mut have) = (f32::MAX, 0.0f32, false, false);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (a.0 + dx * t).round() as i32;
        let y = (a.1 + dy * t).round() as i32;
        if x < 0 || y < 0 || x >= iw as i32 || y >= ih as i32 {
            continue;
        }
        let v = lum[(y as u32 * iw + x as u32) as usize];
        let now = if v < 100 {
            true
        } else if v > 156 {
            false
        } else {
            dark
        };
        if have && now != dark {
            if run > 0.0 {
                min_run = min_run.min(run);
            }
            run = 0.0;
        }
        run += 1.0;
        dark = now;
        have = true;
    }
    if min_run == f32::MAX { 1.0 } else { min_run }
}

/// Walk outward from a scan-line end in unit direction `dir`, returning the position of
/// the last dark pixel before a `gap`-wide stretch of light (the quiet zone) — i.e. the
/// outer edge of the outermost bar.
fn extend_endpoint(
    lum: &[u8],
    iw: u32,
    ih: u32,
    from: (f32, f32),
    dir: (f32, f32),
    gap: f32,
) -> (f32, f32) {
    let mut last_dark = 0.0;
    let mut k = 0.0;
    loop {
        let x = (from.0 + dir.0 * k).round() as i32;
        let y = (from.1 + dir.1 * k).round() as i32;
        if x < 0 || y < 0 || x >= iw as i32 || y >= ih as i32 {
            break;
        }
        if lum[(y as u32 * iw + x as u32) as usize] < 100 {
            last_dark = k;
        }
        if k - last_dark > gap {
            break;
        }
        k += 1.0;
        if k > (iw + ih) as f32 {
            break; // safety
        }
    }
    (from.0 + dir.0 * last_dark, from.1 + dir.1 * last_dark)
}

/// Count dark↔light transitions sampling the segment `a`→`b` of the luma buffer
/// (hysteresis band avoids counting noise). A barcode row has many; a quiet zone or a
/// line of text has far fewer — which is how the height walk knows where the bars end.
fn transitions_along(lum: &[u8], iw: u32, ih: u32, a: (f32, f32), b: (f32, f32)) -> u32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let steps = dx.hypot(dy).round() as i32;
    if steps <= 0 {
        return 0;
    }
    let (mut count, mut dark, mut have) = (0u32, false, false);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (a.0 + dx * t).round() as i32;
        let y = (a.1 + dy * t).round() as i32;
        if x < 0 || y < 0 || x >= iw as i32 || y >= ih as i32 {
            continue;
        }
        let v = lum[(y as u32 * iw + x as u32) as usize];
        let now = if v < 100 {
            true
        } else if v > 156 {
            false
        } else {
            dark
        };
        if have && now != dark {
            count += 1;
        }
        dark = now;
        have = true;
    }
    count
}

/// Full quad from a QR code's three finder-pattern centres (used when no alignment
/// pattern is reported). The right-angle corner is the top-left finder; the opposite
/// corner is reconstructed (affine), then the quad is grown outward.
fn qr_quad(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> [(f32, f32); 4] {
    let p = [a, b, c];
    // The finder whose two edges are closest to perpendicular is the top-left.
    let mut tl = 0;
    let mut best = f32::MAX;
    for i in 0..3 {
        let (j, k) = ((i + 1) % 3, (i + 2) % 3);
        let e1 = (p[j].0 - p[i].0, p[j].1 - p[i].1);
        let e2 = (p[k].0 - p[i].0, p[k].1 - p[i].1);
        let l1 = e1.0.hypot(e1.1).max(1e-3);
        let l2 = e2.0.hypot(e2.1).max(1e-3);
        let cos = ((e1.0 * e2.0 + e1.1 * e2.1) / (l1 * l2)).abs();
        if cos < best {
            best = cos;
            tl = i;
        }
    }
    let (tlp, o1, o2) = (p[tl], p[(tl + 1) % 3], p[(tl + 2) % 3]);
    let br = (o1.0 + o2.0 - tlp.0, o1.1 + o2.1 - tlp.1); // corner opposite top-left
    expand_quad([tlp, o1, br, o2], 1.16) // perimeter order: TL -> side -> BR -> side
}

/// Grow a quad outward from its centroid by factor `k` (so finder/alignment centres,
/// which sit inside the symbol edge, reach the true corners).
fn expand_quad(mut quad: [(f32, f32); 4], k: f32) -> [(f32, f32); 4] {
    let cx = quad.iter().map(|q| q.0).sum::<f32>() / 4.0;
    let cy = quad.iter().map(|q| q.1).sum::<f32>() / 4.0;
    for q in &mut quad {
        q.0 = cx + (q.0 - cx) * k;
        q.1 = cy + (q.1 - cy) * k;
    }
    quad
}

/// Order four corner points (Data Matrix / Aztec / PDF417 / a QR's finder+alignment
/// quad) into a non-self-crossing quad by sorting around their centroid.
fn order_quad(pts: &[(f32, f32)]) -> [(f32, f32); 4] {
    let cx = pts.iter().map(|p| p.0).sum::<f32>() / pts.len() as f32;
    let cy = pts.iter().map(|p| p.1).sum::<f32>() / pts.len() as f32;
    let mut v: Vec<(f32, (f32, f32))> = pts
        .iter()
        .map(|&q| ((q.1 - cy).atan2(q.0 - cx), q))
        .collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    [v[0].1, v[1].1, v[2].1, v[3].1]
}

/// Axis-aligned fallback quad (unexpected point counts).
fn bbox_quad(pts: &[(f32, f32)]) -> [(f32, f32); 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxing::BarcodeFormat;

    fn approx(a: (f32, f32), b: (f32, f32)) {
        assert!(
            (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3,
            "{a:?} != {b:?}"
        );
    }

    #[rstest::rstest]
    #[case(BarcodeFormat::QR_CODE, true)]
    #[case(BarcodeFormat::MICRO_QR_CODE, true)]
    #[case(BarcodeFormat::RECTANGULAR_MICRO_QR_CODE, true)]
    #[case(BarcodeFormat::CODE_128, false)]
    #[case(BarcodeFormat::DATA_MATRIX, false)]
    #[case(BarcodeFormat::AZTEC, false)]
    fn is_qr_cases(#[case] f: BarcodeFormat, #[case] want: bool) {
        assert_eq!(is_qr(&f), want);
    }

    #[rstest::rstest]
    #[case(BarcodeFormat::CODE_128, true)]
    #[case(BarcodeFormat::EAN_13, true)]
    #[case(BarcodeFormat::UPC_A, true)]
    #[case(BarcodeFormat::CODE_39, true)]
    #[case(BarcodeFormat::QR_CODE, false)]
    #[case(BarcodeFormat::DATA_MATRIX, false)]
    #[case(BarcodeFormat::AZTEC, false)]
    #[case(BarcodeFormat::PDF_417, false)]
    fn is_1d_cases(#[case] f: BarcodeFormat, #[case] want: bool) {
        assert_eq!(is_1d(&f), want);
    }

    #[test]
    fn bbox_quad_bounds_scattered_points() {
        let q = bbox_quad(&[(3.0, 1.0), (0.0, 4.0), (7.0, 2.0)]);
        // x in [0,7], y in [1,4]; corners TL,TR,BR,BL.
        assert_eq!(q, [(0.0, 1.0), (7.0, 1.0), (7.0, 4.0), (0.0, 4.0)]);
    }

    #[test]
    fn expand_quad_identity_at_factor_one() {
        let q = [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        assert_eq!(expand_quad(q, 1.0), q);
    }

    #[test]
    fn expand_quad_doubles_about_centroid() {
        // Unit-ish square, centroid (1,1); k=2 -> each corner twice as far from centre.
        let q = [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        let e = expand_quad(q, 2.0);
        approx(e[0], (-1.0, -1.0));
        approx(e[1], (3.0, -1.0));
        approx(e[2], (3.0, 3.0));
        approx(e[3], (-1.0, 3.0));
    }

    #[test]
    fn order_quad_sorts_scrambled_corners_by_angle() {
        // Square corners fed out of order come back in CCW angular order starting from
        // the most negative atan2 (the top-left corner relative to the centroid).
        let scrambled = [(2.0, 2.0), (0.0, 0.0), (0.0, 2.0), (2.0, 0.0)];
        let ordered = order_quad(&scrambled);
        assert_eq!(
            ordered,
            [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]
        );
    }

    #[test]
    fn qr_quad_reconstructs_and_expands_about_centre() {
        // Three finder centres of an axis-aligned QR: TL at origin, plus the two
        // adjacent corners. The right-angle corner (TL) is detected, the opposite
        // corner reconstructed to (10,10), then the quad grown 1.16x about (5,5).
        let q = qr_quad((0.0, 0.0), (10.0, 0.0), (0.0, 10.0));
        // Centroid is preserved by the symmetric expansion.
        let cx = q.iter().map(|p| p.0).sum::<f32>() / 4.0;
        let cy = q.iter().map(|p| p.1).sum::<f32>() / 4.0;
        approx((cx, cy), (5.0, 5.0));
        // Pre-expansion quad is [(0,0),(10,0),(10,10),(0,10)]; expand 1.16 about (5,5).
        approx(q[0], (5.0 + (0.0 - 5.0) * 1.16, 5.0 + (0.0 - 5.0) * 1.16));
        approx(q[2], (5.0 + (10.0 - 5.0) * 1.16, 5.0 + (10.0 - 5.0) * 1.16));
    }
}
