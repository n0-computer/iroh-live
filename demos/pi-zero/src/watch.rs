//! EGL/GLES2 video viewer — windowed (winit) or direct-to-HDMI (DRM/KMS).
//!
//! Uses [`GlesRenderer`] from `rusty-codecs` for GLES2 rendering. The DRM
//! and windowed display backends handle EGL context + buffer swapping.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use glow::HasContext;
use iroh::Watcher;
use iroh_live::{
    media::{format::VideoFrame, subscribe::VideoTrack},
    moq::MoqSession,
};
use rusty_codecs::render::gles::GlesRenderer;

/// Poll interval between frame checks (≈250 fps ceiling).
const POLL_INTERVAL: Duration = Duration::from_millis(4);

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
            if *frame_count <= 3 {
                tracing::info!(
                    frame = *frame_count,
                    w = f.width(), h = f.height(),
                    format = ?std::mem::discriminant(&f.data),
                    "decoding frame"
                );
            }
            unsafe { renderer.upload_frame(f) };
        }
    }
    is_new
}

/// Prints FPS and RTT stats every second.
#[allow(dead_code)]
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

use std::os::fd::AsFd;

use drm::{Device as _, control::Device as ControlDevice};
use gbm::AsRaw;

struct Card(std::fs::File);
impl AsFd for Card {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl ControlDevice for Card {}

/// Switches the current VT to graphics mode (hides the console text cursor)
/// and takes DRM master. Returns the tty fd to restore on drop.
struct VtGuard(std::fs::File);

impl VtGuard {
    fn activate() -> Result<Self> {
        use std::os::unix::io::AsRawFd;

        // Open current tty.
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty0")
            .or_else(|_| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/tty")
            })
            .context("open /dev/tty0 (run as root or from a linux console)")?;

        // KD_GRAPHICS = 0x01, KDSETMODE = 0x4B3A
        let ret = unsafe { libc::ioctl(tty.as_raw_fd(), 0x4B3A, 0x01) };
        if ret != 0 {
            tracing::warn!("KDSETMODE(KD_GRAPHICS) failed — console text may remain visible");
        }

        Ok(Self(tty))
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // KD_TEXT = 0x00
        unsafe { libc::ioctl(self.0.as_raw_fd(), 0x4B3A, 0x00) };
    }
}

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
    mode: drm::control::Mode,
    front_bo: gbm::BufferObject<()>,
    front_fb: drm::control::framebuffer::Handle,
    width: u32,
    height: u32,
    _vt: Option<VtGuard>,
}

impl DrmDisplay {
    fn init() -> Result<Self> {
        let card = {
            let mut found = None;
            for path in ["/dev/dri/card1", "/dev/dri/card0"] {
                if let Ok(f) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                {
                    found = Some(Card(f));
                    tracing::info!(path, "opened DRM device");
                    break;
                }
            }
            found.context("no DRM device found")?
        };

        // Switch VT to graphics mode (hides console text) and grab DRM master.
        let vt = match VtGuard::activate() {
            Ok(vt) => Some(vt),
            Err(e) => {
                tracing::warn!(%e, "VT switch failed — may need root or a linux console");
                None
            }
        };
        card.acquire_master_lock()
            .context("acquire DRM master (try running as root)")?;

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
                let crtc = card
                    .get_encoder(enc)
                    .context("get_encoder")?
                    .crtc()
                    .context("no CRTC")?;
                tracing::info!(connector = ?ch, mode = ?mode.size(), "found output");
                result = Some((ch, crtc, mode));
                break;
            }
            result.context("no connected display found")?
        };

        let (width, height) = (mode.size().0 as u32, mode.size().1 as u32);

        // GBM
        let gbm_device = gbm::Device::new(card).context("gbm::Device::new")?;
        // Force LINEAR modifier for scanout compatibility on vc4.
        let gbm_surface = gbm_device
            .create_surface_with_modifiers::<()>(
                width,
                height,
                drm_fourcc::DrmFourcc::Xrgb8888,
                [drm_fourcc::DrmModifier::Linear].iter().copied(),
            )
            .or_else(|_| {
                // Fallback: no explicit modifier.
                gbm_device.create_surface::<()>(
                    width,
                    height,
                    drm_fourcc::DrmFourcc::Xrgb8888,
                    gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
                )
            })
            .context("create_surface")?;

        // EGL
        let egl = unsafe {
            khronos_egl::DynamicInstance::<khronos_egl::EGL1_4>::load_required()
                .context("load EGL")?
        };
        let egl_display =
            unsafe { egl.get_display(gbm_device.as_raw() as khronos_egl::NativeDisplayType) }
                .context("eglGetDisplay")?;
        egl.initialize(egl_display).context("eglInitialize")?;

        let config = egl
            .choose_first_config(
                egl_display,
                &[
                    khronos_egl::RED_SIZE,
                    8,
                    khronos_egl::GREEN_SIZE,
                    8,
                    khronos_egl::BLUE_SIZE,
                    8,
                    khronos_egl::ALPHA_SIZE,
                    0,
                    khronos_egl::SURFACE_TYPE,
                    khronos_egl::WINDOW_BIT,
                    khronos_egl::RENDERABLE_TYPE,
                    khronos_egl::OPENGL_ES2_BIT,
                    khronos_egl::NONE,
                ],
            )
            .context("eglChooseConfig")?
            .context("no matching EGL config")?;

        egl.bind_api(khronos_egl::OPENGL_ES_API)
            .context("eglBindAPI")?;

        let context = egl
            .create_context(
                egl_display,
                config,
                None,
                &[khronos_egl::CONTEXT_CLIENT_VERSION, 2, khronos_egl::NONE],
            )
            .context("eglCreateContext")?;

        let egl_surface = unsafe {
            egl.create_window_surface(
                egl_display,
                config,
                gbm_surface.as_raw() as khronos_egl::NativeWindowType,
                None,
            )
        }
        .context("eglCreateWindowSurface")?;

        egl.make_current(
            egl_display,
            Some(egl_surface),
            Some(egl_surface),
            Some(context),
        )
        .context("eglMakeCurrent")?;

        // GLES2
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map(|p| p as *const _)
                    .unwrap_or(std::ptr::null())
            })
        };
        tracing::info!(
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            "GLES2 ready"
        );

        let renderer = unsafe { GlesRenderer::new(gl)? };

        // Initial black frame → set CRTC mode.
        unsafe { renderer.draw(width as i32, height as i32) };
        egl.swap_buffers(egl_display, egl_surface)
            .context("initial swap")?;

        let front_bo = unsafe { gbm_surface.lock_front_buffer() }.context("lock")?;
        tracing::info!(
            bo_w = front_bo.width(), bo_h = front_bo.height(),
            stride = front_bo.stride(), modifier = ?front_bo.modifier(),
            format = ?front_bo.format(), "front BO"
        );
        let front_fb = gbm_device
            .add_planar_framebuffer(&front_bo, drm::control::FbCmd2Flags::MODIFIERS)
            .context("addfb")?;
        // Get the CRTC's currently active mode (set by the kernel console).
        // Using this exact mode avoids EINVAL from vc4's atomic check — the
        // mode was already validated when the console set it up.
        let crtc_info = gbm_device.get_crtc(crtc).context("get_crtc")?;
        let active_mode = crtc_info.mode().context("CRTC has no active mode")?;
        tracing::info!(?front_fb, ?crtc, ?connector, mode = ?active_mode.size(), "set_crtc");
        gbm_device
            .set_crtc(
                crtc,
                Some(front_fb),
                (0, 0),
                &[connector],
                Some(active_mode),
            )
            .context("set_crtc")?;

        println!("rendering to HDMI ({width}x{height})");

        Ok(Self {
            renderer,
            egl,
            egl_display,
            egl_surface,
            gbm_device,
            gbm_surface,
            connector,
            crtc,
            mode: active_mode,
            front_bo,
            front_fb,
            width,
            height,
            _vt: vt,
        })
    }

    /// Presents the current GLES2 framebuffer to the display.
    fn flip(&mut self) -> Result<()> {
        unsafe { self.renderer.draw(self.width as i32, self.height as i32) };
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .context("swap")?;

        let new_bo = unsafe { self.gbm_surface.lock_front_buffer() }.context("lock")?;
        let new_fb = self
            .gbm_device
            .add_planar_framebuffer(&new_bo, drm::control::FbCmd2Flags::MODIFIERS)
            .context("addfb")?;

        // set_crtc is synchronous (waits for vblank internally).
        // page_flip is async and returns EBUSY if a flip is pending,
        // which requires event-loop integration to handle correctly.
        self.gbm_device
            .set_crtc(
                self.crtc,
                Some(new_fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .context("flip set_crtc")?;

        self.gbm_device.destroy_framebuffer(self.front_fb).ok();
        let old_bo = std::mem::replace(&mut self.front_bo, new_bo);
        drop(old_bo);
        self.front_fb = new_fb;
        Ok(())
    }
}

// ── DRM render loops ───────────────────────────────────────────────

/// Renders a remote broadcast to HDMI via DRM/KMS + GBM + EGL + GLES2.
///
/// Spawns a dedicated render thread so the tokio runtime stays free for
/// packet ingestion and decode. Frames are forwarded via a bounded channel.
pub(crate) async fn run_drm(mut video_track: VideoTrack, _session: MoqSession) -> Result<()> {
    use tokio::sync::mpsc as tokio_mpsc;

    // Channel from async world (frame producer) to render thread (consumer).
    let (frame_tx, frame_rx) = tokio_mpsc::channel::<VideoFrame>(4);

    // Render thread — owns DRM display, receives frames, renders.
    let render_handle = std::thread::Builder::new()
        .name("drm-render".into())
        .spawn(move || -> Result<()> {
            let mut disp = DrmDisplay::init()?;
            let mut frame_rx = frame_rx;
            let mut frame_count = 0u64;
            let mut fps_last = Instant::now();

            println!("ctrl-c to quit");

            loop {
                // Block until a frame arrives (or channel closes).
                let frame = match frame_rx.blocking_recv() {
                    Some(f) => f,
                    None => break, // channel closed, exit
                };

                // Drain any newer frames — display the latest.
                let mut latest = frame;
                while let Ok(newer) = frame_rx.try_recv() {
                    latest = newer;
                }

                frame_count += 1;
                unsafe { disp.renderer.upload_frame(&latest) };
                disp.flip()?;

                let elapsed = fps_last.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    let fps = frame_count as f32 / elapsed.as_secs_f32();
                    frame_count = 0;
                    fps_last = Instant::now();
                    println!("fps: {fps:.0}");
                }
            }

            Ok(())
        })
        .context("spawn render thread")?;

    // Async frame pump — runs on tokio, feeds the render thread.
    loop {
        match video_track.next_frame().await {
            Some(frame) => {
                if frame_tx.send(frame).await.is_err() {
                    break; // render thread exited
                }
            }
            None => break, // track closed
        }
    }

    render_handle
        .join()
        .map_err(|_| anyhow::anyhow!("render thread panicked"))??;
    Ok(())
}

/// Renders a local [`VideoTrack`] (e.g. TestVideoSource) to HDMI — no network needed.
pub(crate) fn run_fb_demo(mut video_track: VideoTrack) -> Result<()> {
    let mut disp = DrmDisplay::init()?;
    let mut last_ts: Option<Duration> = None;
    let mut frame_count = 0u64;
    let mut fps_last = Instant::now();

    println!("fb-demo running, ctrl-c to quit");

    loop {
        try_upload_frame(
            &mut disp.renderer,
            &mut video_track,
            &mut last_ts,
            &mut frame_count,
        );
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

    use glutin::{
        config::ConfigTemplateBuilder,
        context::{ContextAttributesBuilder, NotCurrentGlContext, Version},
        display::{GetGlDisplay, GlDisplay},
        surface::{GlSurface, SurfaceAttributesBuilder, WindowSurface},
    };
    use glutin_winit::DisplayBuilder;
    use raw_window_handle::HasWindowHandle;
    use winit::{
        application::ApplicationHandler,
        event::{ElementState, KeyEvent, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{Key, NamedKey},
        window::{Fullscreen, Window, WindowId},
    };

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

            let config_template = ConfigTemplateBuilder::new().with_api(glutin::config::Api::GLES2);

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
            let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw, w, h);
            let surface = unsafe {
                gl_display
                    .create_window_surface(&gl_config, &surface_attrs)
                    .expect("create window surface")
            };

            let context = not_current.make_current(&surface).expect("make current");

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
                    if let (Some(surface), Some(context)) = (&self.surface, &self.context) {
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

                    event_loop
                        .set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
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
