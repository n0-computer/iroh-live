use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use clap::Parser;
use glow::HasContext;
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextAttributesBuilder, NotCurrentGlContext, Version},
    display::{GetGlDisplay, GlDisplay},
    surface::{GlSurface, SurfaceAttributesBuilder, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use iroh::Endpoint;
use iroh_live::{
    Live,
    media::{
        audio_backend::AudioBackend, codec::DefaultDecoders, format::PlaybackConfig,
        subscribe::VideoTrack,
    },
    ticket::LiveTicket,
};
use raw_window_handle::HasWindowHandle;
use rusty_codecs::render::gles::GlesRenderer;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

#[derive(Debug, Parser)]
#[clap(about = "Minimal GLES2 video viewer for iroh-live")]
struct Cli {
    ticket: LiveTicket,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let audio_ctx = AudioBackend::default();

    println!("connecting to {} ...", cli.ticket);
    let (live, video_track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        let ticket = cli.ticket.clone();
        async move {
            let endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
            let live = Live::new(endpoint);
            let (_session, track) = live
                .subscribe_media_track::<DefaultDecoders>(
                    ticket.endpoint,
                    &ticket.broadcast_name,
                    &audio_ctx,
                    PlaybackConfig::default(),
                )
                .await?;
            let video = track.video.context("broadcast has no video track")?;
            println!("connected");
            anyhow::Ok((live, video))
        }
    })?;

    let _guard = rt.enter();

    let event_loop = EventLoop::new().context("create event loop")?;
    let mut app = App {
        _live: live,
        renderer: None,
        surface: None,
        context: None,
        window: None,
        video_track,
        last_ts: None,
        frame_count: 0,
        fps_last: Instant::now(),
    };
    event_loop.run_app(&mut app).context("event loop")?;
    Ok(())
}

struct App {
    renderer: Option<GlesRenderer>,
    surface: Option<glutin::surface::Surface<WindowSurface>>,
    context: Option<glutin::context::PossiblyCurrentContext>,
    window: Option<Window>,
    _live: Live,
    video_track: VideoTrack,
    last_ts: Option<Duration>,
    frame_count: u64,
    fps_last: Instant,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes().with_title("iroh-live opengl viewer");
        let config_template = ConfigTemplateBuilder::new().with_api(glutin::config::Api::GLES2);
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(attrs));

        let (window, gl_config) = display_builder
            .build(event_loop, config_template, |mut configs| {
                configs.next().expect("no GL config")
            })
            .expect("display builder failed");

        let window = window.expect("no window");
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

        let gl =
            unsafe { glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s)) };
        tracing::info!(
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            "GLES2 ready"
        );

        let renderer = unsafe { GlesRenderer::new(gl).expect("GlesRenderer::new") };

        self.renderer = Some(renderer);
        self.surface = Some(surface);
        self.context = Some(context);
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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

                let frame = self.video_track.current_frame();
                let is_new = frame
                    .as_ref()
                    .is_some_and(|f| self.last_ts.is_none_or(|prev| f.timestamp != prev));

                if is_new && let Some(f) = &frame {
                    self.last_ts = Some(f.timestamp);
                    self.frame_count += 1;
                    unsafe { renderer.upload_frame(f) };
                }

                let window = self.window.as_ref().unwrap();
                let size = window.inner_size();
                unsafe { renderer.draw(size.width as i32, size.height as i32) };
                surface.swap_buffers(context).ok();

                // FPS stats every second.
                let elapsed = self.fps_last.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    let fps = self.frame_count as f32 / elapsed.as_secs_f32();
                    self.frame_count = 0;
                    self.fps_last = Instant::now();
                    println!("fps: {fps:.0}");
                }

                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(4),
                ));
                window.request_redraw();
            }

            _ => {}
        }
    }
}
