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

/// The extension functions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Extensions {
    get_native_client_buffer: GetNativeClientBufferFn,
    create_image: CreateImageFn,
    destroy_image: DestroyImageFn,
    image_target_texture: ImageTargetTextureFn,
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
    ///
    /// Returns `None` if the driver lacks any of them, with a warning for each.
    pub(crate) fn load(egl: &Egl) -> Option<Self> {
        // SAFETY: Each type matches the signature in the EGL or GLES extension
        // spec of the function it names.
        let (get_native_client_buffer, create_image, destroy_image, image_target_texture) = unsafe {
            (
                resolve(egl, "eglGetNativeClientBufferANDROID"),
                resolve(egl, "eglCreateImageKHR"),
                resolve(egl, "eglDestroyImageKHR"),
                resolve(egl, "glEGLImageTargetTexture2DOES"),
            )
        };
        Some(Self {
            get_native_client_buffer: get_native_client_buffer?,
            create_image: create_image?,
            destroy_image: destroy_image?,
            image_target_texture: image_target_texture?,
        })
    }

    /// Converts an `AHardwareBuffer` pointer into an `EGLClientBuffer`.
    ///
    /// Returns `None` if the conversion fails.
    ///
    /// # Safety
    ///
    /// `hardware_buffer` must be a valid `AHardwareBuffer*` with an acquired
    /// reference.
    pub(crate) unsafe fn get_native_client_buffer(
        &self,
        hardware_buffer: *const c_void,
    ) -> Option<*mut c_void> {
        let result = unsafe { (self.get_native_client_buffer)(hardware_buffer) };
        (!result.is_null()).then_some(result)
    }

    /// Creates an `EGLImage` from an `EGLClientBuffer`.
    ///
    /// Returns `None` if creation fails.
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
            unsafe { (self.create_image)(display, no_context, target, client_buffer, attrs) };
        (!result.is_null()).then_some(result)
    }

    /// Destroys an `EGLImage`.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `EGLDisplay`. `image` must be a valid `EGLImage`.
    pub(crate) unsafe fn destroy_image(&self, display: *mut c_void, image: *mut c_void) {
        unsafe { (self.destroy_image)(display, image) };
    }

    /// Binds an `EGLImage` to the texture currently bound to `target`.
    ///
    /// # Safety
    ///
    /// `image` must be a valid `EGLImage`. A GL context must be current on this
    /// thread.
    pub(crate) unsafe fn image_target_texture_2d(&self, target: u32, image: *mut c_void) {
        unsafe { (self.image_target_texture)(target, image) };
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
