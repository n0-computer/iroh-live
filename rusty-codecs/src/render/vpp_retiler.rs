//! VAAPI Video Post-Processing retiler.
//!
//! Uses a VPP identity blit to convert decoded surfaces from an incompatible
//! tiling modifier (e.g. Y_TILED on Intel MTL) to one that the GPU's render
//! engine can import. The VPP runs on the SFC fixed-function hardware, which
//! sits between the media decode engine and the render engine — this is Intel's
//! intended path for tiling conversion.
//!
//! Shared by both the wgpu (Vulkan DMA-BUF import) and GLES (EGL DMA-BUF
//! import) rendering paths.
//!
//! Requires the `vaapi` feature.

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use anyhow::Result;
use cros_codecs::libva as va;
use tracing::debug;

use crate::format::{DmaBufInfo, DmaBufPlaneInfo};

/// VAProcPipelineParameterBuffer from va/va_vpp.h.
///
/// Not included in cros-libva's generated bindings. For an identity blit
/// (format/tiling conversion), only `surface` needs to be set.
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

/// VAAPI VPP retiler.
///
/// Uses a VPP identity blit to convert decoded surfaces from an incompatible
/// tiling modifier to one the render engine can import. Caches the raw VA
/// display and VPP config across frames.
pub struct VppRetiler {
    dpy: va::VADisplay,
    config_id: va::VAConfigID,
    _drm_file: File,
}

impl std::fmt::Debug for VppRetiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VppRetiler").finish_non_exhaustive()
    }
}

impl VppRetiler {
    /// Creates a VPP retiler, preferring the given render node path if
    /// provided, then falling back to common render node paths.
    pub fn new(preferred_render_node: Option<&str>) -> Result<Self> {
        // DRI render nodes are numbered from 128. Enumerate a reasonable range
        // to support multi-GPU and renamed nodes.
        let fallback_paths: Vec<String> =
            (128..136).map(|i| format!("/dev/dri/renderD{i}")).collect();
        let fallbacks: Vec<&str> = fallback_paths.iter().map(|s| s.as_str()).collect();
        let candidates: Vec<&str> = preferred_render_node
            .into_iter()
            .chain(fallbacks.into_iter())
            .collect();

        unsafe {
            for path in candidates {
                if let Ok(file) = File::options().read(true).write(true).open(path) {
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

                    let mut rt_attr = va::VAConfigAttrib {
                        type_: va::VAConfigAttribType::VAConfigAttribRTFormat,
                        value: 0,
                    };
                    va_check(
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
                    va_check(
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

                    debug!("VPP retiler initialized on {path}");
                    return Ok(Self {
                        dpy,
                        config_id,
                        _drm_file: file,
                    });
                }
            }
            Err(anyhow::anyhow!("no VA display found for VPP retiler"))
        }
    }

    /// Re-tiles a DMA-BUF surface via VPP identity blit.
    ///
    /// Uses an inner function + cleanup-on-error pattern to ensure VA resources
    /// are always released, even when intermediate steps fail.
    pub fn retile(&self, info: &DmaBufInfo) -> Result<DmaBufInfo> {
        // Track allocated VA resources for cleanup on error.
        struct VppResources {
            dpy: va::VADisplay,
            input_surface: va::VASurfaceID,
            output_surface: va::VASurfaceID,
            context_id: va::VAContextID,
            buf_id: va::VABufferID,
        }

        impl VppResources {
            fn cleanup(&mut self) {
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

        impl Drop for VppResources {
            fn drop(&mut self) {
                self.cleanup();
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

            // Import decoded DMA-BUF as VA surface using DRM_PRIME_2 (preserves modifier).
            let fd_dup = libc::dup(info.fd.as_raw_fd());
            if fd_dup < 0 {
                return Err(anyhow::anyhow!(
                    "dup input fd: {}",
                    std::io::Error::last_os_error()
                ));
            }

            {
                // Build a PRIME2 descriptor matching the decoded surface layout.
                let mut prime_desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
                prime_desc.fourcc = info.drm_format;
                prime_desc.width = info.coded_width;
                prime_desc.height = info.coded_height;
                prime_desc.num_objects = 1;
                prime_desc.objects[0].fd = fd_dup;
                prime_desc.objects[0].size = 0; // driver calculates
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
                    &mut res.input_surface,
                    1,
                    attribs.as_mut_ptr(),
                    attribs.len() as u32,
                );
                // Close the dup'd FD — the VA driver imports the DMA-BUF handle
                // during vaCreateSurfaces and does not take ownership of the FD.
                libc::close(fd_dup);
                if status != va::VA_STATUS_SUCCESS as i32 {
                    res.input_surface = 0; // not created
                    return Err(anyhow::anyhow!(
                        "vaCreateSurfaces(VPP input) failed: VA status {status}"
                    ));
                }
            }

            // Create output surface with VPP_WRITE + EXPORT hints.
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

                va_check(
                    va::vaCreateSurfaces(
                        self.dpy,
                        va::VA_RT_FORMAT_YUV420,
                        info.coded_width,
                        info.coded_height,
                        &mut res.output_surface,
                        1,
                        attribs.as_mut_ptr(),
                        attribs.len() as u32,
                    ),
                    "vaCreateSurfaces(VPP output)",
                )?;
            }

            // Create VPP context.
            va_check(
                va::vaCreateContext(
                    self.dpy,
                    self.config_id,
                    info.coded_width as i32,
                    info.coded_height as i32,
                    0,
                    &mut res.output_surface,
                    1,
                    &mut res.context_id,
                ),
                "vaCreateContext(VPP)",
            )?;

            // Create pipeline parameter buffer (identity blit).
            let mut pipeline_param: VaProcPipelineParameterBuffer = std::mem::zeroed();
            pipeline_param.surface = res.input_surface;

            va_check(
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
            va_check(
                va::vaBeginPicture(self.dpy, res.context_id, res.output_surface),
                "vaBeginPicture(VPP)",
            )?;
            va_check(
                va::vaRenderPicture(self.dpy, res.context_id, &mut res.buf_id, 1),
                "vaRenderPicture(VPP)",
            )?;
            va_check(
                va::vaEndPicture(self.dpy, res.context_id),
                "vaEndPicture(VPP)",
            )?;
            va_check(
                va::vaSyncSurface(self.dpy, res.output_surface),
                "vaSyncSurface(VPP)",
            )?;

            // Export output surface as PRIME2.
            let mut desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
            va_check(
                va::vaExportSurfaceHandle(
                    self.dpy,
                    res.output_surface,
                    va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                    va::VA_EXPORT_SURFACE_READ_ONLY | va::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                    &mut desc as *mut _ as *mut std::ffi::c_void,
                ),
                "vaExportSurfaceHandle(VPP output)",
            )?;

            // Take ownership of the primary object FD. Close any additional
            // object FDs (e.g. CCS aux planes) to prevent per-frame FD leaks.
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
                display_width: info.display_width,
                display_height: info.display_height,
                planes: (0..layer.num_planes as usize)
                    .map(|i| DmaBufPlaneInfo {
                        offset: layer.offset[i],
                        pitch: layer.pitch[i],
                    })
                    .collect(),
            };

            // Cleanup VA resources (exported FD keeps the buffer alive).
            // Drop guard handles this — explicit cleanup so Drop doesn't double-free.
            res.cleanup();
            std::mem::forget(res);

            Ok(result)
        }
    }
}

impl Drop for VppRetiler {
    fn drop(&mut self) {
        unsafe {
            va::vaDestroyConfig(self.dpy, self.config_id);
            va::vaTerminate(self.dpy);
        }
    }
}

/// Checks a VAStatus and returns an error if not `VA_STATUS_SUCCESS`.
fn va_check(status: va::VAStatus, op: &str) -> Result<()> {
    if status != va::VA_STATUS_SUCCESS as i32 {
        Err(anyhow::anyhow!("{op} failed: VA status {status}"))
    } else {
        Ok(())
    }
}
