//! Metering math: short-term RMS, linear->dBFS, and the meters' normalized `(db+60)/60`
//! scale.

/// Short-term RMS of a frame.
pub(crate) fn rms(buf: &[f32]) -> f32 {
    let s: f32 = buf.iter().map(|&v| v * v).sum();
    (s / buf.len() as f32).sqrt()
}

pub(crate) fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-6 { -90.0 } else { 20.0 * x.log10() }
}

pub(crate) fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear 0..1 level -> dBFS -> the meters' `(db+60)/60` normalized scale.
pub(crate) fn level_to_meter(p: f32) -> f32 {
    if p <= 1e-6 {
        0.0
    } else {
        ((20.0 * p.min(1.0).log10() + 60.0) / 60.0).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_maps_dbfs_landmarks_to_meter_scale() {
        assert_eq!(level_to_meter(1.0), 1.0); // 0 dBFS -> top of the meter
        assert_eq!(level_to_meter(0.0), 0.0); // silence -> floor
        assert_eq!(level_to_meter(0.000_5), 0.0); // below the -60 dBFS floor clamps to 0
        // -6 dBFS (linear 0.5) lands at ~0.90, the recorded "too loud" line.
        assert!(
            (level_to_meter(0.5) - 0.90).abs() < 0.01,
            "level_to_meter(0.5) = {}",
            level_to_meter(0.5)
        );
    }

    #[test]
    fn lin_to_db_landmarks() {
        assert!((lin_to_db(1.0) - 0.0).abs() < 1e-4);
        assert!((lin_to_db(0.1) - (-20.0)).abs() < 1e-3);
        assert_eq!(lin_to_db(0.0), -90.0); // silence floor
    }
}
