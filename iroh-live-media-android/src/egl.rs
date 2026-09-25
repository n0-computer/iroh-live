//! EGL and GLES extension functions for rendering an `AHardwareBuffer`.
//!
//! These functions are not available at link time, so [`Extensions::load`]
//! resolves them through `eglGetProcAddress`. A hardware buffer becomes a
//! texture in three steps:
//!
//! ```text
//! AHardwareBuffer
//!   -> eglGetNativeClientBufferANDROID -> EGLClientBuffer
//!   -> eglCreateImageKHR               -> EGLImage
//!   -> glEGLImageTargetTexture2DOES     -> GL_TEXTURE_EXTERNAL_OES texture
//! ```

use std::ffi::c_void;

use khronos_egl as egl_api;

/// The EGL instance the renderer loads.
pub(crate) type Egl = egl_api::DynamicInstance<egl_api::EGL1_4>;

type GetNativeClientBufferFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type CreateImageFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type DestroyImageFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type ImageTargetTextureFn = unsafe extern "C" fn(u32, *mut c_void);

/// The extension functions, each `None` if the driver lacks it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Extensions {
    get_native_client_buffer: Option<GetNativeClientBufferFn>,
    create_image: Option<CreateImageFn>,
    destroy_image: Option<DestroyImageFn>,
    image_target_texture: Option<ImageTargetTextureFn>,
}

/// Resolves `name` through `eglGetProcAddress`.
///
/// # Safety
///
/// `T` must be a function pointer type that matches the symbol's signature.
unsafe fn resolve<T: Copy>(egl: &Egl, name: &str) -> Option<T> {
    let function = egl.get_proc_address(name);
    if function.is_none() {
        tracing::warn!(name, "EGL extension function not available");
    }
    // SAFETY: Android's "system" ABI is "C", both are function pointers of the
    // same size, and the caller guarantees the signature.
    function.map(|function| unsafe { std::mem::transmute_copy(&function) })
}

impl Extensions {
    /// Resolves every extension function through `egl`.
    pub(crate) fn load(egl: &Egl) -> Self {
        // SAFETY: Each type matches the signature in the EGL or GLES extension
        // spec of the function it names.
        unsafe {
            Self {
                get_native_client_buffer: resolve(egl, "eglGetNativeClientBufferANDROID"),
                create_image: resolve(egl, "eglCreateImageKHR"),
                destroy_image: resolve(egl, "eglDestroyImageKHR"),
                image_target_texture: resolve(egl, "glEGLImageTargetTexture2DOES"),
            }
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
    pub(crate) unsafe fn get_native_client_buffer(
        &self,
        hardware_buffer: *const c_void,
    ) -> Option<*mut c_void> {
        let result = unsafe { self.get_native_client_buffer?(hardware_buffer) };
        (!result.is_null()).then_some(result)
    }

    /// Creates an `EGLImage` from an `EGLClientBuffer`.
    ///
    /// Returns `None` if the extension is missing or creation fails.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `EGLDisplay`. `client_buffer` must be a valid
    /// `EGLClientBuffer`. `attrs` must be an `EGL_NONE`-terminated attribute
    /// list.
    pub(crate) unsafe fn create_image(
        &self,
        display: *mut c_void,
        target: u32,
        client_buffer: *mut c_void,
        attrs: *const i32,
    ) -> Option<*mut c_void> {
        let no_context = std::ptr::null_mut();
        let result =
            unsafe { self.create_image?(display, no_context, target, client_buffer, attrs) };
        (!result.is_null()).then_some(result)
    }

    /// Destroys an `EGLImage`.
    ///
    /// Does nothing if the extension is missing.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `EGLDisplay`. `image` must be a valid `EGLImage`.
    pub(crate) unsafe fn destroy_image(&self, display: *mut c_void, image: *mut c_void) {
        if let Some(destroy) = self.destroy_image {
            unsafe { destroy(display, image) };
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
    pub(crate) unsafe fn image_target_texture_2d(&self, target: u32, image: *mut c_void) -> bool {
        let Some(bind) = self.image_target_texture else {
            return false;
        };
        unsafe { bind(target, image) };
        true
    }
}

/// Creates a `glow::Context` that resolves GL functions through `egl`.
///
/// # Safety
///
/// An EGL context must be current on the calling thread.
pub(crate) unsafe fn create_glow_context(egl: &Egl) -> glow::Context {
    unsafe {
        glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |function| function as *const c_void)
        })
    }
}
