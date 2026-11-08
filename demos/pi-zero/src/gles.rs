//! A GLES2 video renderer over `glow`.
//!
//! The Pi Zero has no Vulkan or wgpu, so `iroh_live::media::video::render`
//! cannot draw here. I420 frames from openh264 go up as three `LUMINANCE`
//! textures, and a shader converts the colour. This keeps the CPU free for
//! decoding. Other surfaces go up as one RGBA texture. The renderer works with
//! any EGL/GLES2 context.

use anyhow::{Context as _, Result, bail};
use glow::HasContext;
use iroh_live::media::video::{Frame, Surface};
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

/// A texture and the size it is allocated at.
#[derive(Debug)]
struct PlaneTexture {
    texture: glow::Texture,
    /// `(0, 0)` until the first upload allocates the texture.
    size: (u32, u32),
}

impl PlaneTexture {
    /// Creates a texture with linear filtering and edge clamping.
    ///
    /// # Safety
    ///
    /// The GL context of `gl` must be current on the calling thread.
    unsafe fn new(gl: &glow::Context) -> Result<Self> {
        // SAFETY: the caller keeps the context current.
        unsafe {
            let texture = gl.create_texture().map_err(|e| anyhow::anyhow!(e))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for param in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, param, glow::LINEAR as i32);
            }
            for param in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, param, glow::CLAMP_TO_EDGE as i32);
            }
            Ok(Self {
                texture,
                size: (0, 0),
            })
        }
    }

    /// Uploads `data` of `format`, reallocating the texture only when its size changes.
    ///
    /// # Safety
    ///
    /// The GL context of `gl` must be current on the calling thread.
    unsafe fn upload(&mut self, gl: &glow::Context, format: u32, data: &[u8], w: u32, h: u32) {
        let pixels = glow::PixelUnpackData::Slice(Some(data));
        // SAFETY: the caller keeps the context current, and `data` holds
        // `w * h` pixels of `format`, which the callers slice it to.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            if (w, h) == self.size {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    format,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
            } else {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    format as i32,
                    w as i32,
                    h as i32,
                    0,
                    format,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
                self.size = (w, h);
            }
        }
    }
}

/// A GLES2 renderer with RGBA and I420 upload paths.
///
/// The caller creates the GL context and swaps buffers. The context must be
/// current on the thread that drops the renderer, which frees its GL objects.
#[derive(Debug)]
pub(crate) struct GlesRenderer {
    gl: glow::Context,
    /// The fullscreen triangle both programs draw.
    vbo: glow::Buffer,
    rgba_program: glow::Program,
    rgba_a_pos: u32,
    rgba: PlaneTexture,
    i420_program: glow::Program,
    i420_a_pos: u32,
    y: PlaneTexture,
    u: PlaneTexture,
    v: PlaneTexture,
    active: ActiveMode,
}

/// Compiles a shader of `kind` from `source`.
///
/// # Safety
///
/// The GL context of `gl` must be current on the calling thread.
unsafe fn compile_shader(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::Shader> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let shader = gl.create_shader(kind).map_err(|e| anyhow::anyhow!(e))?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            bail!("shader compile: {log}");
        }
        Ok(shader)
    }
}

/// Links the vertex shader `vs` and the fragment shader `fs` into a program.
///
/// # Safety
///
/// The GL context of `gl` must be current on the calling thread.
unsafe fn link_program(
    gl: &glow::Context,
    vs: glow::Shader,
    fs: glow::Shader,
) -> Result<glow::Program> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow::anyhow!(e))?;
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            bail!("shader link: {log}");
        }
        Ok(program)
    }
}

impl GlesRenderer {
    /// Creates both shader programs, the vertex buffer and the textures.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn new(gl: glow::Context) -> Result<Self> {
        // The fullscreen triangle, in the [0, 2] square the vertex shader maps
        // to clip space.
        let vertices: Vec<u8> = [0.0f32, 0.0, 2.0, 0.0, 0.0, 2.0]
            .iter()
            .flat_map(|coordinate| coordinate.to_ne_bytes())
            .collect();
        // SAFETY: the caller keeps the context current.
        unsafe {
            let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;
            let rgba_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, RGBA_FRAG_SRC)?;
            let rgba_program = link_program(&gl, vs, rgba_fs)?;
            gl.delete_shader(rgba_fs);
            let rgba_a_pos = gl
                .get_attrib_location(rgba_program, "a_pos")
                .context("a_pos not found in RGBA program")?;

            let i420_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, I420_FRAG_SRC)?;
            let i420_program = link_program(&gl, vs, i420_fs)?;
            gl.delete_shader(i420_fs);
            gl.delete_shader(vs);
            let i420_a_pos = gl
                .get_attrib_location(i420_program, "a_pos")
                .context("a_pos not found in I420 program")?;

            // The Y, U and V samplers read texture units 0, 1 and 2.
            gl.use_program(Some(i420_program));
            for (name, unit) in [("u_y_tex", 0), ("u_u_tex", 1), ("u_v_tex", 2)] {
                if let Some(location) = gl.get_uniform_location(i420_program, name) {
                    gl.uniform_1_i32(Some(&location), unit);
                }
            }

            let vbo = gl.create_buffer().map_err(|e| anyhow::anyhow!(e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertices, glow::STATIC_DRAW);

            // The I420 planes are packed rows. GL's default alignment of 4 would
            // read past a row whose length is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            Ok(Self {
                rgba: PlaneTexture::new(&gl)?,
                y: PlaneTexture::new(&gl)?,
                u: PlaneTexture::new(&gl)?,
                v: PlaneTexture::new(&gl)?,
                gl,
                vbo,
                rgba_program,
                rgba_a_pos,
                i420_program,
                i420_a_pos,
                active: ActiveMode::Rgba,
            })
        }
    }

    /// Uploads RGBA pixel data of `w` x `h`.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) {
        self.active = ActiveMode::Rgba;
        let pixels = &rgba[..(w * h * 4) as usize];
        // SAFETY: the caller keeps the context current.
        unsafe { self.rgba.upload(&self.gl, glow::RGBA, pixels, w, h) };
    }

    /// Uploads the three I420 planes of a `w` x `h` picture for the shader to convert.
    ///
    /// The planes must be tightly packed. GLES2 has no `GL_UNPACK_ROW_LENGTH`,
    /// so row padding has to be removed on the CPU first.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn upload_i420(&mut self, y: &[u8], u: &[u8], v: &[u8], w: u32, h: u32) {
        self.active = ActiveMode::I420;
        let (chroma_w, chroma_h) = (w.div_ceil(2), h.div_ceil(2));
        let chroma = (chroma_w * chroma_h) as usize;
        let gl = &self.gl;
        // SAFETY: the caller keeps the context current.
        unsafe {
            self.y
                .upload(gl, glow::LUMINANCE, &y[..(w * h) as usize], w, h);
            self.u
                .upload(gl, glow::LUMINANCE, &u[..chroma], chroma_w, chroma_h);
            self.v
                .upload(gl, glow::LUMINANCE, &v[..chroma], chroma_w, chroma_h);
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
            // SAFETY: the caller keeps the context current.
            Surface::I420(i420) => unsafe {
                self.upload_i420(i420.y(), i420.u(), i420.v(), i420.width(), i420.height());
            },
            other => match other.to_rgba(&Default::default()) {
                // SAFETY: the caller keeps the context current.
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
        let gl = &self.gl;
        let a_pos = match self.active {
            ActiveMode::Rgba => self.rgba_a_pos,
            ActiveMode::I420 => self.i420_a_pos,
        };
        // SAFETY: the caller keeps the context current, and every object bound
        // here was created on it in `new`.
        unsafe {
            gl.viewport(0, 0, vp_w, vp_h);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            match self.active {
                ActiveMode::Rgba => {
                    gl.use_program(Some(self.rgba_program));
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.rgba.texture));
                }
                ActiveMode::I420 => {
                    gl.use_program(Some(self.i420_program));
                    for (unit, plane) in [
                        (glow::TEXTURE0, &self.y),
                        (glow::TEXTURE1, &self.u),
                        (glow::TEXTURE2, &self.v),
                    ] {
                        gl.active_texture(unit);
                        gl.bind_texture(glow::TEXTURE_2D, Some(plane.texture));
                    }
                }
            }
            gl.vertex_attrib_pointer_f32(a_pos, 2, glow::FLOAT, false, 0, 0);
            gl.enable_vertex_attrib_array(a_pos);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.disable_vertex_attrib_array(a_pos);
        }
    }
}

impl Drop for GlesRenderer {
    fn drop(&mut self) {
        // SAFETY: the type's contract keeps the context current on the thread
        // that drops it, and every object was created on that context.
        unsafe {
            for plane in [&self.rgba, &self.y, &self.u, &self.v] {
                self.gl.delete_texture(plane.texture);
            }
            self.gl.delete_program(self.rgba_program);
            self.gl.delete_program(self.i420_program);
            self.gl.delete_buffer(self.vbo);
        }
    }
}
