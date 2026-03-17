//! GLES2 video frame renderer using `glow`.
//!
//! Handles two upload paths:
//! - **RGBA**: single `GL_TEXTURE_2D` upload (packed frames, GPU fallback).
//! - **NV12**: Y plane as `LUMINANCE` + UV plane as `LUMINANCE_ALPHA`, converted
//!   to RGBA in a fragment shader. Avoids the CPU NV12→RGBA conversion that
//!   dominates render time on low-power hardware.
//!
//! Works with any EGL/GLES2 context (Linux DRM/KMS, Android, glutin+winit).
//! Requires the `gles` feature.

use anyhow::{Context as _, Result, bail};
use glow::HasContext;

use crate::format::{FrameData, VideoFrame};

// ── GLES2 shaders ──────────────────────────────────────────────────

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

/// BT.601 limited-range NV12→RGBA conversion.
///
/// Y plane sampled from `LUMINANCE` texture (value in `.r`).
/// UV plane sampled from `LUMINANCE_ALPHA` texture (U in `.r`, V in `.a`).
const NV12_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y_tex;
uniform sampler2D u_uv_tex;
void main() {
    float y_raw = texture2D(u_y_tex, v_uv).r;
    float u_raw = texture2D(u_uv_tex, v_uv).r;
    float v_raw = texture2D(u_uv_tex, v_uv).a;
    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float u = (u_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float v = (v_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float r = y + 1.402 * v;
    float g = y - 0.344136 * u - 0.714136 * v;
    float b = y + 1.772 * u;
    gl_FragColor = vec4(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}";

// ── GlesRenderer ───────────────────────────────────────────────────

/// Tracks which upload path was last used, so `draw` picks the right program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveMode {
    Rgba,
    Nv12,
}

/// GLES2 renderer with RGBA and NV12 upload paths.
///
/// Platform-agnostic — works with any `glow::Context`. The caller is
/// responsible for creating the GL context and swapping buffers.
#[derive(Debug)]
pub struct GlesRenderer {
    gl: glow::Context,
    // Shared vertex state.
    vbo: glow::Buffer,
    a_pos_loc: u32,
    // RGBA path.
    rgba_program: glow::Program,
    rgba_texture: glow::Texture,
    // NV12 path.
    nv12_program: glow::Program,
    nv12_a_pos_loc: u32,
    y_texture: glow::Texture,
    uv_texture: glow::Texture,
    // State tracking.
    active: ActiveMode,
    tex_width: u32,
    tex_height: u32,
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
    /// The GL context must be current on the calling thread.
    pub unsafe fn new(gl: glow::Context) -> Result<Self> {
        let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;

        // RGBA program.
        let rgba_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, RGBA_FRAG_SRC)?;
        let rgba_program = link_program(&gl, vs, rgba_fs)?;
        unsafe { gl.delete_shader(rgba_fs) };
        let a_pos_loc = unsafe { gl.get_attrib_location(rgba_program, "a_pos") }
            .context("a_pos not found in RGBA program")?;

        // NV12 program.
        let nv12_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, NV12_FRAG_SRC)?;
        let nv12_program = link_program(&gl, vs, nv12_fs)?;
        unsafe { gl.delete_shader(nv12_fs) };
        unsafe { gl.delete_shader(vs) };
        let nv12_a_pos_loc = unsafe { gl.get_attrib_location(nv12_program, "a_pos") }
            .context("a_pos not found in NV12 program")?;

        // Bind NV12 sampler uniforms (texture units 0 and 1).
        unsafe { gl.use_program(Some(nv12_program)) };
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_y_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 0) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_uv_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 1) };
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

        // Textures.
        let rgba_texture = create_texture(&gl)?;
        let y_texture = create_texture(&gl)?;
        let uv_texture = create_texture(&gl)?;

        Ok(Self {
            gl,
            vbo,
            a_pos_loc,
            rgba_program,
            rgba_texture,
            nv12_program,
            nv12_a_pos_loc,
            y_texture,
            uv_texture,
            active: ActiveMode::Rgba,
            tex_width: 0,
            tex_height: 0,
        })
    }

    /// Uploads RGBA pixel data to the texture.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) {
        self.active = ActiveMode::Rgba;
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

    /// Uploads NV12 planes directly — no CPU color conversion.
    ///
    /// Y plane is uploaded as `LUMINANCE` (1 byte/pixel), UV plane as
    /// `LUMINANCE_ALPHA` (2 bytes/pixel, interleaved U+V).
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_nv12(
        &mut self,
        y_data: &[u8],
        y_stride: u32,
        uv_data: &[u8],
        uv_stride: u32,
        w: u32,
        h: u32,
    ) {
        self.active = ActiveMode::Nv12;
        let uv_h = h.div_ceil(2);
        let uv_w = w / 2;

        // Y plane: LUMINANCE, full resolution.
        // If stride matches width we can upload directly; otherwise we need to
        // strip padding. GLES2 has no GL_UNPACK_ROW_LENGTH.
        if y_stride == w {
            upload_tex(
                &self.gl,
                self.y_texture,
                glow::LUMINANCE,
                &y_data[..(w * h) as usize],
                w,
                h,
                &mut self.tex_width,
                &mut self.tex_height,
            );
        } else {
            let stripped = strip_stride(y_data, w as usize, y_stride as usize, h as usize);
            upload_tex(
                &self.gl,
                self.y_texture,
                glow::LUMINANCE,
                &stripped,
                w,
                h,
                &mut self.tex_width,
                &mut self.tex_height,
            );
        }

        // UV plane: LUMINANCE_ALPHA, half resolution.
        // Each texel = 2 bytes (U, V), so row = uv_w * 2.
        let uv_row = uv_w * 2;
        if uv_stride == uv_row {
            upload_tex_uncached(
                &self.gl,
                self.uv_texture,
                glow::LUMINANCE_ALPHA,
                &uv_data[..(uv_row * uv_h) as usize],
                uv_w,
                uv_h,
            );
        } else {
            let stripped =
                strip_stride(uv_data, uv_row as usize, uv_stride as usize, uv_h as usize);
            upload_tex_uncached(
                &self.gl,
                self.uv_texture,
                glow::LUMINANCE_ALPHA,
                &stripped,
                uv_w,
                uv_h,
            );
        }
    }

    /// Uploads a [`VideoFrame`], choosing the NV12 fast path when available.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_frame(&mut self, frame: &VideoFrame) {
        match &frame.data {
            FrameData::Nv12(planes) => {
                // SAFETY: caller guarantees GL context is current.
                unsafe {
                    self.upload_nv12(
                        &planes.y_data,
                        planes.y_stride,
                        &planes.uv_data,
                        planes.uv_stride,
                        planes.width,
                        planes.height,
                    )
                };
            }
            _ => {
                let rgba = frame.rgba_image();
                // SAFETY: caller guarantees GL context is current.
                unsafe { self.upload_rgba(rgba.as_raw(), rgba.width(), rgba.height()) };
            }
        }
    }

    /// Draws the uploaded frame as a fullscreen triangle.
    ///
    /// Clears to black and renders using whichever program matches the last
    /// upload. The caller must swap buffers after this call.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn draw(&self, vp_w: i32, vp_h: i32) {
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
                ActiveMode::Nv12 => {
                    self.gl.use_program(Some(self.nv12_program));
                    self.gl.vertex_attrib_pointer_f32(
                        self.nv12_a_pos_loc,
                        2,
                        glow::FLOAT,
                        false,
                        0,
                        0,
                    );
                    self.gl.enable_vertex_attrib_array(self.nv12_a_pos_loc);
                    self.gl.active_texture(glow::TEXTURE0);
                    self.gl.bind_texture(glow::TEXTURE_2D, Some(self.y_texture));
                    self.gl.active_texture(glow::TEXTURE1);
                    self.gl
                        .bind_texture(glow::TEXTURE_2D, Some(self.uv_texture));
                }
            }

            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);

            match self.active {
                ActiveMode::Rgba => self.gl.disable_vertex_attrib_array(self.a_pos_loc),
                ActiveMode::Nv12 => self.gl.disable_vertex_attrib_array(self.nv12_a_pos_loc),
            }
        }
    }
}

// ── Texture upload helpers ──────────────────────────────────────────

/// Uploads pixel data, reusing `tex_sub_image_2d` when dimensions match.
///
/// `cached_w` / `cached_h` track the last allocated size for this texture.
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

/// Uploads pixel data without dimension caching (used for UV plane which
/// has different dimensions than Y).
unsafe fn upload_tex_uncached(
    gl: &glow::Context,
    texture: glow::Texture,
    format: u32,
    data: &[u8],
    w: u32,
    h: u32,
) {
    unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(texture)) };
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
}

/// Strips row padding from strided pixel data.
///
/// GLES2 lacks `GL_UNPACK_ROW_LENGTH`, so we copy rows into a contiguous
/// buffer when stride > row width.
fn strip_stride(data: &[u8], row_bytes: usize, stride: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(row_bytes * rows);
    for y in 0..rows {
        let start = y * stride;
        let end = start + row_bytes;
        if end <= data.len() {
            out.extend_from_slice(&data[start..end]);
        }
    }
    out
}
