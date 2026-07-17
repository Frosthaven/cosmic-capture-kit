//! The neural VAD wrapper: an `earshot` detector fed by 48->16 kHz (3:1) decimation and
//! 256-sample reframing. earshot wants f32 in [-1, 1] (it debug-asserts this), so samples
//! are clamped before the detector.

/// Neural voice-activity detector state: the earshot detector plus the 48->16 kHz
/// decimation accumulator and the 256-sample reframing buffer.
pub(crate) struct Vad {
    det: Box<earshot::Detector>,
    // earshot 48->16 kHz decimation + 256-sample reframing
    decim: [f32; 3],
    decim_n: usize,
    buf: Vec<f32>,
    last: f32,
}

impl Vad {
    pub(crate) fn new() -> Self {
        Self {
            det: earshot::Detector::default_boxed(),
            decim: [0.0; 3],
            decim_n: 0,
            buf: Vec::with_capacity(512),
            last: 0.0,
        }
    }

    /// The most recent voice-activity probability (0..1).
    pub(crate) fn last(&self) -> f32 {
        self.last
    }

    /// Decimate the cleaned signal 48->16 kHz and run earshot on each full 256-sample
    /// (16 ms) frame, updating `last`. earshot wants f32 in [-1, 1].
    pub(crate) fn push_samples(&mut self, cleaned: &[f32]) {
        for &s in cleaned.iter() {
            // earshot requires samples in [-1, 1] (it debug-asserts this); AGC2 can push
            // peaks past 1.0, so clamp before feeding the VAD.
            self.decim[self.decim_n] = s.clamp(-1.0, 1.0);
            self.decim_n += 1;
            if self.decim_n == 3 {
                let avg = (self.decim[0] + self.decim[1] + self.decim[2]) / 3.0;
                self.buf.push(avg);
                self.decim_n = 0;
            }
        }
        while self.buf.len() >= 256 {
            self.last = self.det.predict_f32(&self.buf[..256]);
            self.buf.drain(..256);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests exercise only the 48->16 kHz decimation, 256-sample reframing, and
    // the [-1, 1] clamp. They stay below 768 input samples (256 decimated) so the
    // neural `predict_f32` path is never reached — no model inference is run.

    #[test]
    fn new_starts_empty_with_zero_last() {
        let v = Vad::new();
        assert_eq!(v.last(), 0.0);
        assert_eq!(v.buf.len(), 0);
        assert_eq!(v.decim_n, 0);
    }

    #[test]
    fn decimation_averages_three_to_one() {
        let mut v = Vad::new();
        v.push_samples(&[0.3, 0.6, 0.9]); // mean 0.6
        assert_eq!(v.buf.len(), 1);
        assert!((v.buf[0] - 0.6).abs() < 1e-6);
        assert_eq!(v.decim_n, 0); // group consumed
        assert_eq!(v.last(), 0.0); // no full 256-sample frame -> no inference
    }

    #[test]
    fn clamps_out_of_range_before_decimation() {
        let mut v = Vad::new();
        // +5 and -5 clamp to +1 and -1; mean with 0.0 is 0.0.
        v.push_samples(&[5.0, -5.0, 0.0]);
        assert_eq!(v.buf.len(), 1);
        assert!(v.buf[0].abs() < 1e-6);
    }

    #[test]
    fn clamp_visible_in_partial_group() {
        let mut v = Vad::new();
        v.push_samples(&[2.0, -3.0]); // only 2 of 3 -> stays in the decim accumulator
        assert_eq!(v.buf.len(), 0);
        assert_eq!(v.decim_n, 2);
        assert_eq!(v.decim[0], 1.0); // 2.0 clamped to 1.0
        assert_eq!(v.decim[1], -1.0); // -3.0 clamped to -1.0
    }

    #[test]
    fn decim_accumulator_carries_across_calls() {
        let mut v = Vad::new();
        v.push_samples(&[0.2, 0.4]); // decim_n = 2
        assert_eq!(v.buf.len(), 0);
        v.push_samples(&[0.6, 0.1, 0.1]); // completes (0.2+0.4+0.6)/3 = 0.4, then 2 leftover
        assert_eq!(v.buf.len(), 1);
        assert!((v.buf[0] - 0.4).abs() < 1e-6);
        assert_eq!(v.decim_n, 2);
    }

    #[test]
    fn buffers_full_groups_without_inference_under_256() {
        let mut v = Vad::new();
        let samples = vec![0.5f32; 300]; // 100 decimated samples, < 256 -> no predict
        v.push_samples(&samples);
        assert_eq!(v.buf.len(), 100);
        assert!((v.buf[0] - 0.5).abs() < 1e-6);
        assert_eq!(v.last(), 0.0);
    }
}
