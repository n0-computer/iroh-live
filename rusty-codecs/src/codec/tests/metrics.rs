/// Per-channel and combined image quality metrics for codec testing.
///
/// Uses standard codec conformance metrics (PSNR, MAE, max error) as defined by ITU-T.
/// Computes PSNR, mean absolute error, and max error per RGB channel,
/// enabling detection of colorspace conversion bugs (e.g. BT.601/709 mismatch,
/// range clamping, chroma subsampling artifacts).
#[derive(Debug, Clone)]
pub(super) struct FrameMetrics {
    /// PSNR per channel `[R, G, B]` in dB. `f64::INFINITY` means identical.
    pub psnr: [f64; 3],
    /// Combined PSNR across all channels.
    pub psnr_combined: f64,
    /// Maximum absolute error per channel.
    pub max_error: [u8; 3],
    /// Mean absolute error per channel.
    pub mae: [f64; 3],
}

impl FrameMetrics {
    /// Minimum PSNR across channels — the weakest link.
    #[allow(dead_code, reason = "useful for future test assertions")]
    pub(super) fn min_psnr(&self) -> f64 {
        self.psnr.iter().copied().fold(f64::INFINITY, f64::min)
    }

    /// Maximum absolute error across any channel.
    pub(super) fn max_error_any(&self) -> u8 {
        *self.max_error.iter().max().unwrap()
    }

    /// Detects per-channel color bias (one channel significantly worse than others).
    ///
    /// Returns `Some((channel_name, deficit_db))` if any channel's PSNR is more
    /// than `threshold_db` below the best channel. This catches tinting issues
    /// like VAAPI green shift.
    pub(super) fn color_bias(&self, threshold_db: f64) -> Option<(&'static str, f64)> {
        let names = ["R", "G", "B"];
        let best = self
            .psnr
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(f64::NEG_INFINITY, f64::max);
        if !best.is_finite() {
            return None;
        }
        for (i, &p) in self.psnr.iter().enumerate() {
            let deficit = best - p;
            if deficit > threshold_db {
                return Some((names[i], deficit));
            }
        }
        None
    }
}

impl std::fmt::Display for FrameMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PSNR[R={:.1} G={:.1} B={:.1} combined={:.1}] max_err[R={} G={} B={}] MAE[R={:.1} G={:.1} B={:.1}]",
            self.psnr[0],
            self.psnr[1],
            self.psnr[2],
            self.psnr_combined,
            self.max_error[0],
            self.max_error[1],
            self.max_error[2],
            self.mae[0],
            self.mae[1],
            self.mae[2],
        )
    }
}

/// Computes quality metrics between original and decoded RGBA pixel buffers.
///
/// Both slices must have identical length (`w * h * 4` bytes, RGBA order).
/// Alpha channel is ignored in quality calculations.
pub(super) fn compute_metrics(original: &[u8], decoded: &[u8]) -> FrameMetrics {
    assert_eq!(
        original.len(),
        decoded.len(),
        "buffer length mismatch: {} vs {}",
        original.len(),
        decoded.len()
    );
    let pixel_count = original.len() / 4;
    assert!(pixel_count > 0, "empty image");

    let mut sum_sq = [0.0f64; 3];
    let mut sum_abs = [0.0f64; 3];
    let mut max_err = [0u8; 3];

    for i in 0..pixel_count {
        let base = i * 4;
        for ch in 0..3 {
            let diff = original[base + ch].abs_diff(decoded[base + ch]);
            sum_sq[ch] += (diff as f64) * (diff as f64);
            sum_abs[ch] += diff as f64;
            max_err[ch] = max_err[ch].max(diff);
        }
    }

    let n = pixel_count as f64;
    let psnr = std::array::from_fn(|ch| {
        let mse = sum_sq[ch] / n;
        if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    });

    let total_mse = (sum_sq[0] + sum_sq[1] + sum_sq[2]) / (3.0 * n);
    let psnr_combined = if total_mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / total_mse).log10()
    };

    let mae = std::array::from_fn(|ch| sum_abs[ch] / n);

    FrameMetrics {
        psnr,
        psnr_combined,
        max_error: max_err,
        mae,
    }
}

/// Compares two decoded frames pixel-by-pixel and returns per-channel metrics.
///
/// Useful for cross-backend comparison: encode once, decode with two different
/// decoders, diff the outputs to find backend-specific color deviations.
pub(super) fn compare_frames(frame_a: &[u8], frame_b: &[u8]) -> FrameMetrics {
    compute_metrics(frame_a, frame_b)
}

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn identical_frames() {
        let frame = vec![128u8; 64 * 64 * 4];
        let m = compute_metrics(&frame, &frame);
        assert!(m.psnr_combined.is_infinite());
        assert_eq!(m.max_error_any(), 0);
        assert_eq!(m.mae, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn known_psnr() {
        // Single pixel, R differs by 10, G/B identical.
        let orig = vec![100, 200, 50, 255];
        let decoded = vec![110, 200, 50, 255];
        let m = compute_metrics(&orig, &decoded);
        // MSE_R = 100, PSNR_R = 10*log10(65025/100) = 28.13
        assert!((m.psnr[0] - 28.13).abs() < 0.1);
        assert!(m.psnr[1].is_infinite()); // G identical
        assert!(m.psnr[2].is_infinite()); // B identical
        assert_eq!(m.max_error, [10, 0, 0]);
    }

    #[test]
    fn color_bias_detection() {
        let orig = vec![100, 100, 100, 255];
        // Green channel is way off, R/B are close.
        let decoded = vec![101, 130, 101, 255];
        let m = compute_metrics(&orig, &decoded);
        let bias = m.color_bias(5.0);
        assert!(bias.is_some());
        assert_eq!(bias.unwrap().0, "G");
    }

    #[test]
    fn no_bias_when_uniform_error() {
        let orig = vec![100, 100, 100, 255];
        let decoded = vec![110, 110, 110, 255];
        let m = compute_metrics(&orig, &decoded);
        assert!(m.color_bias(5.0).is_none());
    }
}
