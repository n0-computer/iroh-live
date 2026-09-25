//! GLES2 renderer that owns its EGL context.
//!
//! Kotlin only passes in a `Surface`. All EGL and GL calls happen in Rust.

use std::ffi::c_void;

use glow::HasContext;
use khronos_egl as egl_api;
use n0_error::{Result, StdResultExt, anyerr, bail_any};

use crate::egl::{self, Egl, Extensions};

/// `GL_TEXTURE_EXTERNAL_OES`, which glow does not define.
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_TRUE: i32 = 1;
const EGL_NONE: i32 = 0x3038;

const VERT_SRC: &str = "\
#version 100
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(a_pos.x, 1.0 - a_pos.y);
}";

/// Fragment shader for `AHardwareBuffer` frames, rotated by `u_rotation` degrees clockwise.
const OES_FRAG_SRC: &str = "\
#extension GL_OES_EGL_image_external : require
precision mediump float;
varying vec2 v_uv;
uniform samplerExternalOES u_tex;
uniform int u_rotation;
void main() {
    vec2 uv = v_uv;
    if (u_rotation == 90) {
        uv = vec2(v_uv.y, 1.0 - v_uv.x);
    } else if (u_rotation == 180) {
        uv = vec2(1.0 - v_uv.x, 1.0 - v_uv.y);
    } else if (u_rotation == 270) {
        uv = vec2(1.0 - v_uv.y, v_uv.x);
    }
    gl_FragColor = texture2D(u_tex, uv);
}";

/// Fragment shader that converts BT.601 limited-range NV12 to RGB, rotated like the OES one.
const NV12_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y_tex;
uniform sampler2D u_uv_tex;
uniform int u_rotation;
void main() {
    vec2 uv = v_uv;
    if (u_rotation == 90) {
        uv = vec2(v_uv.y, 1.0 - v_uv.x);
    } else if (u_rotation == 180) {
        uv = vec2(1.0 - v_uv.x, 1.0 - v_uv.y);
    } else if (u_rotation == 270) {
        uv = vec2(1.0 - v_uv.y, v_uv.x);
    }
    float y_raw = texture2D(u_y_tex, uv).r;
    float u_raw = texture2D(u_uv_tex, uv).r;
    float v_raw = texture2D(u_uv_tex, uv).a;
    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float u = (u_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float v = (v_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float r = y + 1.402 * v;
    float g = y - 0.344136 * u - 0.714136 * v;
    float b = y + 1.772 * u;
    gl_FragColor = vec4(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}";

/// Video renderer for one Android window.
///
/// Owns the EGL context and the window surface. Draw a frame with
/// [`Self::render_hardware_buffer`] or [`Self::render_nv12`], then show it with
/// [`Self::swap_buffers`].
#[derive(derive_more::Debug)]
pub struct AndroidRenderer {
    gl: glow::Context,
    #[debug(skip)]
    egl: Egl,
    extensions: Extensions,
    egl_display: egl_api::Display,
    egl_context: egl_api::Context,
    egl_surface: egl_api::Surface,
    oes_program: glow::Program,
    oes_texture: glow::Texture,
    oes_a_pos_loc: u32,
    oes_rotation_loc: Option<glow::UniformLocation>,
    nv12_program: glow::Program,
    nv12_y_texture: glow::Texture,
    nv12_uv_texture: glow::Texture,
    nv12_a_pos_loc: u32,
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
    /// returns, the new EGL context is current on the calling thread. Fails if
    /// any EGL or GL setup step fails.
    ///
    /// # Safety
    ///
    /// `native_window` must be a valid `ANativeWindow*`.
    pub unsafe fn new(native_window: *mut c_void) -> Result<Self> {
        let egl = unsafe { Egl::load_required().std_context("load EGL")? };

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

        let gl = unsafe { egl::create_glow_context(&egl) };
        let extensions = Extensions::load(&egl);

        // Both programs share the vertex shader.
        let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;

        let oes_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, OES_FRAG_SRC)?;
        let oes_program = link_program(&gl, vs, oes_fs)?;
        unsafe { gl.delete_shader(oes_fs) };
        let oes_a_pos_loc = unsafe { gl.get_attrib_location(oes_program, "a_pos") }
            .std_context("a_pos not found in OES program")?;
        let oes_rotation_loc = unsafe { gl.get_uniform_location(oes_program, "u_rotation") };

        let nv12_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, NV12_FRAG_SRC)?;
        let nv12_program = link_program(&gl, vs, nv12_fs)?;
        unsafe {
            gl.delete_shader(nv12_fs);
            gl.delete_shader(vs);
        }
        let nv12_a_pos_loc = unsafe { gl.get_attrib_location(nv12_program, "a_pos") }
            .std_context("a_pos not found in NV12 program")?;
        let nv12_rotation_loc = unsafe { gl.get_uniform_location(nv12_program, "u_rotation") };
        // The Y plane goes on texture unit 0 and the UV plane on unit 1.
        unsafe { gl.use_program(Some(nv12_program)) };
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_y_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 0) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_uv_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 1) };
        }

        let oes_texture = create_tex(&gl, GL_TEXTURE_EXTERNAL_OES)?;
        let nv12_y_texture = create_tex(&gl, glow::TEXTURE_2D)?;
        let nv12_uv_texture = create_tex(&gl, glow::TEXTURE_2D)?;

        // One oversized triangle covers the whole viewport.
        let vertices: [f32; 6] = [0.0, 0.0, 2.0, 0.0, 0.0, 2.0];
        let vert_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                vertices.len() * std::mem::size_of::<f32>(),
            )
        };
        let vbo = unsafe { gl.create_buffer() }.map_err(|e| anyerr!(e))?;
        unsafe {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vert_bytes, glow::STATIC_DRAW);
        }

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            // The NV12 planes are packed rows. GL's default alignment of 4
            // would read past a row whose length is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        }

        tracing::info!(
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
            oes_a_pos_loc,
            oes_rotation_loc,
            nv12_program,
            nv12_y_texture,
            nv12_uv_texture,
            nv12_a_pos_loc,
            nv12_rotation_loc,
            vbo,
        })
    }

    /// Draws an `AHardwareBuffer` frame, letterboxed and rotated.
    ///
    /// The buffer is imported as an `EGLImage` for this one draw, without a
    /// copy. If the import fails, this logs a warning and draws nothing. Call
    /// [`Self::swap_buffers`] afterwards, then release the buffer.
    ///
    /// # Safety
    ///
    /// - The EGL context must be current on the calling thread.
    /// - `buffer_ptr` must be a valid `AHardwareBuffer*` with an acquired reference.
    pub unsafe fn render_hardware_buffer(
        &self,
        buffer_ptr: *mut c_void,
        surface_w: i32,
        surface_h: i32,
        video_w: u32,
        video_h: u32,
        rotation_degrees: u32,
    ) {
        let Some(client_buffer) = (unsafe {
            self.extensions
                .get_native_client_buffer(buffer_ptr as *const c_void)
        }) else {
            tracing::warn!("eglGetNativeClientBufferANDROID failed");
            return;
        };

        let attrs = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
        let Some(egl_image) = (unsafe {
            self.extensions.create_image(
                self.egl_display.as_ptr(),
                EGL_NATIVE_BUFFER_ANDROID,
                client_buffer,
                attrs.as_ptr(),
            )
        }) else {
            tracing::warn!("eglCreateImageKHR failed");
            return;
        };

        let bound = unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl
                .bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.oes_texture));
            self.extensions
                .image_target_texture_2d(GL_TEXTURE_EXTERNAL_OES, egl_image)
        };
        if !bound {
            tracing::warn!("glEGLImageTargetTexture2DOES is not available");
            unsafe {
                self.extensions
                    .destroy_image(self.egl_display.as_ptr(), egl_image)
            };
            return;
        }

        unsafe {
            self.draw(
                self.oes_program,
                self.oes_a_pos_loc,
                self.oes_rotation_loc.as_ref(),
                (surface_w, surface_h),
                (video_w, video_h),
                rotation_degrees,
            );
            self.extensions
                .destroy_image(self.egl_display.as_ptr(), egl_image);
        }
    }

    /// Draws an NV12 frame from CPU memory, letterboxed and rotated.
    ///
    /// The planes are uploaded as textures and converted to RGB in the shader,
    /// with no CPU color conversion. Strides are in bytes. A plane shorter than
    /// the picture is skipped with a warning. Call [`Self::swap_buffers`]
    /// afterwards.
    ///
    /// # Safety
    ///
    /// The EGL context must be current on the calling thread.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call carries both planes, their strides, the picture and the surface"
    )]
    pub unsafe fn render_nv12(
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
            tracing::warn!(
                width,
                height,
                y_len = y_upload.len(),
                uv_len = uv_upload.len(),
                "NV12 planes are shorter than the picture, skipping the frame"
            );
            return;
        }

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
                self.nv12_a_pos_loc,
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
        program: glow::Program,
        a_pos_loc: u32,
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
        unsafe {
            self.gl.viewport(0, 0, surface_w, surface_h);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            self.gl.viewport(vp_x, vp_y, vp_w, vp_h);
            self.gl.use_program(Some(program));
            if let Some(loc) = rotation_loc {
                self.gl.uniform_1_i32(Some(loc), rotation_degrees as i32);
            }
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .vertex_attrib_pointer_f32(a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(a_pos_loc);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            self.gl.disable_vertex_attrib_array(a_pos_loc);
        }
    }

    /// Presents the drawn frame by swapping the EGL buffers.
    pub fn swap_buffers(&self) {
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .ok();
    }

    /// Makes the EGL context current on the calling thread.
    ///
    /// Call it before drawing. A Kotlin coroutine can resume on a different
    /// thread than the one that last drew.
    pub fn make_current(&self) {
        self.egl
            .make_current(
                self.egl_display,
                Some(self.egl_surface),
                Some(self.egl_surface),
                Some(self.egl_context),
            )
            .ok();
    }

    /// Destroys the EGL surface and context.
    ///
    /// The EGL display stays initialized.
    pub fn teardown(&self) {
        self.egl
            .make_current(self.egl_display, None, None, None)
            .ok();
        self.egl
            .destroy_surface(self.egl_display, self.egl_surface)
            .ok();
        self.egl
            .destroy_context(self.egl_display, self.egl_context)
            .ok();
        tracing::info!("EGL teardown complete");
    }
}

/// Creates a texture with linear filtering and edge clamping.
fn create_tex(gl: &glow::Context, target: u32) -> Result<glow::Texture> {
    let texture = unsafe { gl.create_texture() }.map_err(|e| anyerr!(e))?;
    unsafe {
        gl.bind_texture(target, Some(texture));
        gl.tex_parameter_i32(target, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    }
    Ok(texture)
}

/// Returns the centered `(x, y, w, h)` viewport that keeps the video's aspect ratio.
fn letterbox_viewport(sw: i32, sh: i32, vw: u32, vh: u32) -> (i32, i32, i32, i32) {
    if sw <= 0 || sh <= 0 || vw == 0 || vh == 0 {
        return (0, 0, sw.max(1), sh.max(1));
    }
    let video_aspect = vw as f32 / vh as f32;
    let surface_aspect = sw as f32 / sh as f32;
    if video_aspect > surface_aspect {
        // Wider than the surface: bars above and below.
        let vp_w = sw;
        let vp_h = (sw as f32 / video_aspect) as i32;
        (0, (sh - vp_h) / 2, vp_w, vp_h)
    } else {
        // Taller than the surface: bars left and right.
        let vp_h = sh;
        let vp_w = (sh as f32 * video_aspect) as i32;
        ((sw - vp_w) / 2, 0, vp_w, vp_h)
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

fn compile_shader(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::Shader> {
    let shader = unsafe { gl.create_shader(kind) }.map_err(|e| anyerr!(e))?;
    unsafe { gl.shader_source(shader, source) };
    unsafe { gl.compile_shader(shader) };
    if !unsafe { gl.get_shader_compile_status(shader) } {
        let log = unsafe { gl.get_shader_info_log(shader) };
        unsafe { gl.delete_shader(shader) };
        bail_any!("shader compile: {log}");
    }
    Ok(shader)
}

fn link_program(gl: &glow::Context, vs: glow::Shader, fs: glow::Shader) -> Result<glow::Program> {
    let program = unsafe { gl.create_program() }.map_err(|e| anyerr!(e))?;
    unsafe {
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
    }
    if !unsafe { gl.get_program_link_status(program) } {
        let log = unsafe { gl.get_program_info_log(program) };
        unsafe { gl.delete_program(program) };
        bail_any!("shader link: {log}");
    }
    Ok(program)
}
