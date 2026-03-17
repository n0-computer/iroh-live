//! GLES2 video frame renderer using `glow`.
//!
//! Uploads RGBA [`VideoFrame`]s to a `GL_TEXTURE_2D` and draws a fullscreen
//! textured triangle. Works with any EGL/GLES2 context (Linux DRM/KMS,
//! Android, glutin+winit, etc.).
//!
//! Requires the `gles` feature.

use anyhow::{Context as _, Result, bail};
use glow::HasContext;

use crate::format::VideoFrame;

// ── GLES2 shaders ──────────────────────────────────────────────────

const VERT_SRC: &str = "\
#version 100
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(a_pos.x, 1.0 - a_pos.y);
}";

const FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
}";

/// GLES2 renderer: uploads RGBA textures and draws a fullscreen triangle.
///
/// Platform-agnostic — works with any `glow::Context`. The caller is
/// responsible for creating the GL context and swapping buffers.
#[derive(Debug)]
pub struct GlesRenderer {
    gl: glow::Context,
    program: glow::Program,
    vbo: glow::Buffer,
    texture: glow::Texture,
    a_pos_loc: u32,
    tex_width: u32,
    tex_height: u32,
}

impl GlesRenderer {
    /// Creates the shader program, VBO, and texture.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn new(gl: glow::Context) -> Result<Self> {
        let vs =
            unsafe { gl.create_shader(glow::VERTEX_SHADER) }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.shader_source(vs, VERT_SRC) };
        unsafe { gl.compile_shader(vs) };
        if !unsafe { gl.get_shader_compile_status(vs) } {
            bail!("vertex shader: {}", unsafe { gl.get_shader_info_log(vs) });
        }

        let fs =
            unsafe { gl.create_shader(glow::FRAGMENT_SHADER) }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.shader_source(fs, FRAG_SRC) };
        unsafe { gl.compile_shader(fs) };
        if !unsafe { gl.get_shader_compile_status(fs) } {
            bail!("fragment shader: {}", unsafe { gl.get_shader_info_log(fs) });
        }

        let program = unsafe { gl.create_program() }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.attach_shader(program, vs) };
        unsafe { gl.attach_shader(program, fs) };
        unsafe { gl.link_program(program) };
        if !unsafe { gl.get_program_link_status(program) } {
            bail!("link: {}", unsafe { gl.get_program_info_log(program) });
        }
        unsafe { gl.delete_shader(vs) };
        unsafe { gl.delete_shader(fs) };

        let a_pos_loc = unsafe { gl.get_attrib_location(program, "a_pos") }
            .context("a_pos attribute not found")?;

        // Fullscreen triangle: three vertices covering [-1,1] clip space.
        let vertices: [f32; 6] = [0.0, 0.0, 2.0, 0.0, 0.0, 2.0];
        let vert_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                vertices.len() * std::mem::size_of::<f32>(),
            )
        };
        let vbo = unsafe { gl.create_buffer() }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo)) };
        unsafe { gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vert_bytes, glow::STATIC_DRAW) };

        let texture = unsafe { gl.create_texture() }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(texture)) };
        unsafe {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            )
        };
        unsafe {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            )
        };
        unsafe {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            )
        };
        unsafe {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            )
        };

        Ok(Self {
            gl,
            program,
            vbo,
            texture,
            a_pos_loc,
            tex_width: 0,
            tex_height: 0,
        })
    }

    /// Uploads RGBA pixel data to the texture.
    ///
    /// Uses `tex_sub_image_2d` when dimensions match the previous upload,
    /// avoiding a full texture reallocation.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) {
        unsafe { self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture)) };
        if w != self.tex_width || h != self.tex_height {
            unsafe {
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA as i32,
                    w as i32,
                    h as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rgba)),
                );
            }
            self.tex_width = w;
            self.tex_height = h;
        } else {
            unsafe {
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rgba)),
                );
            }
        }
    }

    /// Uploads a [`VideoFrame`] by converting it to RGBA first.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_frame(&mut self, frame: &VideoFrame) {
        let rgba = frame.rgba_image();
        unsafe { self.upload_rgba(rgba.as_raw(), rgba.width(), rgba.height()) };
    }

    /// Draws the textured fullscreen triangle.
    ///
    /// Clears the viewport to black and renders the texture. The caller
    /// must swap buffers after this call.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn draw(&self, vp_w: i32, vp_h: i32) {
        unsafe {
            self.gl.viewport(0, 0, vp_w, vp_h);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);

            self.gl.use_program(Some(self.program));
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .vertex_attrib_pointer_f32(self.a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(self.a_pos_loc);

            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));

            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);

            self.gl.disable_vertex_attrib_array(self.a_pos_loc);
        }
    }
}
