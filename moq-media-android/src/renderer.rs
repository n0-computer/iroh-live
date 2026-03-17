//! Android GLES2 renderer with EGL context management.
//!
//! Owns the full EGL lifecycle (display, context, surface) and renders
//! `AHardwareBuffer` frames via `GL_TEXTURE_EXTERNAL_OES` or NV12 planes
//! via `sampler2D`. Kotlin only provides the `Surface` handle — all GL
//! and EGL calls happen in Rust.

use std::ffi::c_void;

use anyhow::{Context as _, Result, bail};
use glow::HasContext;
use khronos_egl as egl_api;

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

/// NV12→RGBA fragment shader using `sampler2D` (Y as LUMINANCE, UV as LUMINANCE_ALPHA).
///
/// Used for CPU NV12 frames (direct camera passthrough) — avoids the expensive
/// CPU NV12→RGBA conversion + AHardwareBuffer allocation.
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

/// Renders video frames on Android with full EGL lifecycle ownership.
///
/// Manages the EGL display, context, and surface internally. The caller
/// provides a native window (from `ANativeWindow_fromSurface`) at
/// construction time. All GL and EGL calls happen through this struct.
pub struct AndroidRenderer {
    gl: glow::Context,
    // EGL state.
    egl: egl_api::DynamicInstance<egl_api::EGL1_4>,
    egl_display: egl_api::Display,
    egl_config: egl_api::Config,
    egl_context: egl_api::Context,
    egl_surface: egl_api::Surface,
    // OES path (HardwareBuffer frames).
    oes_program: glow::Program,
    oes_texture: glow::Texture,
    oes_a_pos_loc: u32,
    oes_rotation_loc: Option<glow::UniformLocation>,
    // NV12 path (CPU camera frames — avoids RGBA conversion).
    nv12_program: glow::Program,
    nv12_y_texture: glow::Texture,
    nv12_uv_texture: glow::Texture,
    nv12_a_pos_loc: u32,
    nv12_rotation_loc: Option<glow::UniformLocation>,
    // Shared.
    vbo: glow::Buffer,
}

// SAFETY: EGL/GL resources are only used from the GL thread, guaranteed by
// the caller contract. The EGL instance (libloading::Library) is Send.
unsafe impl Send for AndroidRenderer {}

impl std::fmt::Debug for AndroidRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidRenderer").finish()
    }
}

impl AndroidRenderer {
    /// Creates a renderer with a full EGL context bound to the given native window.
    ///
    /// `native_window` is an `ANativeWindow*` obtained from
    /// `ANativeWindow_fromSurface`. This function initialises EGL (display,
    /// config, context, surface), makes it current, compiles shaders, and
    /// allocates GL resources.
    ///
    /// # Safety
    /// `native_window` must be a valid `ANativeWindow*`.
    pub unsafe fn new(native_window: *mut c_void) -> Result<Self> {
        // ── EGL setup ───────────────────────────────────────────────
        let egl = unsafe {
            egl_api::DynamicInstance::<egl_api::EGL1_4>::load_required()
                .map_err(|e| anyhow::anyhow!("load EGL: {e}"))?
        };

        let egl_display =
            unsafe { egl.get_display(egl_api::DEFAULT_DISPLAY) }.context("eglGetDisplay failed")?;
        egl.initialize(egl_display)
            .map_err(|e| anyhow::anyhow!("eglInitialize: {e}"))?;

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
            .map_err(|e| anyhow::anyhow!("eglChooseConfig: {e}"))?
            .context("no matching EGL config")?;

        egl.bind_api(egl_api::OPENGL_ES_API)
            .map_err(|e| anyhow::anyhow!("eglBindAPI: {e}"))?;

        let egl_context = egl
            .create_context(
                egl_display,
                config,
                None,
                &[egl_api::CONTEXT_CLIENT_VERSION, 2, egl_api::NONE],
            )
            .map_err(|e| anyhow::anyhow!("eglCreateContext: {e}"))?;

        let egl_surface = unsafe {
            egl.create_window_surface(
                egl_display,
                config,
                native_window as egl_api::NativeWindowType,
                None,
            )
        }
        .map_err(|e| anyhow::anyhow!("eglCreateWindowSurface: {e}"))?;

        egl.make_current(
            egl_display,
            Some(egl_surface),
            Some(egl_surface),
            Some(egl_context),
        )
        .map_err(|e| anyhow::anyhow!("eglMakeCurrent: {e}"))?;

        // ── GL setup ────────────────────────────────────────────────
        let gl = unsafe { egl::create_glow_context() };

        // Shared vertex shader.
        let vs = compile_shader(&gl, glow::VERTEX_SHADER, VERT_SRC)?;

        // OES program (HardwareBuffer frames).
        let oes_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, OES_FRAG_SRC)?;
        let oes_program = link_program(&gl, vs, oes_fs)?;
        unsafe { gl.delete_shader(oes_fs) };
        let oes_a_pos_loc = unsafe { gl.get_attrib_location(oes_program, "a_pos") }
            .context("a_pos not found in OES program")?;
        let oes_rotation_loc = unsafe { gl.get_uniform_location(oes_program, "u_rotation") };

        // NV12 program (CPU camera frames).
        let nv12_fs = compile_shader(&gl, glow::FRAGMENT_SHADER, NV12_FRAG_SRC)?;
        let nv12_program = link_program(&gl, vs, nv12_fs)?;
        unsafe {
            gl.delete_shader(nv12_fs);
            gl.delete_shader(vs);
        }
        let nv12_a_pos_loc = unsafe { gl.get_attrib_location(nv12_program, "a_pos") }
            .context("a_pos not found in NV12 program")?;
        let nv12_rotation_loc = unsafe { gl.get_uniform_location(nv12_program, "u_rotation") };
        // Bind NV12 sampler uniforms.
        unsafe { gl.use_program(Some(nv12_program)) };
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_y_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 0) };
        }
        if let Some(loc) = unsafe { gl.get_uniform_location(nv12_program, "u_uv_tex") } {
            unsafe { gl.uniform_1_i32(Some(&loc), 1) };
        }

        // OES texture.
        let oes_texture = create_tex(&gl, GL_TEXTURE_EXTERNAL_OES)?;

        // NV12 plane textures (TEXTURE_2D).
        let nv12_y_texture = create_tex(&gl, glow::TEXTURE_2D)?;
        let nv12_uv_texture = create_tex(&gl, glow::TEXTURE_2D)?;

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
            egl,
            egl_display,
            egl_config: config,
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
                self.egl_display.as_ptr() as *mut c_void,
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
                .bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.oes_texture));
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
            self.gl.use_program(Some(self.oes_program));
            if let Some(ref loc) = self.oes_rotation_loc {
                self.gl.uniform_1_i32(Some(loc), rotation_degrees as i32);
            }
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .vertex_attrib_pointer_f32(self.oes_a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(self.oes_a_pos_loc);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            self.gl.disable_vertex_attrib_array(self.oes_a_pos_loc);
        }

        // Cleanup.
        unsafe { egl::destroy_image(self.egl_display.as_ptr() as *mut c_void, egl_image) };
    }

    /// Renders NV12 planes directly to the viewport via GPU shader conversion.
    ///
    /// Uploads Y plane as `LUMINANCE` and UV plane as `LUMINANCE_ALPHA`,
    /// converts to RGBA in the fragment shader. No CPU color conversion or
    /// AHardwareBuffer allocation needed.
    ///
    /// # Safety
    /// The EGL context must be current on the calling thread.
    #[allow(clippy::too_many_arguments)]
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
        let uv_w = width / 2;

        // GLES2 has no GL_UNPACK_ROW_LENGTH, so when stride > width we must
        // strip the row padding before upload to avoid a green stripe.
        let y_stripped;
        let y_upload: &[u8] = if y_stride == width {
            y_data
        } else {
            y_stripped = strip_stride(y_data, width as usize, y_stride as usize, height as usize);
            &y_stripped
        };

        let uv_row_bytes = uv_w * 2; // LUMINANCE_ALPHA = 2 bytes/texel
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

        // Upload Y plane (LUMINANCE, full res, no padding).
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

        // Upload UV plane (LUMINANCE_ALPHA, half res, no padding).
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

        // Swap dimensions for 90°/270° rotation.
        let (disp_w, disp_h) = if rotation_degrees == 90 || rotation_degrees == 270 {
            (height, width)
        } else {
            (width, height)
        };

        // Clear + letterbox + draw.
        unsafe {
            self.gl.viewport(0, 0, surface_w, surface_h);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        let (vp_x, vp_y, vp_w, vp_h) = letterbox_viewport(surface_w, surface_h, disp_w, disp_h);
        unsafe {
            self.gl.viewport(vp_x, vp_y, vp_w, vp_h);
            self.gl.use_program(Some(self.nv12_program));
            if let Some(ref loc) = self.nv12_rotation_loc {
                self.gl.uniform_1_i32(Some(loc), rotation_degrees as i32);
            }
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .vertex_attrib_pointer_f32(self.nv12_a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(self.nv12_a_pos_loc);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            self.gl.disable_vertex_attrib_array(self.nv12_a_pos_loc);
        }
    }

    /// Swaps the EGL front and back buffers (presents the rendered frame).
    pub fn swap_buffers(&self) {
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .ok();
    }

    /// Ensures the EGL context is current on the calling thread.
    ///
    /// Coroutine dispatchers may resume on a different thread; this re-binds
    /// the context if needed. A no-op if already current.
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

    /// Tears down the EGL surface and context. The display stays alive.
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

/// Creates a texture with LINEAR filtering and CLAMP_TO_EDGE wrapping.
fn create_tex(gl: &glow::Context, target: u32) -> Result<glow::Texture> {
    let texture = unsafe { gl.create_texture() }.map_err(|e| anyhow::anyhow!(e))?;
    unsafe {
        gl.bind_texture(target, Some(texture));
        gl.tex_parameter_i32(target, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    }
    Ok(texture)
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

/// Strips row padding from strided pixel data.
///
/// GLES2 lacks `GL_UNPACK_ROW_LENGTH`, so we copy rows into a contiguous
/// buffer when stride > row width.
fn strip_stride(data: &[u8], row_bytes: usize, stride: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(row_bytes * rows);
    for y in 0..rows {
        let start = y * stride;
        let end = (start + row_bytes).min(data.len());
        if start < data.len() {
            out.extend_from_slice(&data[start..end]);
            // Pad short last row with zeros.
            if end - start < row_bytes {
                out.resize(out.len() + row_bytes - (end - start), 0);
            }
        }
    }
    out
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
