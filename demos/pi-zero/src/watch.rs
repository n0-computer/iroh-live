//! EGL/GLES2 video viewer, in a winit window or straight to HDMI over DRM/KMS.
//!
//! Both backends draw with [`GlesRenderer`] and manage their own EGL context.

use std::{
    os::{fd::AsFd, unix::io::AsRawFd},
    sync::Arc,
    time::{Duration, Instant},
};

use drm::{Device as _, control::Device as ControlDevice};
use gbm::AsRaw;
use glow::HasContext;
use iroh_live::media::{Player, VideoFrames, video::Frame};
use n0_error::{Result, StdResultExt as _, anyerr};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{info, warn};
#[cfg(feature = "windowed")]
use {
    glutin::{
        config::ConfigTemplateBuilder,
        context::{ContextAttributesBuilder, NotCurrentGlContext, Version},
        display::{GetGlDisplay, GlDisplay},
        surface::{GlSurface, SurfaceAttributesBuilder, WindowSurface},
    },
    glutin_winit::DisplayBuilder,
    raw_window_handle::HasWindowHandle,
    std::num::NonZeroU32,
    winit::{
        application::ApplicationHandler,
        event::{ElementState, KeyEvent, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{Key, NamedKey},
        window::{Fullscreen, Window, WindowId},
    },
};

use crate::gles::GlesRenderer;

/// How often the window checks for a new frame, which caps it near 250 fps.
#[cfg(feature = "windowed")]
const POLL_INTERVAL: Duration = Duration::from_millis(4);

/// Uploads the newest frame if one arrived since the last call.
///
/// Returns whether it uploaded a frame.
#[cfg(feature = "windowed")]
fn try_upload_frame(
    renderer: &mut GlesRenderer,
    frames: &mut VideoFrames,
    frame_count: &mut u64,
) -> bool {
    let Some(frame) = frames.try_next() else {
        return false;
    };
    *frame_count += 1;
    if *frame_count <= 3 {
        info!(frame = *frame_count, size = %frame.size(), "decoding frame");
    }
    // SAFETY: the window made its GL context current on this thread before
    // creating the renderer, and nothing else makes another one current.
    unsafe { renderer.upload_frame(&frame) };
    true
}

/// Prints playback stats once a second.
#[cfg(feature = "windowed")]
fn print_stats(player: &Player, frame_count: &mut u64, fps_last: &mut Instant) {
    let elapsed = fps_last.elapsed();
    if elapsed < Duration::from_secs(1) {
        return;
    }
    let fps = *frame_count as f32 / elapsed.as_secs_f32();
    *frame_count = 0;
    *fps_last = Instant::now();

    println!("fps: {fps:.0}  {}", player.stats().summary());
}

/// A DRM device node opened for modesetting.
struct Card(std::fs::File);
impl AsFd for Card {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl ControlDevice for Card {}

/// Keeps the current VT in graphics mode, which hides the console text.
///
/// Restores text mode on drop.
struct VtGuard(std::fs::File);

impl VtGuard {
    /// Switches the current VT to graphics mode.
    fn activate() -> Result<Self> {
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
            .std_context("open /dev/tty0 (run as root or from a linux console)")?;

        // SAFETY: KDSETMODE takes an integer argument and touches no memory of
        // ours, and `tty` is an open console.
        let ret = unsafe { libc::ioctl(tty.as_raw_fd(), KDSETMODE, KD_GRAPHICS) };
        if ret != 0 {
            warn!("KDSETMODE(KD_GRAPHICS) failed - console text may remain visible");
        }

        Ok(Self(tty))
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        // SAFETY: as in `activate`; the console is still open.
        unsafe { libc::ioctl(self.0.as_raw_fd(), KDSETMODE, KD_TEXT) };
    }
}

/// The console ioctl that switches between text and graphics mode, from `linux/kd.h`.
const KDSETMODE: libc::c_ulong = 0x4B3A;
/// The `KDSETMODE` argument that stops the console drawing text over the picture.
const KD_GRAPHICS: libc::c_int = 0x01;
/// The `KDSETMODE` argument that gives the console back.
const KD_TEXT: libc::c_int = 0x00;

/// A DRM/KMS, GBM and EGL display that owns the GPU resources for HDMI output.
///
/// Its EGL context is current on the thread that created it. Use it on that
/// thread only.
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
    /// Opens the first DRM device with a connected output and sets up GLES2 on it.
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
                    info!(path, "opened DRM device");
                    break;
                }
            }
            found.std_context("no DRM device found")?
        };

        // Hide the console and take DRM master.
        let vt = match VtGuard::activate() {
            Ok(vt) => Some(vt),
            Err(e) => {
                warn!(error = %e, "VT switch failed - may need root or a linux console");
                None
            }
        };
        card.acquire_master_lock()
            .std_context("acquire DRM master (try running as root)")?;

        let res = card.resource_handles().std_context("resource_handles")?;
        let (connector, crtc, mode) = {
            let mut result = None;
            for &ch in res.connectors() {
                let conn = card.get_connector(ch, false).std_context("get_connector")?;
                if conn.state() != drm::control::connector::State::Connected {
                    continue;
                }
                let modes = conn.modes().to_vec();
                let mode = *modes.first().std_context("connector has no modes")?;
                let enc = conn.current_encoder().std_context("no encoder")?;
                let crtc = card
                    .get_encoder(enc)
                    .std_context("get_encoder")?
                    .crtc()
                    .std_context("no CRTC")?;
                info!(connector = ?ch, mode = ?mode.size(), "found output");
                result = Some((ch, crtc, mode));
                break;
            }
            result.std_context("no connected display found")?
        };

        let (width, height) = (mode.size().0 as u32, mode.size().1 as u32);

        let gbm_device = gbm::Device::new(card).std_context("gbm::Device::new")?;
        // vc4 scans out only LINEAR buffers.
        let gbm_surface = gbm_device
            .create_surface_with_modifiers::<()>(
                width,
                height,
                drm_fourcc::DrmFourcc::Xrgb8888,
                [drm_fourcc::DrmModifier::Linear].iter().copied(),
            )
            .or_else(|_| {
                gbm_device.create_surface::<()>(
                    width,
                    height,
                    drm_fourcc::DrmFourcc::Xrgb8888,
                    gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
                )
            })
            .std_context("create_surface")?;

        // SAFETY: loads the system's libEGL, whose initializers have no
        // preconditions of ours.
        let egl = unsafe {
            khronos_egl::DynamicInstance::<khronos_egl::EGL1_4>::load_required()
                .std_context("load EGL")?
        };
        // SAFETY: the GBM device is a valid native display, and outlives the EGL
        // display as a field of the same struct.
        let egl_display =
            unsafe { egl.get_display(gbm_device.as_raw() as khronos_egl::NativeDisplayType) }
                .std_context("eglGetDisplay")?;
        egl.initialize(egl_display).std_context("eglInitialize")?;

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
            .std_context("eglChooseConfig")?
            .std_context("no matching EGL config")?;

        egl.bind_api(khronos_egl::OPENGL_ES_API)
            .std_context("eglBindAPI")?;

        let context = egl
            .create_context(
                egl_display,
                config,
                None,
                &[khronos_egl::CONTEXT_CLIENT_VERSION, 2, khronos_egl::NONE],
            )
            .std_context("eglCreateContext")?;

        // SAFETY: the GBM surface is a valid native window for this display,
        // and outlives the EGL surface as a field of the same struct.
        let egl_surface = unsafe {
            egl.create_window_surface(
                egl_display,
                config,
                gbm_surface.as_raw() as khronos_egl::NativeWindowType,
                None,
            )
        }
        .std_context("eglCreateWindowSurface")?;

        egl.make_current(
            egl_display,
            Some(egl_surface),
            Some(egl_surface),
            Some(context),
        )
        .std_context("eglMakeCurrent")?;

        // SAFETY: the context is current on this thread, so EGL returns
        // function pointers that are valid for it.
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map(|p| p as *const _)
                    .unwrap_or(std::ptr::null())
            })
        };
        // SAFETY: the context was made current on this thread above.
        info!(
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            "GLES2 ready"
        );
        // SAFETY: as above.
        let renderer = unsafe { GlesRenderer::new(gl)? };

        // Initial black frame, then set the CRTC mode.
        // SAFETY: the context is current on this thread.
        unsafe { renderer.draw(width as i32, height as i32) };
        egl.swap_buffers(egl_display, egl_surface)
            .std_context("initial swap")?;

        // SAFETY: called once, right after the first swap.
        let front_bo = unsafe { gbm_surface.lock_front_buffer() }.std_context("lock")?;
        info!(
            bo_w = front_bo.width(), bo_h = front_bo.height(),
            stride = front_bo.stride(), modifier = ?front_bo.modifier(),
            format = ?front_bo.format(), "front BO"
        );
        let front_fb = gbm_device
            .add_planar_framebuffer(&front_bo, drm::control::FbCmd2Flags::MODIFIERS)
            .std_context("addfb")?;
        // Reuse the mode the kernel console set. vc4's atomic check already
        // accepted it, and a connector mode can fail there with EINVAL.
        let crtc_info = gbm_device.get_crtc(crtc).std_context("get_crtc")?;
        let active_mode = crtc_info.mode().std_context("CRTC has no active mode")?;
        info!(?front_fb, ?crtc, ?connector, mode = ?active_mode.size(), "set_crtc");
        gbm_device
            .set_crtc(
                crtc,
                Some(front_fb),
                (0, 0),
                &[connector],
                Some(active_mode),
            )
            .std_context("set_crtc")?;

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

    /// Draws the uploaded frame and shows it on the display.
    fn flip(&mut self) -> Result<()> {
        // SAFETY: the display is used on the thread whose context is current.
        unsafe { self.renderer.draw(self.width as i32, self.height as i32) };
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .std_context("swap")?;

        // SAFETY: called once, right after this flip's swap.
        let new_bo = unsafe { self.gbm_surface.lock_front_buffer() }.std_context("lock")?;
        let new_fb = self
            .gbm_device
            .add_planar_framebuffer(&new_bo, drm::control::FbCmd2Flags::MODIFIERS)
            .std_context("addfb")?;

        // `set_crtc` waits for vblank. `page_flip` returns EBUSY while a flip
        // is pending and would need an event loop to handle that.
        self.gbm_device
            .set_crtc(
                self.crtc,
                Some(new_fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .std_context("flip set_crtc")?;

        self.gbm_device.destroy_framebuffer(self.front_fb).ok();
        let old_bo = std::mem::replace(&mut self.front_bo, new_bo);
        drop(old_bo);
        self.front_fb = new_fb;
        Ok(())
    }
}

/// Renders a remote broadcast to HDMI over DRM/KMS.
pub(crate) async fn run_drm(player: Player) -> Result<()> {
    show_on_hdmi(player.video()).await
}

/// Renders a local frame stream to HDMI.
///
/// Used with [`VideoSource::test_pattern`](iroh_live::media::VideoSource::test_pattern).
pub(crate) async fn run_fb_demo(frames: VideoFrames) -> Result<()> {
    show_on_hdmi(frames).await
}

/// Shows `frames` on HDMI until they end.
///
/// The display lives on a thread of its own: its EGL context is current on
/// one thread only, which a task on the runtime could leave at any `.await`.
async fn show_on_hdmi(mut frames: VideoFrames) -> Result<()> {
    let (frame_tx, frame_rx) = tokio_mpsc::channel::<Arc<Frame>>(4);

    let render_handle = std::thread::Builder::new()
        .name("drm-render".into())
        .spawn(move || render_loop(frame_rx))
        .std_context("spawn render thread")?;

    // Ctrl-C ends the loop instead of the process, so the render thread
    // drops its display and the console goes back to text mode.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        let frame = tokio::select! {
            frame = frames.next() => frame,
            _ = &mut ctrl_c => {
                info!("interrupted, restoring the console");
                None
            }
        };
        let Some(frame) = frame else {
            break;
        };
        if frame_tx.send(frame).await.is_err() {
            break; // render thread exited
        }
    }

    // The render thread exits only when the channel closes, so drop the
    // sender before the join or both wait forever.
    drop(frame_tx);

    // `join` blocks its thread, so keep it off the async workers.
    tokio::task::spawn_blocking(move || render_handle.join())
        .await
        .std_context("failed to join the render thread")?
        .map_err(|_| anyerr!("render thread panicked"))??;
    Ok(())
}

/// Opens the display and shows the newest frame of `frame_rx` until it closes.
fn render_loop(mut frame_rx: tokio_mpsc::Receiver<Arc<Frame>>) -> Result<()> {
    let mut disp = DrmDisplay::init()?;
    let mut frame_count = 0u64;
    let mut fps_last = Instant::now();

    println!("ctrl-c to quit");

    while let Some(frame) = frame_rx.blocking_recv() {
        // Show only the newest frame.
        let mut latest = frame;
        while let Ok(newer) = frame_rx.try_recv() {
            latest = newer;
        }

        frame_count += 1;
        // SAFETY: `DrmDisplay::init` made its context current on this thread.
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
}

/// Renders video in a glutin and winit window.
#[cfg(feature = "windowed")]
pub(crate) fn run_windowed(player: Player, fullscreen: bool) -> Result<()> {
    /// The winit application state.
    struct App {
        renderer: Option<GlesRenderer>,
        surface: Option<glutin::surface::Surface<WindowSurface>>,
        context: Option<glutin::context::PossiblyCurrentContext>,
        window: Option<Window>,
        frames: VideoFrames,
        player: Player,
        fullscreen: bool,
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

            // SAFETY: the config and the attributes come from this display, and
            // the raw window handle belongs to a window that outlives the context.
            let not_current = unsafe {
                gl_display
                    .create_context(&gl_config, &context_attrs)
                    .expect("create GL context")
            };

            let size = window.inner_size();
            let (w, h) = (
                // A window can report 0x0, and a surface needs a non-zero size.
                NonZeroU32::new(size.width.max(1)).expect("max(1) is non-zero"),
                NonZeroU32::new(size.height.max(1)).expect("max(1) is non-zero"),
            );
            let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw, w, h);
            // SAFETY: as above; the window outlives the surface.
            let surface = unsafe {
                gl_display
                    .create_window_surface(&gl_config, &surface_attrs)
                    .expect("create window surface")
            };

            let context = not_current.make_current(&surface).expect("make current");

            // SAFETY: the context was made current on this thread above, so its
            // function pointers are valid.
            let gl = unsafe {
                glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s))
            };
            // SAFETY: the context is current on this thread.
            info!(
                renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
                "GLES2 windowed context ready"
            );

            // SAFETY: the context is current on this thread.
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
                            NonZeroU32::new(size.width.max(1)).expect("max(1) is non-zero"),
                            NonZeroU32::new(size.height.max(1)).expect("max(1) is non-zero"),
                        );
                    }
                }

                WindowEvent::RedrawRequested => {
                    let Some(renderer) = &mut self.renderer else {
                        return;
                    };
                    let Some(surface) = &self.surface else { return };
                    let Some(context) = &self.context else { return };

                    try_upload_frame(renderer, &mut self.frames, &mut self.frame_count);

                    // The window is set together with the renderer, surface
                    // and context.
                    let window = self
                        .window
                        .as_ref()
                        .expect("a window, since the GL context exists");
                    let size = window.inner_size();
                    // SAFETY: the event loop runs on the thread whose context is
                    // current.
                    unsafe {
                        renderer.draw(size.width as i32, size.height as i32);
                    }
                    surface.swap_buffers(context).ok();

                    print_stats(&self.player, &mut self.frame_count, &mut self.fps_last);

                    event_loop
                        .set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
                    window.request_redraw();
                }

                _ => {}
            }
        }
    }

    let event_loop = EventLoop::new().std_context("create event loop")?;

    let mut app = App {
        renderer: None,
        surface: None,
        context: None,
        window: None,
        frames: player.video(),
        player,
        fullscreen,
        frame_count: 0,
        fps_last: Instant::now(),
    };

    event_loop.run_app(&mut app).std_context("event loop")?;
    Ok(())
}
