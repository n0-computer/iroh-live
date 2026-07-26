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

#[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
use crate::format::NativeFrameHandle;
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

/// BT.601 limited-range NV12→RGBA conversion for RG8 UV texture.
///
/// Same color math as [`NV12_FRAG_SRC`], but reads UV from `.r`/`.g` instead
/// of `.r`/`.a`. Used when the UV plane comes from DMA-BUF import as
/// `DRM_FORMAT_GR88`, which EGL maps to `GL_RG8` (not `LUMINANCE_ALPHA`).
const NV12_RG_FRAG_SRC: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y_tex;
uniform sampler2D u_uv_tex;
void main() {
    float y_raw = texture2D(u_y_tex, v_uv).r;
    float u_raw = texture2D(u_uv_tex, v_uv).r;
    float v_raw = texture2D(u_uv_tex, v_uv).g;
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
    /// CPU upload NV12 — UV in `LUMINANCE_ALPHA` (U in `.r`, V in `.a`).
    Nv12,
    /// DMA-BUF import NV12 — UV in `RG8` (U in `.r`, V in `.g`).
    Nv12Rg,
}

/// Describes which render path was used for the last frame upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, derive_more::Display)]
pub enum GlesRenderPath {
    /// No frame uploaded yet.
    #[default]
    #[display("none")]
    None,
    /// CPU RGBA packed upload.
    #[display("cpu-rgba")]
    CpuRgba,
    /// CPU NV12 plane upload + GPU shader conversion.
    #[display("gpu-nv12")]
    CpuNv12,
    /// Zero-copy DMA-BUF EGL import (Linux).
    #[display("dmabuf-zerocopy")]
    DmaBuf,
}

/// GLES2 renderer with RGBA, NV12, and zero-copy DMA-BUF upload paths.
///
/// Platform-agnostic — works with any `glow::Context`. The caller is
/// responsible for creating the GL context and swapping buffers.
///
/// When the `gles-dmabuf` feature is enabled and the EGL display supports
/// `EGL_EXT_image_dma_buf_import`, GPU frames with DMA-BUF handles bypass
/// CPU readback entirely.
#[derive(Debug)]
pub struct GlesRenderer {
    gl: glow::Context,
    // Shared vertex state.
    vbo: glow::Buffer,
    a_pos_loc: u32,
    // RGBA path.
    rgba_program: glow::Program,
    rgba_texture: glow::Texture,
    // NV12 path (LUMINANCE_ALPHA UV — CPU upload).
    nv12_program: glow::Program,
    nv12_a_pos_loc: u32,
    // NV12 path (RG8 UV — DMA-BUF import).
    nv12_rg_program: glow::Program,
    nv12_rg_a_pos_loc: u32,
    y_texture: glow::Texture,
    uv_texture: glow::Texture,
    // State tracking.
    active: ActiveMode,
    tex_width: u32,
    tex_height: u32,
    uv_tex_width: u32,
    uv_tex_height: u32,
    /// Which render path was used for the last frame.
    last_render_path: GlesRenderPath,
    // DMA-BUF zero-copy import (Linux only).
    #[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
    dmabuf_importer: Option<super::gles_dmabuf::GlesDmaBufImporter>,
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

        // NV12 program (LUMINANCE_ALPHA UV — CPU upload).
        let nv12_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, NV12_FRAG_SRC)?;
        let nv12_program = link_program(&gl, vs, nv12_fs)?;
        unsafe { gl.delete_shader(nv12_fs) };
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

        // NV12 RG program (RG8 UV — DMA-BUF import).
        let nv12_rg_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, NV12_RG_FRAG_SRC)?;
        let nv12_rg_program = link_program(&gl, vs, nv12_rg_fs)?;
        unsafe { gl.delete_shader(nv12_rg_fs) };
        unsafe { gl.delete_shader(vs) };
        let nv12_rg_a_pos_loc = unsafe { gl.get_attrib_location(nv12_rg_program, "a_pos") }
            .context("a_pos not found in NV12 RG program")?;

        unsafe { gl.use_program(Some(nv12_rg_program)) };
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_rg_program, "u_y_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 0) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_rg_program, "u_uv_tex") } {
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

        #[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
        let dmabuf_importer = match unsafe { super::gles_dmabuf::GlesDmaBufImporter::new() } {
            Ok(imp) => imp,
            Err(e) => {
                tracing::debug!("EGL DMA-BUF import unavailable: {e}");
                None
            }
        };

        Ok(Self {
            gl,
            vbo,
            a_pos_loc,
            rgba_program,
            rgba_texture,
            nv12_program,
            nv12_a_pos_loc,
            nv12_rg_program,
            nv12_rg_a_pos_loc,
            y_texture,
            uv_texture,
            active: ActiveMode::Rgba,
            tex_width: 0,
            tex_height: 0,
            uv_tex_width: 0,
            uv_tex_height: 0,
            last_render_path: GlesRenderPath::None,
            #[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
            dmabuf_importer,
        })
    }

    /// Uploads RGBA pixel data to the texture.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) {
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
            unsafe {
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
            }
        } else {
            let stripped = strip_stride(y_data, w as usize, y_stride as usize, h as usize);
            unsafe {
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
        }

        // UV plane: LUMINANCE_ALPHA, half resolution.
        // Each texel = 2 bytes (U, V), so row = uv_w * 2.
        let uv_row = uv_w * 2;
        if uv_stride == uv_row {
            unsafe {
                upload_tex(
                    &self.gl,
                    self.uv_texture,
                    glow::LUMINANCE_ALPHA,
                    &uv_data[..(uv_row * uv_h) as usize],
                    uv_w,
                    uv_h,
                    &mut self.uv_tex_width,
                    &mut self.uv_tex_height,
                );
            }
        } else {
            let stripped =
                strip_stride(uv_data, uv_row as usize, uv_stride as usize, uv_h as usize);
            unsafe {
                upload_tex(
                    &self.gl,
                    self.uv_texture,
                    glow::LUMINANCE_ALPHA,
                    &stripped,
                    uv_w,
                    uv_h,
                    &mut self.uv_tex_width,
                    &mut self.uv_tex_height,
                );
            }
        }
    }

    /// Returns which render path was used for the last uploaded frame.
    pub fn last_render_path(&self) -> GlesRenderPath {
        self.last_render_path
    }

    /// Uploads a [`VideoFrame`], choosing the best available path.
    ///
    /// Priority: DMA-BUF zero-copy (if `gles-dmabuf` feature and GPU frame)
    /// → NV12 direct upload → RGBA fallback.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn upload_frame(&mut self, frame: &VideoFrame) {
        // Try zero-copy DMA-BUF import for GPU frames.
        #[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
        if let FrameData::Gpu(gpu) = &frame.data
            && let Some(ref mut importer) = self.dmabuf_importer
            && !importer.is_disabled()
            && let Some(NativeFrameHandle::DmaBuf(ref info)) = gpu.native_handle()
        {
            match unsafe { importer.import_nv12(&self.gl, info, self.y_texture, self.uv_texture) } {
                Ok((w, h)) => {
                    importer.record_success();
                    self.active = ActiveMode::Nv12Rg;
                    self.tex_width = w;
                    self.tex_height = h;
                    self.set_render_path(GlesRenderPath::DmaBuf);
                    return;
                }
                Err(e) => {
                    if importer.record_failure(&e) {
                        self.dmabuf_importer = None;
                    }
                }
            }
        }

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
                self.set_render_path(GlesRenderPath::CpuNv12);
            }
            _ => {
                let rgba = frame.rgba_image();
                // SAFETY: caller guarantees GL context is current.
                unsafe { self.upload_rgba(rgba.as_raw(), rgba.width(), rgba.height()) };
                self.set_render_path(GlesRenderPath::CpuRgba);
            }
        }
    }

    /// Updates the render path and logs on first change.
    fn set_render_path(&mut self, path: GlesRenderPath) {
        if self.last_render_path != path {
            tracing::info!(path = %path, prev = %self.last_render_path, "GLES render path changed");
            self.last_render_path = path;
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
                ActiveMode::Nv12 | ActiveMode::Nv12Rg => {
                    let (program, a_pos) = if self.active == ActiveMode::Nv12Rg {
                        (self.nv12_rg_program, self.nv12_rg_a_pos_loc)
                    } else {
                        (self.nv12_program, self.nv12_a_pos_loc)
                    };
                    self.gl.use_program(Some(program));
                    self.gl
                        .vertex_attrib_pointer_f32(a_pos, 2, glow::FLOAT, false, 0, 0);
                    self.gl.enable_vertex_attrib_array(a_pos);
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
                ActiveMode::Nv12Rg => self.gl.disable_vertex_attrib_array(self.nv12_rg_a_pos_loc),
            }
        }
    }

    /// Uploads a frame and draws it in one call.
    ///
    /// Combines [`upload_frame`](Self::upload_frame) and [`draw`](Self::draw).
    /// The caller must swap buffers after this call.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    pub unsafe fn render_frame(&mut self, frame: &VideoFrame, vp_w: i32, vp_h: i32) {
        unsafe {
            self.upload_frame(frame);
            self.draw(vp_w, vp_h);
        }
    }

    /// Returns a reference to the underlying `glow::Context`.
    pub fn gl(&self) -> &glow::Context {
        &self.gl
    }

    /// Returns the dimensions of the last uploaded texture.
    pub fn texture_dimensions(&self) -> (u32, u32) {
        (self.tex_width, self.tex_height)
    }
}

impl Drop for GlesRenderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.rgba_texture);
            self.gl.delete_texture(self.y_texture);
            self.gl.delete_texture(self.uv_texture);
            self.gl.delete_program(self.rgba_program);
            self.gl.delete_program(self.nv12_program);
            self.gl.delete_program(self.nv12_rg_program);
            self.gl.delete_buffer(self.vbo);
        }
    }
}

// ── Texture upload helpers ──────────────────────────────────────────

/// Uploads pixel data, reusing `tex_sub_image_2d` when dimensions match.
///
/// `cached_w` / `cached_h` track the last allocated size for this texture.
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
