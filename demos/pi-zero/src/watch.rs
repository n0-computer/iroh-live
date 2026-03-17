//! EGL/GLES2 video viewer — windowed (winit) or direct-to-HDMI (DRM/KMS).
//!
//! Shares a [`GlesRenderer`] between both display backends. The renderer
//! uploads RGBA frames and draws a fullscreen textured triangle.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use glow::HasContext;
use iroh::Watcher;
use iroh_live::media::subscribe::VideoTrack;
use iroh_live::moq::MoqSession;

/// Poll interval between frame checks (≈250 fps ceiling).
const POLL_INTERVAL: Duration = Duration::from_millis(4);

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

// ── GlesRenderer ───────────────────────────────────────────────────

/// GLES2 renderer: uploads RGBA textures and draws a fullscreen triangle.
struct GlesRenderer {
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
    unsafe fn new(gl: glow::Context) -> Result<Self> {
        // SAFETY: caller guarantees the GL context is current.
        let vs = unsafe { gl.create_shader(glow::VERTEX_SHADER) }.map_err(|e| anyhow::anyhow!(e))?;
        unsafe { gl.shader_source(vs, VERT_SRC) };
        unsafe { gl.compile_shader(vs) };
        if !unsafe { gl.get_shader_compile_status(vs) } {
            bail!("vertex shader: {}", unsafe { gl.get_shader_info_log(vs) });
        }

        let fs = unsafe { gl.create_shader(glow::FRAGMENT_SHADER) }.map_err(|e| anyhow::anyhow!(e))?;
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
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32) };
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32) };
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32) };
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32) };

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
    unsafe fn upload(&mut self, rgba: &[u8], w: u32, h: u32) {
        unsafe { self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture)) };
        if w != self.tex_width || h != self.tex_height {
            unsafe {
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D, 0, glow::RGBA as i32,
                    w as i32, h as i32, 0,
                    glow::RGBA, glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rgba)),
                );
            }
            self.tex_width = w;
            self.tex_height = h;
        } else {
            unsafe {
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D, 0, 0, 0,
                    w as i32, h as i32,
                    glow::RGBA, glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rgba)),
                );
            }
        }
    }

    /// Draws the textured fullscreen triangle.
    unsafe fn draw(&self, vp_w: i32, vp_h: i32) {
        unsafe {
            self.gl.viewport(0, 0, vp_w, vp_h);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);

            self.gl.use_program(Some(self.program));
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl.vertex_attrib_pointer_f32(self.a_pos_loc, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(self.a_pos_loc);

            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));

            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);

            self.gl.disable_vertex_attrib_array(self.a_pos_loc);
        }
    }
}

/// Uploads the current frame (if new) and returns whether a new frame was uploaded.
fn try_upload_frame(
    renderer: &mut GlesRenderer,
    track: &mut VideoTrack,
    last_ts: &mut Option<Duration>,
    frame_count: &mut u64,
) -> bool {
    let frame = track.current_frame();
    let is_new = frame
        .as_ref()
        .is_some_and(|f| last_ts.is_none_or(|prev| f.timestamp != prev));

    if is_new {
        if let Some(f) = &frame {
            *last_ts = Some(f.timestamp);
            *frame_count += 1;
            let rgba = f.rgba_image();
            unsafe {
                renderer.upload(rgba.as_raw(), rgba.width(), rgba.height());
            }
        }
    }
    is_new
}

/// Prints FPS and RTT stats every second.
fn print_stats(
    session: &MoqSession,
    track: &VideoTrack,
    frame_count: &mut u64,
    fps_last: &mut Instant,
) {
    let elapsed = fps_last.elapsed();
    if elapsed < Duration::from_secs(1) {
        return;
    }
    let fps = *frame_count as f32 / elapsed.as_secs_f32();
    *frame_count = 0;
    *fps_last = Instant::now();

    let conn = session.conn();
    let path_list = conn.paths().get();
    let rtt = path_list
        .iter()
        .find(|p| p.is_selected())
        .and_then(|p| p.rtt())
        .unwrap_or_default();
    println!(
        "fps: {fps:.0}  rtt: {}ms  decoder: {}",
        rtt.as_millis(),
        track.decoder_name(),
    );
}

// ── DRM/KMS direct-to-HDMI ────────────────────────────────────────

// ── DRM display setup (shared by run_drm + run_fb_demo) ───────────

use drm::control::Device as ControlDevice;
use gbm::AsRaw;
use std::os::fd::AsFd;

struct Card(std::fs::File);
impl AsFd for Card {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl ControlDevice for Card {}

/// DRM/KMS + GBM + EGL display — owns all GPU resources for direct HDMI output.
struct DrmDisplay {
    renderer: GlesRenderer,
    egl: khronos_egl::DynamicInstance<khronos_egl::EGL1_4>,
    egl_display: khronos_egl::Display,
    egl_surface: khronos_egl::Surface,
    gbm_device: gbm::Device<Card>,
    gbm_surface: gbm::Surface<()>,
    connector: drm::control::connector::Handle,
    crtc: drm::control::crtc::Handle,
    front_bo: gbm::BufferObject<()>,
    front_fb: drm::control::framebuffer::Handle,
    width: u32,
    height: u32,
}

impl DrmDisplay {
    fn init() -> Result<Self> {
        let card = {
            let mut found = None;
            for path in ["/dev/dri/card1", "/dev/dri/card0"] {
                if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(path) {
                    found = Some(Card(f));
                    tracing::info!(path, "opened DRM device");
                    break;
                }
            }
            found.context("no DRM device found")?
        };

        // Find a connected output.
        let res = card.resource_handles().context("resource_handles")?;
        let (connector, crtc, mode) = {
            let mut result = None;
            for &ch in res.connectors() {
                let conn = card.get_connector(ch, false).context("get_connector")?;
                if conn.state() != drm::control::connector::State::Connected {
                    continue;
                }
                let modes = conn.modes().to_vec();
                let mode = modes.first().context("connector has no modes")?.clone();
                let enc = conn.current_encoder().context("no encoder")?;
                let crtc = card.get_encoder(enc).context("get_encoder")?.crtc().context("no CRTC")?;
                tracing::info!(connector = ?ch, mode = ?mode.size(), "found output");
                result = Some((ch, crtc, mode));
                break;
            }
            result.context("no connected display found")?
        };

        let (width, height) = (mode.size().0 as u32, mode.size().1 as u32);

        // GBM
        let gbm_device = gbm::Device::new(card).context("gbm::Device::new")?;
        let gbm_surface = gbm_device
            .create_surface::<()>(
                width, height,
                drm_fourcc::DrmFourcc::Xrgb8888,
                gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
            )
            .context("create_surface")?;

        // EGL
        let egl = unsafe {
            khronos_egl::DynamicInstance::<khronos_egl::EGL1_4>::load_required()
                .context("load EGL")?
        };
        let egl_display = unsafe {
            egl.get_display(gbm_device.as_raw() as khronos_egl::NativeDisplayType)
        }
        .context("eglGetDisplay")?;
        egl.initialize(egl_display).context("eglInitialize")?;

        let config = egl
            .choose_first_config(egl_display, &[
                khronos_egl::RED_SIZE, 8, khronos_egl::GREEN_SIZE, 8,
                khronos_egl::BLUE_SIZE, 8, khronos_egl::ALPHA_SIZE, 0,
                khronos_egl::SURFACE_TYPE, khronos_egl::WINDOW_BIT,
                khronos_egl::RENDERABLE_TYPE, khronos_egl::OPENGL_ES2_BIT,
                khronos_egl::NONE,
            ])
            .context("eglChooseConfig")?
            .context("no matching EGL config")?;

        egl.bind_api(khronos_egl::OPENGL_ES_API).context("eglBindAPI")?;

        let context = egl
            .create_context(egl_display, config, None, &[
                khronos_egl::CONTEXT_CLIENT_VERSION, 2, khronos_egl::NONE,
            ])
            .context("eglCreateContext")?;

        let egl_surface = unsafe {
            egl.create_window_surface(
                egl_display, config,
                gbm_surface.as_raw() as khronos_egl::NativeWindowType, None,
            )
        }
        .context("eglCreateWindowSurface")?;

        egl.make_current(egl_display, Some(egl_surface), Some(egl_surface), Some(context))
            .context("eglMakeCurrent")?;

        // GLES2
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s).map(|p| p as *const _).unwrap_or(std::ptr::null())
            })
        };
        tracing::info!(renderer = unsafe { gl.get_parameter_string(glow::RENDERER) }, "GLES2 ready");

        let renderer = unsafe { GlesRenderer::new(gl)? };

        // Initial black frame → set CRTC mode.
        unsafe { renderer.draw(width as i32, height as i32) };
        egl.swap_buffers(egl_display, egl_surface).context("initial swap")?;

        let front_bo = unsafe { gbm_surface.lock_front_buffer() }.context("lock")?;
        let front_fb = gbm_device.add_framebuffer(&front_bo, 24, 32).context("addfb")?;
        gbm_device.set_crtc(crtc, Some(front_fb), (0, 0), &[connector], Some(mode))
            .context("set_crtc")?;

        println!("rendering to HDMI ({width}x{height})");

        Ok(Self {
            renderer, egl, egl_display, egl_surface,
            gbm_device, gbm_surface, connector, crtc,
            front_bo, front_fb, width, height,
        })
    }

    /// Presents the current GLES2 framebuffer to the display.
    fn flip(&mut self) -> Result<()> {
        unsafe { self.renderer.draw(self.width as i32, self.height as i32) };
        self.egl.swap_buffers(self.egl_display, self.egl_surface).context("swap")?;

        let new_bo = unsafe { self.gbm_surface.lock_front_buffer() }.context("lock")?;
        let new_fb = self.gbm_device.add_framebuffer(&new_bo, 24, 32).context("addfb")?;
        self.gbm_device
            .set_crtc(self.crtc, Some(new_fb), (0, 0), &[self.connector], None)
            .context("set_crtc")?;

        self.gbm_device.destroy_framebuffer(self.front_fb).ok();
        let old_bo = std::mem::replace(&mut self.front_bo, new_bo);
        drop(old_bo);
        self.front_fb = new_fb;
        Ok(())
    }
}

// ── DRM render loops ───────────────────────────────────────────────

/// Renders a remote broadcast to HDMI via DRM/KMS + GBM + EGL + GLES2.
pub(crate) fn run_drm(mut video_track: VideoTrack, session: MoqSession) -> Result<()> {
    let mut disp = DrmDisplay::init()?;
    let mut last_ts: Option<Duration> = None;
    let mut frame_count = 0u64;
    let mut fps_last = Instant::now();

    println!("ctrl-c to quit");

    loop {
        try_upload_frame(&mut disp.renderer, &mut video_track, &mut last_ts, &mut frame_count);
        disp.flip()?;
        print_stats(&session, &video_track, &mut frame_count, &mut fps_last);
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Renders a local [`VideoTrack`] (e.g. TestVideoSource) to HDMI — no network needed.
pub(crate) fn run_fb_demo(mut video_track: VideoTrack) -> Result<()> {
    let mut disp = DrmDisplay::init()?;
    let mut last_ts: Option<Duration> = None;
    let mut frame_count = 0u64;
    let mut fps_last = Instant::now();

    println!("fb-demo running, ctrl-c to quit");

    loop {
        try_upload_frame(&mut disp.renderer, &mut video_track, &mut last_ts, &mut frame_count);
        disp.flip()?;

        let elapsed = fps_last.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let fps = frame_count as f32 / elapsed.as_secs_f32();
            frame_count = 0;
            fps_last = Instant::now();
            println!("fps: {fps:.0}");
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

// ── Windowed (glutin + winit) ──────────────────────────────────────

/// Renders video in a window using glutin + winit + GLES2.
#[cfg(feature = "windowed")]
pub(crate) fn run_windowed(
    video_track: VideoTrack,
    session: MoqSession,
    fullscreen: bool,
) -> Result<()> {
    use std::num::NonZeroU32;

    use glutin::config::ConfigTemplateBuilder;
    use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, Version};
    use glutin::display::{GetGlDisplay, GlDisplay};
    use glutin::surface::{GlSurface, SurfaceAttributesBuilder, WindowSurface};
    use glutin_winit::DisplayBuilder;
    use raw_window_handle::HasWindowHandle;
    use winit::application::ApplicationHandler;
    use winit::event::{ElementState, KeyEvent, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::keyboard::{Key, NamedKey};
    use winit::window::{Fullscreen, Window, WindowId};

    struct App {
        renderer: Option<GlesRenderer>,
        surface: Option<glutin::surface::Surface<WindowSurface>>,
        context: Option<glutin::context::PossiblyCurrentContext>,
        window: Option<Window>,
        video_track: VideoTrack,
        session: MoqSession,
        fullscreen: bool,
        last_ts: Option<Duration>,
        frame_count: u64,
        fps_last: Instant,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }

            let mut attrs = Window::default_attributes().with_title("iroh-live viewer");
            if self.fullscreen {
                attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
            }

            let config_template = ConfigTemplateBuilder::new()
                .with_api(glutin::config::Api::GLES2);

            let display_builder = DisplayBuilder::new().with_window_attributes(Some(attrs));

            let (window, gl_config) = display_builder
                .build(event_loop, config_template, |mut configs| {
                    configs.next().expect("no GL config")
                })
                .expect("display builder failed");

            let window = window.expect("no window created");
            let raw = window.window_handle().expect("window handle").as_raw();

            let gl_display = gl_config.display();
            let context_attrs = ContextAttributesBuilder::new()
                .with_context_api(glutin::context::ContextApi::Gles(Some(Version::new(2, 0))))
                .build(Some(raw));

            let not_current = unsafe {
                gl_display
                    .create_context(&gl_config, &context_attrs)
                    .expect("create GL context")
            };

            let size = window.inner_size();
            let (w, h) = (
                NonZeroU32::new(size.width.max(1)).unwrap(),
                NonZeroU32::new(size.height.max(1)).unwrap(),
            );
            let surface_attrs =
                SurfaceAttributesBuilder::<WindowSurface>::new().build(raw, w, h);
            let surface = unsafe {
                gl_display
                    .create_window_surface(&gl_config, &surface_attrs)
                    .expect("create window surface")
            };

            let context = not_current
                .make_current(&surface)
                .expect("make current");

            let gl = unsafe {
                glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s))
            };
            tracing::info!(
                renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
                "GLES2 windowed context ready"
            );

            let gles = unsafe { GlesRenderer::new(gl).expect("GlesRenderer::new") };

            self.renderer = Some(gles);
            self.surface = Some(surface);
            self.context = Some(context);
            self.window = Some(window);
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _id: WindowId,
            event: WindowEvent,
        ) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            logical_key: Key::Named(NamedKey::Escape),
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => event_loop.exit(),

                WindowEvent::Resized(size) => {
                    if let (Some(surface), Some(context)) =
                        (&self.surface, &self.context)
                    {
                        surface.resize(
                            context,
                            NonZeroU32::new(size.width.max(1)).unwrap(),
                            NonZeroU32::new(size.height.max(1)).unwrap(),
                        );
                    }
                }

                WindowEvent::RedrawRequested => {
                    let Some(renderer) = &mut self.renderer else {
                        return;
                    };
                    let Some(surface) = &self.surface else { return };
                    let Some(context) = &self.context else { return };

                    try_upload_frame(
                        renderer,
                        &mut self.video_track,
                        &mut self.last_ts,
                        &mut self.frame_count,
                    );

                    let window = self.window.as_ref().unwrap();
                    let size = window.inner_size();
                    unsafe {
                        renderer.draw(size.width as i32, size.height as i32);
                    }
                    surface.swap_buffers(context).ok();

                    print_stats(
                        &self.session,
                        &self.video_track,
                        &mut self.frame_count,
                        &mut self.fps_last,
                    );

                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        Instant::now() + POLL_INTERVAL,
                    ));
                    window.request_redraw();
                }

                _ => {}
            }
        }
    }

    let event_loop = EventLoop::new().context("create event loop")?;

    let mut app = App {
        renderer: None,
        surface: None,
        context: None,
        window: None,
        video_track,
        session,
        fullscreen,
        last_ts: None,
        frame_count: 0,
        fps_last: Instant::now(),
    };

    event_loop.run_app(&mut app).context("event loop")?;
    Ok(())
}
