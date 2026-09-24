//! EGL and GLES extension functions for rendering an `AHardwareBuffer`.
//!
//! These functions are not available at link time. This module resolves them
//! at runtime through `eglGetProcAddress` and caches each pointer after first
//! use. A hardware buffer becomes a texture in three steps:
//!
//! ```text
//! AHardwareBuffer
//!   -> eglGetNativeClientBufferANDROID -> EGLClientBuffer
//!   -> eglCreateImageKHR               -> EGLImage
//!   -> glEGLImageTargetTexture2DOES     -> GL_TEXTURE_EXTERNAL_OES texture
//! ```

use std::{ffi::c_void, sync::OnceLock};

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

/// Loads `eglGetProcAddress` from the already loaded `libEGL.so`.
fn load_egl_get_proc_address() -> Option<EglGetProcAddressFn> {
    *FN_GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: With RTLD_NOLOAD, dlopen only returns a handle if libEGL.so
        // is already loaded. On Android the Java side always loads it. dlsym
        // then resolves the symbol from that library.
        unsafe {
            let lib = libc::dlopen(c"libEGL.so".as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY);
            if lib.is_null() {
                tracing::error!("dlopen(libEGL.so) failed");
                return None;
            }
            let sym = libc::dlsym(lib, c"eglGetProcAddress".as_ptr());
            if sym.is_null() {
                tracing::error!("dlsym(eglGetProcAddress) failed");
                return None;
            }
            Some(std::mem::transmute_copy(&sym))
        }
    })
}

/// Resolves an EGL or GL extension function through `eglGetProcAddress`.
///
/// # Safety
///
/// `T` must match the signature of the symbol. `name` must be NUL-terminated.
unsafe fn resolve_egl_proc<T: Copy>(name: &[u8]) -> Option<T> {
    let get_proc = load_egl_get_proc_address()?;
    // SAFETY: The caller guarantees that `name` is NUL-terminated and that `T`
    // matches the signature of the symbol.
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

/// Creates a `glow::Context` that resolves GL functions through `eglGetProcAddress`.
///
/// # Safety
///
/// An EGL context must be current on the calling thread.
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
/// Returns `None` if the extension is missing or the conversion fails.
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
/// Returns `None` if the extension is missing or creation fails.
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
/// Does nothing if the extension is missing.
///
/// # Safety
///
/// `display` must be a valid `EGLDisplay`. `image` must be a valid `EGLImage`.
pub unsafe fn destroy_image(display: *mut c_void, image: *mut c_void) {
    if let Some(func) = get_destroy_image_fn() {
        unsafe { func(display, image) };
    }
}

/// Binds an `EGLImage` to the texture currently bound to `target`.
///
/// Returns `false` if the extension is missing.
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
