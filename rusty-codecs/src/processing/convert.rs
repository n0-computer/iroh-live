use anyhow::Result;
use yuv::{
    BufferStoreMut, YuvBiPlanarImage, YuvBiPlanarImageMut, YuvChromaSubsampling, YuvPlanarImage,
    YuvPlanarImageMut, YuvRange, YuvStandardMatrix, bgra_to_yuv420, rgba_to_yuv_nv12,
    rgba_to_yuv420, yuv_nv12_to_bgra, yuv_nv12_to_rgba, yuv420_to_bgra, yuv420_to_rgba,
};

use crate::format::PixelFormat;

/// YUV 4:2:0 planar image data.
#[derive(Debug)]
pub struct YuvData {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub y_stride: u32,
    pub u_stride: u32,
    pub v_stride: u32,
}

impl YuvData {
    /// Create a black YUV 4:2:0 frame (BT.601 limited range).
    pub fn black(width: u32, height: u32) -> Self {
        let chroma_w = width.div_ceil(2);
        let chroma_h = height.div_ceil(2);
        Self {
            y: vec![16; (width * height) as usize],
            u: vec![128; (chroma_w * chroma_h) as usize],
            v: vec![128; (chroma_w * chroma_h) as usize],
            width,
            height,
            y_stride: width,
            u_stride: chroma_w,
            v_stride: chroma_w,
        }
    }
}

/// Extracts the owned `Vec` from a `BufferStoreMut::Owned` without copying.
///
/// # Panics
///
/// Panics if the buffer is `Borrowed`.
///
/// Invariant: this is only called on buffers produced by `YuvPlanarImageMut::alloc()`,
/// which allocates each plane as a fresh `Vec<u8>` wrapped in `BufferStoreMut::Owned`.
/// The `Borrowed` variant only appears when a caller constructs a `YuvPlanarImageMut`
/// from external slices, which never happens in our conversion functions.
fn take_owned(buf: BufferStoreMut<'_, u8>) -> Vec<u8> {
    match buf {
        BufferStoreMut::Owned(v) => v,
        BufferStoreMut::Borrowed(_) => unreachable!("alloc() always creates Owned buffers"),
    }
}

/// Convert RGBA pixel data to YUV 4:2:0 planar format (BT.601).
pub fn rgba_to_yuv420_data(src: &[u8], w: u32, h: u32) -> Result<YuvData> {
    let mut planar = YuvPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    rgba_to_yuv420(
        &mut planar,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    Ok(YuvData {
        y_stride: planar.y_stride,
        u_stride: planar.u_stride,
        v_stride: planar.v_stride,
        y: take_owned(planar.y_plane),
        u: take_owned(planar.u_plane),
        v: take_owned(planar.v_plane),
        width: w,
        height: h,
    })
}

/// Convert BGRA pixel data to YUV 4:2:0 planar format (BT.601).
pub fn bgra_to_yuv420_data(src: &[u8], w: u32, h: u32) -> Result<YuvData> {
    let mut planar = YuvPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    bgra_to_yuv420(
        &mut planar,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    Ok(YuvData {
        y_stride: planar.y_stride,
        u_stride: planar.u_stride,
        v_stride: planar.v_stride,
        y: take_owned(planar.y_plane),
        u: take_owned(planar.u_plane),
        v: take_owned(planar.v_plane),
        width: w,
        height: h,
    })
}

/// Dispatch pixel-format-aware RGBA/BGRA → YUV 4:2:0 conversion.
pub fn pixel_format_to_yuv420(src: &[u8], w: u32, h: u32, format: PixelFormat) -> Result<YuvData> {
    match format {
        PixelFormat::Rgba => rgba_to_yuv420_data(src, w, h),
        PixelFormat::Bgra => bgra_to_yuv420_data(src, w, h),
    }
}

/// Convert YUV 4:2:0 planar slices directly to RGBA (BT.601).
///
/// Avoids intermediate `YuvData` allocation — useful when plane data is
/// already available as borrowed slices (e.g. from dav1d decoder).
#[allow(clippy::too_many_arguments, reason = "mirrors YUV plane layout")]
pub fn yuv420_to_rgba_from_slices(
    y: &[u8],
    y_stride: u32,
    u: &[u8],
    u_stride: u32,
    v: &[u8],
    v_stride: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    yuv420_to_rgba_into(
        y, y_stride, u, u_stride, v, v_stride, width, height, &mut rgba,
    )?;
    Ok(rgba)
}

/// Convert YUV 4:2:0 planar slices to RGBA, writing into a caller-provided buffer.
///
/// The buffer must have at least `width * height * 4` bytes. Resized if too
/// small, reused if large enough — this avoids per-frame allocation when the
/// caller retains the buffer across calls.
#[allow(clippy::too_many_arguments, reason = "mirrors YUV plane layout")]
pub fn yuv420_to_rgba_into(
    y: &[u8],
    y_stride: u32,
    u: &[u8],
    u_stride: u32,
    v: &[u8],
    v_stride: u32,
    width: u32,
    height: u32,
    dst: &mut Vec<u8>,
) -> Result<()> {
    let planar = YuvPlanarImage {
        y_plane: y,
        y_stride,
        u_plane: u,
        u_stride,
        v_plane: v,
        v_stride,
        width,
        height,
    };
    let rgba_stride = width * 4;
    let needed = (width * height * 4) as usize;
    dst.resize(needed, 0);
    yuv420_to_rgba(
        &planar,
        dst,
        rgba_stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
    )?;
    Ok(())
}

/// Convert YUV 4:2:0 planar slices directly to BGRA (BT.601).
#[allow(clippy::too_many_arguments, reason = "mirrors YUV plane layout")]
pub fn yuv420_to_bgra_from_slices(
    y: &[u8],
    y_stride: u32,
    u: &[u8],
    u_stride: u32,
    v: &[u8],
    v_stride: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let mut bgra = vec![0u8; (width * height * 4) as usize];
    yuv420_to_bgra_into(
        y, y_stride, u, u_stride, v, v_stride, width, height, &mut bgra,
    )?;
    Ok(bgra)
}

/// Convert YUV 4:2:0 planar slices to BGRA, writing into a caller-provided buffer.
///
/// Same as [`yuv420_to_rgba_into`] but outputs BGRA byte order.
#[allow(clippy::too_many_arguments, reason = "mirrors YUV plane layout")]
pub fn yuv420_to_bgra_into(
    y: &[u8],
    y_stride: u32,
    u: &[u8],
    u_stride: u32,
    v: &[u8],
    v_stride: u32,
    width: u32,
    height: u32,
    dst: &mut Vec<u8>,
) -> Result<()> {
    let planar = YuvPlanarImage {
        y_plane: y,
        y_stride,
        u_plane: u,
        u_stride,
        v_plane: v,
        v_stride,
        width,
        height,
    };
    let bgra_stride = width * 4;
    let needed = (width * height * 4) as usize;
    dst.resize(needed, 0);
    yuv420_to_bgra(
        &planar,
        dst,
        bgra_stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
    )?;
    Ok(())
}

/// NV12 bi-planar data with Y and interleaved UV planes.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "stride/dimension fields used by future render paths"
)]
pub struct Nv12Data {
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub width: u32,
    pub height: u32,
}

impl Nv12Data {
    /// Concatenate Y and UV planes into a single contiguous buffer.
    pub fn into_contiguous(mut self) -> Vec<u8> {
        self.y.extend_from_slice(&self.uv);
        self.y
    }
}

/// Convert RGBA pixel data directly to NV12 bi-planar format (BT.601).
#[cfg_attr(
    not(any(feature = "vaapi", feature = "android")),
    allow(dead_code, reason = "used by vaapi and android encoders")
)]
pub fn rgba_to_nv12_data(src: &[u8], w: u32, h: u32) -> Result<Nv12Data> {
    let mut bi = YuvBiPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    rgba_to_yuv_nv12(
        &mut bi,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    let y = take_owned(bi.y_plane);
    let uv = take_owned(bi.uv_plane);
    Ok(Nv12Data {
        y_stride: bi.y_stride,
        uv_stride: bi.uv_stride,
        y,
        uv,
        width: w,
        height: h,
    })
}

/// Convert BGRA pixel data directly to NV12 bi-planar format (BT.601).
#[cfg_attr(
    not(any(feature = "vaapi", feature = "android")),
    allow(dead_code, reason = "used by vaapi and android encoders")
)]
pub fn bgra_to_nv12_data(src: &[u8], w: u32, h: u32) -> Result<Nv12Data> {
    use yuv::bgra_to_yuv_nv12;
    let mut bi = YuvBiPlanarImageMut::<u8>::alloc(w, h, YuvChromaSubsampling::Yuv420);
    bgra_to_yuv_nv12(
        &mut bi,
        src,
        w * 4,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    let y = take_owned(bi.y_plane);
    let uv = take_owned(bi.uv_plane);
    Ok(Nv12Data {
        y_stride: bi.y_stride,
        uv_stride: bi.uv_stride,
        y,
        uv,
        width: w,
        height: h,
    })
}

/// Dispatch pixel-format-aware RGBA/BGRA -> NV12 conversion.
#[cfg_attr(
    not(any(feature = "vaapi", feature = "android")),
    allow(dead_code, reason = "used by vaapi and android encoders")
)]
pub fn pixel_format_to_nv12(src: &[u8], w: u32, h: u32, format: PixelFormat) -> Result<Nv12Data> {
    match format {
        PixelFormat::Rgba => rgba_to_nv12_data(src, w, h),
        PixelFormat::Bgra => bgra_to_nv12_data(src, w, h),
    }
}

/// Convert NV12 bi-planar data to RGBA (BT.601 limited range).
///
/// `y_plane` and `uv_plane` are separate slices with their own strides.
#[cfg_attr(
    not(any(feature = "vaapi", feature = "android", target_os = "macos")),
    allow(dead_code, reason = "used by vaapi, android, and future vtb decoders")
)]
pub fn nv12_to_rgba_data(
    y_plane: &[u8],
    y_stride: u32,
    uv_plane: &[u8],
    uv_stride: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bi = YuvBiPlanarImage {
        y_plane,
        y_stride,
        uv_plane,
        uv_stride,
        width,
        height,
    };
    let rgba_stride = width * 4;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    yuv_nv12_to_rgba(
        &bi,
        &mut rgba,
        rgba_stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    Ok(rgba)
}

/// Convert NV12 bi-planar data to BGRA (BT.601 limited range).
#[cfg_attr(
    not(feature = "android"),
    allow(
        dead_code,
        reason = "symmetric with nv12_to_rgba_data; used by android decoder"
    )
)]
pub fn nv12_to_bgra_data(
    y_plane: &[u8],
    y_stride: u32,
    uv_plane: &[u8],
    uv_stride: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bi = YuvBiPlanarImage {
        y_plane,
        y_stride,
        uv_plane,
        uv_stride,
        width,
        height,
    };
    let bgra_stride = width * 4;
    let mut bgra = vec![0u8; (width * height * 4) as usize];
    yuv_nv12_to_bgra(
        &bi,
        &mut bgra,
        bgra_stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        yuv::YuvConversionMode::Balanced,
    )?;
    Ok(bgra)
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
    fn rgba_to_nv12_roundtrip() {
        let w = 8u32;
        let h = 8u32;
        // Solid red RGBA
        let rgba: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let nv12 = rgba_to_nv12_data(&rgba, w, h).unwrap();
        assert_eq!(nv12.width, w);
        assert_eq!(nv12.height, h);
        // Y plane: one byte per pixel
        assert!(nv12.y.len() >= (w * h) as usize);
        // UV plane: interleaved, half width * half height * 2
        let chroma_h = h.div_ceil(2);
        assert!(nv12.uv.len() >= (w * chroma_h) as usize);

        // Round-trip: NV12 → RGBA
        let restored =
            nv12_to_rgba_data(&nv12.y, nv12.y_stride, &nv12.uv, nv12.uv_stride, w, h).unwrap();
        assert_eq!(restored.len(), (w * h * 4) as usize);
        // Center pixel should still be reddish
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert!(restored[mid] > 150, "R={} should be high", restored[mid]);
        assert!(restored[mid + 1] < 100, "G={}", restored[mid + 1]);
        assert!(restored[mid + 2] < 100, "B={}", restored[mid + 2]);
    }

    #[test]
    fn bgra_to_nv12_roundtrip() {
        let w = 8u32;
        let h = 8u32;
        // Solid blue in BGRA (B=255, G=0, R=0, A=255)
        let bgra: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let nv12 = bgra_to_nv12_data(&bgra, w, h).unwrap();
        assert_eq!(nv12.width, w);
        assert_eq!(nv12.height, h);

        // Round-trip: NV12 → BGRA
        let restored =
            nv12_to_bgra_data(&nv12.y, nv12.y_stride, &nv12.uv, nv12.uv_stride, w, h).unwrap();
        assert_eq!(restored.len(), (w * h * 4) as usize);
        // Center pixel should be bluish (BGRA order)
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert!(restored[mid] > 150, "B={} should be high", restored[mid]);
    }

    #[test]
    fn nv12_into_contiguous() {
        let nv12 = Nv12Data {
            y: vec![16; 16],
            uv: vec![128; 8],
            y_stride: 4,
            uv_stride: 4,
            width: 4,
            height: 4,
        };
        let buf = nv12.into_contiguous();
        assert_eq!(buf.len(), 24);
        assert_eq!(&buf[..16], &[16u8; 16]);
        assert_eq!(&buf[16..], &[128u8; 8]);
    }

    #[test]
    fn pixel_format_to_nv12_dispatch() {
        let w = 4u32;
        let h = 4u32;
        let data: Vec<u8> = vec![128; (w * h * 4) as usize];
        pixel_format_to_nv12(&data, w, h, PixelFormat::Rgba).unwrap();
        pixel_format_to_nv12(&data, w, h, PixelFormat::Bgra).unwrap();
    }

    #[test]
    fn yuv420_to_bgra_from_slices_basic() {
        let w = 4u32;
        let h = 4u32;
        // Create YUV from known RGBA, then convert to BGRA
        let rgba: Vec<u8> = [255, 0, 0, 255].repeat((w * h) as usize);
        let yuv = rgba_to_yuv420_data(&rgba, w, h).unwrap();
        let bgra = yuv420_to_bgra_from_slices(
            &yuv.y,
            yuv.y_stride,
            &yuv.u,
            yuv.u_stride,
            &yuv.v,
            yuv.v_stride,
            w,
            h,
        )
        .unwrap();
        assert_eq!(bgra.len(), (w * h * 4) as usize);
        // BGRA: B should be low for red input, R (at offset 2) should be high
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert!(
            bgra[mid + 2] > 150,
            "BGRA R={} should be high",
            bgra[mid + 2]
        );
        assert!(bgra[mid] < 100, "BGRA B={} should be low", bgra[mid]);
    }

    #[test]
    fn yuv420_to_rgba_from_slices_basic() {
        let w = 4u32;
        let h = 4u32;
        let rgba_in: Vec<u8> = [0, 255, 0, 255].repeat((w * h) as usize);
        let yuv = rgba_to_yuv420_data(&rgba_in, w, h).unwrap();
        let rgba_out = yuv420_to_rgba_from_slices(
            &yuv.y,
            yuv.y_stride,
            &yuv.u,
            yuv.u_stride,
            &yuv.v,
            yuv.v_stride,
            w,
            h,
        )
        .unwrap();
        assert_eq!(rgba_out.len(), (w * h * 4) as usize);
        // Green should survive roundtrip
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert!(
            rgba_out[mid + 1] > 150,
            "G={} should be high",
            rgba_out[mid + 1]
        );
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
