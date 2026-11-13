//! A GLES2 video renderer over `glow`.
//!
//! The Pi Zero has no Vulkan or wgpu, so `iroh_live::media::video::render`
//! cannot draw here. I420 frames from openh264 go up as three `LUMINANCE`
//! textures, and a shader converts the colour. This keeps the CPU free for
//! decoding. Other surfaces go up as one RGBA texture. The renderer works with
//! any EGL/GLES2 context.

use gles_yuv::Program;
use glow::HasContext;
use iroh_live::media::video::{Frame, Surface};
use n0_error::Result;
use tracing::warn;

const RGBA_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
}";

/// The reads of an I420 fragment shader, which [`gles_yuv::YUV_TO_RGB`] completes.
///
/// Each plane is a `LUMINANCE` texture, so every read uses `.r`. Bilinear
/// filtering upsamples the half-size chroma planes.
const I420_FRAG_HEAD: &str = "\
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
";

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
        Ok(Self {
            // SAFETY: the caller keeps the context current.
            texture: unsafe { gles_yuv::texture(gl, glow::TEXTURE_2D)? },
            size: (0, 0),
        })
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
    rgba_program: Program,
    rgba: PlaneTexture,
    i420_program: Program,
    y: PlaneTexture,
    u: PlaneTexture,
    v: PlaneTexture,
    active: ActiveMode,
}

impl GlesRenderer {
    /// Creates both shader programs, the vertex buffer and the textures.
    ///
    /// # Safety
    ///
    /// The GL context must be current on the calling thread.
    pub(crate) unsafe fn new(gl: glow::Context) -> Result<Self> {
        let i420 = [I420_FRAG_HEAD, gles_yuv::YUV_TO_RGB].concat();
        // SAFETY: the caller keeps the context current.
        unsafe {
            let [rgba_program, i420_program] =
                gles_yuv::programs(&gl, [RGBA_FRAG_SRC, i420.as_str()])?;
            // The Y, U and V samplers read texture units 0, 1 and 2.
            gles_yuv::bind_samplers(
                &gl,
                i420_program.program,
                &[("u_y_tex", 0), ("u_u_tex", 1), ("u_v_tex", 2)],
            );
            let vbo = gles_yuv::triangle(&gl)?;

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
                i420_program,
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
        // SAFETY: the caller keeps the context current, and every object bound
        // here was created on it in `new`.
        unsafe {
            gl.viewport(0, 0, vp_w, vp_h);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let program = match self.active {
                ActiveMode::Rgba => {
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.rgba.texture));
                    self.rgba_program
                }
                ActiveMode::I420 => {
                    for (unit, plane) in [
                        (glow::TEXTURE0, &self.y),
                        (glow::TEXTURE1, &self.u),
                        (glow::TEXTURE2, &self.v),
                    ] {
                        gl.active_texture(unit);
                        gl.bind_texture(glow::TEXTURE_2D, Some(plane.texture));
                    }
                    self.i420_program
                }
            };
            gl.use_program(Some(program.program));
            gles_yuv::draw(gl, self.vbo, program.a_pos);
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
            self.gl.delete_program(self.rgba_program.program);
            self.gl.delete_program(self.i420_program.program);
            self.gl.delete_buffer(self.vbo);
        }
    }
}
