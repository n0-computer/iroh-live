//! Safe wrappers around EGL/GLES extension functions for HardwareBuffer rendering.
//!
//! Android's zero-copy video rendering pipeline requires EGL extension calls
//! that are not exposed by the Java SDK. This module resolves them at runtime
//! via `dlopen("libEGL.so")` → `eglGetProcAddress` and caches the function
//! pointers in `OnceLock` statics for zero-overhead repeat calls.
//!
//! # Rendering pipeline
//!
//! ```text
//! AHardwareBuffer
//!   → eglGetNativeClientBufferANDROID → EGLClientBuffer
//!   → eglCreateImageKHR               → EGLImage
//!   → glEGLImageTargetTexture2DOES     → GL_TEXTURE_EXTERNAL_OES texture
//! ```

use std::ffi::c_void;
use std::sync::OnceLock;

type EglGetProcAddressFn = unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void;
type GetNativeClientBufferFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type CreateImageFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type DestroyImageFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type ImageTargetTextureFn = unsafe extern "C" fn(u32, *mut c_void);

static FN_GET_PROC_ADDRESS: OnceLock<Option<EglGetProcAddressFn>> = OnceLock::new();
static FN_GET_NATIVE_CLIENT_BUFFER: OnceLock<Option<GetNativeClientBufferFn>> = OnceLock::new();
static FN_CREATE_IMAGE: OnceLock<Option<CreateImageFn>> = OnceLock::new();
static FN_DESTROY_IMAGE: OnceLock<Option<DestroyImageFn>> = OnceLock::new();
static FN_IMAGE_TARGET_TEXTURE: OnceLock<Option<ImageTargetTextureFn>> = OnceLock::new();

/// Loads `eglGetProcAddress` from `libEGL.so` via dlopen + dlsym.
fn load_egl_get_proc_address() -> Option<EglGetProcAddressFn> {
    *FN_GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: dlopen with RTLD_NOLOAD only returns a handle if
        // libEGL.so is already loaded (it always is on Android — the
        // Java side loads it). dlsym resolves from that library.
        unsafe {
            let lib = libc::dlopen(
                b"libEGL.so\0".as_ptr().cast(),
                libc::RTLD_NOLOAD | libc::RTLD_LAZY,
            );
            if lib.is_null() {
                tracing::error!("dlopen(libEGL.so) failed");
                return None;
            }
            let sym = libc::dlsym(lib, b"eglGetProcAddress\0".as_ptr().cast());
            if sym.is_null() {
                tracing::error!("dlsym(eglGetProcAddress) failed");
                return None;
            }
            Some(std::mem::transmute_copy(&sym))
        }
    })
}

/// Resolves an EGL/GL extension function pointer via `eglGetProcAddress`.
///
/// # Safety
///
/// The caller must ensure `T` matches the actual signature of the resolved
/// symbol. The name must be a null-terminated byte string.
unsafe fn resolve_egl_proc<T: Copy>(name: &[u8]) -> Option<T> {
    let get_proc = load_egl_get_proc_address()?;
    // SAFETY: eglGetProcAddress is the EGL-spec way to resolve extension
    // functions. The caller guarantees name is null-terminated and T matches
    // the symbol's actual signature.
    unsafe {
        let sym = get_proc(name.as_ptr().cast());
        if sym.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy(&sym))
        }
    }
}

fn get_native_client_buffer_fn() -> Option<GetNativeClientBufferFn> {
    *FN_GET_NATIVE_CLIENT_BUFFER.get_or_init(|| {
        // SAFETY: function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglGetNativeClientBufferANDROID\0") }
    })
}

fn get_create_image_fn() -> Option<CreateImageFn> {
    *FN_CREATE_IMAGE.get_or_init(|| {
        // SAFETY: function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglCreateImageKHR\0") }
    })
}

fn get_destroy_image_fn() -> Option<DestroyImageFn> {
    *FN_DESTROY_IMAGE.get_or_init(|| {
        // SAFETY: function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglDestroyImageKHR\0") }
    })
}

fn get_image_target_texture_fn() -> Option<ImageTargetTextureFn> {
    *FN_IMAGE_TARGET_TEXTURE.get_or_init(|| {
        // SAFETY: function signature matches the GLES extension spec.
        unsafe { resolve_egl_proc(b"glEGLImageTargetTexture2DOES\0") }
    })
}

/// Creates a `glow::Context` by resolving GL functions from `eglGetProcAddress`.
///
/// Must be called while an EGL context is current on the calling thread.
///
/// # Safety
/// An EGL context must be current.
pub unsafe fn create_glow_context() -> glow::Context {
    let get_proc = load_egl_get_proc_address();
    unsafe {
        glow::Context::from_loader_function(|name| {
            let Ok(c_name) = std::ffi::CString::new(name) else {
                return std::ptr::null();
            };
            get_proc
                .and_then(|f| {
                    let p = f(c_name.as_ptr());
                    if p.is_null() {
                        None
                    } else {
                        Some(p as *const _)
                    }
                })
                .unwrap_or(std::ptr::null())
        })
    }
}

/// Converts an `AHardwareBuffer` pointer into an `EGLClientBuffer`.
///
/// Returns `None` if the extension is unavailable or the conversion fails.
///
/// # Safety
///
/// `hardware_buffer` must be a valid `AHardwareBuffer*` with an acquired
/// reference.
pub unsafe fn get_native_client_buffer(hardware_buffer: *const c_void) -> Option<*mut c_void> {
    let func = get_native_client_buffer_fn()?;
    let result = unsafe { func(hardware_buffer) };
    if result.is_null() { None } else { Some(result) }
}

/// Creates an `EGLImage` from an `EGLClientBuffer`.
///
/// Returns `None` if the extension is unavailable or creation fails.
///
/// # Safety
///
/// `display` must be a valid `EGLDisplay`. `client_buffer` must be a valid
/// `EGLClientBuffer`. `attrs` must be a null-terminated EGL attribute list.
pub unsafe fn create_image(
    display: *mut c_void,
    target: u32,
    client_buffer: *mut c_void,
    attrs: *const i32,
) -> Option<*mut c_void> {
    let func = get_create_image_fn()?;
    let result = unsafe {
        func(
            display,
            std::ptr::null_mut(), // EGL_NO_CONTEXT
            target,
            client_buffer,
            attrs,
        )
    };
    if result.is_null() { None } else { Some(result) }
}

/// Destroys an `EGLImage`.
///
/// # Safety
///
/// `display` must be a valid `EGLDisplay`. `image` must be a valid `EGLImage`.
pub unsafe fn destroy_image(display: *mut c_void, image: *mut c_void) {
    if let Some(func) = get_destroy_image_fn() {
        unsafe { func(display, image) };
    }
}

/// Binds an `EGLImage` to the current GL texture target.
///
/// # Safety
///
/// `image` must be a valid `EGLImage`. A GL context must be current on this
/// thread.
pub unsafe fn image_target_texture_2d(target: u32, image: *mut c_void) -> bool {
    let Some(func) = get_image_target_texture_fn() else {
        tracing::error!("glEGLImageTargetTexture2DOES not available");
        return false;
    };
    unsafe { func(target, image) };
    true
}
