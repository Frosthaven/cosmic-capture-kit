//! RGBA→NV12 colour conversion (threaded, BT.709 limited range), feeding the
//! hardware encoder half the bytes and skipping ffmpeg's CPU colour conversion.

/// Convert packed RGBA (`w*h*4`) to NV12 (`w*h` Y plane + `w*h/2` interleaved UV),
/// BT.709 limited range, multithreaded over even row bands. `out` must be
/// `w*h*3/2` bytes. Halving the bytes piped to the encoder (and skipping ffmpeg's
/// CPU colour conversion) is the difference between ~60 and 120+ fps at 5K.
pub fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let y_size = w * h;
    let (yp, uvp) = out.split_at_mut(y_size);
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min((h / 2).max(1));
    let band = (h.div_ceil(nthreads) + 1) & !1; // even rows per band (UV is 2x2)
    std::thread::scope(|s| {
        let mut yrest: &mut [u8] = yp;
        let mut uvrest: &mut [u8] = uvp;
        let mut row = 0;
        while row < h {
            let rows = band.min(h - row);
            let (yc, yt) = yrest.split_at_mut(rows * w);
            let (uvc, uvt) = uvrest.split_at_mut(rows / 2 * w);
            yrest = yt;
            uvrest = uvt;
            let start = row;
            s.spawn(move || nv12_band(rgba, w, start, rows, yc, uvc));
            row += rows;
        }
    });
}

// BT.709 limited-range RGB(full)→YCbCr integer coefficients (×256), matching the
// `bt709`/`tv` tags we set on the encode so colours stay accurate for HD.
// The pixel index drives different strides into `src` (×4) and `dst` (×1), so an
// explicit range loop is clearer than zipping mismatched-stride iterators.
#[allow(clippy::needless_range_loop)]
fn nv12_band(rgba: &[u8], w: usize, start: usize, rows: usize, yp: &mut [u8], uvp: &mut [u8]) {
    for r in 0..rows {
        let src = &rgba[(start + r) * w * 4..];
        let dst = &mut yp[r * w..r * w + w];
        for x in 0..w {
            let p = x * 4;
            let (rr, gg, bb) = (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32);
            dst[x] = (((47 * rr + 157 * gg + 16 * bb + 128) >> 8) + 16) as u8;
        }
    }
    // Truncating chroma bounds (DRAGON-277): UV is 2x2-subsampled, so an odd trailing
    // row/column has no complete sample pair. Capture paths feed even dims (the workers
    // evenize — h264/NV12 require it) — but if an odd dim ever slips through, skip the
    // trailing row/column instead of indexing one past the end (the band slices are
    // floor-sized, so the historical full-range loops walked off `dst`/`uvp`).
    for r in (0..(rows & !1)).step_by(2) {
        let src = &rgba[(start + r) * w * 4..];
        let dst = &mut uvp[(r / 2) * w..(r / 2) * w + w];
        let mut o = 0;
        for x in (0..(w & !1)).step_by(2) {
            let p = x * 4;
            let (rr, gg, bb) = (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32);
            let u = (((-26 * rr - 87 * gg + 112 * bb + 128) >> 8) + 128).clamp(0, 255);
            let v = (((112 * rr - 102 * gg - 10 * bb + 128) >> 8) + 128).clamp(0, 255);
            dst[o] = u as u8;
            dst[o + 1] = v as u8;
            o += 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_nv12_solid_colors_are_bt709_limited() {
        // 2x2 frame -> 4 luma bytes + 2 interleaved chroma bytes.
        let mut out = [0u8; 2 * 2 * 3 / 2];

        rgba_to_nv12(&[0u8; 2 * 2 * 4], 2, 2, &mut out); // black (alpha ignored)
        assert_eq!(&out[..4], &[16, 16, 16, 16], "black luma = 16 (limited range)");
        assert_eq!(&out[4..], &[128, 128], "neutral chroma = 128");

        rgba_to_nv12(&[255u8; 2 * 2 * 4], 2, 2, &mut out); // white
        assert_eq!(&out[..4], &[235, 235, 235, 235], "white luma = 235 (limited range)");
    }

    #[test]
    fn rgba_to_nv12_odd_dims_do_not_panic() {
        // DRAGON-277: a hand-drawn odd-sized region used to walk the UV write one past
        // the row slice (`len is 787 but the index is 787`) and take the recording down.
        // Odd dims are truncated at the chroma loops now; the call must simply complete.
        for (w, h) in [(3usize, 3usize), (5, 4), (4, 5), (787, 3)] {
            let rgba = vec![128u8; w * h * 4];
            let mut out = vec![0u8; w * h * 3 / 2];
            rgba_to_nv12(&rgba, w, h, &mut out);
        }
    }
}
