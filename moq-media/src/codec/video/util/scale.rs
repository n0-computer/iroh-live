use anyhow::Result;
use pic_scale::{ImageStore, ImageStoreMut, ResamplingFunction, Scaler as PicScaler, Scaling};

/// Image scaler wrapping pic-scale.
///
/// Optionally scales RGBA frames to target dimensions using bilinear filtering.
/// If no target dimensions are set, frames pass through unchanged.
#[derive(Debug)]
pub(crate) struct Scaler {
    target_width: Option<u32>,
    target_height: Option<u32>,
}

impl Scaler {
    /// Create a new scaler. If `dims` is `None`, frames pass through unscaled.
    pub(crate) fn new(dims: Option<(u32, u32)>) -> Self {
        Self {
            target_width: dims.map(|(w, _)| w),
            target_height: dims.map(|(_, h)| h),
        }
    }

    /// Update the target dimensions.
    pub(crate) fn set_target_dimensions(&mut self, w: u32, h: u32) {
        self.target_width = Some(w);
        self.target_height = Some(h);
    }

    /// Scale an RGBA buffer. Returns `None` if no scaling is needed (pass-through).
    /// Returns `Some((scaled_data, new_w, new_h))` when scaling was performed.
    pub(crate) fn scale_rgba(
        &self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
    ) -> Result<Option<(Vec<u8>, u32, u32)>> {
        let (tw, th) = match (self.target_width, self.target_height) {
            (Some(w), Some(h)) if w != src_w || h != src_h => (w, h),
            _ => return Ok(None),
        };

        let scaler = PicScaler::new(ResamplingFunction::Bilinear);
        let src_store = ImageStore::<u8, 4>::new(src.to_vec(), src_w as usize, src_h as usize)?;
        let mut dst_store = ImageStoreMut::<u8, 4>::alloc(tw as usize, th as usize);
        scaler.resize_rgba(&src_store, &mut dst_store, false)?;

        let out = dst_store.as_bytes().to_vec();
        Ok(Some((out, tw, th)))
    }
}

/// Compute dimensions that fit `(src_w, src_h)` within `(max_w, max_h)`,
/// preserving aspect ratio. Never upscales.
pub(crate) fn fit_within(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w <= max_w && src_h <= max_h {
        return (src_w, src_h);
    }
    let ratio_w = max_w as f64 / src_w as f64;
    let ratio_h = max_h as f64 / src_h as f64;
    let ratio = ratio_w.min(ratio_h);
    let w = ((src_w as f64 * ratio).round() as u32).max(1);
    let h = ((src_h as f64 * ratio).round() as u32).max(1);
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_scaling_passthrough() {
        let scaler = Scaler::new(Some((640, 480)));
        // Same dimensions → None (pass-through)
        let src = vec![128u8; 640 * 480 * 4];
        let result = scaler.scale_rgba(&src, 640, 480).unwrap();
        assert!(result.is_none(), "same dimensions should pass through");
    }

    #[test]
    fn no_target_passthrough() {
        let scaler = Scaler::new(None);
        let src = vec![128u8; 100 * 100 * 4];
        let result = scaler.scale_rgba(&src, 100, 100).unwrap();
        assert!(result.is_none(), "no target should pass through");
    }

    #[test]
    fn downscale() {
        let scaler = Scaler::new(Some((320, 240)));
        let src = vec![200u8; 640 * 480 * 4];
        let result = scaler.scale_rgba(&src, 640, 480).unwrap();
        let (data, w, h) = result.expect("should have scaled");
        assert_eq!(w, 320);
        assert_eq!(h, 240);
        assert_eq!(data.len(), (320 * 240 * 4) as usize);
    }

    #[test]
    fn set_target_dimensions() {
        let mut scaler = Scaler::new(None);
        let src = vec![100u8; 640 * 480 * 4];
        // No target → pass-through
        assert!(scaler.scale_rgba(&src, 640, 480).unwrap().is_none());
        // Set target → should scale
        scaler.set_target_dimensions(320, 240);
        let result = scaler.scale_rgba(&src, 640, 480).unwrap();
        let (_, w, h) = result.expect("should scale after set_target_dimensions");
        assert_eq!(w, 320);
        assert_eq!(h, 240);
    }

    #[test]
    fn small_scale() {
        let scaler = Scaler::new(Some((2, 2)));
        let src = vec![255u8; 4 * 4 * 4];
        let result = scaler.scale_rgba(&src, 4, 4).unwrap();
        let (data, w, h) = result.expect("should scale");
        assert_eq!(w, 2);
        assert_eq!(h, 2);
        assert_eq!(data.len(), 2 * 2 * 4);
    }

    // fit_within tests

    #[test]
    fn fit_within_no_upscale() {
        // Source is smaller than max → should return source dims
        assert_eq!(fit_within(320, 240, 1920, 1080), (320, 240));
    }

    #[test]
    fn fit_within_downscale_width() {
        // 1920×1080 into 640×640 → limited by width → 640×360
        let (w, h) = fit_within(1920, 1080, 640, 640);
        assert_eq!(w, 640);
        assert_eq!(h, 360);
    }

    #[test]
    fn fit_within_downscale_height() {
        // 1920×1080 into 1920×540 → limited by height → 960×540
        let (w, h) = fit_within(1920, 1080, 1920, 540);
        assert_eq!(w, 960);
        assert_eq!(h, 540);
    }

    #[test]
    fn fit_within_exact() {
        // Source matches max exactly → no change
        assert_eq!(fit_within(640, 480, 640, 480), (640, 480));
    }

    #[test]
    fn fit_within_preserves_aspect_ratio() {
        let (w, h) = fit_within(1920, 1080, 640, 480);
        // Aspect ratio of 1920:1080 = 16:9
        // 640/1920 = 0.333..., 480/1080 = 0.444...
        // Limited by width: 640 × 360
        assert_eq!(w, 640);
        assert_eq!(h, 360);
    }
}
