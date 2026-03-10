# moq-media Standalone Viewer Example

An egui application in `moq-media/examples/viewer.rs` that exercises the decoupled
pipeline API. No network required -- capture → encode → pipe → decode → render locally.

## Features

- **Capture source selector**: Camera, Screen, Test pattern
- **Codec selector**: H.264, AV1, VAAPI H.264, VTB H.264 (platform-gated)
- **Encoder backend selector**: Software, Hardware (auto-detects available)
- **Decoder backend selector**: Software, Hardware (auto-detects available)
- **Rendering mode**: wgpu (GPU shader) or Software (CPU upload)
- **Video view**: Rendered output filling available space
- **Debug panel**: FPS, source→display latency, encode/decode time, bandwidth, frame size

## Dependencies

```toml
# moq-media/Cargo.toml [dev-dependencies]
eframe = { version = "0.33.0", features = ["wgpu"] }
egui-wgpu = "0.33.0"

# moq-media/Cargo.toml [features]  (already exists, no change)
# wgpu = ["dep:wgpu"]
```

The example uses feature flags:
```
cargo run --example viewer --features "default,wgpu,capture"
cargo run --example viewer --features "default,wgpu,vaapi,capture"
```

Without `wgpu` feature, only software rendering is available.

## Architecture

```
[CaptureSource] → [VideoEncoderPipeline] → [PipeSink]
                                                |
                                          [media_pipe]
                                                |
[egui render] ← [VideoDecoderPipeline] ← [PipeSource]
```

All encoding/decoding happens on OS threads. The egui UI thread just reads
the latest decoded frame and renders it.

## UI Layout

```
+------------------------------------------------------------------+
| Viewer                                                           |
+------------------------------------------------------------------+
| [Camera v] [H.264 v] [SW Enc v] [SW Dec v] [wgpu v] [Start/Stop]|
+------------------------------------------------------------------+
|                                                                  |
|                                                                  |
|                    Video Output                                  |
|                                                                  |
|                                                                  |
+------------------------------------------------------------------+
| FPS: 30.1 | Latency: 12ms | Enc: 3.2ms | Dec: 1.1ms            |
| Bitrate: 2.4 Mbps | Frame: 1280x720 | Codec: h264-sw           |
+------------------------------------------------------------------+
```

## Implementation

### CLI Args

```rust
#[derive(Debug, Parser)]
struct Cli {
    /// Force software rendering (no wgpu)
    #[clap(long)]
    software: bool,
}
```

### State Machine

```rust
struct ViewerApp {
    // Config (changed via UI)
    capture_source: CaptureKind,
    codec: CodecKind,
    encoder_backend: BackendKind,
    decoder_backend: BackendKind,
    render_mode: RenderMode,

    // Running pipeline (None when stopped)
    pipeline: Option<RunningPipeline>,

    // Stats
    stats: Stats,

    // Rendering
    #[cfg(feature = "wgpu")]
    wgpu_renderer: Option<WgpuVideoRenderer>,
    texture: Option<egui::TextureHandle>,
    #[cfg(feature = "wgpu")]
    wgpu_texture_id: Option<egui::TextureId>,
    #[cfg(feature = "wgpu")]
    render_state: Option<egui_wgpu::RenderState>,
}

struct RunningPipeline {
    encoder: VideoEncoderPipeline,
    decoder: VideoDecoderPipeline,
}
```

### Enums for Selectors

```rust
#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter, strum::Display)]
enum CaptureKind {
    #[cfg(feature = "capture-camera")]
    Camera,
    #[cfg(feature = "capture-screen")]
    Screen,
    TestPattern,
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter, strum::Display)]
enum CodecKind {
    #[cfg(feature = "h264")]
    H264,
    #[cfg(feature = "av1")]
    Av1,
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264,
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    VtbH264,
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter, strum::Display)]
enum BackendKind {
    Auto,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter, strum::Display)]
enum RenderMode {
    Software,
    #[cfg(feature = "wgpu")]
    Wgpu,
}
```

### Pipeline Construction

```rust
impl ViewerApp {
    fn start_pipeline(&mut self) -> Result<()> {
        // 1. Create video source
        let source: Box<dyn VideoSource> = match self.capture_source {
            CaptureKind::Camera => Box::new(CameraCapturer::new()?),
            CaptureKind::Screen => Box::new(ScreenCapturer::new()?),
            CaptureKind::TestPattern => Box::new(TestPatternSource::new(1280, 720)),
        };

        // 2. Create encoder
        let preset = VideoPreset::P720;
        let (encoder, video_config): (Box<dyn VideoEncoder>, VideoConfig) = match self.codec {
            CodecKind::H264 => {
                let e = codec::H264Encoder::with_preset(preset)?;
                let c = e.config();
                (Box::new(e), c)
            }
            // ... other codecs
        };

        // 3. Create pipe
        let (sink, pipe_source) = media_pipe(32);

        // 4. Create encoder pipeline
        let encoder_pipeline = VideoEncoderPipeline::new(source, encoder, sink);

        // 5. Create decoder pipeline
        let decode_config = DecodeConfig {
            backend: match self.decoder_backend {
                BackendKind::Auto => DecoderBackend::Auto,
                BackendKind::Software => DecoderBackend::Software,
            },
            ..Default::default()
        };
        let decoder_pipeline = VideoDecoderPipeline::new::<DynamicVideoDecoder>(
            "viewer".into(),
            pipe_source,
            &video_config,
            &decode_config,
        )?;

        self.pipeline = Some(RunningPipeline {
            encoder: encoder_pipeline,
            decoder: decoder_pipeline,
        });
        Ok(())
    }
}
```

### Stats Collection

```rust
struct Stats {
    frame_count: u64,
    last_frame_time: Instant,
    fps: f32,
    fps_samples: VecDeque<Instant>,  // rolling window

    /// Time from source capture to decoded frame available.
    latency_ms: f32,

    /// Encoded frame size (last).
    frame_bytes: usize,

    /// Rolling bitrate estimate.
    bitrate_bps: f64,
    bitrate_window: VecDeque<(Instant, usize)>,

    /// Frame dimensions.
    width: u32,
    height: u32,

    /// Active codec/decoder names.
    codec_name: String,
    decoder_name: String,
}

impl Stats {
    fn update(&mut self, frame: &DecodedVideoFrame) {
        let now = Instant::now();

        // FPS: count frames in last 1s window
        self.fps_samples.push_back(now);
        while self.fps_samples.front().is_some_and(|t| now - *t > Duration::from_secs(1)) {
            self.fps_samples.pop_front();
        }
        self.fps = self.fps_samples.len() as f32;

        // Latency: frame.timestamp vs wall clock
        // (requires source to stamp with Instant, or we measure receive time - send time)

        // Dimensions
        let (w, h) = frame.dimensions();
        self.width = w;
        self.height = h;

        self.frame_count += 1;
        self.last_frame_time = now;
    }
}
```

### Rendering

```rust
impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Top panel: selectors
        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("Source")
                    .selected_text(self.capture_source.to_string())
                    .show_ui(ui, |ui| {
                        for kind in CaptureKind::iter() {
                            ui.selectable_value(&mut self.capture_source, kind, kind.to_string());
                        }
                    });

                egui::ComboBox::from_label("Codec")
                    .selected_text(self.codec.to_string())
                    .show_ui(ui, |ui| {
                        for kind in CodecKind::iter() {
                            ui.selectable_value(&mut self.codec, kind, kind.to_string());
                        }
                    });

                // ... encoder_backend, decoder_backend, render_mode selectors ...

                if self.pipeline.is_some() {
                    if ui.button("Stop").clicked() {
                        self.pipeline = None;
                    }
                } else if ui.button("Start").clicked() {
                    if let Err(e) = self.start_pipeline() {
                        tracing::error!("Failed to start pipeline: {e:#}");
                    }
                }
            });
        });

        // Bottom panel: stats
        egui::TopBottomPanel::bottom("stats").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("FPS: {:.0}", self.stats.fps));
                ui.separator();
                ui.label(format!("Latency: {:.1}ms", self.stats.latency_ms));
                ui.separator();
                ui.label(format!("{}x{}", self.stats.width, self.stats.height));
                ui.separator();
                ui.label(format!("Codec: {}", self.stats.codec_name));
            });
        });

        // Central panel: video
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(ref mut pipeline) = self.pipeline {
                if let Some(frame) = pipeline.decoder.frames.current_frame() {
                    self.stats.update(&frame);

                    let available = ui.available_size();
                    pipeline.decoder.handle.set_viewport(
                        available.x as u32,
                        available.y as u32,
                    );

                    match self.render_mode {
                        #[cfg(feature = "wgpu")]
                        RenderMode::Wgpu => {
                            self.render_wgpu(ui, &frame);
                        }
                        RenderMode::Software => {
                            self.render_software(ui, &frame);
                        }
                    }
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Press Start to begin");
                });
            }
        });

        // Request continuous repaint while pipeline is running
        if self.pipeline.is_some() {
            ctx.request_repaint();
        }
    }
}
```

**Software rendering path:**
```rust
fn render_software(&mut self, ui: &mut egui::Ui, frame: &DecodedVideoFrame) {
    let (w, h) = frame.dimensions();
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        frame.img().as_raw(),
    );
    let texture = self.texture.get_or_insert_with(|| {
        ui.ctx().load_texture("video", image.clone(), Default::default())
    });
    texture.set(image, Default::default());
    ui.image(egui::load::SizedTexture::from_handle(texture));
}
```

**wgpu rendering path:**
```rust
#[cfg(feature = "wgpu")]
fn render_wgpu(&mut self, ui: &mut egui::Ui, frame: &DecodedVideoFrame) {
    let renderer = self.wgpu_renderer.as_mut().unwrap();
    let view = renderer.render(frame);

    let render_state = self.render_state.as_ref().unwrap();
    let mut egui_renderer = render_state.renderer.write();

    match self.wgpu_texture_id {
        Some(id) => {
            egui_renderer.update_egui_texture_from_wgpu_texture(
                renderer.device(), view, wgpu::FilterMode::Linear, id,
            );
        }
        None => {
            let id = egui_renderer.register_native_texture(
                renderer.device(), view, wgpu::FilterMode::Linear,
            );
            self.wgpu_texture_id = Some(id);
        }
    }

    if let Some(id) = self.wgpu_texture_id {
        let (w, h) = frame.dimensions();
        ui.image(egui::load::SizedTexture::new(id, [w as f32, h as f32]));
    }
}
```

### Test Pattern Source

A `VideoSource` that generates colored test frames without requiring capture hardware:

```rust
struct TestPatternSource {
    format: VideoFormat,
    frame_index: u32,
    started: bool,
}

impl TestPatternSource {
    fn new(width: u32, height: u32) -> Self {
        Self {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            frame_index: 0,
            started: false,
        }
    }
}

impl VideoSource for TestPatternSource {
    fn name(&self) -> &str { "test-pattern" }
    fn format(&self) -> VideoFormat { self.format.clone() }
    fn start(&mut self) -> Result<()> { self.started = true; Ok(()) }
    fn stop(&mut self) -> Result<()> { self.started = false; Ok(()) }
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.started { return Ok(None); }
        let [w, h] = self.format.dimensions;
        let frame = codec::test_util::make_test_pattern(w, h, self.frame_index);
        self.frame_index += 1;
        Ok(Some(frame))
    }
}
```

### eframe Setup

```rust
fn main() -> eframe::Result<()> {
    tracing_subscriber::init();
    let cli = Cli::parse();

    let native_options = eframe::NativeOptions {
        #[cfg(feature = "wgpu")]
        renderer: if cli.software {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        ..Default::default()
    };

    eframe::run_native(
        "moq-media viewer",
        native_options,
        Box::new(|cc| {
            let mut app = ViewerApp::default();

            #[cfg(feature = "wgpu")]
            if let Some(ref wgpu_state) = cc.wgpu_render_state {
                app.render_state = Some(wgpu_state.clone());
                app.wgpu_renderer = Some(WgpuVideoRenderer::new(
                    wgpu_state.device.clone(),
                    wgpu_state.queue.clone(),
                ));
            }

            Ok(Box::new(app))
        }),
    )
}
```

## Latency Measurement

To measure source→display latency without network:

1. **Source timestamps**: Each `VideoFrame` from capture gets `Instant::now()` at capture time.
   Store in a side channel (the pipeline's MediaPacket carries a Duration timestamp).
2. **Pipeline approach**: Wrap the VideoSource to record capture `Instant` per frame index.
   When the decoded frame arrives, compare `Instant::now() - capture_instant`.
3. **Simple approach**: Use the frame's `timestamp` field (Duration since source start) and
   compare with `pipeline_start.elapsed()`. The difference is the pipeline latency.

```rust
// In stats update:
let pipeline_elapsed = self.pipeline_start.elapsed();
let frame_timestamp = frame.timestamp;
self.stats.latency_ms = (pipeline_elapsed - frame_timestamp).as_secs_f32() * 1000.0;
```

## Files

- `moq-media/examples/viewer.rs` -- the example
- No new library files needed (uses pipeline.rs and transport.rs from the API decoupling refactor)

## Cargo Commands

```sh
# Software only (no GPU features)
cargo run --example viewer

# With wgpu rendering
cargo run --example viewer --features wgpu

# With all hardware backends (Linux)
cargo run --example viewer --features "wgpu,vaapi,capture"

# With all hardware backends (macOS)
cargo run --example viewer --features "wgpu,videotoolbox,capture"
```
