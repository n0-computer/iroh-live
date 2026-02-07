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
