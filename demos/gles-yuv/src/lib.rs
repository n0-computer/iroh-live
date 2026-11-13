//! GLES2 helpers that the Pi Zero and Android players share.
//!
//! Both players draw a picture as one fullscreen triangle, and convert BT.601
//! limited-range YUV to RGB in a fragment shader. Every function here makes GL
//! calls, so the GL context behind `gl` must be current on the calling thread.

use glow::HasContext;
use n0_error::{Result, StdResultExt as _, anyerr, bail_any};

/// The vertex shader of every program: a fullscreen triangle with flipped texture rows.
pub const VERTEX_SHADER: &str = "\
#version 100
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(a_pos.x, 1.0 - a_pos.y);
}";

/// The end of a fragment shader that converts `y_raw`, `u_raw` and `v_raw` to RGB.
///
/// It reads the three floats as BT.601 limited range, writes `gl_FragColor` and
/// closes `main`. Each player prepends the reads for its plane layout.
pub const YUV_TO_RGB: &str = "    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float u = (u_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float v = (v_raw - 16.0 / 255.0) * (255.0 / 224.0) - 0.5;
    float r = y + 1.402 * v;
    float g = y - 0.344136 * u - 0.714136 * v;
    float b = y + 1.772 * u;
    gl_FragColor = vec4(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}";

/// A linked program over [`VERTEX_SHADER`] and the location of its `a_pos` attribute.
#[derive(Debug, Clone, Copy)]
pub struct Program {
    /// The linked program.
    pub program: glow::Program,
    /// The location of the vertex position attribute.
    pub a_pos: u32,
}

/// Links one program per fragment shader source, each over [`VERTEX_SHADER`].
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
pub unsafe fn programs<const N: usize>(
    gl: &glow::Context,
    fragments: [&str; N],
) -> Result<[Program; N]> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let vs = compile_shader(gl, glow::VERTEX_SHADER, VERTEX_SHADER)?;
        let programs: Result<Vec<Program>> = fragments
            .iter()
            .map(|source| {
                let fs = compile_shader(gl, glow::FRAGMENT_SHADER, source)?;
                let program = link_program(gl, vs, fs);
                gl.delete_shader(fs);
                let program = program?;
                let a_pos = gl
                    .get_attrib_location(program, "a_pos")
                    .std_context("a_pos not found in the program")?;
                Ok(Program { program, a_pos })
            })
            .collect();
        gl.delete_shader(vs);
        Ok(programs?
            .try_into()
            .expect("one program per fragment shader"))
    }
}

/// Points each named sampler uniform of `program` at its texture unit.
///
/// Leaves `program` in use.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
pub unsafe fn bind_samplers(gl: &glow::Context, program: glow::Program, samplers: &[(&str, i32)]) {
    // SAFETY: the caller keeps the context current.
    unsafe {
        gl.use_program(Some(program));
        for &(name, unit) in samplers {
            if let Some(location) = gl.get_uniform_location(program, name) {
                gl.uniform_1_i32(Some(&location), unit);
            }
        }
    }
}

/// Creates a texture for `target` with linear filtering and edge clamping.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
pub unsafe fn texture(gl: &glow::Context, target: u32) -> Result<glow::Texture> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let texture = gl.create_texture().map_err(|e| anyerr!(e))?;
        gl.bind_texture(target, Some(texture));
        for param in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
            gl.tex_parameter_i32(target, param, glow::LINEAR as i32);
        }
        for param in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
            gl.tex_parameter_i32(target, param, glow::CLAMP_TO_EDGE as i32);
        }
        Ok(texture)
    }
}

/// Creates the vertex buffer of the fullscreen triangle that [`draw`] draws.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
pub unsafe fn triangle(gl: &glow::Context) -> Result<glow::Buffer> {
    // The [0, 2] square that the vertex shader maps to clip space.
    let vertices: Vec<u8> = [0.0f32, 0.0, 2.0, 0.0, 0.0, 2.0]
        .iter()
        .flat_map(|coordinate| coordinate.to_ne_bytes())
        .collect();
    // SAFETY: the caller keeps the context current.
    unsafe {
        let vbo = gl.create_buffer().map_err(|e| anyerr!(e))?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertices, glow::STATIC_DRAW);
        Ok(vbo)
    }
}

/// Draws the triangle in `vbo` with the program in use, whose `a_pos` attribute is at `a_pos`.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread, and `vbo`
/// must come from [`triangle`] on that context.
pub unsafe fn draw(gl: &glow::Context, vbo: glow::Buffer, a_pos: u32) {
    // SAFETY: the caller keeps the context current and passes its buffer.
    unsafe {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.vertex_attrib_pointer_f32(a_pos, 2, glow::FLOAT, false, 0, 0);
        gl.enable_vertex_attrib_array(a_pos);
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        gl.disable_vertex_attrib_array(a_pos);
    }
}

/// Compiles a shader of `kind` from `source`.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
unsafe fn compile_shader(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::Shader> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let shader = gl.create_shader(kind).map_err(|e| anyerr!(e))?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            bail_any!("shader compile: {log}");
        }
        Ok(shader)
    }
}

/// Links the vertex shader `vs` and the fragment shader `fs` into a program.
///
/// # Safety
///
/// The GL context behind `gl` must be current on the calling thread.
unsafe fn link_program(
    gl: &glow::Context,
    vs: glow::Shader,
    fs: glow::Shader,
) -> Result<glow::Program> {
    // SAFETY: the caller keeps the context current.
    unsafe {
        let program = gl.create_program().map_err(|e| anyerr!(e))?;
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            bail_any!("shader link: {log}");
        }
        Ok(program)
    }
}
