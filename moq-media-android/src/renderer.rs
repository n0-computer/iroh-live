//! Android GLES2 renderer for HardwareBuffer video frames.
//!
//! Imports `AHardwareBuffer` frames as `GL_TEXTURE_EXTERNAL_OES` via EGL,
//! then draws a fullscreen textured triangle. All GL calls happen in Rust —
//! the Kotlin side only manages the EGL context lifecycle and swaps buffers.

use std::ffi::c_void;

use anyhow::{Context as _, Result, bail};
use glow::HasContext;

use crate::egl;

/// `GL_TEXTURE_EXTERNAL_OES` — not in glow's constants.
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
/// `EGL_NATIVE_BUFFER_ANDROID` target for `eglCreateImageKHR`.
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
/// `EGL_IMAGE_PRESERVED_KHR`.
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
/// `EGL_TRUE`.
const EGL_TRUE: i32 = 1;
/// `EGL_NONE`.
const EGL_NONE: i32 = 0x3038;

const VERT_SRC: &str = "\
#version 100
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(a_pos.x, 1.0 - a_pos.y);
}";

/// Fragment shader using `samplerExternalOES` for HardwareBuffer textures.
///
/// Applies sensor rotation via `u_rotation` uniform (0/90/180/270 degrees CW).
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

/// Renders `AHardwareBuffer` frames via EGL external texture import.
///
/// Owns a GLES2 shader program for `GL_TEXTURE_EXTERNAL_OES` and handles
/// the per-frame EGLImage lifecycle. The caller must ensure the EGL context
/// is current on the calling thread for all methods.
pub struct AndroidRenderer {
    gl: glow::Context,
    program: glow::Program,
    texture: glow::Texture,
    a_pos_loc: u32,
    rotation_loc: Option<glow::UniformLocation>,
    vbo: glow::Buffer,
    egl_display: *mut c_void,
}

// SAFETY: The raw EGL display pointer is only used from the GL thread,
// which is guaranteed by the caller contract (EGL context must be current).
unsafe impl Send for AndroidRenderer {}

impl std::fmt::Debug for AndroidRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidRenderer")
            .field("egl_display", &self.egl_display)
            .finish()
    }
}

impl AndroidRenderer {
    /// Creates the renderer: compiles the OES shader, allocates the texture and VBO.
    ///
    /// `egl_display` is the native `EGLDisplay` pointer (from Kotlin's
    /// `EGLDisplay.getNativeHandle()`).
    ///
    /// # Safety
    /// The EGL context must be current on the calling thread.
    pub unsafe fn new(egl_display: *mut c_void) -> Result<Self> {
        let gl = unsafe { egl::create_glow_context() };

        // Compile shaders.
        let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;
        let fs = compile_shader(&gl, glow::FRAGMENT_SHADER, OES_FRAG_SRC)?;
        let program = link_program(&gl, vs, fs)?;
        unsafe {
            gl.delete_shader(vs);
            gl.delete_shader(fs);
        }

        let a_pos_loc =
            unsafe { gl.get_attrib_location(program, "a_pos") }.context("a_pos not found")?;
        let rotation_loc = unsafe { gl.get_uniform_location(program, "u_rotation") };

        // OES texture.
        let texture = unsafe { gl.create_texture() }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe {
            gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(texture));
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
        }

        // Fullscreen triangle VBO.
        let vertices: [f32; 6] = [0.0, 0.0, 2.0, 0.0, 0.0, 2.0];
        let vert_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                vertices.len() * std::mem::size_of::<f32>(),
            )
        };
        let vbo = unsafe { gl.create_buffer() }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vert_bytes, glow::STATIC_DRAW);
        }

        unsafe { gl.clear_color(0.0, 0.0, 0.0, 1.0) };

        tracing::info!(
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            "AndroidRenderer ready"
        );

        Ok(Self {
            gl,
            program,
            texture,
            a_pos_loc,
            rotation_loc,
            vbo,
            egl_display,
        })
    }

    /// Renders an `AHardwareBuffer` frame to the current viewport.
    ///
    /// Creates an EGLImage from the buffer, binds it to the OES texture,
    /// draws a letterboxed fullscreen triangle, and destroys the EGLImage.
    /// The caller must swap buffers and release the HardwareBuffer afterward.
    ///
    /// # Safety
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
        // AHardwareBuffer → EGLClientBuffer.
        let Some(client_buffer) =
            (unsafe { egl::get_native_client_buffer(buffer_ptr as *const c_void) })
        else {
            tracing::warn!("eglGetNativeClientBufferANDROID failed");
            return;
        };

        // EGLClientBuffer → EGLImage.
        let attrs = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
        let Some(egl_image) = (unsafe {
            egl::create_image(
                self.egl_display,
                EGL_NATIVE_BUFFER_ANDROID,
                client_buffer,
                attrs.as_ptr(),
            )
        }) else {
            tracing::warn!("eglCreateImageKHR failed");
            return;
        };

        // Bind EGLImage → OES texture.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl
                .bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.texture));
            egl::image_target_texture_2d(GL_TEXTURE_EXTERNAL_OES, egl_image);
        }

        // Swap video dimensions for 90°/270° rotation (frame is sideways).
        let (disp_w, disp_h) = if rotation_degrees == 90 || rotation_degrees == 270 {
            (video_h, video_w)
        } else {
            (video_w, video_h)
        };

        // Clear full surface, then draw letterboxed.
        unsafe {
            self.gl.viewport(0, 0, surface_w, surface_h);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        let (vp_x, vp_y, vp_w, vp_h) = letterbox_viewport(surface_w, surface_h, disp_w, disp_h);
        unsafe {
            self.gl.viewport(vp_x, vp_y, vp_w, vp_h);
            self.gl.use_program(Some(self.program));
            if let Some(ref loc) = self.rotation_loc {
                self.gl.uniform_1_i32(Some(loc), rotation_degrees as i32);
            }
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .vertex_attrib_pointer_f32(self.a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(self.a_pos_loc);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            self.gl.disable_vertex_attrib_array(self.a_pos_loc);
        }

        // Cleanup.
        unsafe { egl::destroy_image(self.egl_display, egl_image) };
    }
}

/// Computes a letterboxed viewport that preserves the video's aspect ratio.
fn letterbox_viewport(sw: i32, sh: i32, vw: u32, vh: u32) -> (i32, i32, i32, i32) {
    if sw <= 0 || sh <= 0 || vw == 0 || vh == 0 {
        return (0, 0, sw.max(1), sh.max(1));
    }
    let video_aspect = vw as f32 / vh as f32;
    let surface_aspect = sw as f32 / sh as f32;
    if video_aspect > surface_aspect {
        // Video wider than surface — pillarbox top/bottom.
        let vp_w = sw;
        let vp_h = (sw as f32 / video_aspect) as i32;
        (0, (sh - vp_h) / 2, vp_w, vp_h)
    } else {
        // Video taller — letterbox left/right.
        let vp_h = sh;
        let vp_w = (sh as f32 * video_aspect) as i32;
        ((sw - vp_w) / 2, 0, vp_w, vp_h)
    }
}

fn compile_shader(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::Shader> {
    let shader = unsafe { gl.create_shader(kind) }.map_err(|e| anyhow::anyhow!(e))?;
    unsafe { gl.shader_source(shader, source) };
    unsafe { gl.compile_shader(shader) };
    if !unsafe { gl.get_shader_compile_status(shader) } {
        let log = unsafe { gl.get_shader_info_log(shader) };
        unsafe { gl.delete_shader(shader) };
        bail!("shader compile: {log}");
    }
    Ok(shader)
}

fn link_program(gl: &glow::Context, vs: glow::Shader, fs: glow::Shader) -> Result<glow::Program> {
    let program = unsafe { gl.create_program() }.map_err(|e| anyhow::anyhow!(e))?;
    unsafe {
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
    }
    if !unsafe { gl.get_program_link_status(program) } {
        let log = unsafe { gl.get_program_info_log(program) };
        unsafe { gl.delete_program(program) };
        bail!("shader link: {log}");
    }
    Ok(program)
}
