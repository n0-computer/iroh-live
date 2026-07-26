//! GPU-accelerated video widget using [`gtk4::GLArea`] and [`GlesRenderer`].
//!
//! NV12 frames are uploaded as Y + UV textures and converted to RGBA by the
//! GPU. DMA-BUF frames bypass CPU entirely when `gles-dmabuf` is enabled.

use std::cell::RefCell;
use std::rc::Rc;

use glow::HasContext;
use gtk4::GLArea;
use gtk4::prelude::*;
use moq_media::format::VideoFrame;
use rusty_codecs::render::gles::GlesRenderer;

/// Loads a `glow::Context` by resolving GL function pointers via EGL.
///
/// GTK4 on Linux uses EGL. When a GLArea context is current,
/// `eglGetProcAddress` returns the correct function pointer. We resolve
/// it from `libEGL.so` which GTK4 always has loaded.
fn load_gl_from_egl() -> glow::Context {
    type EglGetProcAddrFn =
        unsafe extern "C" fn(*const std::ffi::c_char) -> *const std::ffi::c_void;

    let egl_fn: EglGetProcAddrFn = unsafe {
        // Try global symbol table first (usually works since GTK4 loads EGL).
        let mut sym = libc::dlsym(libc::RTLD_DEFAULT, c"eglGetProcAddress".as_ptr());
        if sym.is_null() {
            let lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY);
            if !lib.is_null() {
                sym = libc::dlsym(lib, c"eglGetProcAddress".as_ptr());
            }
        }
        if sym.is_null() {
            let lib = libc::dlopen(c"libEGL.so".as_ptr(), libc::RTLD_LAZY);
            if !lib.is_null() {
                sym = libc::dlsym(lib, c"eglGetProcAddress".as_ptr());
            }
        }
        assert!(!sym.is_null(), "eglGetProcAddress not found");
        std::mem::transmute(sym)
    };

    unsafe {
        glow::Context::from_loader_function(|name| {
            let Ok(c_name) = std::ffi::CString::new(name) else {
                return std::ptr::null();
            };
            egl_fn(c_name.as_ptr()) as *const _
        })
    }
}

/// GPU-accelerated video widget backed by a [`gtk4::GLArea`].
///
/// Uses the GLES2 NV12 shader from `rusty-codecs` to convert YUV frames
/// on the GPU. When the `gles-dmabuf` feature is enabled, DMA-BUF frames
/// (e.g. from VAAPI decoders) are imported zero-copy via EGL — no CPU
/// involvement at all.
///
/// # Usage
///
/// ```ignore
/// let widget = GlVideoWidget::new();
/// container.append(widget.gl_area());
///
/// // In a glib timeout callback:
/// widget.upload_frame(&frame);
/// ```
#[derive(Debug, Clone)]
pub struct GlVideoWidget {
    gl_area: GLArea,
    inner: Rc<RefCell<Option<Inner>>>,
}

struct Inner {
    renderer: GlesRenderer,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

impl Default for GlVideoWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl GlVideoWidget {
    /// Creates a new GL-accelerated video widget.
    ///
    /// The GL context and renderer are initialized lazily on the first
    /// `realize` signal from GTK.
    pub fn new() -> Self {
        let gl_area = GLArea::new();
        gl_area.set_hexpand(true);
        gl_area.set_vexpand(true);

        let inner: Rc<RefCell<Option<Inner>>> = Rc::new(RefCell::new(None));

        // Initialize GlesRenderer when the GL context becomes available.
        {
            let inner = Rc::clone(&inner);
            gl_area.connect_realize(move |area| {
                area.make_current();
                if let Some(err) = area.error() {
                    tracing::error!("GLArea realize error: {err}");
                    return;
                }

                let gl = load_gl_from_egl();
                tracing::info!(
                    renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
                    version = unsafe { gl.get_parameter_string(glow::VERSION) },
                    "GLArea GL context ready"
                );

                match unsafe { GlesRenderer::new(gl) } {
                    Ok(renderer) => {
                        *inner.borrow_mut() = Some(Inner { renderer });
                    }
                    Err(e) => {
                        tracing::error!("GlesRenderer init failed: {e}");
                    }
                }
            });
        }

        Self { gl_area, inner }
    }

    /// Returns the underlying [`GLArea`] for adding to a container.
    pub fn gl_area(&self) -> &GLArea {
        &self.gl_area
    }

    /// Uploads a frame and triggers a redraw.
    ///
    /// Makes the GL context current, uploads the frame via
    /// [`GlesRenderer::upload_frame`], draws, and tells GTK the
    /// content changed. Must be called from the GTK main thread.
    pub fn upload_frame(&self, frame: &VideoFrame) {
        self.gl_area.make_current();
        let mut inner = self.inner.borrow_mut();
        let Some(ref mut state) = *inner else {
            return;
        };

        let w = self.gl_area.width();
        let h = self.gl_area.height();
        unsafe {
            state.renderer.upload_frame(frame);
            state.renderer.draw(w, h);
        }
        self.gl_area.queue_draw();
    }
}
