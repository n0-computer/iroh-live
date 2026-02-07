use anyhow::Result;
use yuvutils_rs::{
    YuvChromaSubsampling, YuvPlanarImageMut, YuvRange, YuvStandardMatrix, bgra_to_yuv420,
    rgba_to_yuv420,
};

use crate::av::PixelFormat;

/// YUV 4:2:0 planar image data.
#[derive(Debug)]
pub(crate) struct YuvData {
    pub(crate) y: Vec<u8>,
    pub(crate) u: Vec<u8>,
    pub(crate) v: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) y_stride: u32,
    pub(crate) u_stride: u32,
    pub(crate) v_stride: u32,
}

/// Convert RGBA pixel data to YUV 4:2:0 planar format (BT.601).
pub(crate) fn rgba_to_yuv420_data(src: &[u8], w: u32, h: u32) -> Result<YuvData> {
    let mut planar = YuvPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    rgba_to_yuv420(
        &mut planar,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuvutils_rs::YuvConversionMode::Balanced,
    )?;
    Ok(YuvData {
        y_stride: planar.y_stride,
        u_stride: planar.u_stride,
        v_stride: planar.v_stride,
        y: planar.y_plane.borrow().to_vec(),
        u: planar.u_plane.borrow().to_vec(),
        v: planar.v_plane.borrow().to_vec(),
        width: w,
        height: h,
    })
}

/// Convert BGRA pixel data to YUV 4:2:0 planar format (BT.601).
pub(crate) fn bgra_to_yuv420_data(src: &[u8], w: u32, h: u32) -> Result<YuvData> {
    let mut planar = YuvPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    bgra_to_yuv420(
        &mut planar,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuvutils_rs::YuvConversionMode::Balanced,
    )?;
    Ok(YuvData {
        y_stride: planar.y_stride,
        u_stride: planar.u_stride,
        v_stride: planar.v_stride,
        y: planar.y_plane.borrow().to_vec(),
        u: planar.u_plane.borrow().to_vec(),
        v: planar.v_plane.borrow().to_vec(),
        width: w,
        height: h,
    })
}

/// Dispatch pixel-format-aware RGBA/BGRA → YUV 4:2:0 conversion.
pub(crate) fn pixel_format_to_yuv420(
    src: &[u8],
    w: u32,
    h: u32,
    format: PixelFormat,
) -> Result<YuvData> {
    match format {
        PixelFormat::Rgba => rgba_to_yuv420_data(src, w, h),
        PixelFormat::Bgra => bgra_to_yuv420_data(src, w, h),
    }
}
