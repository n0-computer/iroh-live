//! Zero-copy DMA-BUF → EGL/GLES texture import for Linux.
//!
//! Imports NV12 DMA-BUFs directly as GL textures using:
//! - `EGL_EXT_image_dma_buf_import` + `EGL_EXT_image_dma_buf_import_modifiers`
//! - `glEGLImageTargetTexture2DOES` to bind EGLImages as `GL_TEXTURE_EXTERNAL_OES`
//!
//! Each NV12 frame produces two EGLImages: one for the Y plane (R8) and one
//! for the UV plane (RG8). These are bound to the existing NV12 shader
//! program in [`super::gles::GlesRenderer`].
//!
//! Requires the `gles-dmabuf` feature (implies `gles`).

use std::{
    ffi::{CStr, c_void},
    os::fd::AsRawFd,
    sync::OnceLock,
};

use anyhow::{Context as _, Result, bail};
use glow::HasContext;
use tracing::{debug, info, trace, warn};

use crate::format::DmaBufInfo;

// ── EGL constants ────────────────────────────────────────────────────

const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_NONE: i32 = 0x3038;
const EGL_TRUE: i32 = 1;
const EGL_NO_CONTEXT: *mut c_void = std::ptr::null_mut();
/// DRM fourcc for R8 (single-channel Y plane).
const DRM_FORMAT_R8: u32 = u32::from_le_bytes(*b"R8  ");
/// DRM fourcc for GR88 (two-channel UV plane).
const DRM_FORMAT_GR88: u32 = u32::from_le_bytes(*b"GR88");

// ── EGL function pointer types ───────────────────────────────────────

type EglGetProcAddressFn = unsafe extern "C" fn(procname: *const std::ffi::c_char) -> *mut c_void;
type EglGetCurrentDisplayFn = unsafe extern "C" fn() -> *mut c_void;
type EglCreateImageKhrFn = unsafe extern "C" fn(
    dpy: *mut c_void,
    ctx: *mut c_void,
    target: u32,
    buffer: *mut c_void,
    attrib_list: *const i32,
) -> *mut c_void;
type EglDestroyImageKhrFn = unsafe extern "C" fn(dpy: *mut c_void, image: *mut c_void) -> i32;
type GlEglImageTargetTexture2dOesFn = unsafe extern "C" fn(target: u32, image: *mut c_void);
type EglQueryStringFn =
    unsafe extern "C" fn(dpy: *mut c_void, name: i32) -> *const std::ffi::c_char;
type EglGetErrorFn = unsafe extern "C" fn() -> i32;
type EglQueryDmaBufFormatsFn =
    unsafe extern "C" fn(dpy: *mut c_void, max: i32, formats: *mut i32, count: *mut i32) -> u32;
type EglQueryDmaBufModifiersFn = unsafe extern "C" fn(
    dpy: *mut c_void,
    format: i32,
    max: i32,
    modifiers: *mut u64,
    external_only: *mut u32,
    count: *mut i32,
) -> u32;

// ── Cached function pointers ─────────────────────────────────────────

static FN_GET_PROC_ADDRESS: OnceLock<Option<EglGetProcAddressFn>> = OnceLock::new();
static FN_GET_CURRENT_DISPLAY: OnceLock<Option<EglGetCurrentDisplayFn>> = OnceLock::new();
static FN_CREATE_IMAGE: OnceLock<Option<EglCreateImageKhrFn>> = OnceLock::new();
static FN_DESTROY_IMAGE: OnceLock<Option<EglDestroyImageKhrFn>> = OnceLock::new();
static FN_IMAGE_TARGET_TEXTURE: OnceLock<Option<GlEglImageTargetTexture2dOesFn>> = OnceLock::new();
static FN_QUERY_STRING: OnceLock<Option<EglQueryStringFn>> = OnceLock::new();
static FN_GET_ERROR: OnceLock<Option<EglGetErrorFn>> = OnceLock::new();
static FN_QUERY_DMABUF_FORMATS: OnceLock<Option<EglQueryDmaBufFormatsFn>> = OnceLock::new();
static FN_QUERY_DMABUF_MODIFIERS: OnceLock<Option<EglQueryDmaBufModifiersFn>> = OnceLock::new();

/// Loads `eglGetProcAddress` from `libEGL.so` via dlopen.
fn load_egl_get_proc_address() -> Option<EglGetProcAddressFn> {
    *FN_GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: dlopen with RTLD_NOW loads libEGL.so (must already be loaded
        // since we have an active EGL context).
        unsafe {
            let lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY);
            let lib = if lib.is_null() {
                libc::dlopen(c"libEGL.so".as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY)
            } else {
                lib
            };
            if lib.is_null() {
                warn!("dlopen(libEGL.so) failed — DMA-BUF import unavailable");
                return None;
            }
            let sym = libc::dlsym(lib, c"eglGetProcAddress".as_ptr());
            if sym.is_null() {
                warn!("dlsym(eglGetProcAddress) failed");
                return None;
            }
            Some(std::mem::transmute_copy(&sym))
        }
    })
}

/// Resolves an EGL/GL extension function via `eglGetProcAddress`.
///
/// # Safety
/// `T` must match the actual function signature.
unsafe fn resolve_egl_proc<T: Copy>(name: &CStr) -> Option<T> {
    let get_proc = load_egl_get_proc_address()?;
    unsafe {
        let sym = get_proc(name.as_ptr());
        if sym.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy(&sym))
        }
    }
}

fn get_current_display_fn() -> Option<EglGetCurrentDisplayFn> {
    *FN_GET_CURRENT_DISPLAY.get_or_init(|| {
        // SAFETY: function signature matches EGL spec.
        unsafe { resolve_egl_proc(c"eglGetCurrentDisplay") }
    })
}

fn create_image_fn() -> Option<EglCreateImageKhrFn> {
    *FN_CREATE_IMAGE.get_or_init(|| {
        // SAFETY: function signature matches EGL extension spec.
        unsafe { resolve_egl_proc(c"eglCreateImageKHR") }
    })
}

fn destroy_image_fn() -> Option<EglDestroyImageKhrFn> {
    *FN_DESTROY_IMAGE.get_or_init(|| {
        // SAFETY: function signature matches EGL extension spec.
        unsafe { resolve_egl_proc(c"eglDestroyImageKHR") }
    })
}

fn image_target_texture_fn() -> Option<GlEglImageTargetTexture2dOesFn> {
    *FN_IMAGE_TARGET_TEXTURE.get_or_init(|| {
        // SAFETY: function signature matches GL extension spec.
        unsafe { resolve_egl_proc(c"glEGLImageTargetTexture2DOES") }
    })
}

fn query_string_fn() -> Option<EglQueryStringFn> {
    *FN_QUERY_STRING.get_or_init(|| {
        // SAFETY: function signature matches EGL spec.
        unsafe { resolve_egl_proc(c"eglQueryString") }
    })
}

fn get_error_fn() -> Option<EglGetErrorFn> {
    *FN_GET_ERROR.get_or_init(|| {
        // SAFETY: function signature matches EGL spec.
        unsafe { resolve_egl_proc(c"eglGetError") }
    })
}

/// Returns the EGL error name for a given error code.
fn egl_error_name(code: i32) -> &'static str {
    match code {
        0x3000 => "EGL_SUCCESS",
        0x3001 => "EGL_NOT_INITIALIZED",
        0x3002 => "EGL_BAD_ACCESS",
        0x3003 => "EGL_BAD_ALLOC",
        0x3004 => "EGL_BAD_ATTRIBUTE",
        0x3005 => "EGL_BAD_CONFIG",
        0x3006 => "EGL_BAD_CONTEXT",
        0x3007 => "EGL_BAD_CURRENT_SURFACE",
        0x3008 => "EGL_BAD_DISPLAY",
        0x3009 => "EGL_BAD_MATCH",
        0x300A => "EGL_BAD_NATIVE_PIXMAP",
        0x300B => "EGL_BAD_NATIVE_WINDOW",
        0x300C => "EGL_BAD_PARAMETER",
        0x300D => "EGL_BAD_SURFACE",
        _ => "UNKNOWN",
    }
}

fn query_dmabuf_formats_fn() -> Option<EglQueryDmaBufFormatsFn> {
    *FN_QUERY_DMABUF_FORMATS.get_or_init(|| {
        // SAFETY: function signature matches EGL_EXT_image_dma_buf_import_modifiers spec.
        unsafe { resolve_egl_proc(c"eglQueryDmaBufFormatsEXT") }
    })
}

fn query_dmabuf_modifiers_fn() -> Option<EglQueryDmaBufModifiersFn> {
    *FN_QUERY_DMABUF_MODIFIERS.get_or_init(|| {
        // SAFETY: function signature matches EGL_EXT_image_dma_buf_import_modifiers spec.
        unsafe { resolve_egl_proc(c"eglQueryDmaBufModifiersEXT") }
    })
}

// ── Extension check ──────────────────────────────────────────────────

/// Checks whether the current EGL display supports the required DMA-BUF
/// import extensions.
fn check_egl_extensions(dpy: *mut c_void) -> Result<()> {
    let query = query_string_fn().context("eglQueryString not available")?;
    // EGL_EXTENSIONS = 0x3055
    let ext_ptr = unsafe { query(dpy, 0x3055) };
    if ext_ptr.is_null() {
        bail!("eglQueryString(EGL_EXTENSIONS) returned null");
    }
    let extensions = unsafe { CStr::from_ptr(ext_ptr) }.to_str().unwrap_or("");

    let required = ["EGL_EXT_image_dma_buf_import", "EGL_KHR_image_base"];
    for ext in required {
        if !extensions.contains(ext) {
            bail!("missing required EGL extension: {ext}");
        }
    }
    // Optional but nice — modifiers support.
    if extensions.contains("EGL_EXT_image_dma_buf_import_modifiers") {
        debug!("EGL_EXT_image_dma_buf_import_modifiers available");
    }

    Ok(())
}

/// Formats a DRM fourcc as a human-readable string (e.g. `R8  `, `GR88`, `NV12`).
fn fourcc_str(fourcc: u32) -> String {
    let bytes = fourcc.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!(
            "{}{}{}{}",
            bytes[0] as char, bytes[1] as char, bytes[2] as char, bytes[3] as char
        )
    } else {
        format!("0x{fourcc:08x}")
    }
}

/// Formats a DRM modifier as a known name or hex value.
///
/// Values from `drm_fourcc.h` — `fourcc_mod_code(INTEL, N)` = `0x100000000000000 | N`.
fn modifier_name(m: u64) -> String {
    const INTEL: u64 = 0x100000000000000;
    match m {
        0x0 => "LINEAR".to_string(),
        m if m == INTEL | 1 => "I915_X_TILED".to_string(),
        m if m == INTEL | 2 => "I915_Y_TILED".to_string(),
        m if m == INTEL | 3 => "I915_Yf_TILED".to_string(),
        m if m == INTEL | 4 => "I915_Y_TILED_CCS".to_string(),
        m if m == INTEL | 5 => "I915_Yf_TILED_CCS".to_string(),
        m if m == INTEL | 6 => "I915_Y_TILED_GEN12_RC_CCS".to_string(),
        m if m == INTEL | 7 => "I915_Y_TILED_GEN12_MC_CCS".to_string(),
        m if m == INTEL | 8 => "I915_Y_TILED_GEN12_RC_CCS_CC".to_string(),
        m if m == INTEL | 9 => "I915_4_TILED".to_string(),
        m if m == INTEL | 10 => "I915_4_TILED_DG2_RC_CCS".to_string(),
        m if m == INTEL | 11 => "I915_4_TILED_DG2_MC_CCS".to_string(),
        m if m == INTEL | 12 => "I915_4_TILED_DG2_RC_CCS_CC".to_string(),
        m if m == INTEL | 13 => "I915_4_TILED_MTL_RC_CCS".to_string(),
        m if m == INTEL | 14 => "I915_4_TILED_MTL_MC_CCS".to_string(),
        m if m == INTEL | 15 => "I915_4_TILED_MTL_RC_CCS_CC".to_string(),
        m if m == INTEL | 16 => "I915_4_TILED_LNL_CCS".to_string(),
        m if m == INTEL | 17 => "I915_4_TILED_BMG_CCS".to_string(),
        _ => format!("0x{m:x}"),
    }
}

/// Queries and logs DMA-BUF formats and modifiers supported by EGL.
///
/// Requires `EGL_EXT_image_dma_buf_import_modifiers`. Purely diagnostic —
/// failures are logged and silently ignored.
fn log_egl_dmabuf_support(dpy: *mut c_void) {
    let Some(query_formats) = query_dmabuf_formats_fn() else {
        debug!("eglQueryDmaBufFormatsEXT not available, skipping diagnostics");
        return;
    };
    let Some(query_modifiers) = query_dmabuf_modifiers_fn() else {
        debug!("eglQueryDmaBufModifiersEXT not available, skipping diagnostics");
        return;
    };

    // Query format count.
    let mut count: i32 = 0;
    let ok = unsafe { query_formats(dpy, 0, std::ptr::null_mut(), &mut count) };
    if ok != 1 || count <= 0 {
        debug!("eglQueryDmaBufFormatsEXT returned no formats (ok={ok}, count={count})");
        return;
    }

    // Query actual formats.
    let mut formats = vec![0i32; count as usize];
    let ok = unsafe { query_formats(dpy, count, formats.as_mut_ptr(), &mut count) };
    if ok != 1 {
        debug!("eglQueryDmaBufFormatsEXT failed on second call");
        return;
    }
    formats.truncate(count as usize);

    info!(count = formats.len(), "EGL DMA-BUF supported formats");

    // For key formats, query modifiers.
    let interesting = [
        DRM_FORMAT_R8,
        DRM_FORMAT_GR88,
        u32::from_le_bytes(*b"NV12"),
        u32::from_le_bytes(*b"P010"),
    ];

    for fourcc in interesting {
        let supported = formats.contains(&(fourcc as i32));
        let name = fourcc_str(fourcc);
        if !supported {
            info!(format = %name, "  format NOT supported by EGL");
            continue;
        }

        // Query modifier count.
        let mut mod_count: i32 = 0;
        let ok = unsafe {
            query_modifiers(
                dpy,
                fourcc as i32,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut mod_count,
            )
        };
        if ok != 1 || mod_count <= 0 {
            info!(format = %name, "  format supported, no modifiers reported");
            continue;
        }

        let mut modifiers = vec![0u64; mod_count as usize];
        let mut external_only = vec![0u32; mod_count as usize];
        let ok = unsafe {
            query_modifiers(
                dpy,
                fourcc as i32,
                mod_count,
                modifiers.as_mut_ptr(),
                external_only.as_mut_ptr(),
                &mut mod_count,
            )
        };
        if ok != 1 {
            info!(format = %name, "  format supported, modifier query failed");
            continue;
        }
        modifiers.truncate(mod_count as usize);
        external_only.truncate(mod_count as usize);

        let mod_strs: Vec<String> = modifiers
            .iter()
            .zip(external_only.iter())
            .map(|(m, ext)| {
                let ext_tag = if *ext != 0 { " (ext-only)" } else { "" };
                format!("{}{ext_tag}", modifier_name(*m))
            })
            .collect();

        info!(
            format = %name,
            modifiers = %mod_strs.join(", "),
            "  format supported"
        );
    }
}

/// Queries non-external-only modifiers for a given DRM format.
///
/// Returns an empty list if the query extensions are unavailable or the
/// format is not supported.
fn query_non_external_modifiers(dpy: *mut c_void, fourcc: u32) -> Vec<u64> {
    let Some(query_modifiers) = query_dmabuf_modifiers_fn() else {
        return Vec::new();
    };

    let mut mod_count: i32 = 0;
    let ok = unsafe {
        query_modifiers(
            dpy,
            fourcc as i32,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut mod_count,
        )
    };
    if ok != 1 || mod_count <= 0 {
        return Vec::new();
    }

    let mut modifiers = vec![0u64; mod_count as usize];
    let mut external_only = vec![0u32; mod_count as usize];
    let ok = unsafe {
        query_modifiers(
            dpy,
            fourcc as i32,
            mod_count,
            modifiers.as_mut_ptr(),
            external_only.as_mut_ptr(),
            &mut mod_count,
        )
    };
    if ok != 1 {
        return Vec::new();
    }
    modifiers.truncate(mod_count as usize);
    external_only.truncate(mod_count as usize);

    modifiers
        .into_iter()
        .zip(external_only)
        .filter(|(_, ext)| *ext == 0)
        .map(|(m, _)| m)
        .collect()
}

// ── EGLImage wrapper ─────────────────────────────────────────────────

/// RAII wrapper around an EGLImage — destroys on drop.
struct EglImage {
    dpy: *mut c_void,
    image: *mut c_void,
}

impl Drop for EglImage {
    fn drop(&mut self) {
        if let Some(destroy) = destroy_image_fn() {
            unsafe { destroy(self.dpy, self.image) };
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────

/// Manages EGL DMA-BUF import state for zero-copy GLES rendering.
///
/// Probes for required EGL extensions on creation and caches the EGL
/// display handle. When the DMA-BUF modifier is incompatible (e.g.
/// Y_TILED on Intel MTL), uses VPP retiling to convert to a supported
/// modifier before import.
pub struct GlesDmaBufImporter {
    dpy: *mut c_void,
    /// Consecutive import failure count — disables after threshold.
    failures: u32,
    /// Non-external-only modifiers supported by EGL for R8 format (Y plane).
    /// Used to decide whether VPP retiling is needed.
    supported_r8_modifiers: Vec<u64>,
    /// VAAPI VPP retiler for incompatible modifiers. Lazily initialized on
    /// first use. `None` means not yet attempted; `Some(None)` means init
    /// failed and we shouldn't retry.
    #[cfg(feature = "vaapi")]
    vpp_retiler: Option<Option<super::vpp_retiler::VppRetiler>>,
}

// SAFETY: The EGL display and function pointers are process-global singletons.
// EGL calls are only made while the EGL context is current on the calling thread,
// which is enforced by the caller (same constraint as GlesRenderer).
unsafe impl Send for GlesDmaBufImporter {}

impl GlesDmaBufImporter {
    /// Probes for EGL DMA-BUF import support.
    ///
    /// Returns `Ok(Some(...))` if extensions are available, `Ok(None)` if the
    /// EGL display is unavailable, or `Err` if extension probing fails.
    ///
    /// # Safety
    /// An EGL context must be current on the calling thread.
    pub unsafe fn new() -> Result<Option<Self>> {
        let Some(get_display) = get_current_display_fn() else {
            return Ok(None);
        };
        let dpy = unsafe { get_display() };
        if dpy.is_null() {
            return Ok(None);
        }

        check_egl_extensions(dpy)?;
        log_egl_dmabuf_support(dpy);

        // Verify function pointers are resolvable.
        if create_image_fn().is_none() {
            bail!("eglCreateImageKHR not available");
        }
        if image_target_texture_fn().is_none() {
            bail!("glEGLImageTargetTexture2DOES not available");
        }

        let supported_r8_modifiers = query_non_external_modifiers(dpy, DRM_FORMAT_R8);
        debug!(
            modifiers = ?supported_r8_modifiers.iter().map(|m| modifier_name(*m)).collect::<Vec<_>>(),
            "EGL DMA-BUF import ready, R8 supported modifiers"
        );

        Ok(Some(Self {
            dpy,
            failures: 0,
            supported_r8_modifiers,
            #[cfg(feature = "vaapi")]
            vpp_retiler: None,
        }))
    }

    /// Whether the importer has been disabled due to repeated failures.
    pub fn is_disabled(&self) -> bool {
        self.failures >= 3
    }

    /// Records a failure. Returns `true` if the importer is now disabled.
    pub fn record_failure(&mut self, err: &anyhow::Error) -> bool {
        self.failures += 1;
        if self.failures >= 3 {
            warn!("EGL DMA-BUF import failed 3 times, disabling zero-copy path: {err}");
            true
        } else {
            debug!(
                "EGL DMA-BUF import failed (attempt {}): {err}",
                self.failures
            );
            false
        }
    }

    /// Resets the failure counter (call on success).
    pub fn record_success(&mut self) {
        self.failures = 0;
    }

    /// Imports a DMA-BUF as NV12 Y and UV EGL textures and binds them to the
    /// given GL texture objects.
    ///
    /// The Y plane is imported as R8 (`DRM_FORMAT_R8`) and bound to `y_texture`.
    /// The UV plane is imported as RG88 (`DRM_FORMAT_GR88`) and bound to `uv_texture`.
    ///
    /// When the DMA-BUF modifier is not in EGL's supported list (e.g. Y_TILED
    /// on Intel MTL), uses VPP retiling to convert to a compatible modifier
    /// before import.
    ///
    /// # Safety
    /// The EGL/GL context must be current. The textures must be valid.
    pub unsafe fn import_nv12(
        &mut self,
        gl: &glow::Context,
        info: &DmaBufInfo,
        y_texture: glow::Texture,
        uv_texture: glow::Texture,
    ) -> Result<(u32, u32)> {
        if info.planes.len() < 2 {
            bail!(
                "DMA-BUF has {} planes, need at least 2 for NV12",
                info.planes.len()
            );
        }

        // If the modifier is not EGL-compatible, try VPP re-tiling.
        #[cfg(feature = "vaapi")]
        let retiled: DmaBufInfo;
        let info = if self.supported_r8_modifiers.is_empty()
            || self.supported_r8_modifiers.contains(&info.modifier)
        {
            info
        } else {
            #[cfg(feature = "vaapi")]
            {
                let retiler_slot =
                    self.vpp_retiler.get_or_insert_with(
                        || match super::vpp_retiler::VppRetiler::new(None) {
                            Ok(r) => {
                                info!(
                                    "initialized VPP retiler for EGL DMA-BUF modifier conversion"
                                );
                                Some(r)
                            }
                            Err(e) => {
                                warn!("VPP retiler init failed: {e}");
                                None
                            }
                        },
                    );
                let retiler = retiler_slot.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "DMA-BUF modifier {} ({:#x}) not EGL-compatible and VPP retiler unavailable",
                        modifier_name(info.modifier),
                        info.modifier,
                    )
                })?;
                retiled = retiler.retile(info)?;
                trace!(
                    old_modifier = %modifier_name(info.modifier),
                    new_modifier = %modifier_name(retiled.modifier),
                    "VPP retiled for EGL import"
                );
                &retiled
            }
            #[cfg(not(feature = "vaapi"))]
            bail!(
                "DMA-BUF modifier {} ({:#x}) not EGL-compatible (supported: {:?}), \
                 enable `vaapi` feature for VPP retiling",
                modifier_name(info.modifier),
                info.modifier,
                self.supported_r8_modifiers
                    .iter()
                    .map(|m| modifier_name(*m))
                    .collect::<Vec<_>>(),
            )
        };

        let fd = info.fd.as_raw_fd();
        let w = info.display_width;
        let h = info.display_height;

        // Import Y plane as R8.
        let y_image = self.create_plane_image(
            fd,
            DRM_FORMAT_R8,
            w,
            h,
            info.planes[0].offset,
            info.planes[0].pitch,
            info.modifier,
        )?;

        // Import UV plane as GR88 (half width, half height).
        let uv_w = w / 2;
        let uv_h = h.div_ceil(2);
        let uv_image = self.create_plane_image(
            fd,
            DRM_FORMAT_GR88,
            uv_w,
            uv_h,
            info.planes[1].offset,
            info.planes[1].pitch,
            info.modifier,
        )?;

        // Bind Y EGLImage to y_texture.
        let target_fn =
            image_target_texture_fn().context("glEGLImageTargetTexture2DOES unavailable")?;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(y_texture));
            target_fn(glow::TEXTURE_2D, y_image.image);
            gl.bind_texture(glow::TEXTURE_2D, Some(uv_texture));
            target_fn(glow::TEXTURE_2D, uv_image.image);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }

        // EGLImages are destroyed here but the GL textures retain ownership
        // of the imported storage until they're re-bound or deleted.
        drop(y_image);
        drop(uv_image);

        Ok((w, h))
    }

    /// Creates a single-plane EGLImage from a DMA-BUF fd.
    #[allow(
        clippy::too_many_arguments,
        reason = "EGL attribute mapping needs all plane params"
    )]
    fn create_plane_image(
        &self,
        fd: i32,
        fourcc: u32,
        width: u32,
        height: u32,
        offset: u32,
        pitch: u32,
        modifier: u64,
    ) -> Result<EglImage> {
        let modifier_lo = (modifier & 0xFFFF_FFFF) as i32;
        let modifier_hi = ((modifier >> 32) & 0xFFFF_FFFF) as i32;

        let attrs = [
            EGL_WIDTH,
            width as i32,
            EGL_HEIGHT,
            height as i32,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as i32,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            offset as i32,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            pitch as i32,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            modifier_lo,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            modifier_hi,
            EGL_IMAGE_PRESERVED_KHR,
            EGL_TRUE,
            EGL_NONE,
        ];

        let create = create_image_fn().context("eglCreateImageKHR unavailable")?;
        let image = unsafe {
            create(
                self.dpy,
                EGL_NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(), // no client buffer — fd comes from attribs
                attrs.as_ptr(),
            )
        };
        if image.is_null() {
            let egl_err = get_error_fn().map(|f| unsafe { f() }).unwrap_or(0);
            bail!(
                "eglCreateImageKHR failed for fourcc={} ({fourcc:#x}) {width}x{height} \
                 modifier={} ({modifier:#x}), eglGetError={} ({egl_err:#x})",
                fourcc_str(fourcc),
                modifier_name(modifier),
                egl_error_name(egl_err),
            );
        }

        Ok(EglImage {
            dpy: self.dpy,
            image,
        })
    }
}

impl std::fmt::Debug for GlesDmaBufImporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlesDmaBufImporter")
            .field("failures", &self.failures)
            .finish()
    }
}
