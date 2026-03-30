//! VAAPI VPP hardware scaler for NV12 surfaces.
//!
//! Scales NV12 DMA-BUF frames to a different resolution on the GPU using the
//! VA-API Video Processing Pipeline. The output surface and VPP context are
//! cached across frames, eliminating per-frame Vulkan resource churn that the
//! `VppColorConverter` (which creates/destroys per call) cannot avoid.
//!
//! Typical use: the encoder receives a 1080p DMA-BUF from PipeWire capture
//! but targets 720p. The VPP scaler runs a single GPU blit to produce a
//! 720p NV12 surface, which the encoder imports directly.

use std::os::fd::AsRawFd;

use anyhow::Result;
use cros_codecs::libva as va;

use super::encoder::{VaProcPipelineParameterBuffer, vpp_va_check};
use crate::format::{DmaBufInfo, DmaBufPlaneInfo};

/// VAAPI VPP hardware scaler for NV12 surfaces.
///
/// Caches the VA display, VPP config, context, and output surface across
/// frames. Only recreates when the source or destination dimensions change.
pub(crate) struct VppScaler {
    dpy: va::VADisplay,
    config_id: va::VAConfigID,
    context_id: va::VAContextID,
    /// Output surface, reused across frames.
    output_surface: va::VASurfaceID,
    _drm_file: std::fs::File,
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
}

impl VppScaler {
    /// Creates a VPP scaler for the given source and destination dimensions.
    ///
    /// Opens the first available VA display, checks VPP support, and
    /// pre-allocates the output surface. Returns an error if no VA display
    /// supports VPP.
    pub(crate) fn new(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Result<Self> {
        // SAFETY: All VA-API calls operate on freshly-opened display handles.
        // Error codes are checked after each call.
        unsafe {
            let render_paths: Vec<String> =
                (128..136).map(|i| format!("/dev/dri/renderD{i}")).collect();

            for path in &render_paths {
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
                    "vaGetConfigAttributes(VPP scale)",
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
                    "vaCreateConfig(VPP scale)",
                )?;

                // Create output NV12 surface at destination dimensions.
                let mut output_surface: va::VASurfaceID = 0;
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
                        | va::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER)
                        as i32;

                    vpp_va_check(
                        va::vaCreateSurfaces(
                            dpy,
                            va::VA_RT_FORMAT_YUV420,
                            dst_w,
                            dst_h,
                            &mut output_surface,
                            1,
                            attribs.as_mut_ptr(),
                            attribs.len() as u32,
                        ),
                        "vaCreateSurfaces(VPP scale output)",
                    )?;
                }

                // Create VPP context.
                let mut context_id: va::VAContextID = 0;
                vpp_va_check(
                    va::vaCreateContext(
                        dpy,
                        config_id,
                        dst_w as i32,
                        dst_h as i32,
                        0,
                        &mut output_surface,
                        1,
                        &mut context_id,
                    ),
                    "vaCreateContext(VPP scale)",
                )?;

                tracing::info!(
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    render_node = %path,
                    "VPP hardware scaler initialized"
                );

                return Ok(Self {
                    dpy,
                    config_id,
                    context_id,
                    output_surface,
                    _drm_file: file,
                    src_width: src_w,
                    src_height: src_h,
                    dst_width: dst_w,
                    dst_height: dst_h,
                });
            }

            Err(anyhow::anyhow!(
                "no VA display found for VPP hardware scaler"
            ))
        }
    }

    /// Returns the source dimensions this scaler was configured for.
    pub(crate) fn src_dims(&self) -> (u32, u32) {
        (self.src_width, self.src_height)
    }

    /// Returns the destination dimensions this scaler was configured for.
    pub(crate) fn dst_dims(&self) -> (u32, u32) {
        (self.dst_width, self.dst_height)
    }

    /// Scales an NV12 DMA-BUF to the destination dimensions via VPP blit.
    ///
    /// Imports the input DMA-BUF as a VA surface, runs the VPP pipeline to
    /// scale it onto the cached output surface, then exports the result as
    /// a new DMA-BUF.
    pub(crate) fn scale(&self, info: &DmaBufInfo) -> Result<DmaBufInfo> {
        // SAFETY: All VA-API calls use self.dpy (valid from new()) and
        // surface IDs allocated in this scope or in new(). The input
        // surface is cleaned up via `vaDestroySurfaces` before return.
        unsafe {
            // Import input DMA-BUF as VA surface.
            let mut input_surface: va::VASurfaceID = 0;
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
                    va::VA_RT_FORMAT_YUV420,
                    info.coded_width,
                    info.coded_height,
                    &mut input_surface,
                    1,
                    attribs.as_mut_ptr(),
                    attribs.len() as u32,
                );
                libc::close(fd_dup);
                if status != va::VA_STATUS_SUCCESS as i32 {
                    return Err(anyhow::anyhow!(
                        "vaCreateSurfaces(VPP scale input) failed: VA status {status}"
                    ));
                }
            }

            // Run VPP scaling pipeline.
            let scale_result = self.run_vpp_pipeline(input_surface);

            // Always destroy the input surface.
            va::vaDestroySurfaces(self.dpy, &mut input_surface, 1);

            scale_result?;

            // Export scaled output as DRM_PRIME_2.
            let mut desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
            vpp_va_check(
                va::vaExportSurfaceHandle(
                    self.dpy,
                    self.output_surface,
                    va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                    va::VA_EXPORT_SURFACE_READ_ONLY | va::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                    &mut desc as *mut _ as *mut std::ffi::c_void,
                ),
                "vaExportSurfaceHandle(VPP scale output)",
            )?;

            // Take ownership of the primary FD, close any extras.
            use std::os::fd::{FromRawFd, OwnedFd};
            let primary_fd = OwnedFd::from_raw_fd(desc.objects[0].fd);
            for i in 1..desc.num_objects as usize {
                libc::close(desc.objects[i].fd);
            }

            let layer = &desc.layers[0];
            Ok(DmaBufInfo {
                fd: primary_fd,
                modifier: desc.objects[0].drm_format_modifier,
                drm_format: desc.fourcc,
                coded_width: desc.width,
                coded_height: desc.height,
                display_width: self.dst_width,
                display_height: self.dst_height,
                planes: (0..layer.num_planes as usize)
                    .map(|i| DmaBufPlaneInfo {
                        offset: layer.offset[i],
                        pitch: layer.pitch[i],
                    })
                    .collect(),
            })
        }
    }

    /// Runs the VPP pipeline: submits the input surface to the cached
    /// context and waits for completion.
    ///
    /// # Safety
    ///
    /// `input_surface` must be a valid VASurfaceID on `self.dpy`.
    unsafe fn run_vpp_pipeline(&self, input_surface: va::VASurfaceID) -> Result<()> {
        let mut pipeline_param: VaProcPipelineParameterBuffer = unsafe { std::mem::zeroed() };
        pipeline_param.surface = input_surface;

        let mut buf_id: va::VABufferID = 0;
        vpp_va_check(
            unsafe {
                va::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    va::VABufferType::VAProcPipelineParameterBufferType,
                    std::mem::size_of::<VaProcPipelineParameterBuffer>() as u32,
                    1,
                    &mut pipeline_param as *mut _ as *mut std::ffi::c_void,
                    &mut buf_id,
                )
            },
            "vaCreateBuffer(VPP scale)",
        )?;

        let result = (|| -> Result<()> {
            vpp_va_check(
                unsafe { va::vaBeginPicture(self.dpy, self.context_id, self.output_surface) },
                "vaBeginPicture(VPP scale)",
            )?;
            vpp_va_check(
                unsafe { va::vaRenderPicture(self.dpy, self.context_id, &mut buf_id, 1) },
                "vaRenderPicture(VPP scale)",
            )?;
            vpp_va_check(
                unsafe { va::vaEndPicture(self.dpy, self.context_id) },
                "vaEndPicture(VPP scale)",
            )?;
            vpp_va_check(
                unsafe { va::vaSyncSurface(self.dpy, self.output_surface) },
                "vaSyncSurface(VPP scale)",
            )?;
            Ok(())
        })();

        unsafe { va::vaDestroyBuffer(self.dpy, buf_id) };
        result
    }
}

impl Drop for VppScaler {
    fn drop(&mut self) {
        // SAFETY: All IDs are valid from successful new() construction.
        unsafe {
            va::vaDestroyContext(self.dpy, self.context_id);
            va::vaDestroySurfaces(self.dpy, &mut self.output_surface, 1);
            va::vaDestroyConfig(self.dpy, self.config_id);
            va::vaTerminate(self.dpy);
        }
    }
}

// Safety: VppScaler is only accessed from the encoder's single thread.
unsafe impl Send for VppScaler {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scales a 1920x1080 test surface to 1280x720 via VPP.
    ///
    /// Verifies the output DmaBufInfo has the correct destination dimensions.
    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_vpp_scale_1080p_to_720p() {
        let scaler = VppScaler::new(1920, 1080, 1280, 720).expect("VPP scaler init");
        assert_eq!(scaler.src_dims(), (1920, 1080));
        assert_eq!(scaler.dst_dims(), (1280, 720));

        // Create a test NV12 surface by allocating a driver surface and exporting it.
        let test_info = create_test_nv12_dmabuf(1920, 1080);
        let scaled = scaler.scale(&test_info).expect("VPP scale");

        assert_eq!(scaled.display_width, 1280);
        assert_eq!(scaled.display_height, 720);
        assert!(scaled.coded_width >= 1280);
        assert!(scaled.coded_height >= 720);
        assert!(scaled.planes.len() >= 2, "NV12 needs at least 2 planes");
    }

    /// Encodes a test pattern at 1080p through a 720p VAAPI encoder, exercising
    /// the VPP scaling path. Verifies the encoder produces output.
    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_with_vpp_scaling() {
        use crate::{
            format::{NalFormat, VideoEncoderConfig, VideoFrame},
            traits::{VideoEncoder, VideoEncoderFactory},
        };

        // Create an encoder targeting 720p.
        let config = VideoEncoderConfig {
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: Some(2_000_000),
            keyframe_interval: Some(30),
            nal_format: NalFormat::AnnexB,
            scale_mode: crate::format::ScaleMode::default(),
        };
        let mut encoder =
            super::super::VaapiEncoder::with_config(config).expect("create VAAPI encoder");

        // Feed a 1080p RGBA frame. The encoder should use VPP to scale it
        // to 720p before encoding (CPU path, since this is RGBA not DMA-BUF,
        // but it validates the full pipeline accepts mismatched input).
        let rgba = vec![128u8; 1920 * 1080 * 4];
        let frame =
            VideoFrame::new_rgba(rgba.into(), 1920, 1080, std::time::Duration::from_millis(0));
        encoder.push_frame(frame).expect("push 1080p frame");

        assert_eq!(
            encoder.name(),
            "h264-vaapi",
            "encoder should identify as h264-vaapi"
        );

        // Push a second frame to flush the pipeline.
        let rgba2 = vec![64u8; 1920 * 1080 * 4];
        let frame2 = VideoFrame::new_rgba(
            rgba2.into(),
            1920,
            1080,
            std::time::Duration::from_millis(33),
        );
        encoder.push_frame(frame2).expect("push second frame");

        // Drain all available packets.
        let mut packet_count = 0;
        while let Some(_pkt) = encoder.pop_packet().expect("pop_packet") {
            packet_count += 1;
        }
        assert!(
            packet_count > 0,
            "encoder should produce at least one packet"
        );
    }

    /// Creates a test NV12 DMA-BUF by allocating a VA surface and exporting it.
    fn create_test_nv12_dmabuf(width: u32, height: u32) -> DmaBufInfo {
        unsafe {
            let render_paths: Vec<String> =
                (128..136).map(|i| format!("/dev/dri/renderD{i}")).collect();

            for path in &render_paths {
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

                let mut surface: va::VASurfaceID = 0;
                let mut attribs: [va::VASurfaceAttrib; 1] = std::mem::zeroed();
                attribs[0].type_ = va::VASurfaceAttribType::VASurfaceAttribPixelFormat;
                attribs[0].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[0].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[0].value.value.i = va::VA_FOURCC_NV12 as i32;

                let st = va::vaCreateSurfaces(
                    dpy,
                    va::VA_RT_FORMAT_YUV420,
                    width,
                    height,
                    &mut surface,
                    1,
                    attribs.as_mut_ptr(),
                    1,
                );
                assert_eq!(st, va::VA_STATUS_SUCCESS as i32, "vaCreateSurfaces failed");

                let mut desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
                let st = va::vaExportSurfaceHandle(
                    dpy,
                    surface,
                    va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                    va::VA_EXPORT_SURFACE_READ_ONLY | va::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                    &mut desc as *mut _ as *mut std::ffi::c_void,
                );
                assert_eq!(
                    st,
                    va::VA_STATUS_SUCCESS as i32,
                    "vaExportSurfaceHandle failed"
                );

                use std::os::fd::{FromRawFd, OwnedFd};
                let fd = OwnedFd::from_raw_fd(desc.objects[0].fd);
                for i in 1..desc.num_objects as usize {
                    libc::close(desc.objects[i].fd);
                }

                let layer = &desc.layers[0];
                let info = DmaBufInfo {
                    fd,
                    modifier: desc.objects[0].drm_format_modifier,
                    drm_format: desc.fourcc,
                    coded_width: desc.width,
                    coded_height: desc.height,
                    display_width: width,
                    display_height: height,
                    planes: (0..layer.num_planes as usize)
                        .map(|i| DmaBufPlaneInfo {
                            offset: layer.offset[i],
                            pitch: layer.pitch[i],
                        })
                        .collect(),
                };

                // Don't destroy surface/display — the exported FD keeps the buffer alive.
                // This leaks the VA display in tests, which is acceptable.
                return info;
            }

            panic!("no VA display available for test");
        }
    }
}
