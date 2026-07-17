//! Pure CPU pixel-format helpers: bytes-per-pixel, crop-and-convert, and the
//! SPA/DRM fourcc mapping. Shared between the PipeWire consumers.

use pipewire::spa;

/// Bytes per source pixel for the formats we request. `None` = unsupported.
pub(crate) fn bytes_per_pixel(fmt: spa::param::video::VideoFormat) -> Option<usize> {
    use spa::param::video::VideoFormat as V;
    match fmt {
        V::BGRx | V::RGBx | V::BGRA | V::RGBA => Some(4),
        V::RGB => Some(3),
        _ => None,
    }
}

/// Convert a cropped sub-rect of `src` (with `stride` bytes/row, pixel layout
/// `fmt`) into tightly-packed RGBA in `out` (`cw*ch*4`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_crop(
    src: &[u8],
    stride: usize,
    fmt: spa::param::video::VideoFormat,
    bpp: usize,
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
    out: &mut [u8],
) {
    use spa::param::video::VideoFormat as V;
    // Index of the (R, G, B) bytes within a source pixel; alpha is forced opaque.
    let (ri, gi, bi) = match fmt {
        V::RGBx | V::RGBA | V::RGB => (0, 1, 2),
        V::BGRx | V::BGRA => (2, 1, 0),
        _ => (0, 1, 2),
    };
    let (cw, ch) = (cw as usize, ch as usize);
    let row_bytes = cw * 4;
    // Crop + colour-convert is per-row independent, and on a large region it's the main
    // CPU cost of a frame — so split the output rows across cores (like the NV12 pass).
    let nthreads = if cw * ch < 80_000 {
        1 // small region — thread-spawn overhead isn't worth it
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(ch.max(1))
    };
    let band = ch.div_ceil(nthreads).max(1);
    std::thread::scope(|s| {
        let mut rest: &mut [u8] = out;
        let mut y0 = 0usize;
        while y0 < ch {
            let rows = band.min(ch - y0);
            let (chunk, tail) = rest.split_at_mut(rows * row_bytes);
            rest = tail;
            let start = y0;
            s.spawn(move || {
                for r in 0..rows {
                    let srow = (cy as usize + start + r) * stride + cx as usize * bpp;
                    let drow = r * row_bytes;
                    for x in 0..cw {
                        let sidx = srow + x * bpp;
                        let didx = drow + x * 4;
                        if sidx + bpp > src.len() || didx + 4 > chunk.len() {
                            break;
                        }
                        chunk[didx] = src[sidx + ri];
                        chunk[didx + 1] = src[sidx + gi];
                        chunk[didx + 2] = src[sidx + bi];
                        chunk[didx + 3] = 255;
                    }
                }
            });
            y0 += rows;
        }
    });
}

/// DRM `FourCC` for a PipeWire video format (only the single-plane packed RGB cases
/// a compositor offers for screen capture). `None` = we don't map it (caller falls
/// back). The DRM names encode little-endian byte order, the reverse of the SPA name.
#[cfg(feature = "zero-copy")]
pub(crate) fn drm_fourcc(fmt: spa::param::video::VideoFormat) -> Option<u32> {
    use spa::param::video::VideoFormat as V;
    // fourcc_code(a,b,c,d) = a | b<<8 | c<<16 | d<<24
    let cc = |s: &[u8; 4]| s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24;
    Some(match fmt {
        V::BGRx => cc(b"XR24"), // DRM_FORMAT_XRGB8888
        V::RGBx => cc(b"XB24"), // DRM_FORMAT_XBGR8888
        V::BGRA => cc(b"AR24"), // DRM_FORMAT_ARGB8888
        V::RGBA => cc(b"AB24"), // DRM_FORMAT_ABGR8888
        _ => return None,
    })
}
