//! GLES2 renderer that owns its EGL context.
//!
//! Kotlin only passes in a `Surface`. All EGL and GL calls happen in Rust.

use std::ffi::c_void;

use gles_yuv::Program;
use glow::HasContext;
use khronos_egl as egl_api;
use n0_error::{Result, StdResultExt};
use tracing::{debug, info, warn};

use crate::egl::{self, Egl, Extensions};

/// `GL_TEXTURE_EXTERNAL_OES`, which glow does not define.
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
// EGL constants for importing an `AHardwareBuffer`, from `eglext.h`.
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_TRUE: i32 = 1;
const EGL_NONE: i32 = 0x3038;

/// The start of `main` in both fragment shaders, which rotates `v_uv` into `uv`.
///
/// The rotation is `u_rotation` degrees clockwise.
const ROTATE: &str = "\
void main() {
    vec2 uv = v_uv;
    if (u_rotation == 90) {
        uv = vec2(v_uv.y, 1.0 - v_uv.x);
    } else if (u_rotation == 180) {
        uv = vec2(1.0 - v_uv.x, 1.0 - v_uv.y);
    } else if (u_rotation == 270) {
        uv = vec2(1.0 - v_uv.y, v_uv.x);
    }
";

/// Declarations of the fragment shader for `AHardwareBuffer` frames.
const OES_FRAG_HEAD: &str = "\
#extension GL_OES_EGL_image_external : require
precision mediump float;
varying vec2 v_uv;
uniform samplerExternalOES u_tex;
uniform int u_rotation;
";

/// The end of the fragment shader for `AHardwareBuffer` frames.
const OES_FRAG_TAIL: &str = "    gl_FragColor = texture2D(u_tex, uv);
}";

/// Declarations of the fragment shader that converts NV12 to RGB.
const NV12_FRAG_HEAD: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y_tex;
uniform sampler2D u_uv_tex;
uniform int u_rotation;
";

/// The plane reads of the NV12 fragment shader.
const NV12_READS: &str = "    float y_raw = texture2D(u_y_tex, uv).r;
    float u_raw = texture2D(u_uv_tex, uv).r;
    float v_raw = texture2D(u_uv_tex, uv).a;
";

/// Video renderer for one Android window.
///
/// Owns the EGL context and the window surface. Draw a frame with
/// [`Self::render_hardware_buffer`] or [`Self::render_nv12`], then show it with
/// [`Self::swap_buffers`].
#[derive(derive_more::Debug)]
pub(crate) struct AndroidRenderer {
    gl: glow::Context,
    #[debug(skip)]
    egl: Egl,
    /// `None` if the driver lacks them, and then hardware buffers are not drawn.
    extensions: Option<Extensions>,
    egl_display: egl_api::Display,
    egl_context: egl_api::Context,
    egl_surface: egl_api::Surface,
    oes_program: Program,
    oes_texture: glow::Texture,
    oes_rotation_loc: Option<glow::UniformLocation>,
    nv12_program: Program,
    nv12_y_texture: glow::Texture,
    nv12_uv_texture: glow::Texture,
    nv12_rotation_loc: Option<glow::UniformLocation>,
    vbo: glow::Buffer,
}

// SAFETY: Callers only use the EGL and GL handles from the thread where the
// context is current. The EGL instance (`libloading::Library`) is `Send`.
unsafe impl Send for AndroidRenderer {}

impl AndroidRenderer {
    /// Creates a renderer that draws to `native_window`.
    ///
    /// `native_window` comes from `ANativeWindow_fromSurface`. When this
    /// returns, the new EGL context is current on the calling thread.
    ///
    /// # Errors
    ///
    /// Fails if any EGL or GL setup step fails, such as a missing EGL library,
    /// no matching config, or a shader that does not compile.
    ///
    /// # Safety
    ///
    /// `native_window` must be a valid `ANativeWindow*`.
    pub(crate) unsafe fn new(native_window: *mut c_void) -> Result<Self> {
        // SAFETY: Loading runs the initializers of the system's libEGL, which
        // we trust.
        let egl = unsafe { Egl::load_required().std_context("load EGL")? };

        // SAFETY: `DEFAULT_DISPLAY` is always a valid native display.
        let egl_display = unsafe { egl.get_display(egl_api::DEFAULT_DISPLAY) }
            .std_context("eglGetDisplay failed")?;
        egl.initialize(egl_display).std_context("eglInitialize")?;

        let config = egl
            .choose_first_config(
                egl_display,
                &[
                    egl_api::RED_SIZE,
                    8,
                    egl_api::GREEN_SIZE,
                    8,
                    egl_api::BLUE_SIZE,
                    8,
                    egl_api::ALPHA_SIZE,
                    8,
                    egl_api::RENDERABLE_TYPE,
                    egl_api::OPENGL_ES2_BIT,
                    egl_api::SURFACE_TYPE,
                    egl_api::WINDOW_BIT,
                    egl_api::NONE,
                ],
            )
            .std_context("eglChooseConfig")?
            .std_context("no matching EGL config")?;

        egl.bind_api(egl_api::OPENGL_ES_API)
            .std_context("eglBindAPI")?;

        let egl_context = egl
            .create_context(
                egl_display,
                config,
                None,
                &[egl_api::CONTEXT_CLIENT_VERSION, 2, egl_api::NONE],
            )
            .std_context("eglCreateContext")?;

        // SAFETY: The caller guarantees `native_window`.
        let egl_surface = unsafe {
            egl.create_window_surface(
                egl_display,
                config,
                native_window as egl_api::NativeWindowType,
                None,
            )
        }
        .std_context("eglCreateWindowSurface")?;

        egl.make_current(
            egl_display,
            Some(egl_surface),
            Some(egl_surface),
            Some(egl_context),
        )
        .std_context("eglMakeCurrent")?;

        // SAFETY: The context was made current on this thread just above. It
        // stays current for every GL call in the rest of this function.
        let gl = unsafe { egl::create_glow_context(&egl) };
        let extensions = Extensions::load(&egl);

        let oes = [OES_FRAG_HEAD, ROTATE, OES_FRAG_TAIL].concat();
        let nv12 = [NV12_FRAG_HEAD, ROTATE, NV12_READS, gles_yuv::YUV_TO_RGB].concat();
        // SAFETY: As above, for every GL call in the rest of this function.
        let [oes_program, nv12_program] =
            unsafe { gles_yuv::programs(&gl, [oes.as_str(), nv12.as_str()])? };
        let oes_rotation_loc =
            unsafe { gl.get_uniform_location(oes_program.program, "u_rotation") };
        let nv12_rotation_loc =
            unsafe { gl.get_uniform_location(nv12_program.program, "u_rotation") };
        // The Y plane goes on texture unit 0 and the UV plane on unit 1.
        unsafe {
            gles_yuv::bind_samplers(
                &gl,
                nv12_program.program,
                &[("u_y_tex", 0), ("u_uv_tex", 1)],
            );
        }

        let oes_texture = unsafe { gles_yuv::texture(&gl, GL_TEXTURE_EXTERNAL_OES)? };
        let nv12_y_texture = unsafe { gles_yuv::texture(&gl, glow::TEXTURE_2D)? };
        let nv12_uv_texture = unsafe { gles_yuv::texture(&gl, glow::TEXTURE_2D)? };
        let vbo = unsafe { gles_yuv::triangle(&gl)? };

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            // The NV12 planes are packed rows. GL's default alignment of 4
            // would read past a row whose length is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        }

        info!(
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            "AndroidRenderer ready"
        );

        Ok(Self {
            gl,
            egl,
            extensions,
            egl_display,
            egl_context,
            egl_surface,
            oes_program,
            oes_texture,
            oes_rotation_loc,
            nv12_program,
            nv12_y_texture,
            nv12_uv_texture,
            nv12_rotation_loc,
            vbo,
        })
    }

    /// Draws an `AHardwareBuffer` frame, letterboxed and rotated.
    ///
    /// The buffer is imported as an `EGLImage` for this one draw, without a
    /// copy. If the import fails, this logs a warning and draws nothing. If the
    /// driver lacks the EGL extensions, it draws nothing, and [`Self::new`]
    /// warned about it. Call [`Self::swap_buffers`] afterwards, then release
    /// the buffer.
    ///
    /// # Safety
    ///
    /// - The EGL context must be current on the calling thread.
    /// - `buffer_ptr` must be a valid `AHardwareBuffer*` with an acquired reference.
    pub(crate) unsafe fn render_hardware_buffer(
        &self,
        buffer_ptr: *mut c_void,
        surface_w: i32,
        surface_h: i32,
        video_w: u32,
        video_h: u32,
        rotation_degrees: u32,
    ) {
        let Some(extensions) = &self.extensions else {
            return;
        };
        // SAFETY: The caller guarantees the buffer and a current context, for
        // this and every EGL and GL call below.
        let Some(client_buffer) =
            (unsafe { extensions.get_native_client_buffer(buffer_ptr as *const c_void) })
        else {
            warn!("eglGetNativeClientBufferANDROID failed");
            return;
        };

        let attrs = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
        let Some(egl_image) = (unsafe {
            extensions.create_image(
                self.egl_display.as_ptr(),
                EGL_NATIVE_BUFFER_ANDROID,
                client_buffer,
                attrs.as_ptr(),
            )
        }) else {
            warn!("eglCreateImageKHR failed");
            return;
        };

        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl
                .bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.oes_texture));
            extensions.image_target_texture_2d(GL_TEXTURE_EXTERNAL_OES, egl_image);
            self.draw(
                self.oes_program,
                self.oes_rotation_loc.as_ref(),
                (surface_w, surface_h),
                (video_w, video_h),
                rotation_degrees,
            );
            extensions.destroy_image(self.egl_display.as_ptr(), egl_image);
        }
    }

    /// Draws an NV12 frame from CPU memory, letterboxed and rotated.
    ///
    /// The planes are uploaded as textures and converted to RGB in the shader,
    /// with no CPU color conversion. Strides are in bytes. A frame whose planes
    /// are shorter than the picture is skipped with a warning. Call
    /// [`Self::swap_buffers`] afterwards.
    ///
    /// # Safety
    ///
    /// The EGL context must be current on the calling thread.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call carries both planes, their strides, the picture and the surface"
    )]
    pub(crate) unsafe fn render_nv12(
        &self,
        y_data: &[u8],
        y_stride: u32,
        uv_data: &[u8],
        uv_stride: u32,
        width: u32,
        height: u32,
        surface_w: i32,
        surface_h: i32,
        rotation_degrees: u32,
    ) {
        let uv_h = height.div_ceil(2);
        let uv_w = width.div_ceil(2);

        // GLES2 has no GL_UNPACK_ROW_LENGTH. Row padding must be stripped
        // before upload, or it shows up as a green stripe.
        let y_stripped;
        let y_upload: &[u8] = if y_stride == width {
            y_data
        } else {
            y_stripped = strip_stride(y_data, width as usize, y_stride as usize, height as usize);
            &y_stripped
        };

        let uv_row_bytes = uv_w * 2; // LUMINANCE_ALPHA has 2 bytes per texel.
        let uv_stripped;
        let uv_upload: &[u8] = if uv_stride == uv_row_bytes {
            uv_data
        } else {
            uv_stripped = strip_stride(
                uv_data,
                uv_row_bytes as usize,
                uv_stride as usize,
                uv_h as usize,
            );
            &uv_stripped
        };

        if y_upload.len() < (width * height) as usize
            || uv_upload.len() < (uv_row_bytes * uv_h) as usize
        {
            warn!(
                width,
                height,
                y_len = y_upload.len(),
                uv_len = uv_upload.len(),
                "NV12 planes are shorter than the picture, skipping the frame"
            );
            return;
        }

        // SAFETY: The caller guarantees a current context, for this and the
        // GL calls below. Each plane holds at least the bytes the upload reads,
        // as checked above.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl
                .bind_texture(glow::TEXTURE_2D, Some(self.nv12_y_texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::LUMINANCE as i32,
                width as i32,
                height as i32,
                0,
                glow::LUMINANCE,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(y_upload)),
            );
        }

        unsafe {
            self.gl.active_texture(glow::TEXTURE1);
            self.gl
                .bind_texture(glow::TEXTURE_2D, Some(self.nv12_uv_texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::LUMINANCE_ALPHA as i32,
                uv_w as i32,
                uv_h as i32,
                0,
                glow::LUMINANCE_ALPHA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(uv_upload)),
            );
        }

        unsafe {
            self.draw(
                self.nv12_program,
                self.nv12_rotation_loc.as_ref(),
                (surface_w, surface_h),
                (width, height),
                rotation_degrees,
            );
        }
    }

    /// Clears the surface and draws the bound textures with `program`.
    ///
    /// # Safety
    ///
    /// The EGL context must be current on the calling thread.
    unsafe fn draw(
        &self,
        program: Program,
        rotation_loc: Option<&glow::UniformLocation>,
        (surface_w, surface_h): (i32, i32),
        (video_w, video_h): (u32, u32),
        rotation_degrees: u32,
    ) {
        // At 90 or 270 degrees, the displayed width and height swap.
        let (disp_w, disp_h) = match rotation_degrees {
            90 | 270 => (video_h, video_w),
            _ => (video_w, video_h),
        };
        let (vp_x, vp_y, vp_w, vp_h) = letterbox_viewport(surface_w, surface_h, disp_w, disp_h);
        // SAFETY: The caller guarantees a current context.
        unsafe {
            self.gl.viewport(0, 0, surface_w, surface_h);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            self.gl.viewport(vp_x, vp_y, vp_w, vp_h);
            self.gl.use_program(Some(program.program));
            if let Some(loc) = rotation_loc {
                self.gl.uniform_1_i32(Some(loc), rotation_degrees as i32);
            }
            gles_yuv::draw(&self.gl, self.vbo, program.a_pos);
        }
    }

    /// Presents the drawn frame by swapping the EGL buffers.
    ///
    /// A failed swap, such as on a surface Android already destroyed, is
    /// logged at debug and drops the frame.
    pub(crate) fn swap_buffers(&self) {
        if let Err(err) = self.egl.swap_buffers(self.egl_display, self.egl_surface) {
            debug!(error = %err, "eglSwapBuffers failed, dropping the frame");
        }
    }

    /// Makes the EGL context current on the calling thread.
    ///
    /// Call it before drawing. A Kotlin coroutine can resume on a different
    /// thread than the one that last drew.
    pub(crate) fn make_current(&self) {
        if let Err(err) = self.egl.make_current(
            self.egl_display,
            Some(self.egl_surface),
            Some(self.egl_surface),
            Some(self.egl_context),
        ) {
            warn!(error = %err, "eglMakeCurrent failed, so the next draws go nowhere");
        }
    }

    /// Destroys the EGL surface and context.
    ///
    /// The EGL display stays initialized. A step that fails is logged at debug,
    /// and the teardown goes on.
    pub(crate) fn teardown(&self) {
        if let Err(err) = self.egl.make_current(self.egl_display, None, None, None) {
            debug!(error = %err, "releasing the EGL context failed");
        }
        if let Err(err) = self.egl.destroy_surface(self.egl_display, self.egl_surface) {
            debug!(error = %err, "destroying the EGL surface failed");
        }
        if let Err(err) = self.egl.destroy_context(self.egl_display, self.egl_context) {
            debug!(error = %err, "destroying the EGL context failed");
        }
        info!("EGL teardown complete");
    }
}

/// Returns the centered `(x, y, w, h)` viewport that keeps the video's aspect ratio.
fn letterbox_viewport(
    surface_width: i32,
    surface_height: i32,
    video_width: u32,
    video_height: u32,
) -> (i32, i32, i32, i32) {
    if surface_width <= 0 || surface_height <= 0 || video_width == 0 || video_height == 0 {
        return (0, 0, surface_width.max(1), surface_height.max(1));
    }
    let video_aspect = video_width as f32 / video_height as f32;
    let surface_aspect = surface_width as f32 / surface_height as f32;
    if video_aspect > surface_aspect {
        // Wider than the surface: bars above and below.
        let height = (surface_width as f32 / video_aspect) as i32;
        (0, (surface_height - height) / 2, surface_width, height)
    } else {
        // Taller than the surface: bars left and right.
        let width = (surface_height as f32 * video_aspect) as i32;
        ((surface_width - width) / 2, 0, width, surface_height)
    }
}

/// Copies strided pixel rows into a packed buffer without row padding.
fn strip_stride(data: &[u8], row_bytes: usize, stride: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(row_bytes * rows);
    for y in 0..rows {
        let start = y * stride;
        let end = (start + row_bytes).min(data.len());
        if start < data.len() {
            out.extend_from_slice(&data[start..end]);
            // Zero-fill a truncated last row.
            if end - start < row_bytes {
                out.resize(out.len() + row_bytes - (end - start), 0);
            }
        }
    }
    out
}
