use anyhow::Result;
use yuvutils_rs::{
    YuvChromaSubsampling, YuvPlanarImage, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420, rgba_to_yuv420, yuv420_to_rgba,
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

/// Convert YUV 4:2:0 planar data back to RGBA format (BT.601).
pub(crate) fn yuv420_to_rgba_data(yuv: &YuvData) -> Result<Vec<u8>> {
    let planar = YuvPlanarImage {
        y_plane: &yuv.y,
        y_stride: yuv.y_stride,
        u_plane: &yuv.u,
        u_stride: yuv.u_stride,
        v_plane: &yuv.v,
        v_stride: yuv.v_stride,
        width: yuv.width,
        height: yuv.height,
    };
    let rgba_stride = yuv.width * 4;
    let mut rgba = vec![0u8; (yuv.width * yuv.height * 4) as usize];
    yuv420_to_rgba(
        &planar,
        &mut rgba,
        rgba_stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
    )?;
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_yuv_basic() {
        // 4×4 solid red image
        let w = 4u32;
        let h = 4u32;
        let rgba: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let yuv = rgba_to_yuv420_data(&rgba, w, h).unwrap();
        assert_eq!(yuv.width, w);
        assert_eq!(yuv.height, h);
        // BT.601 limited range: pure red → Y≈81, U≈90, V≈240
        // Check Y plane average is in the right ballpark
        let y_avg: f64 = yuv.y.iter().map(|&v| v as f64).sum::<f64>() / yuv.y.len() as f64;
        assert!((y_avg - 81.0).abs() < 10.0, "Y average {y_avg} not near 81");
    }

    #[test]
    fn bgra_to_yuv_basic() {
        // 4×4 solid blue image (BGRA: B=255, G=0, R=0, A=255)
        let w = 4u32;
        let h = 4u32;
        let bgra: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let yuv = bgra_to_yuv420_data(&bgra, w, h).unwrap();
        assert_eq!(yuv.width, w);
        assert_eq!(yuv.height, h);
        // BT.601 limited range: pure blue → Y≈41
        let y_avg: f64 = yuv.y.iter().map(|&v| v as f64).sum::<f64>() / yuv.y.len() as f64;
        assert!((y_avg - 41.0).abs() < 10.0, "Y average {y_avg} not near 41");
    }

    #[test]
    fn pixel_format_dispatch() {
        let w = 2u32;
        let h = 2u32;
        let data: Vec<u8> = vec![128; (w * h * 4) as usize];
        // Both formats should succeed
        pixel_format_to_yuv420(&data, w, h, PixelFormat::Rgba).unwrap();
        pixel_format_to_yuv420(&data, w, h, PixelFormat::Bgra).unwrap();
    }

    #[test]
    fn odd_dimensions() {
        // YUV420 requires even chroma planes; verify odd dims don't panic
        let w = 7u32;
        let h = 5u32;
        let rgba: Vec<u8> = vec![100; (w * h * 4) as usize];
        let yuv = rgba_to_yuv420_data(&rgba, w, h).unwrap();
        assert_eq!(yuv.width, w);
        assert_eq!(yuv.height, h);
    }

    #[test]
    fn large_frame() {
        let w = 1920u32;
        let h = 1080u32;
        let rgba: Vec<u8> = vec![64; (w * h * 4) as usize];
        let yuv = rgba_to_yuv420_data(&rgba, w, h).unwrap();
        assert_eq!(yuv.width, w);
        assert_eq!(yuv.height, h);
    }

    #[test]
    fn yuv_plane_sizes() {
        let w = 8u32;
        let h = 6u32;
        let rgba: Vec<u8> = vec![200; (w * h * 4) as usize];
        let yuv = rgba_to_yuv420_data(&rgba, w, h).unwrap();
        // Y plane: one byte per pixel (but may have stride padding)
        assert!(yuv.y.len() >= (w * h) as usize);
        // U and V planes: quarter the pixel count for 4:2:0 (with possible stride padding)
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);
        assert!(yuv.u.len() >= (chroma_w * chroma_h) as usize);
        assert!(yuv.v.len() >= (chroma_w * chroma_h) as usize);
    }
}
