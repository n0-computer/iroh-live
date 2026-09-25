//! A GLES2 video renderer over `glow`.
//!
//! The Pi Zero has no Vulkan or wgpu, so `moq_video::render` cannot draw here.
//! I420 frames from openh264 go up as three `LUMINANCE` textures, and a shader
//! converts the colour. This keeps the CPU free for decoding. Other surfaces
//! go up as one RGBA texture. Works with any EGL/GLES2 context.

use anyhow::{Context as _, Result, bail};
use glow::HasContext;
use moq_video::{Frame, Surface};
use tracing::warn;

const VERT_SRC: &str = "\
#version 100
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(a_pos.x, 1.0 - a_pos.y);
}";

const RGBA_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
}";

/// BT.601 limited-range I420 to RGBA conversion.
///
/// Each plane is a `LUMINANCE` texture, so every read uses `.r`. Bilinear
/// filtering upsamples the half-size chroma planes.
const I420_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y_tex;
uniform sampler2D u_u_tex;
uniform sampler2D u_v_tex;
void main() {
    float y_raw = texture2D(u_y_tex, v_uv).r;
    float u_raw = texture2D(u_u_tex, v_uv).r;
    float v_raw = texture2D(u_v_tex, v_uv).r;
    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float u = (u_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float v = (v_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float r = y + 1.402 * v;
    float g = y - 0.344136 * u - 0.714136 * v;
    float b = y + 1.772 * u;
    gl_FragColor = vec4(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}";

/// The upload path of the last frame, which decides the program `draw` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveMode {
    Rgba,
    I420,
}

/// A GLES2 renderer with RGBA and I420 upload paths.
///
/// The caller creates the GL context and swaps buffers.
#[derive(Debug)]
pub(crate) struct GlesRenderer {
    gl: glow::Context,
    // Shared vertex state.
    vbo: glow::Buffer,
    a_pos_loc: u32,
    // RGBA path.
    rgba_program: glow::Program,
    rgba_texture: glow::Texture,
    // I420 path.
    i420_program: glow::Program,
    i420_a_pos_loc: u32,
    y_texture: glow::Texture,
    u_texture: glow::Texture,
    v_texture: glow::Texture,
    active: ActiveMode,
    tex_width: u32,
    tex_height: u32,
    chroma_tex_width: u32,
    chroma_tex_height: u32,
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
    unsafe { gl.attach_shader(program, vs) };
    unsafe { gl.attach_shader(program, fs) };
    unsafe { gl.link_program(program) };
    if !unsafe { gl.get_program_link_status(program) } {
        let log = unsafe { gl.get_program_info_log(program) };
        unsafe { gl.delete_program(program) };
        bail!("shader link: {log}");
    }
    Ok(program)
}

fn create_texture(gl: &glow::Context) -> Result<glow::Texture> {
    let texture = unsafe { gl.create_texture() }.map_err(|e| anyhow::anyhow!(e))?;
    unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(texture)) };
    for param in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, param, glow::LINEAR as i32) };
    }
    for param in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, param, glow::CLAMP_TO_EDGE as i32) };
    }
    Ok(texture)
}

impl GlesRenderer {
    /// Creates both shader programs, VBO, and textures.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn new(gl: glow::Context) -> Result<Self> {
        let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;

        let rgba_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, RGBA_FRAG_SRC)?;
        let rgba_program = link_program(&gl, vs, rgba_fs)?;
        unsafe { gl.delete_shader(rgba_fs) };
        let a_pos_loc = unsafe { gl.get_attrib_location(rgba_program, "a_pos") }
            .context("a_pos not found in RGBA program")?;

        let nv12_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, I420_FRAG_SRC)?;
        let i420_program = link_program(&gl, vs, nv12_fs)?;
        unsafe { gl.delete_shader(nv12_fs) };
        unsafe { gl.delete_shader(vs) };
        let i420_a_pos_loc = unsafe { gl.get_attrib_location(i420_program, "a_pos") }
            .context("a_pos not found in NV12 program")?;

        // The Y, U and V samplers read texture units 0, 1 and 2.
        unsafe { gl.use_program(Some(i420_program)) };
        if let Some(loc) = unsafe { gl.get_uniform_location(i420_program, "u_y_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 0) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(i420_program, "u_u_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 1) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(i420_program, "u_v_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 2) };
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
        unsafe { gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo)) };
        unsafe { gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vert_bytes, glow::STATIC_DRAW) };

        // The I420 planes are packed rows. GL's default alignment of 4 would
        // read past a row whose length is not a multiple of 4.
        unsafe { gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1) };

        let rgba_texture = create_texture(&gl)?;
        let y_texture = create_texture(&gl)?;
        let u_texture = create_texture(&gl)?;
        let v_texture = create_texture(&gl)?;

        Ok(Self {
            gl,
            vbo,
            a_pos_loc,
            rgba_program,
            rgba_texture,
            i420_program,
            i420_a_pos_loc,
            y_texture,
            u_texture,
            v_texture,
            active: ActiveMode::Rgba,
            tex_width: 0,
            tex_height: 0,
            chroma_tex_width: 0,
            chroma_tex_height: 0,
        })
    }

    /// Uploads RGBA pixel data to the texture.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) {
        self.active = ActiveMode::Rgba;
        unsafe {
            upload_tex(
                &self.gl,
                self.rgba_texture,
                glow::RGBA,
                rgba,
                w,
                h,
                &mut self.tex_width,
                &mut self.tex_height,
            );
        }
    }

    /// Uploads the three I420 planes for the shader to convert.
    ///
    /// The planes must be tightly packed. GLES2 has no `GL_UNPACK_ROW_LENGTH`,
    /// so row padding has to be removed on the CPU first.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn upload_i420(&mut self, y: &[u8], u: &[u8], v: &[u8], w: u32, h: u32) {
        self.active = ActiveMode::I420;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);

        unsafe {
            upload_tex(
                &self.gl,
                self.y_texture,
                glow::LUMINANCE,
                &y[..(w * h) as usize],
                w,
                h,
                &mut self.tex_width,
                &mut self.tex_height,
            );
            upload_tex(
                &self.gl,
                self.u_texture,
                glow::LUMINANCE,
                &u[..(chroma_w * chroma_h) as usize],
                chroma_w,
                chroma_h,
                &mut self.chroma_tex_width,
                &mut self.chroma_tex_height,
            );
            upload_tex(
                &self.gl,
                self.v_texture,
                glow::LUMINANCE,
                &v[..(chroma_w * chroma_h) as usize],
                chroma_w,
                chroma_h,
                &mut self.chroma_tex_width,
                &mut self.chroma_tex_height,
            );
        }
    }

    /// Uploads a decoded frame, as planes if it is I420 and as RGBA otherwise.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn upload_frame(&mut self, frame: &Frame) {
        let size = frame.size();
        match &frame.surface {
            Surface::I420(i420) => unsafe {
                self.upload_i420(i420.y(), i420.u(), i420.v(), i420.width(), i420.height());
            },
            other => match other.to_rgba(&Default::default()) {
                Ok(rgba) => unsafe {
                    self.upload_rgba(rgba.data(), rgba.width(), rgba.height());
                },
                Err(err) => {
                    warn!(error = %err, %size, "failed to convert a frame for display");
                }
            },
        }
    }

    /// Draws the uploaded frame as a fullscreen triangle.
    ///
    /// The caller swaps buffers afterwards.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn draw(&self, vp_w: i32, vp_h: i32) {
        unsafe {
            self.gl.viewport(0, 0, vp_w, vp_h);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);

            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));

            match self.active {
                ActiveMode::Rgba => {
                    self.gl.use_program(Some(self.rgba_program));
                    self.gl
                        .vertex_attrib_pointer_f32(self.a_pos_loc, 2, glow::FLOAT, false, 0, 0);
                    self.gl.enable_vertex_attrib_array(self.a_pos_loc);
                    self.gl.active_texture(glow::TEXTURE0);
                    self.gl
                        .bind_texture(glow::TEXTURE_2D, Some(self.rgba_texture));
                }
                ActiveMode::I420 => {
                    self.gl.use_program(Some(self.i420_program));
                    self.gl.vertex_attrib_pointer_f32(
                        self.i420_a_pos_loc,
                        2,
                        glow::FLOAT,
                        false,
                        0,
                        0,
                    );
                    self.gl.enable_vertex_attrib_array(self.i420_a_pos_loc);
                    self.gl.active_texture(glow::TEXTURE0);
                    self.gl.bind_texture(glow::TEXTURE_2D, Some(self.y_texture));
                    self.gl.active_texture(glow::TEXTURE1);
                    self.gl.bind_texture(glow::TEXTURE_2D, Some(self.u_texture));
                    self.gl.active_texture(glow::TEXTURE2);
                    self.gl.bind_texture(glow::TEXTURE_2D, Some(self.v_texture));
                }
            }

            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);

            match self.active {
                ActiveMode::Rgba => self.gl.disable_vertex_attrib_array(self.a_pos_loc),
                ActiveMode::I420 => self.gl.disable_vertex_attrib_array(self.i420_a_pos_loc),
            }
        }
    }

    /// Uploads a frame and draws it in one call.
    ///
    /// The caller swaps buffers afterwards.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    #[allow(
        dead_code,
        reason = "convenience wrapper for a caller that has no use for upload and draw separately"
    )]
    pub(crate) unsafe fn render_frame(&mut self, frame: &Frame, vp_w: i32, vp_h: i32) {
        unsafe {
            self.upload_frame(frame);
            self.draw(vp_w, vp_h);
        }
    }

    /// Returns a reference to the underlying `glow::Context`.
    #[allow(
        dead_code,
        reason = "exposed for a caller that needs to issue its own GL calls"
    )]
    pub(crate) fn gl(&self) -> &glow::Context {
        &self.gl
    }

    /// Returns the dimensions of the last uploaded texture.
    #[allow(
        dead_code,
        reason = "exposed for a caller that lays out UI around the video size"
    )]
    pub(crate) fn texture_dimensions(&self) -> (u32, u32) {
        (self.tex_width, self.tex_height)
    }
}

impl Drop for GlesRenderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.rgba_texture);
            self.gl.delete_texture(self.y_texture);
            self.gl.delete_texture(self.u_texture);
            self.gl.delete_texture(self.v_texture);
            self.gl.delete_program(self.rgba_program);
            self.gl.delete_program(self.i420_program);
            self.gl.delete_buffer(self.vbo);
        }
    }
}

/// Uploads pixel data, reallocating the texture only when its size changes.
///
/// `cached_w` and `cached_h` hold the texture's allocated size.
#[allow(
    clippy::too_many_arguments,
    reason = "GL upload needs all texture parameters"
)]
unsafe fn upload_tex(
    gl: &glow::Context,
    texture: glow::Texture,
    format: u32,
    data: &[u8],
    w: u32,
    h: u32,
    cached_w: &mut u32,
    cached_h: &mut u32,
) {
    unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(texture)) };
    if w != *cached_w || h != *cached_h {
        unsafe {
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                format as i32,
                w as i32,
                h as i32,
                0,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(data)),
            );
        }
        *cached_w = w;
        *cached_h = h;
    } else {
        unsafe {
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                w as i32,
                h as i32,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(data)),
            );
        }
    }
}
