//! GPU-resident frame wrapper for Android HardwareBuffer-backed images.
//!
//! Wraps an [`ndk::media::image_reader::Image`] that holds a reference to a
//! HardwareBuffer produced by MediaCodec's Surface output mode. Implements
//! [`GpuFrameInner`] so that frames flow through the standard `VideoFrame`
//! pipeline without CPU pixel access on the hot path.

use std::fmt;

use anyhow::{Context, Result};
use image::RgbaImage;
use ndk::{
    hardware_buffer::{HardwareBuffer, HardwareBufferUsage},
    media::image_reader::Image,
};

use crate::{
    format::{GpuFrameInner, GpuPixelFormat, HardwareBufferInfo, NativeFrameHandle, Nv12Planes},
    processing::convert::nv12_to_rgba_data,
};

/// Guard that ensures a locked [`HardwareBuffer`] is unlocked on drop.
///
/// The NDK requires that every successful `lock()` is paired with an
/// `unlock()`. This guard makes that pairing unconditional — even when
/// the code between lock and unlock returns an error.
struct HardwareBufferLockGuard<'a> {
    buffer: &'a HardwareBuffer,
}

impl Drop for HardwareBufferLockGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: The buffer was successfully locked before this guard
        // was created. Unlock must be called exactly once per lock.
        if let Err(e) = self.buffer.unlock() {
            tracing::error!("HardwareBuffer unlock failed in drop: {e}");
        }
    }
}

/// GPU-resident frame backed by an Android HardwareBuffer.
///
/// Holds an [`Image`] acquired from an `ImageReader`. The image keeps the
/// underlying `AHardwareBuffer` alive via the NDK's internal refcount.
/// Dropping this struct releases the image back to the ImageReader's buffer
/// queue.
pub(crate) struct AndroidGpuFrame {
    image: Image,
    width: u32,
    height: u32,
    y_stride: u32,
    uv_offset: u32,
    uv_stride: u32,
}

impl fmt::Debug for AndroidGpuFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidGpuFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

// SAFETY: The Image wraps an AImage pointer. The NDK AImage API is
// thread-safe for read-only access once acquired, and we never mutate
// the image after construction.
unsafe impl Send for AndroidGpuFrame {}
unsafe impl Sync for AndroidGpuFrame {}

impl AndroidGpuFrame {
    /// Creates a new GPU frame from an acquired ImageReader image.
    ///
    /// The caller provides the decoded dimensions and NV12 plane layout
    /// obtained from the MediaCodec output format.
    pub(crate) fn new(
        image: Image,
        width: u32,
        height: u32,
        y_stride: u32,
        uv_offset: u32,
        uv_stride: u32,
    ) -> Self {
        Self {
            image,
            width,
            height,
            y_stride,
            uv_offset,
            uv_stride,
        }
    }
}

impl GpuFrameInner for AndroidGpuFrame {
    fn download_rgba(&self) -> Result<RgbaImage> {
        // Fallback CPU path: lock the HardwareBuffer, read NV12, convert.
        let hw_buf = self
            .image
            .hardware_buffer()
            .map_err(|e| anyhow::anyhow!("failed to get HardwareBuffer from Image: {e:?}"))?;

        // SAFETY: We lock the buffer for CPU read with the correct usage
        // flag. The pointer is valid for the duration of the lock.
        let ptr = hw_buf
            .lock(
                HardwareBufferUsage::CPU_READ_OFTEN,
                None, // no fence
                None, // full rect
            )
            .context("HardwareBuffer lock for CPU read failed")?;

        // Ensure unlock runs even if conversion fails below.
        let _guard = HardwareBufferLockGuard { buffer: &hw_buf };

        let y_plane_size = (self.y_stride * self.height) as usize;
        let uv_plane_size = (self.uv_stride * (self.height / 2)) as usize;

        // SAFETY: The locked pointer is valid for at least y_plane_size +
        // uv_plane_size bytes (guaranteed by the HardwareBuffer allocation).
        let (y_data, uv_data) = unsafe {
            let base = ptr as *const u8;
            let y = std::slice::from_raw_parts(base, y_plane_size);
            let uv = std::slice::from_raw_parts(base.add(self.uv_offset as usize), uv_plane_size);
            (y, uv)
        };

        let rgba = nv12_to_rgba_data(
            y_data,
            self.y_stride,
            uv_data,
            self.uv_stride,
            self.width,
            self.height,
        )?;

        // Guard handles unlock on drop, but we explicitly drop it here
        // before constructing the image to keep the lock scope minimal.
        drop(_guard);

        RgbaImage::from_raw(self.width, self.height, rgba)
            .context("RGBA data size does not match dimensions")
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat {
        GpuPixelFormat::Nv12
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn download_nv12(&self) -> Option<Result<Nv12Planes>> {
        Some((|| {
            let hw_buf = self
                .image
                .hardware_buffer()
                .map_err(|e| anyhow::anyhow!("failed to get HardwareBuffer: {e:?}"))?;

            // SAFETY: Lock for CPU read, pointer valid during lock scope.
            let ptr = hw_buf
                .lock(HardwareBufferUsage::CPU_READ_OFTEN, None, None)
                .context("HardwareBuffer lock for NV12 download failed")?;

            // Ensure unlock runs even if the slice copy panics or we add
            // fallible operations between lock and unlock in the future.
            let _guard = HardwareBufferLockGuard { buffer: &hw_buf };

            let y_plane_size = (self.y_stride * self.height) as usize;
            let uv_plane_size = (self.uv_stride * (self.height / 2)) as usize;

            // SAFETY: Locked pointer is valid for the buffer extent.
            let (y_vec, uv_vec) = unsafe {
                let base = ptr as *const u8;
                let y = std::slice::from_raw_parts(base, y_plane_size).to_vec();
                let uv =
                    std::slice::from_raw_parts(base.add(self.uv_offset as usize), uv_plane_size)
                        .to_vec();
                (y, uv)
            };

            drop(_guard);

            Ok(Nv12Planes {
                y_data: y_vec,
                y_stride: self.y_stride,
                uv_data: uv_vec,
                uv_stride: self.uv_stride,
                width: self.width,
                height: self.height,
            })
        })())
    }

    fn native_handle(&self) -> Option<NativeFrameHandle> {
        let hw_buf = self.image.hardware_buffer().ok()?;
        // Acquire an extra reference so the HardwareBuffer outlives the Image
        // if the consumer holds onto the handle.
        let owned = hw_buf.acquire();
        Some(NativeFrameHandle::HardwareBuffer(HardwareBufferInfo {
            buffer: owned,
            width: self.width,
            height: self.height,
            y_stride: self.y_stride,
            uv_offset: self.uv_offset,
            uv_stride: self.uv_stride,
        }))
    }
}
