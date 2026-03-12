use std::{cell::RefCell, os::fd::AsRawFd, rc::Rc};

use anyhow::{Context, Result};
use cros_codecs::{
    BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution,
    backend::vaapi::encoder::VaapiBackend,
    codec::h264::parser::Level,
    encoder::{
        CodedBitstreamBuffer, FrameMetadata, PredictionStructure, RateControl, Tunings,
        VideoEncoder as CrosVideoEncoder, h264::EncoderConfig,
        stateless::h264::StatelessEncoder as H264StatelessEncoder,
    },
    libva::{
        self as va, Display, Image, MemoryType, Surface, SurfaceMemoryDescriptor, UsageHint,
        VA_FOURCC_NV12, VA_RT_FORMAT_YUV420, VADRMPRIMESurfaceDescriptor,
        VADRMPRIMESurfaceDescriptorLayer, VADRMPRIMESurfaceDescriptorObject, VAEntrypoint,
        VAProfile, VASurfaceAttrib,
    },
    video_frame::{ReadMapping, VideoFrame as CrosVideoFrame, WriteMapping},
};

use crate::{
    codec::h264::annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b},
    config::{H264, VideoCodec, VideoConfig},
    format::{
        DmaBufInfo, EncodedFrame, NalFormat, NativeFrameHandle, ScaleMode, VideoEncoderConfig,
        VideoFrame,
    },
    processing::convert::{YuvData, pixel_format_to_nv12},
    processing::scale::Scaler,
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// Encoder input frame — either CPU NV12 data or a zero-copy DMA-BUF handle.
///
/// Implements `cros_codecs::video_frame::VideoFrame` so it can be submitted to
/// the stateless encoder. The NV12 variant uploads pixel data via image mapping;
/// the DMA-BUF variant imports the buffer directly as a VA surface, avoiding any
/// CPU copy.
#[derive(Debug)]
enum VaapiInputFrame {
    /// CPU-side NV12 pixel data.
    Nv12 {
        data: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Zero-copy DMA-BUF from capture source.
    DmaBuf(DmaBufInfo),
}

impl VaapiInputFrame {
    fn width(&self) -> u32 {
        match self {
            Self::Nv12 { width, .. } => *width,
            Self::DmaBuf(info) => info.coded_width,
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Nv12 { height, .. } => *height,
            Self::DmaBuf(info) => info.coded_height,
        }
    }
}

/// Implements `SurfaceMemoryDescriptor` so the frame can control how the VA
/// surface is backed: driver-allocated for NV12 (same as `()`), or imported
/// from DMA-BUF via `DRM_PRIME_2` for zero-copy.
impl SurfaceMemoryDescriptor for VaapiInputFrame {
    fn add_attrs(&mut self, attrs: &mut Vec<VASurfaceAttrib>) -> Option<Box<dyn std::any::Any>> {
        match self {
            Self::Nv12 { .. } => None,
            Self::DmaBuf(info) => {
                let mut desc = build_prime_descriptor(info);
                attrs.push(VASurfaceAttrib::new_memory_type(MemoryType::DrmPrime2));
                attrs.push(VASurfaceAttrib::new_buffer_descriptor(&mut desc));
                Some(Box::new(desc))
            }
        }
    }
}

/// Builds a `VADRMPRIMESurfaceDescriptor` from our `DmaBufInfo`.
///
/// VA-API layer arrays are fixed at 4 elements, so at most 4 planes are
/// supported (NV12 = 2, I420 = 3).
fn build_prime_descriptor(info: &DmaBufInfo) -> VADRMPRIMESurfaceDescriptor {
    debug_assert!(
        info.planes.len() <= 4,
        "DMA-BUF has {} planes, VA-API supports at most 4",
        info.planes.len()
    );

    let mut objects: [VADRMPRIMESurfaceDescriptorObject; 4] = Default::default();
    objects[0] = VADRMPRIMESurfaceDescriptorObject {
        fd: info.fd.as_raw_fd(),
        size: 0, // driver calculates
        drm_format_modifier: info.modifier,
    };

    let mut layer = VADRMPRIMESurfaceDescriptorLayer {
        drm_format: info.drm_format,
        num_planes: info.planes.len() as u32,
        ..Default::default()
    };
    for (i, plane) in info.planes.iter().enumerate() {
        layer.object_index[i] = 0;
        layer.offset[i] = plane.offset;
        layer.pitch[i] = plane.pitch;
    }

    let mut layers: [VADRMPRIMESurfaceDescriptorLayer; 4] = Default::default();
    layers[0] = layer;

    VADRMPRIMESurfaceDescriptor {
        fourcc: info.drm_format,
        width: info.coded_width,
        height: info.coded_height,
        num_objects: 1,
        objects,
        num_layers: 1,
        layers,
    }
}

struct Nv12ReadMapping<'a> {
    y_plane: &'a [u8],
    uv_plane: &'a [u8],
}

impl<'a> ReadMapping<'a> for Nv12ReadMapping<'a> {
    fn get(&self) -> Vec<&[u8]> {
        vec![self.y_plane, self.uv_plane]
    }
}

struct Nv12WriteMapping;

impl<'a> WriteMapping<'a> for Nv12WriteMapping {
    fn get(&self) -> Vec<RefCell<&'a mut [u8]>> {
        // No-op: NV12 frames are uploaded via `upload_nv12_to_surface` and
        // DMA-BUF frames are imported directly — neither uses write mappings.
        vec![]
    }
}

impl CrosVideoFrame for VaapiInputFrame {
    type MemDescriptor = Self;
    type NativeHandle = Surface<Self>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        Resolution {
            width: self.width(),
            height: self.height(),
        }
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let w = self.width();
        let h = self.height();
        let y_size = (w * h) as usize;
        let uv_size = (w * h.div_ceil(2)) as usize;
        vec![y_size, uv_size]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        match self {
            Self::Nv12 { width, .. } => vec![*width as usize, *width as usize],
            Self::DmaBuf(info) => info.planes.iter().map(|p| p.pitch as usize).collect(),
        }
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        match self {
            Self::Nv12 {
                data,
                width,
                height,
            } => {
                let y_size = (*width as usize) * (*height as usize);
                Ok(Box::new(Nv12ReadMapping {
                    y_plane: &data[..y_size],
                    uv_plane: &data[y_size..],
                }))
            }
            Self::DmaBuf(_) => Err("DMA-BUF frames cannot be CPU-mapped".to_string()),
        }
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Ok(Box::new(Nv12WriteMapping))
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        match self {
            Self::Nv12 {
                data,
                width,
                height,
            } => {
                // Create a driver-allocated VA surface and upload NV12 data.
                let dummy = Self::Nv12 {
                    data: Vec::new(),
                    width: *width,
                    height: *height,
                };
                let mut surfaces = display
                    .create_surfaces(
                        VA_RT_FORMAT_YUV420,
                        Some(VA_FOURCC_NV12),
                        *width,
                        *height,
                        Some(UsageHint::USAGE_HINT_ENCODER),
                        vec![dummy],
                    )
                    .map_err(|e| format!("failed to create VA surface: {e:?}"))?;
                let surface = surfaces.pop().ok_or("no surface created")?;
                upload_nv12_to_surface(display, &surface, data, *width, *height)
                    .map_err(|e| format!("failed to upload NV12 data: {e:?}"))?;
                Ok(surface)
            }
            Self::DmaBuf(info) => {
                // Import the DMA-BUF directly as a VA surface — zero-copy.
                let frame = Self::DmaBuf(DmaBufInfo {
                    fd: info
                        .fd
                        .try_clone()
                        .map_err(|e| format!("failed to dup DMA-BUF fd: {e}"))?,
                    modifier: info.modifier,
                    drm_format: info.drm_format,
                    coded_width: info.coded_width,
                    coded_height: info.coded_height,
                    display_width: info.display_width,
                    display_height: info.display_height,
                    planes: info.planes.clone(),
                });
                let mut surfaces = display
                    .create_surfaces(
                        VA_RT_FORMAT_YUV420,
                        Some(info.drm_format),
                        info.coded_width,
                        info.coded_height,
                        Some(UsageHint::USAGE_HINT_ENCODER),
                        vec![frame],
                    )
                    .map_err(|e| format!("failed to import DMA-BUF as VA surface: {e:?}"))?;
                surfaces
                    .pop()
                    .ok_or_else(|| "no surface created".to_string())
            }
        }
    }
}

/// Upload raw NV12 data to a VA surface using image mapping.
fn upload_nv12_to_surface(
    display: &Rc<Display>,
    surface: &Surface<VaapiInputFrame>,
    nv12: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let image_fmts = display
        .query_image_formats()
        .map_err(|e| anyhow::anyhow!("failed to query image formats: {e:?}"))?;

    let nv12_fmt = image_fmts
        .into_iter()
        .find(|f| f.fourcc == VA_FOURCC_NV12)
        .context("VAAPI display does not support NV12 image format")?;

    let size = (width, height);
    let mut image = Image::create_from(surface, nv12_fmt, size, size)
        .map_err(|e| anyhow::anyhow!("failed to create VAAPI image: {e:?}"))?;

    let va_image = *image.image();
    let dst = image.as_mut();

    let w = width as usize;
    let h = height as usize;

    // Copy Y plane (respecting VA pitch).
    let y_offset = va_image.offsets[0] as usize;
    let y_pitch = va_image.pitches[0] as usize;
    for row in 0..h {
        let src_start = row * w;
        let dst_start = y_offset + row * y_pitch;
        let copy_len = w.min(y_pitch);
        if src_start + copy_len <= nv12.len() && dst_start + copy_len <= dst.len() {
            dst[dst_start..dst_start + copy_len]
                .copy_from_slice(&nv12[src_start..src_start + copy_len]);
        }
    }

    // Copy UV plane (interleaved, respecting VA pitch).
    let uv_offset = va_image.offsets[1] as usize;
    let uv_pitch = va_image.pitches[1] as usize;
    let chroma_h = h.div_ceil(2);
    let chroma_w_bytes = w; // NV12: U+V interleaved = width bytes
    let y_plane_size = w * h;
    for row in 0..chroma_h {
        let src_start = y_plane_size + row * chroma_w_bytes;
        let dst_start = uv_offset + row * uv_pitch;
        let copy_len = chroma_w_bytes.min(uv_pitch);
        if src_start + copy_len <= nv12.len() && dst_start + copy_len <= dst.len() {
            dst[dst_start..dst_start + copy_len]
                .copy_from_slice(&nv12[src_start..src_start + copy_len]);
        }
    }

    // Drop image to unmap and sync to surface.
    drop(image);
    surface
        .sync()
        .map_err(|e| anyhow::anyhow!("VA surface sync failed: {e:?}"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// VAAPI VPP color-space converter
// ---------------------------------------------------------------------------

/// VA RT format for RGB32 surfaces (BGRx/XRGB8888).
const VA_RT_FORMAT_RGB32: u32 = 0x0002_0000;

/// VA RT format for YUV422 surfaces (YUYV).
const VA_RT_FORMAT_YUV422: u32 = 0x0000_0002;

/// DRM fourcc constants.
const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
const DRM_FORMAT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");

/// VAProcPipelineParameterBuffer from va/va_vpp.h.
///
/// Not included in cros-libva's generated bindings.
#[repr(C)]
struct VaProcPipelineParameterBuffer {
    surface: va::VASurfaceID,
    surface_region: *const va::VARectangle,
    surface_color_standard: u32,
    output_region: *const va::VARectangle,
    output_background_color: u32,
    output_color_standard: u32,
    pipeline_flags: u32,
    filter_flags: u32,
    filters: *mut va::VABufferID,
    num_filters: u32,
    forward_references: *mut va::VASurfaceID,
    num_forward_references: u32,
    backward_references: *mut va::VASurfaceID,
    num_backward_references: u32,
    rotation_state: u32,
    blend_state: *const std::ffi::c_void,
    mirror_state: u32,
    additional_outputs: *mut va::VASurfaceID,
    num_additional_outputs: u32,
    input_surface_flag: u32,
    output_surface_flag: u32,
    _pad: [u8; 256],
}

/// Checks a VAStatus and returns an error on failure.
fn vpp_va_check(status: va::VAStatus, op: &str) -> Result<()> {
    if status != va::VA_STATUS_SUCCESS as i32 {
        Err(anyhow::anyhow!("{op} failed: VA status {status}"))
    } else {
        Ok(())
    }
}

/// Maps a DRM fourcc to the VA RT format needed for surface import.
fn drm_fourcc_to_rt_format(fourcc: u32) -> Option<u32> {
    match fourcc {
        DRM_FORMAT_XRGB8888 | DRM_FORMAT_ARGB8888 => Some(VA_RT_FORMAT_RGB32),
        DRM_FORMAT_YUYV => Some(VA_RT_FORMAT_YUV422),
        DRM_FORMAT_NV12 => Some(VA_RT_FORMAT_YUV420),
        _ => None,
    }
}

/// VAAPI VPP color-space converter.
///
/// Converts DMA-BUF frames (BGRx/YUYV/etc.) to NV12 on the GPU using the
/// VA-API Video Processing Pipeline, avoiding CPU pixel copies.
struct VppColorConverter {
    dpy: va::VADisplay,
    config_id: va::VAConfigID,
    _drm_file: std::fs::File,
    target_width: u32,
    target_height: u32,
}

impl VppColorConverter {
    /// Creates a VPP color converter targeting the given output dimensions.
    fn new(target_width: u32, target_height: u32) -> Result<Self> {
        unsafe {
            for path in ["/dev/dri/renderD128", "/dev/dri/renderD129"] {
                let Ok(file) = std::fs::File::options().read(true).write(true).open(path) else {
                    continue;
                };

                let dpy = va::vaGetDisplayDRM(file.as_raw_fd());
                if dpy.is_null() {
                    continue;
                }

                let mut major = 0i32;
                let mut minor = 0i32;
                let st = va::vaInitialize(dpy, &mut major, &mut minor);
                if st != va::VA_STATUS_SUCCESS as i32 {
                    continue;
                }

                // Check VPP support.
                let mut rt_attr = va::VAConfigAttrib {
                    type_: va::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: 0,
                };
                vpp_va_check(
                    va::vaGetConfigAttributes(
                        dpy,
                        va::VAProfile::VAProfileNone,
                        va::VAEntrypoint::VAEntrypointVideoProc,
                        &mut rt_attr,
                        1,
                    ),
                    "vaGetConfigAttributes(VPP)",
                )?;

                let mut config_id: va::VAConfigID = 0;
                vpp_va_check(
                    va::vaCreateConfig(
                        dpy,
                        va::VAProfile::VAProfileNone,
                        va::VAEntrypoint::VAEntrypointVideoProc,
                        &mut rt_attr,
                        1,
                        &mut config_id,
                    ),
                    "vaCreateConfig(VPP)",
                )?;

                tracing::debug!("VPP color converter initialized on {path}");
                return Ok(Self {
                    dpy,
                    config_id,
                    _drm_file: file,
                    target_width,
                    target_height,
                });
            }
            Err(anyhow::anyhow!(
                "no VA display found for VPP color converter"
            ))
        }
    }

    /// Converts a DMA-BUF to NV12 at the target encoder dimensions via VPP.
    ///
    /// Returns a new `DmaBufInfo` with NV12 format. The caller takes ownership
    /// of the output FD.
    fn convert(&self, info: &DmaBufInfo) -> Result<DmaBufInfo> {
        let input_rt = drm_fourcc_to_rt_format(info.drm_format).ok_or_else(|| {
            anyhow::anyhow!(
                "VPP: unsupported input DRM fourcc 0x{:08x}",
                info.drm_format
            )
        })?;

        // Track allocated VA resources for cleanup.
        struct VppResources {
            dpy: va::VADisplay,
            input_surface: va::VASurfaceID,
            output_surface: va::VASurfaceID,
            context_id: va::VAContextID,
            buf_id: va::VABufferID,
        }

        impl Drop for VppResources {
            fn drop(&mut self) {
                unsafe {
                    if self.buf_id != 0 {
                        va::vaDestroyBuffer(self.dpy, self.buf_id);
                    }
                    if self.context_id != 0 {
                        va::vaDestroyContext(self.dpy, self.context_id);
                    }
                    if self.input_surface != 0 {
                        va::vaDestroySurfaces(self.dpy, &mut self.input_surface, 1);
                    }
                    if self.output_surface != 0 {
                        va::vaDestroySurfaces(self.dpy, &mut self.output_surface, 1);
                    }
                }
            }
        }

        unsafe {
            let mut res = VppResources {
                dpy: self.dpy,
                input_surface: 0,
                output_surface: 0,
                context_id: 0,
                buf_id: 0,
            };

            // Import input DMA-BUF as VA surface.
            let fd_dup = libc::dup(info.fd.as_raw_fd());
            if fd_dup < 0 {
                return Err(anyhow::anyhow!(
                    "dup input fd: {}",
                    std::io::Error::last_os_error()
                ));
            }

            {
                let mut prime_desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
                prime_desc.fourcc = info.drm_format;
                prime_desc.width = info.coded_width;
                prime_desc.height = info.coded_height;
                prime_desc.num_objects = 1;
                prime_desc.objects[0].fd = fd_dup;
                prime_desc.objects[0].size = 0;
                prime_desc.objects[0].drm_format_modifier = info.modifier;
                prime_desc.num_layers = 1;
                prime_desc.layers[0].drm_format = info.drm_format;
                prime_desc.layers[0].num_planes = info.planes.len() as u32;
                for (i, plane) in info.planes.iter().enumerate() {
                    prime_desc.layers[0].object_index[i] = 0;
                    prime_desc.layers[0].offset[i] = plane.offset;
                    prime_desc.layers[0].pitch[i] = plane.pitch;
                }

                let mut attribs: [va::VASurfaceAttrib; 2] = std::mem::zeroed();
                attribs[0].type_ = va::VASurfaceAttribType::VASurfaceAttribMemoryType;
                attribs[0].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[0].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[0].value.value.i = va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32;
                attribs[1].type_ = va::VASurfaceAttribType::VASurfaceAttribExternalBufferDescriptor;
                attribs[1].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[1].value.type_ = va::VAGenericValueType::VAGenericValueTypePointer;
                attribs[1].value.value.p = &mut prime_desc as *mut _ as *mut std::ffi::c_void;

                let status = va::vaCreateSurfaces(
                    self.dpy,
                    input_rt,
                    info.coded_width,
                    info.coded_height,
                    &mut res.input_surface,
                    1,
                    attribs.as_mut_ptr(),
                    attribs.len() as u32,
                );
                libc::close(fd_dup);
                if status != va::VA_STATUS_SUCCESS as i32 {
                    res.input_surface = 0;
                    return Err(anyhow::anyhow!(
                        "vaCreateSurfaces(VPP input) failed: VA status {status}"
                    ));
                }
            }

            // Create NV12 output surface at encoder target dimensions.
            {
                let mut attribs: [va::VASurfaceAttrib; 2] = std::mem::zeroed();
                attribs[0].type_ = va::VASurfaceAttribType::VASurfaceAttribPixelFormat;
                attribs[0].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[0].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[0].value.value.i = va::VA_FOURCC_NV12 as i32;
                attribs[1].type_ = va::VASurfaceAttribType::VASurfaceAttribUsageHint;
                attribs[1].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[1].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[1].value.value.i = (va::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE
                    | va::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT)
                    as i32;

                vpp_va_check(
                    va::vaCreateSurfaces(
                        self.dpy,
                        VA_RT_FORMAT_YUV420,
                        self.target_width,
                        self.target_height,
                        &mut res.output_surface,
                        1,
                        attribs.as_mut_ptr(),
                        attribs.len() as u32,
                    ),
                    "vaCreateSurfaces(VPP output)",
                )?;
            }

            // Create VPP context.
            vpp_va_check(
                va::vaCreateContext(
                    self.dpy,
                    self.config_id,
                    self.target_width as i32,
                    self.target_height as i32,
                    0,
                    &mut res.output_surface,
                    1,
                    &mut res.context_id,
                ),
                "vaCreateContext(VPP)",
            )?;

            // Create pipeline parameter buffer (color convert + optional scale).
            let mut pipeline_param: VaProcPipelineParameterBuffer = std::mem::zeroed();
            pipeline_param.surface = res.input_surface;

            vpp_va_check(
                va::vaCreateBuffer(
                    self.dpy,
                    res.context_id,
                    va::VABufferType::VAProcPipelineParameterBufferType,
                    std::mem::size_of::<VaProcPipelineParameterBuffer>() as u32,
                    1,
                    &mut pipeline_param as *mut _ as *mut std::ffi::c_void,
                    &mut res.buf_id,
                ),
                "vaCreateBuffer(VPP)",
            )?;

            // Execute VPP pipeline.
            vpp_va_check(
                va::vaBeginPicture(self.dpy, res.context_id, res.output_surface),
                "vaBeginPicture(VPP)",
            )?;
            vpp_va_check(
                va::vaRenderPicture(self.dpy, res.context_id, &mut res.buf_id, 1),
                "vaRenderPicture(VPP)",
            )?;
            vpp_va_check(
                va::vaEndPicture(self.dpy, res.context_id),
                "vaEndPicture(VPP)",
            )?;
            vpp_va_check(
                va::vaSyncSurface(self.dpy, res.output_surface),
                "vaSyncSurface(VPP)",
            )?;

            // Export NV12 output as DRM_PRIME_2.
            let mut desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
            vpp_va_check(
                va::vaExportSurfaceHandle(
                    self.dpy,
                    res.output_surface,
                    va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                    va::VA_EXPORT_SURFACE_READ_ONLY | va::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                    &mut desc as *mut _ as *mut std::ffi::c_void,
                ),
                "vaExportSurfaceHandle(VPP output)",
            )?;

            // Take ownership of the primary FD, close any extras.
            use std::os::fd::{FromRawFd, OwnedFd};
            let primary_fd = OwnedFd::from_raw_fd(desc.objects[0].fd);
            for i in 1..desc.num_objects as usize {
                libc::close(desc.objects[i].fd);
            }

            let layer = &desc.layers[0];
            let result = DmaBufInfo {
                fd: primary_fd,
                modifier: desc.objects[0].drm_format_modifier,
                drm_format: desc.fourcc,
                coded_width: desc.width,
                coded_height: desc.height,
                display_width: self.target_width,
                display_height: self.target_height,
                planes: (0..layer.num_planes as usize)
                    .map(|i| crate::format::DmaBufPlaneInfo {
                        offset: layer.offset[i],
                        pitch: layer.pitch[i],
                    })
                    .collect(),
            };

            Ok(result)
        }
    }
}

impl Drop for VppColorConverter {
    fn drop(&mut self) {
        unsafe {
            va::vaDestroyConfig(self.dpy, self.config_id);
            va::vaTerminate(self.dpy);
        }
    }
}

// Safety: VppColorConverter is only accessed from the encoder's single thread.
unsafe impl Send for VppColorConverter {}

/// The concrete H264 VAAPI encoder type.
type VaapiH264Encoder =
    H264StatelessEncoder<VaapiInputFrame, VaapiBackend<VaapiInputFrame, Surface<VaapiInputFrame>>>;

/// VAAPI hardware-accelerated H.264 encoder for Linux.
///
/// Uses the `cros-codecs` crate to interface with the VA-API backend.
/// Accepts DMA-BUF GPU frames for zero-copy encode, or falls back to
/// CPU NV12 conversion for RGBA/BGRA/I420 input.
#[derive(derive_more::Debug)]
pub struct VaapiEncoder {
    #[debug(skip)]
    encoder: VaapiH264Encoder,
    frame_layout: FrameLayout,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// avcC description, populated after first keyframe (avcC mode only).
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: std::collections::VecDeque<EncodedFrame>,
    /// Whether the frame input path has been logged (log once on first frame).
    logged_frame_path: bool,
    /// VPP color-space converter for non-NV12 DMA-BUF frames.
    /// Lazily initialized on first non-NV12 DMA-BUF frame.
    #[debug(skip)]
    vpp: Option<VppColorConverter>,
    /// Set when VPP init or conversion fails — prevents re-init every frame.
    vpp_disabled: bool,
}

/// Computes the H.264 level that minimizes decoder DPB buffering.
///
/// The decoder derives its DPB (decoded picture buffer) size from the level.
/// A too-high level (e.g. Level 4.0 for 360p) causes the decoder to allocate
/// 16+ DPB slots and buffer that many frames before output, adding ~500ms of
/// latency. The cros-codecs encoder sets `max_num_ref_frames=1` in the SPS
/// but doesn't set VUI `max_dec_frame_buffering`, so decoders fall back to
/// the level-derived DPB size.
///
/// We pick the level whose `max_dpb_mbs / frame_mbs` yields the smallest
/// DPB that's still >= 2 (1 reference + 1 output). The level is a signaling
/// field — the VAAPI hardware encodes whatever resolution is given regardless.
fn min_dpb_h264_level(width: u32, height: u32) -> Level {
    let w_mb = width.div_ceil(16);
    let h_mb = height.div_ceil(16);
    let frame_mbs = w_mb * h_mb;

    // H.264 Table A-1: (max_dpb_mbs, Level)
    // Sorted by max_dpb_mbs ascending.
    let levels: &[(u32, Level)] = &[
        (396, Level::L1),
        (900, Level::L1_1),
        (2376, Level::L1_2),
        (2376, Level::L1_3),
        (2376, Level::L2_0),
        (4752, Level::L2_1),
        (8100, Level::L2_2),
        (8100, Level::L3),
        (18000, Level::L3_1),
        (20480, Level::L3_2),
        (32768, Level::L4),
        (32768, Level::L4_1),
        (34816, Level::L4_2),
        (110400, Level::L5),
    ];

    for &(max_dpb_mbs, level) in levels {
        // DPB slots = max_dpb_mbs / frame_mbs, need >= 2 for decode to work.
        let dpb = max_dpb_mbs / frame_mbs;
        if dpb >= 2 {
            return level;
        }
    }
    Level::L5
}

impl VaapiEncoder {
    /// Create a VAAPI H.264 encoder instance with fresh state (counter=0).
    fn create_encoder(
        width: u32,
        height: u32,
        framerate: u32,
        bitrate: u64,
        keyframe_interval: u32,
    ) -> Result<(VaapiH264Encoder, bool)> {
        let display =
            Display::open().context("failed to open VAAPI display — no GPU or driver found")?;

        let coded_size = Resolution { width, height };
        let fourcc = Fourcc::from(b"NV12");

        let entrypoints = display
            .query_config_entrypoints(VAProfile::VAProfileH264ConstrainedBaseline)
            .unwrap_or_default();
        let low_power = entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP);

        let level = min_dpb_h264_level(width, height);

        let config = EncoderConfig {
            resolution: coded_size,
            level,
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(bitrate),
                framerate,
                // Constrain QP range to prevent the hardware rate controller from
                // producing large quality swings between IDR and P-frames.
                // Default range (1–51) lets the RC spike QP on keyframes, causing
                // a visible "compression burst" every keyframe interval.
                min_quality: 18,
                max_quality: 36,
            },
            // H.264 requires log2_max_frame_num_minus4 >= 0, so max_frame_num >= 16.
            // cros-codecs derives max_frame_num from the LowDelay limit, so clamp it.
            pred_structure: PredictionStructure::LowDelay {
                limit: keyframe_interval.max(16) as u16,
            },
            ..EncoderConfig::default()
        };

        let encoder = VaapiH264Encoder::new_vaapi(
            display,
            config,
            fourcc,
            coded_size,
            low_power,
            BlockingMode::Blocking,
        )
        .map_err(|e| anyhow::anyhow!("failed to create VAAPI H.264 encoder: {e:?}"))?;

        Ok((encoder, low_power))
    }

    /// Bits-per-pixel factor for H.264 default bitrate calculation.
    const H264_BPP: f32 = 0.07;

    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
        let keyframe_interval = config.keyframe_interval_or_default();
        let nal_format = config.nal_format;

        let coded_size = Resolution { width, height };
        let fourcc = Fourcc::from(b"NV12");

        let frame_layout = FrameLayout {
            format: (fourcc, 0),
            size: coded_size,
            planes: vec![
                PlaneLayout {
                    buffer_index: 0,
                    offset: 0,
                    stride: width as usize,
                },
                PlaneLayout {
                    buffer_index: 0,
                    offset: (width * height) as usize,
                    stride: width as usize,
                },
            ],
        };

        let avcc = if nal_format == NalFormat::Avcc {
            // Extract avcC by priming a temporary encoder with a black IDR frame.
            // cros-codecs only emits SPS/PPS in the first IDR (counter=0).
            let (mut primer, _) =
                Self::create_encoder(width, height, framerate, bitrate, keyframe_interval)?;
            let yuv = YuvData::black(width, height);
            let nv12 = Self::i420_to_nv12(&yuv.y, &yuv.u, &yuv.v, width, height);
            let black = VaapiInputFrame::Nv12 {
                data: nv12,
                width,
                height,
            };
            let meta = FrameMetadata {
                timestamp: 0,
                layout: frame_layout.clone(),
                force_keyframe: true,
            };
            primer
                .encode(meta, black)
                .map_err(|e| anyhow::anyhow!("VAAPI priming encode failed: {e:?}"))?;

            let mut avcc = None;
            while let Some(coded) = primer
                .poll()
                .map_err(|e| anyhow::anyhow!("VAAPI priming poll failed: {e:?}"))?
            {
                if avcc.is_none() {
                    let nals = parse_annex_b(&coded.bitstream);
                    if let Some((sps, pps)) = extract_sps_pps(&nals) {
                        avcc = Some(build_avcc(&sps, &pps));
                    }
                }
            }
            avcc
        } else {
            // Annex B mode: SPS/PPS are inline in keyframes, no priming needed.
            None
        };

        // Create a fresh encoder for actual use.
        let (encoder, _) =
            Self::create_encoder(width, height, framerate, bitrate, keyframe_interval)?;

        tracing::info!(
            width,
            height,
            framerate,
            bitrate,
            "H.264 hardware encoder ready (VAAPI)"
        );

        Ok(Self {
            encoder,
            frame_layout,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            avcc,
            packet_buf: std::collections::VecDeque::new(),
            logged_frame_path: false,
            vpp: None,
            vpp_disabled: false,
        })
    }

    /// Convert I420 planar YUV to NV12 semi-planar format.
    /// NV12 = Y plane followed by interleaved UV plane.
    /// Converts a `VideoFrame` to an NV12 `VaapiInputFrame` via CPU path
    /// (scale + color convert).
    fn build_nv12_input(&mut self, frame: VideoFrame) -> Result<VaapiInputFrame> {
        let frame = self.scale_if_needed(frame)?;
        let [w, h] = frame.dimensions;
        let nv12_data = match &frame.data {
            crate::format::FrameData::Packed { pixel_format, data } => {
                pixel_format_to_nv12(data, w, h, *pixel_format)?
            }
            _ => {
                let img = frame.rgba_image();
                pixel_format_to_nv12(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
            }
        };
        Ok(VaapiInputFrame::Nv12 {
            data: nv12_data.into_contiguous(),
            width: w,
            height: h,
        })
    }

    /// Attempts VPP GPU color conversion for a non-NV12 DMA-BUF frame.
    /// Falls back to CPU NV12 upload if VPP init or conversion fails.
    fn vpp_convert_or_cpu(
        &mut self,
        info: DmaBufInfo,
        frame: VideoFrame,
    ) -> Result<VaapiInputFrame> {
        // Permanently disabled after a failed attempt.
        if self.vpp_disabled {
            return self.build_nv12_input(frame);
        }

        // Lazy-init VPP converter.
        if self.vpp.is_none() {
            match VppColorConverter::new(self.width, self.height) {
                Ok(vpp) => self.vpp = Some(vpp),
                Err(e) => {
                    self.vpp_disabled = true;
                    let fourcc_bytes = info.drm_format.to_le_bytes();
                    let fourcc_str = std::str::from_utf8(&fourcc_bytes).unwrap_or("????");
                    tracing::info!(
                        drm_format = fourcc_str,
                        error = %e,
                        "VAAPI encode: VPP init failed, using CPU NV12 upload"
                    );
                    self.logged_frame_path = true;
                    return self.build_nv12_input(frame);
                }
            }
        }

        let vpp = self.vpp.as_ref().unwrap();
        match vpp.convert(&info) {
            Ok(nv12_info) => {
                if !self.logged_frame_path {
                    let fourcc_bytes = info.drm_format.to_le_bytes();
                    let fourcc_str = std::str::from_utf8(&fourcc_bytes).unwrap_or("????");
                    tracing::info!(
                        drm_format = fourcc_str,
                        "VAAPI encode: zero-copy VPP convert ({fourcc_str} → NV12)"
                    );
                    self.logged_frame_path = true;
                }
                Ok(VaapiInputFrame::DmaBuf(nv12_info))
            }
            Err(e) => {
                // Disable VPP permanently — don't re-init every frame.
                self.vpp_disabled = true;
                self.vpp = None;
                tracing::warn!(
                    error = %e,
                    "VAAPI encode: VPP convert failed, falling back to CPU NV12 upload"
                );
                self.logged_frame_path = true;
                self.build_nv12_input(frame)
            }
        }
    }

    /// Scales the frame to encoder dimensions if needed, based on the
    /// configured [`ScaleMode`].
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        let [fw, fh] = frame.dimensions;
        if fw == self.width && fh == self.height {
            return Ok(frame);
        }
        let (tw, th) = self.scale_mode.resolve((fw, fh), (self.width, self.height));
        if tw == fw && th == fh {
            return Ok(frame);
        }
        self.scaler.set_target_dimensions(tw, th);
        let img = frame.rgba_image();
        let scaled = if self.scale_mode == ScaleMode::Cover {
            self.scaler.scale_cover_rgba(img.as_raw(), fw, fh)?
        } else {
            self.scaler.scale_rgba(img.as_raw(), fw, fh)?
        };
        match scaled {
            Some((data, w, h)) => Ok(VideoFrame::new_rgba(data.into(), w, h, frame.timestamp)),
            None => Ok(frame),
        }
    }

    fn i420_to_nv12(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);

        let y_size = w * h;
        let uv_size = chroma_w * 2 * chroma_h;
        let mut nv12 = vec![0u8; y_size + uv_size];

        // Copy Y plane
        nv12[..y_size.min(y.len())].copy_from_slice(&y[..y_size.min(y.len())]);

        // Interleave U and V into UV plane
        let uv_dst = &mut nv12[y_size..];
        for row in 0..chroma_h {
            let row_offset = row * chroma_w;
            let dst_offset = row * chroma_w * 2;
            for col in 0..chroma_w {
                let idx = row_offset + col;
                let out = dst_offset + col * 2;
                uv_dst[out] = u[idx];
                uv_dst[out + 1] = v[idx];
            }
        }

        nv12
    }

    /// Process a `CodedBitstreamBuffer` into an `EncodedFrame`.
    fn process_coded_output(
        &mut self,
        coded: CodedBitstreamBuffer,
    ) -> Result<Option<EncodedFrame>> {
        // Strip trailing zero-padding from the VA coded buffer. The VAAPI driver
        // pads coded buffers to alignment boundaries, but the H.264 spec guarantees
        // RBSP trailing bits end with a non-zero byte, so trailing zeros are safe to
        // remove. Without this, parse_annex_b includes the padding in the last NAL,
        // causing the decoder to choke on the zero bytes after consuming the real data.
        let annex_b_end = coded
            .bitstream
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |p| p + 1);
        let annex_b = &coded.bitstream[..annex_b_end];
        if annex_b.is_empty() {
            return Ok(None);
        }

        // Detect keyframe by scanning NAL types for IDR (type 5).
        let nals = parse_annex_b(annex_b);
        let is_keyframe = nals
            .iter()
            .any(|nal| !nal.is_empty() && (nal[0] & 0x1F) == 5);

        // In avcC mode, extract SPS/PPS on first keyframe.
        if self.nal_format == NalFormat::Avcc
            && is_keyframe
            && self.avcc.is_none()
            && let Some((sps, pps)) = extract_sps_pps(&nals)
        {
            self.avcc = Some(build_avcc(&sps, &pps));
        }

        let payload: bytes::Bytes = match self.nal_format {
            NalFormat::AnnexB => annex_b.to_vec().into(),
            NalFormat::Avcc => annex_b_to_length_prefixed(annex_b).into(),
        };

        let timestamp_us = coded.metadata.timestamp;

        Ok(Some(EncodedFrame {
            is_keyframe,
            timestamp: std::time::Duration::from_micros(timestamp_us),
            payload,
        }))
    }
}

impl VideoEncoderFactory for VaapiEncoder {
    const ID: &str = "h264-vaapi";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
        let inline = config.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline,
            }),
            description: None,
            coded_width: Some(config.width),
            coded_height: Some(config.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(bitrate),
            framerate: Some(config.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }
}

impl VideoEncoder for VaapiEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
                inline,
            }),
            description: self.avcc.clone().map(Into::into),
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.bitrate),
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        const NV12_FOURCC: u32 = u32::from_le_bytes(*b"NV12");

        // Check for zero-copy DMA-BUF path before any CPU-side scaling.
        let input_frame = if let Some(NativeFrameHandle::DmaBuf(info)) = frame.native_handle() {
            if info.drm_format == NV12_FOURCC
                && info.coded_width == self.width
                && info.coded_height == self.height
                && info.planes.len() <= 4
            {
                if !self.logged_frame_path {
                    tracing::info!("VAAPI encode: zero-copy DMA-BUF import (no CPU copy)");
                    self.logged_frame_path = true;
                }
                VaapiInputFrame::DmaBuf(info)
            } else {
                // Non-NV12 or dimension mismatch — try VPP GPU conversion.
                self.vpp_convert_or_cpu(info, frame)?
            }
        } else {
            if !self.logged_frame_path {
                tracing::info!("VAAPI encode: CPU NV12 upload (no DMA-BUF available)");
                self.logged_frame_path = true;
            }
            self.build_nv12_input(frame)?
        };

        let w = input_frame.width();
        let h = input_frame.height();

        // Build frame metadata with layout matching the input frame.
        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        let layout = match &input_frame {
            VaapiInputFrame::DmaBuf(info) => {
                // Only NV12 DMA-BUFs reach here (format checked in push_frame).
                FrameLayout {
                    format: (Fourcc::from(b"NV12"), info.modifier),
                    size: Resolution {
                        width: w,
                        height: h,
                    },
                    planes: info
                        .planes
                        .iter()
                        .map(|p| PlaneLayout {
                            buffer_index: 0,
                            offset: p.offset as usize,
                            stride: p.pitch as usize,
                        })
                        .collect(),
                }
            }
            VaapiInputFrame::Nv12 { .. } if w == self.width && h == self.height => {
                self.frame_layout.clone()
            }
            VaapiInputFrame::Nv12 { .. } => FrameLayout {
                format: self.frame_layout.format,
                size: Resolution {
                    width: w,
                    height: h,
                },
                planes: vec![
                    PlaneLayout {
                        buffer_index: 0,
                        offset: 0,
                        stride: w as usize,
                    },
                    PlaneLayout {
                        buffer_index: 0,
                        offset: (w * h) as usize,
                        stride: w as usize,
                    },
                ],
            },
        };
        let meta = FrameMetadata {
            timestamp: timestamp_us,
            layout,
            force_keyframe: false,
        };

        // Submit frame to encoder.
        self.encoder
            .encode(meta, input_frame)
            .map_err(|e| anyhow::anyhow!("VAAPI encode failed: {e:?}"))?;

        self.frame_count += 1;

        // Poll for any completed output.
        while let Some(coded) = self
            .encoder
            .poll()
            .map_err(|e| anyhow::anyhow!("VAAPI poll failed: {e:?}"))?
        {
            if let Some(pkt) = self.process_coded_output(coded)? {
                self.packet_buf.push_back(pkt);
            }
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        Ok(self.packet_buf.pop_front())
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        // Drain remaining buffered frames.
        if let Err(e) = self.encoder.drain() {
            tracing::warn!("VAAPI encoder drain failed on drop: {e}");
            return;
        }
        while let Ok(Some(coded)) = self.encoder.poll() {
            if let Ok(Some(pkt)) = self.process_coded_output(coded) {
                self.packet_buf.push_back(pkt);
            }
        }
    }
}

// VaapiEncoder is Send: the libva Display and surfaces use Rc internally,
// but our encoder is only accessed from a single thread at a time.
// The cros-codecs StatelessEncoder is designed for single-threaded use.
// Safety: We ensure no concurrent access to the encoder.
unsafe impl Send for VaapiEncoder {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame,
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
    };

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_avcc_available_at_construction() {
        let enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let desc = enc.config().description;
        assert!(
            desc.is_some(),
            "avcC should be populated at construction time"
        );
        let avcc = desc.unwrap();
        assert_eq!(avcc[0], 1, "avcC should start with version 1");
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_basic() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut packet_count = 0;
        for _ in 0..30 {
            let frame = make_rgba_frame(640, 360, 255, 0, 0);
            enc.push_frame(frame).unwrap();
            while let Some(_pkt) = enc.pop_packet().unwrap() {
                packet_count += 1;
            }
        }
        assert!(
            packet_count > 0,
            "expected at least 1 packet, got {packet_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_decode_roundtrip() {
        use crate::{codec::h264::H264VideoDecoder, format::DecodeConfig, traits::VideoDecoder};

        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut packets = Vec::new();
        for _ in 0..30 {
            let frame = make_rgba_frame(640, 360, 200, 100, 50);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        assert!(!packets.is_empty(), "should have produced packets");

        let config = enc.config();
        assert!(
            config.description.is_some(),
            "avcC should be populated after encoding"
        );

        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();
        let mut decoded_count = 0;
        let ordered = crate::codec::test_util::encoded_frames_to_media_packets(packets);
        for pkt in ordered {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                assert_eq!(frame.img().width(), 640);
                assert_eq!(frame.img().height(), 360);
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_keyframe_interval() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut keyframe_count = 0;
        for _ in 0..60 {
            let frame = make_rgba_frame(640, 360, 128, 128, 128);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                if pkt.is_keyframe {
                    keyframe_count += 1;
                }
            }
        }
        assert!(
            keyframe_count >= 2,
            "expected >= 2 keyframes in 60 frames, got {keyframe_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_timestamps_increase() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P180).unwrap();
        let mut prev_ts = None;
        for _ in 0..10 {
            let frame = make_rgba_frame(320, 180, 64, 64, 64);
            enc.push_frame(frame).unwrap();
            if let Some(pkt) = enc.pop_packet().unwrap() {
                if let Some(prev) = prev_ts {
                    assert!(pkt.timestamp > prev, "timestamps should increase");
                }
                prev_ts = Some(pkt.timestamp);
            }
        }
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_config_fields() {
        let enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::H264(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
        assert_eq!(config.optimize_for_latency, Some(true));
    }
}
