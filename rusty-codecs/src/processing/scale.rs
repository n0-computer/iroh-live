use anyhow::Result;
use pic_scale::{ImageStore, ImageStoreMut, ResamplingFunction, Scaler as PicScaler, Scaling};

/// Controls how source frames are scaled relative to the encoder target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScaleMode {
    /// Fits within the target box preserving aspect ratio. Never upscales —
    /// if the source is smaller than the target, the source dimensions are
    /// used as-is. This is the default.
    #[default]
    Fit,
    /// Scales to the exact target dimensions, potentially distorting
    /// the aspect ratio.
    Stretch,
    /// Scales to cover the target box preserving aspect ratio, then crops
    /// the excess. Both output axes match the target exactly.
    Cover,
}

impl ScaleMode {
    /// Computes the actual encode dimensions for the given source and target.
    ///
    /// The result is always even in both axes (required by most codecs).
    pub fn resolve(&self, source: (u32, u32), target: (u32, u32)) -> (u32, u32) {
        let (w, h) = match self {
            Self::Fit => fit_within(source.0, source.1, target.0, target.1),
            Self::Stretch | Self::Cover => target,
        };
        // Ensure even dimensions (H.264, AV1, VAAPI all require this).
        (w & !1, h & !1)
    }
}

/// Image scaler wrapping pic-scale.
///
/// Optionally scales RGBA frames to target dimensions using bilinear filtering.
/// If no target dimensions are set, frames pass through unchanged. Reuses the
/// destination buffer across calls when target dimensions are stable.
#[derive(Debug)]
pub struct Scaler {
    target_width: Option<u32>,
    target_height: Option<u32>,
    scaler: PicScaler,
    /// Reusable destination buffer: `(width, height, data)`.
    dst_buf: Option<(u32, u32, Vec<u8>)>,
}

impl Scaler {
    /// Creates a new scaler. If `dims` is `None`, frames pass through unscaled.
    pub fn new(dims: Option<(u32, u32)>) -> Self {
        Self {
            target_width: dims.map(|(w, _)| w),
            target_height: dims.map(|(_, h)| h),
            scaler: PicScaler::new(ResamplingFunction::Bilinear),
            dst_buf: None,
        }
    }

    /// Update the target dimensions.
    pub fn set_target_dimensions(&mut self, w: u32, h: u32) {
        self.target_width = Some(w);
        self.target_height = Some(h);
    }

    /// Scales an RGBA buffer to the target dimensions.
    ///
    /// Returns `None` if no scaling is needed (pass-through).
    /// Returns `Some((scaled_data, new_w, new_h))` when scaling was performed.
    /// Reuses the destination buffer when target dimensions are stable.
    pub fn scale_rgba(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
    ) -> Result<Option<(Vec<u8>, u32, u32)>> {
        let (tw, th) = match (self.target_width, self.target_height) {
            (Some(w), Some(h)) if w != src_w || h != src_h => (w, h),
            _ => return Ok(None),
        };

        let expected = (tw as usize) * (th as usize) * 4;
        // Reuse or allocate the destination buffer.
        let mut buf = match self.dst_buf.take() {
            Some((bw, bh, buf)) if bw == tw && bh == th => buf,
            _ => vec![0u8; expected],
        };

        let src_store = ImageStore::<u8, 4>::from_slice(src, src_w as usize, src_h as usize)?;
        let mut dst_store = ImageStoreMut::<u8, 4>::from_slice(&mut buf, tw as usize, th as usize)?;
        self.scaler.resize_rgba(&src_store, &mut dst_store, false)?;
        drop(dst_store);

        let out = buf.clone();
        self.dst_buf = Some((tw, th, buf));
        Ok(Some((out, tw, th)))
    }

    /// Scales an RGBA buffer using cover mode: scale up preserving aspect ratio
    /// to fully cover the target, then center-crop to the exact target size.
    ///
    /// Returns `None` if no scaling is needed (pass-through).
    pub fn scale_cover_rgba(
        &self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
    ) -> Result<Option<(Vec<u8>, u32, u32)>> {
        let (tw, th) = match (self.target_width, self.target_height) {
            (Some(w), Some(h)) if w != src_w || h != src_h => (w, h),
            _ => return Ok(None),
        };

        // Scale to cover: pick the larger ratio so both axes >= target.
        let ratio_w = tw as f64 / src_w as f64;
        let ratio_h = th as f64 / src_h as f64;
        let ratio = ratio_w.max(ratio_h);
        let scaled_w = (src_w as f64 * ratio).ceil() as u32;
        let scaled_h = (src_h as f64 * ratio).ceil() as u32;

        // Scale to intermediate (cover) dimensions.
        let src_store = ImageStore::<u8, 4>::from_slice(src, src_w as usize, src_h as usize)?;
        let mut mid_store = ImageStoreMut::<u8, 4>::alloc(scaled_w as usize, scaled_h as usize);
        self.scaler.resize_rgba(&src_store, &mut mid_store, false)?;

        // Center-crop to target.
        if scaled_w == tw && scaled_h == th {
            let out = mid_store.as_bytes().to_vec();
            return Ok(Some((out, tw, th)));
        }

        let mid_data = mid_store.as_bytes();
        let mid_stride = scaled_w as usize * 4;
        let x_off = ((scaled_w - tw) / 2) as usize;
        let y_off = ((scaled_h - th) / 2) as usize;
        let mut out = vec![0u8; (tw * th * 4) as usize];
        let row_bytes = tw as usize * 4;
        for row in 0..th as usize {
            let src_start = (y_off + row) * mid_stride + x_off * 4;
            let dst_start = row * row_bytes;
            out[dst_start..dst_start + row_bytes]
                .copy_from_slice(&mid_data[src_start..src_start + row_bytes]);
        }
        Ok(Some((out, tw, th)))
    }
}

/// Compute dimensions that fit `(src_w, src_h)` within `(max_w, max_h)`,
/// preserving aspect ratio. Never upscales.
pub fn fit_within(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
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

/// Scales a frame to the encoder's target dimensions if the frame doesn't
/// already match.
///
/// Shared by all encoder backends. Returns the frame unchanged when no
/// scaling is needed.
pub fn scale_frame_if_needed(
    scaler: &mut Scaler,
    scale_mode: ScaleMode,
    target_w: u32,
    target_h: u32,
    frame: crate::format::VideoFrame,
) -> anyhow::Result<crate::format::VideoFrame> {
    let [fw, fh] = frame.dimensions;
    if fw == target_w && fh == target_h {
        return Ok(frame);
    }
    let (tw, th) = scale_mode.resolve((fw, fh), (target_w, target_h));
    if tw == fw && th == fh {
        return Ok(frame);
    }
    scaler.set_target_dimensions(tw, th);
    let img = frame.rgba_image();
    let scaled = if scale_mode == ScaleMode::Cover {
        scaler.scale_cover_rgba(img.as_raw(), fw, fh)?
    } else {
        scaler.scale_rgba(img.as_raw(), fw, fh)?
    };
    match scaled {
        Some((data, w, h)) => Ok(crate::format::VideoFrame::new_rgba(
            data.into(),
            w,
            h,
            frame.timestamp,
        )),
        None => Ok(frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_scaling_passthrough() {
        let mut scaler = Scaler::new(Some((640, 480)));
        // Same dimensions → None (pass-through)
        let src = vec![128u8; 640 * 480 * 4];
        let result = scaler.scale_rgba(&src, 640, 480).unwrap();
        assert!(result.is_none(), "same dimensions should pass through");
    }

    #[test]
    fn no_target_passthrough() {
        let mut scaler = Scaler::new(None);
        let src = vec![128u8; 100 * 100 * 4];
        let result = scaler.scale_rgba(&src, 100, 100).unwrap();
        assert!(result.is_none(), "no target should pass through");
    }

    #[test]
    fn downscale() {
        let mut scaler = Scaler::new(Some((320, 240)));
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
        let mut scaler = Scaler::new(Some((2, 2)));
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

    /// Simulates the encoder thread scaling logic:
    /// given source dimensions and encoder target, compute fit_within then scale.
    fn encoder_scale_scenario(src_w: u32, src_h: u32, enc_w: u32, enc_h: u32) -> (u32, u32) {
        let target = fit_within(src_w, src_h, enc_w, enc_h);
        let mut scaler = Scaler::new(Some(target));
        let src = vec![128u8; (src_w * src_h * 4) as usize];
        match scaler.scale_rgba(&src, src_w, src_h).unwrap() {
            Some((data, w, h)) => {
                assert_eq!(data.len(), (w * h * 4) as usize);
                (w, h)
            }
            None => (src_w, src_h),
        }
    }

    #[test]
    fn encoder_scale_4k_camera_to_1080p() {
        // Logitech BRIO 4K (3840×2160) → 1080p encoder (1920×1080)
        let (w, h) = encoder_scale_scenario(3840, 2160, 1920, 1080);
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
    }

    #[test]
    fn encoder_scale_ultrawide_screen_to_1080p() {
        // Pro Display XDR (3008×1692) → 1080p encoder (1920×1080)
        let (w, h) = encoder_scale_scenario(3008, 1692, 1920, 1080);
        assert!(w <= 1920);
        assert!(h <= 1080);
        // 3008:1692 ≈ 16:9, so both dimensions scale down proportionally
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
    }

    #[test]
    fn encoder_scale_1080p_camera_to_720p() {
        // 1080p camera → 720p encoder
        let (w, h) = encoder_scale_scenario(1920, 1080, 1280, 720);
        assert_eq!(w, 1280);
        assert_eq!(h, 720);
    }

    #[test]
    fn encoder_scale_already_fits() {
        // 720p camera → 1080p encoder: no scaling needed
        let (w, h) = encoder_scale_scenario(1280, 720, 1920, 1080);
        assert_eq!(w, 1280);
        assert_eq!(h, 720);
    }
}
