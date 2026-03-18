//! Zero-copy CVPixelBuffer → wgpu texture import via CVMetalTextureCache.
//!
//! Mirrors the DMA-BUF import path (`dmabuf_import.rs`) for Linux. On macOS
//! and iOS, decoded NV12 frames from VideoToolbox (and BGRA frames from
//! ScreenCaptureKit/AVFoundation) live in IOSurface-backed GPU memory.
//! CVMetalTextureCache creates Metal textures that alias the same memory —
//! true zero-copy.

use std::ptr::{self, NonNull};

use anyhow::{Result, bail};
use metal::foreign_types::ForeignType;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVImageBuffer, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane,
};
use objc2_metal::MTLPixelFormat;

use crate::format::CvPixelBufferInfo;

// ── Frame resource lifetime ────────────────────────────────────────────

/// Retained CVMetalTexture refs that must stay alive while the GPU reads.
/// CVMetalTexture is a type alias for CVImageBuffer in objc2.
#[derive(Debug)]
struct FrameResources {
    _cv_y: CFRetained<CVImageBuffer>,
    _cv_uv: CFRetained<CVImageBuffer>,
}

// ── Imported frame ─────────────────────────────────────────────────────

/// NV12 frame imported as separate Y and UV wgpu textures (zero-copy).
#[derive(Debug)]
pub struct ImportedMetalNv12 {
    pub y_texture: wgpu::Texture,
    pub uv_texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
}

// ── MetalImporter ──────────────────────────────────────────────────────

/// Imports CVPixelBuffer frames as wgpu textures via CVMetalTextureCache.
#[derive(Debug)]
pub struct MetalImporter {
    texture_cache: CFRetained<CVMetalTextureCache>,
    /// Double-buffered: previous frame's resources kept alive until the
    /// next frame arrives, ensuring the GPU has finished reading.
    prev_resources: Option<FrameResources>,
    current_resources: Option<FrameResources>,
}

// Safety: CVMetalTextureCache is thread-safe once created. The importer
// is used only from the render thread.
unsafe impl Send for MetalImporter {}

impl MetalImporter {
    /// Creates a new importer by extracting the Metal device from wgpu.
    ///
    /// Returns `None` if the wgpu device is not Metal-backed.
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        let raw_device_ptr = unsafe {
            device.as_hal::<wgpu_hal::api::Metal>().map(|hal_dev| {
                let mtl_dev = hal_dev.raw_device().lock();
                mtl_dev.as_ptr() as *mut std::ffi::c_void
            })?
        };

        // Create the texture cache from the raw Metal device pointer.
        // objc2's typed API requires &ProtocolObject<dyn MTLDevice>, but we
        // only have a raw pointer from wgpu-hal. Use the C FFI directly.
        let mut cache_ptr: *mut CVMetalTextureCache = ptr::null_mut();
        let ret = unsafe {
            objc2_core_video::CVMetalTextureCacheCreate(
                None,
                None,
                // Safety: raw_device_ptr is a valid MTLDevice from wgpu-hal.
                &*raw_device_ptr
                    .cast::<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice>>(),
                None,
                NonNull::new(&mut cache_ptr).unwrap(),
            )
        };

        if ret != 0 || cache_ptr.is_null() {
            tracing::warn!("CVMetalTextureCacheCreate failed: {ret}");
            return None;
        }

        let texture_cache = unsafe { CFRetained::from_raw(NonNull::new(cache_ptr).unwrap()) };
        tracing::info!("CVMetalTextureCache created for zero-copy render");

        Some(Self {
            texture_cache,
            prev_resources: None,
            current_resources: None,
        })
    }

    /// Import an NV12 CVPixelBuffer as Y + UV wgpu textures (zero-copy).
    pub fn import_nv12(
        &mut self,
        device: &wgpu::Device,
        info: &CvPixelBufferInfo,
    ) -> Result<ImportedMetalNv12> {
        let pb = info.pixel_buffer();

        // Flush stale textures so IOSurface-backed buffers can be reused.
        self.texture_cache.flush(0);

        let plane_count = unsafe { CVPixelBufferGetPlaneCount(pb) };
        if plane_count < 2 {
            bail!("CVPixelBuffer has {plane_count} planes, need 2 for NV12");
        }

        let w = info.width as usize;
        let h = info.height as usize;
        let uv_w = unsafe { CVPixelBufferGetWidthOfPlane(pb, 1) };
        let uv_h = unsafe { CVPixelBufferGetHeightOfPlane(pb, 1) };

        // Create Y plane CVMetalTexture (R8Unorm)
        let mut cv_y_ptr: *mut CVImageBuffer = ptr::null_mut();
        let ret = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.texture_cache,
                pb,
                None,
                MTLPixelFormat::R8Unorm,
                w,
                h,
                0,
                NonNull::new(&mut cv_y_ptr).unwrap(),
            )
        };
        if ret != 0 || cv_y_ptr.is_null() {
            bail!("CVMetalTextureCacheCreateTextureFromImage (Y) failed: {ret}");
        }
        let cv_y = unsafe { CFRetained::from_raw(NonNull::new(cv_y_ptr).unwrap()) };

        // Create UV plane CVMetalTexture (RG8Unorm)
        let mut cv_uv_ptr: *mut CVImageBuffer = ptr::null_mut();
        let ret = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.texture_cache,
                pb,
                None,
                MTLPixelFormat::RG8Unorm,
                uv_w,
                uv_h,
                1,
                NonNull::new(&mut cv_uv_ptr).unwrap(),
            )
        };
        if ret != 0 || cv_uv_ptr.is_null() {
            bail!("CVMetalTextureCacheCreateTextureFromImage (UV) failed: {ret}");
        }
        let cv_uv = unsafe { CFRetained::from_raw(NonNull::new(cv_uv_ptr).unwrap()) };

        // Extract MTLTexture from CVMetalTexture
        let mtl_y_obj = CVMetalTextureGetTexture(&cv_y)
            .ok_or_else(|| anyhow::anyhow!("CVMetalTextureGetTexture (Y) returned null"))?;
        let mtl_uv_obj = CVMetalTextureGetTexture(&cv_uv)
            .ok_or_else(|| anyhow::anyhow!("CVMetalTextureGetTexture (UV) returned null"))?;

        // Convert objc2 MTLTexture protocol objects to metal crate Texture.
        // Both are wrappers around the same raw pointer — we retain for the
        // metal crate's ownership model.
        let y_raw = objc2::rc::Retained::as_ptr(&mtl_y_obj) as *mut metal::MTLTexture;
        let uv_raw = objc2::rc::Retained::as_ptr(&mtl_uv_obj) as *mut metal::MTLTexture;

        // Safety: retain the MTLTexture for the metal crate wrapper.
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRetain(cf: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        let y_mtl =
            unsafe { metal::Texture::from_ptr(CFRetain(y_raw.cast()).cast::<metal::MTLTexture>()) };
        let uv_mtl = unsafe {
            metal::Texture::from_ptr(CFRetain(uv_raw.cast()).cast::<metal::MTLTexture>())
        };

        // Create wgpu textures via HAL
        let y_hal = unsafe {
            wgpu_hal::metal::Device::texture_from_raw(
                y_mtl,
                wgpu::TextureFormat::R8Unorm,
                metal::MTLTextureType::D2,
                1,
                1,
                wgpu_hal::CopyExtent {
                    width: w as u32,
                    height: h as u32,
                    depth: 1,
                },
            )
        };
        let uv_hal = unsafe {
            wgpu_hal::metal::Device::texture_from_raw(
                uv_mtl,
                wgpu::TextureFormat::Rg8Unorm,
                metal::MTLTextureType::D2,
                1,
                1,
                wgpu_hal::CopyExtent {
                    width: uv_w as u32,
                    height: uv_h as u32,
                    depth: 1,
                },
            )
        };

        let y_tex = unsafe {
            device.create_texture_from_hal::<wgpu_hal::api::Metal>(
                y_hal,
                &wgpu::TextureDescriptor {
                    label: Some("metal_y"),
                    size: wgpu::Extent3d {
                        width: w as u32,
                        height: h as u32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let uv_tex = unsafe {
            device.create_texture_from_hal::<wgpu_hal::api::Metal>(
                uv_hal,
                &wgpu::TextureDescriptor {
                    label: Some("metal_uv"),
                    size: wgpu::Extent3d {
                        width: uv_w as u32,
                        height: uv_h as u32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rg8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };

        // Rotate frame resources (double-buffer for GPU read safety)
        self.prev_resources = self.current_resources.take();
        self.current_resources = Some(FrameResources {
            _cv_y: cv_y,
            _cv_uv: cv_uv,
        });

        Ok(ImportedMetalNv12 {
            y_texture: y_tex,
            uv_texture: uv_tex,
            width: w as u32,
            height: h as u32,
        })
    }
}
