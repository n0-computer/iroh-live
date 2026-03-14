//! Minimal wgpu+winit video viewer — no egui dependency.
//!
//! Connects to a remote broadcast and renders decoded video frames
//! fullscreen using wgpu. Press ESC or close the window to quit.
//! Stats are printed to stdout every second.
//!
//! ```sh
//! cargo run --example watch-wgpu --features wgpu -- --ticket <TICKET>
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use iroh::{Endpoint, EndpointId, Watcher};
use iroh_live::{
    Live,
    media::{
        audio_backend::AudioBackend,
        codec::DefaultDecoders,
        format::{DecodeConfig, DecoderBackend, PlaybackConfig},
        render::WgpuVideoRenderer,
        subscribe::VideoTrack,
    },
    moq::MoqSession,
    ticket::LiveTicket,
};
use n0_error::Result;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Window, WindowId},
};

#[derive(Debug, Parser)]
struct Cli {
    #[clap(long, conflicts_with = "endpoint-id")]
    ticket: Option<LiveTicket>,
    #[clap(long, conflicts_with = "ticket", requires = "name")]
    endpoint_id: Option<EndpointId>,
    #[clap(long, conflicts_with = "ticket", requires = "endpoint-id")]
    name: Option<String>,
    /// Start in fullscreen mode.
    #[clap(long)]
    fullscreen: bool,
    /// Decoder: "auto" (try HW then SW) or "software" (force SW).
    #[clap(long, default_value = "auto")]
    decoder: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let ticket = match (&cli.ticket, &cli.endpoint_id, &cli.name) {
        (Some(ticket), None, None) => ticket.clone(),
        (None, Some(endpoint_id), Some(name)) => LiveTicket::new(*endpoint_id, name.clone()),
        _ => {
            eprintln!("Usage: --ticket <TICKET> or --endpoint-id <ID> --name <NAME>");
            std::process::exit(1);
        }
    };
    let backend = match cli.decoder.as_str() {
        "auto" => DecoderBackend::Auto,
        "software" | "sw" => DecoderBackend::Software,
        other => {
            eprintln!("Unknown decoder: {other}. Use 'auto' or 'software'");
            std::process::exit(1);
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let audio_ctx = AudioBackend::default();

    println!("connecting to {ticket} ...");
    let (_endpoint, session, video_track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let playback_config = PlaybackConfig {
                decode_config: DecodeConfig {
                    backend,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (session, track) = live
                .subscribe_media_track::<DefaultDecoders>(
                    ticket.endpoint,
                    &ticket.broadcast_name,
                    &audio_ctx,
                    playback_config,
                )
                .await?;
            println!("connected!");
            let video = track.video.expect("no video track in broadcast");
            n0_error::Ok((endpoint, session, video))
        }
    })?;

    let event_loop = EventLoop::new().expect("failed to create event loop");
    let mut app = WgpuApp {
        state: None,
        video_track,
        session,
        _audio_ctx: audio_ctx,
        _rt: rt,
        fullscreen: cli.fullscreen,
        frame_count: 0,
        fps_last_update: Instant::now(),
        fps: 0.0,
    };
    event_loop.run_app(&mut app).expect("event loop failed");
    Ok(())
}

struct GpuState {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    renderer: WgpuVideoRenderer,
    blit_pipeline: wgpu::RenderPipeline,
    blit_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

struct WgpuApp {
    state: Option<GpuState>,
    video_track: VideoTrack,
    session: MoqSession,
    _audio_ctx: AudioBackend,
    _rt: tokio::runtime::Runtime,
    fullscreen: bool,
    frame_count: u64,
    fps_last_update: Instant,
    fps: f32,
}

impl WgpuApp {
    fn update_fps(&mut self) {
        self.frame_count += 1;
        let elapsed = self.fps_last_update.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.frame_count as f32 / elapsed.as_secs_f32();
            self.frame_count = 0;
            self.fps_last_update = Instant::now();
            // Print stats to stdout.
            let conn = self.session.conn();
            let path_list = conn.paths().get();
            let rtt = path_list
                .iter()
                .find(|p| p.is_selected())
                .map(|p| p.rtt())
                .unwrap_or_default();
            println!(
                "fps: {:.0}  rtt: {}ms  decoder: {}",
                self.fps,
                rtt.as_millis(),
                self.video_track.decoder_name(),
            );
        }
    }
}

impl ApplicationHandler for WgpuApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let mut attrs = Window::default_attributes().with_title("iroh-live viewer");
        if self.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).unwrap();

        let (adapter, device, queue) = pollster::block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await
                .expect("no suitable adapter");
            let (device, queue) = adapter
                .request_device(&Default::default())
                .await
                .expect("device creation failed");
            (adapter, device, queue)
        });

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        // Blit pipeline: renders a texture to a fullscreen quad.
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let blit_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("blit_bind_group"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit_pipeline_layout"),
            bind_group_layouts: &[&blit_bind_group_layout],
            push_constant_ranges: &[],
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let renderer = WgpuVideoRenderer::new(device.clone(), queue.clone());

        self.state = Some(GpuState {
            window,
            device,
            queue,
            surface,
            surface_config,
            renderer,
            blit_pipeline,
            blit_bind_group_layout,
            sampler,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(state) = &mut self.state {
                    state.surface_config.width = size.width.max(1);
                    state.surface_config.height = size.height.max(1);
                    state
                        .surface
                        .configure(&state.device, &state.surface_config);
                    state.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.update_fps();

                let Some(state) = &mut self.state else {
                    return;
                };

                // Get latest decoded frame.
                let Some(frame) = self.video_track.current_frame() else {
                    state.window.request_redraw();
                    return;
                };

                // Update viewport.
                self.video_track
                    .set_viewport(state.surface_config.width, state.surface_config.height);

                // Render frame to video texture.
                let video_view = state.renderer.render(&frame);

                // Blit video texture to surface.
                let output = match state.surface.get_current_texture() {
                    Ok(t) => t,
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        state
                            .surface
                            .configure(&state.device, &state.surface_config);
                        state.window.request_redraw();
                        return;
                    }
                    Err(e) => {
                        eprintln!("surface error: {e}");
                        return;
                    }
                };
                let surface_view = output
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                let bind_group = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("blit_bind_group"),
                    layout: &state.blit_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(video_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&state.sampler),
                        },
                    ],
                });

                let mut encoder =
                    state
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("blit_encoder"),
                        });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("blit_pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &surface_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&state.blit_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }
                state.queue.submit([encoder.finish()]);
                output.present();
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

/// Fullscreen blit shader: renders a texture to a fullscreen triangle.
const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) id: u32) -> VertexOutput {
    // Fullscreen triangle: 3 vertices cover the entire screen.
    let uv = vec2<f32>(f32((id << 1u) & 2u), f32(id & 2u));
    var out: VertexOutput;
    out.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y); // flip Y for texture coords
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"#;
