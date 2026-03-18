#![cfg(any_codec)]
//! Standalone media viewer with multi-tile pipeline comparison.
//!
//! Captures frames from a test pattern (or camera/screen), encodes, pipes
//! through an in-memory channel, decodes, and renders — no network required.
//! Multiple tiles can run simultaneously to compare encoder/decoder/render
//! combinations side by side.
//!
//! ```sh
//! cargo run -p moq-media --example viewer --features h264
//! cargo run -p moq-media --example viewer --features "h264,wgpu"
//! cargo run -p moq-media --example viewer --features "h264,vaapi,wgpu"
//! ```

use std::{
    collections::VecDeque,
    sync::mpsc,
    time::{Duration, Instant},
};

use eframe::egui;
#[cfg(feature = "capture-camera")]
use moq_media::capture::CameraCapturer;
#[cfg(feature = "capture-screen")]
use moq_media::capture::{MonitorInfo, ScreenCapturer, WindowInfo};
use moq_media::{
    codec::{DynamicVideoDecoder, VideoCodec},
    format::{
        DecodeConfig, DecoderBackend, PixelFormat, VideoEncoderConfig, VideoFormat, VideoFrame,
        VideoPreset,
    },
    pipeline::{VideoDecoderPipeline, VideoEncoderPipeline},
    subscribe::VideoTrack,
    traits::{VideoEncoder, VideoSource},
    transport::media_pipe,
};
#[cfg(feature = "wgpu")]
use moq_media_egui::create_egui_wgpu_config;
use moq_media_egui::{
    FrameView, format_bitrate,
    overlay::{DebugOverlay, StatCategory},
};
use strum::VariantArray;
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Source selection — dynamically discovered from compiled backends
// ---------------------------------------------------------------------------

#[cfg(any(feature = "capture-camera", feature = "capture-screen"))]
use moq_media::capture::CaptureBackend;

/// A video source option discovered at runtime from available backends.
#[derive(Debug, Clone)]
enum DiscoveredSource {
    TestPattern,
    #[cfg(feature = "capture-camera")]
    Camera {
        backend: CaptureBackend,
        name: String,
    },
    #[cfg(feature = "capture-screen")]
    Screen {
        backend: CaptureBackend,
        monitor: MonitorInfo,
    },
    #[cfg(feature = "capture-screen")]
    Window {
        window: WindowInfo,
    },
}

impl PartialEq for DiscoveredSource {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::TestPattern, Self::TestPattern) => true,
            #[cfg(feature = "capture-camera")]
            (
                Self::Camera {
                    backend: a,
                    name: na,
                },
                Self::Camera {
                    backend: b,
                    name: nb,
                },
            ) => a == b && na == nb,
            #[cfg(feature = "capture-screen")]
            (
                Self::Screen {
                    backend: a,
                    monitor: ma,
                },
                Self::Screen {
                    backend: b,
                    monitor: mb,
                },
            ) => a == b && ma.id == mb.id,
            #[cfg(feature = "capture-screen")]
            (Self::Window { window: a }, Self::Window { window: b }) => a.id == b.id,
            _ => false,
        }
    }
}

impl std::fmt::Display for DiscoveredSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TestPattern => write!(f, "Test Pattern"),
            #[cfg(feature = "capture-camera")]
            Self::Camera { backend, name } => write!(f, "{name} ({backend})"),
            #[cfg(feature = "capture-screen")]
            Self::Screen { monitor, .. } => {
                let [w, h] = monitor.dimensions;
                if monitor.is_primary {
                    write!(f, "{} (primary, {}x{})", monitor.name, w, h)
                } else {
                    write!(f, "{} ({}x{})", monitor.name, w, h)
                }
            }
            #[cfg(feature = "capture-screen")]
            Self::Window { window } => {
                let [w, h] = window.dimensions;
                let display = window
                    .display_id
                    .map(|id| format!(" @{id}"))
                    .unwrap_or_default();
                if window.app_name.is_empty() || window.app_name == window.title {
                    write!(f, "{}{display} ({}x{})", window.title, w, h)
                } else {
                    write!(
                        f,
                        "{}: {}{display} ({}x{})",
                        window.app_name, window.title, w, h
                    )
                }
            }
        }
    }
}

impl DiscoveredSource {
    fn create(&self, preset: VideoPreset) -> anyhow::Result<Box<dyn VideoSource>> {
        let (w, h) = preset.dimensions();
        match self {
            Self::TestPattern => Ok(Box::new(TestPatternSource::new(w, h))),
            #[cfg(feature = "capture-camera")]
            Self::Camera { backend, .. } => {
                let config = moq_media::capture::CameraConfig::default();
                Ok(Box::new(CameraCapturer::with_backend(*backend, &config)?))
            }
            #[cfg(feature = "capture-screen")]
            Self::Screen { monitor, .. } => Ok(Box::new(ScreenCapturer::with_monitor(monitor)?)),
            #[cfg(feature = "capture-screen")]
            Self::Window { window } => {
                let config = moq_media::capture::ScreenConfig::default();
                Ok(Box::new(ScreenCapturer::with_window(window.id, &config)?))
            }
        }
    }
}

/// Discovers all available video sources from compiled capture backends.
fn discover_sources() -> Vec<DiscoveredSource> {
    let mut sources = vec![DiscoveredSource::TestPattern];

    #[cfg(feature = "capture-screen")]
    if let Ok(monitors) = ScreenCapturer::list() {
        for monitor in monitors {
            let backend = monitor.backend;
            sources.push(DiscoveredSource::Screen { backend, monitor });
        }
    }

    #[cfg(feature = "capture-screen")]
    if let Ok(windows) = ScreenCapturer::list_windows() {
        for window in windows {
            sources.push(DiscoveredSource::Window { window });
        }
    }

    #[cfg(feature = "capture-camera")]
    {
        // Group cameras by backend so we can label them.
        if let Ok(cameras) = CameraCapturer::list() {
            for cam in cameras {
                sources.push(DiscoveredSource::Camera {
                    backend: cam.backend,
                    name: cam.name,
                });
            }
        }
        // Add backends that have no enumerable cameras but can open a default.
        let listed_backends: Vec<_> = sources
            .iter()
            .filter_map(|s| match s {
                DiscoveredSource::Camera { backend, .. } => Some(*backend),
                _ => None,
            })
            .collect();
        for backend in CameraCapturer::list_backends() {
            if !listed_backends.contains(&backend) {
                sources.push(DiscoveredSource::Camera {
                    backend,
                    name: "Camera".into(),
                });
            }
        }
    }

    sources
}

// ---------------------------------------------------------------------------
// Render mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum RenderMode {
    Software,
    #[cfg(feature = "wgpu")]
    #[strum(serialize = "wgpu")]
    Wgpu,
}

// ---------------------------------------------------------------------------
// Pipeline mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum PipelineMode {
    /// Raw capture → display (no encoding/decoding).
    Direct,
    /// Capture → encode → decode → display.
    #[strum(serialize = "Encode/Decode")]
    EncodeDecode,
}

// ---------------------------------------------------------------------------
// Direct capture (no encode/decode)
// ---------------------------------------------------------------------------

/// Captures frames directly from a source (no encode/decode).
///
/// The capture thread exits when this struct is dropped because the
/// `sync_channel` receiver is dropped, causing the sender to fail.
struct DirectCapture {
    rx: mpsc::Receiver<VideoFrame>,
    current: Option<VideoFrame>,
    #[allow(dead_code, reason = "kept for diagnostics")]
    source_name: String,
    _thread: std::thread::JoinHandle<()>,
}

impl DirectCapture {
    fn new(mut source: Box<dyn VideoSource>) -> anyhow::Result<Self> {
        let source_name = source.name().to_string();
        let (tx, rx) = mpsc::sync_channel(2);
        source.start()?;
        let thread = std::thread::Builder::new()
            .name(format!("direct-{source_name}"))
            .spawn(move || {
                loop {
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            if tx.send(frame).is_err() {
                                break;
                            }
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                        Err(e) => {
                            tracing::warn!("direct capture error: {e:#}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            rx,
            current: None,
            source_name,
            _thread: thread,
        })
    }

    fn current_frame(&mut self) -> Option<&VideoFrame> {
        while let Ok(frame) = self.rx.try_recv() {
            self.current = Some(frame);
        }
        self.current.as_ref()
    }
}

// ---------------------------------------------------------------------------
// SMPTE test pattern source
// ---------------------------------------------------------------------------

const SMPTE_BARS: [[u8; 3]; 7] = [
    [255, 255, 255], // White
    [255, 255, 0],   // Yellow
    [0, 255, 255],   // Cyan
    [0, 255, 0],     // Green
    [255, 0, 255],   // Magenta
    [255, 0, 0],     // Red
    [0, 0, 255],     // Blue
];

const BALL_RADIUS: u32 = 15;
const BALL_BORDER: u32 = 3;

struct TestPatternSource {
    format: VideoFormat,
    frame_index: u64,
    started: bool,
    buffer: Vec<u8>,
    background: Vec<u8>,
}

impl TestPatternSource {
    fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        let background = Self::render_background(width, height);
        Self {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            frame_index: 0,
            started: false,
            buffer: vec![0u8; size],
            background,
        }
    }

    fn render_background(w: u32, h: u32) -> Vec<u8> {
        let mut data = vec![0u8; (w * h * 4) as usize];
        let bar_end = h * 70 / 100;
        let ramp_end = h * 85 / 100;

        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let (r, g, b) = if y < bar_end {
                    let bar_idx = (x * 7 / w) as usize;
                    let bar_idx = bar_idx.min(6);
                    let c = SMPTE_BARS[bar_idx];
                    (c[0], c[1], c[2])
                } else if y < ramp_end {
                    let v = (x * 255 / w.max(1)) as u8;
                    (v, v, v)
                } else {
                    (0, 0, 0)
                };
                data[idx] = r;
                data[idx + 1] = g;
                data[idx + 2] = b;
                data[idx + 3] = 255;
            }
        }
        data
    }

    fn stamp_ball(buf: &mut [u8], w: u32, h: u32, frame_index: u64) {
        let radius = BALL_RADIUS.min(w / 4).min(h / 4);
        if radius == 0 {
            return;
        }
        let outer = radius + BALL_BORDER;

        let range = w.saturating_sub(2 * outer).max(1);
        let period = 2 * range as u64;
        let pos_in_period = frame_index % period.max(1);
        let ball_x = if pos_in_period < range as u64 {
            outer + pos_in_period as u32
        } else {
            outer + (period - pos_in_period) as u32
        };
        let ball_y = h / 2;

        let outer_r2 = (outer * outer) as i64;
        let inner_r2 = (radius * radius) as i64;
        let y_min = ball_y.saturating_sub(outer);
        let y_max = (ball_y + outer).min(h);
        let x_min = ball_x.saturating_sub(outer);
        let x_max = (ball_x + outer).min(w);

        for y in y_min..y_max {
            let dy = y as i64 - ball_y as i64;
            for x in x_min..x_max {
                let dx = x as i64 - ball_x as i64;
                let d2 = dx * dx + dy * dy;
                if d2 <= outer_r2 {
                    let idx = ((y * w + x) * 4) as usize;
                    if d2 <= inner_r2 {
                        buf[idx] = 255;
                        buf[idx + 1] = 255;
                        buf[idx + 2] = 255;
                    } else {
                        buf[idx] = 0;
                        buf[idx + 1] = 0;
                        buf[idx + 2] = 0;
                    }
                    buf[idx + 3] = 255;
                }
            }
        }
    }
}

impl VideoSource for TestPatternSource {
    fn name(&self) -> &str {
        "test-pattern"
    }
    fn format(&self) -> VideoFormat {
        self.format.clone()
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.started = true;
        self.frame_index = 0;
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.started = false;
        Ok(())
    }
    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        if !self.started {
            return Ok(None);
        }
        let [w, h] = self.format.dimensions;
        self.buffer.copy_from_slice(&self.background);
        Self::stamp_ball(&mut self.buffer, w, h, self.frame_index * 4);
        self.frame_index += 1;

        Ok(Some(VideoFrame::new_rgba(
            bytes::Bytes::copy_from_slice(&self.buffer),
            w,
            h,
            std::time::Duration::ZERO,
        )))
    }
}

// ---------------------------------------------------------------------------
// Pipeline settings
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
struct PipelineSettings {
    source: DiscoveredSource,
    codec: VideoCodec,
    preset: VideoPreset,
    backend: DecoderBackend,
    render_mode: RenderMode,
    pipeline_mode: PipelineMode,
}

// ---------------------------------------------------------------------------
// Running pipeline
// ---------------------------------------------------------------------------

enum TilePipeline {
    EncodeDecode {
        _encoder: VideoEncoderPipeline,
        video_track: VideoTrack,
    },
    Direct(DirectCapture),
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    fps: f32,
    fps_samples: VecDeque<Instant>,
    delay_ms: f32,
    /// Baseline for delay measurement: (wall_time, pts) of the first frame seen.
    /// Delay is computed as drift from this baseline:
    ///   delay = (now - base_wall) - (frame.pts - base_pts)
    /// This measures how much the pipeline has fallen behind the expected cadence,
    /// independent of encoder frame_count / framerate drift vs wall clock.
    baseline: Option<(Instant, Duration)>,
    width: u32,
    height: u32,
}

impl Stats {
    fn update(&mut self, frame: &VideoFrame) {
        let now = Instant::now();
        self.fps_samples.push_back(now);
        while self
            .fps_samples
            .front()
            .is_some_and(|t| now - *t > Duration::from_secs(1))
        {
            self.fps_samples.pop_front();
        }
        self.fps = self.fps_samples.len() as f32;

        let (base_wall, base_pts) = *self.baseline.get_or_insert((now, frame.timestamp));
        let wall_delta = now.duration_since(base_wall);
        let pts_delta = frame.timestamp.saturating_sub(base_pts);
        self.delay_ms = wall_delta.saturating_sub(pts_delta).as_secs_f32() * 1000.0;

        let (w, h) = (frame.width(), frame.height());
        self.width = w;
        self.height = h;
    }
}

// ---------------------------------------------------------------------------
// Video view (software + wgpu rendering)
// ---------------------------------------------------------------------------

fn new_frame_view(
    ctx: &egui::Context,
    id: u64,
    #[allow(unused_variables, reason = "used only with wgpu feature")] render_mode: RenderMode,
    #[cfg(feature = "wgpu")] wgpu_render_state: Option<&egui_wgpu::RenderState>,
) -> FrameView {
    let name = format!("video-{id}");
    #[cfg(feature = "wgpu")]
    if render_mode == RenderMode::Wgpu {
        return FrameView::new_wgpu(ctx, &name, wgpu_render_state);
    }
    FrameView::new(ctx, &name)
}

// ---------------------------------------------------------------------------
// Tile
// ---------------------------------------------------------------------------

struct Tile {
    id: u64,
    settings: PipelineSettings,
    pipeline: Option<TilePipeline>,
    video_view: FrameView,
    stats: Stats,
    metrics: moq_media::stats::MetricsCollector,
    overlay: DebugOverlay,
    encoder_name: String,
    decoder_name: String,
    encoder_bitrate: Option<u64>,
    error_msg: Option<String>,
}

impl Tile {
    fn new(
        id: u64,
        settings: PipelineSettings,
        ctx: &egui::Context,
        rt: &Runtime,
        #[cfg(feature = "wgpu")] wgpu_render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        let video_view = new_frame_view(
            ctx,
            id,
            settings.render_mode,
            #[cfg(feature = "wgpu")]
            wgpu_render_state,
        );

        let metrics = moq_media::stats::MetricsCollector::new();
        metrics.register_defaults();
        let mut tile = Self {
            id,
            settings,
            pipeline: None,
            video_view,
            stats: Stats::default(),
            metrics,
            overlay: DebugOverlay::new(&[
                StatCategory::Capture,
                StatCategory::Render,
                StatCategory::Time,
            ]),
            encoder_name: String::new(),
            decoder_name: String::new(),
            encoder_bitrate: None,
            error_msg: None,
        };
        tile.start_pipeline(rt);
        tile
    }

    fn start_pipeline(&mut self, rt: &Runtime) {
        let _guard = rt.enter();

        let source = match self.settings.source.create(self.settings.preset) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(format!("Source: {e:#}"));
                return;
            }
        };

        match self.settings.pipeline_mode {
            PipelineMode::Direct => {
                self.encoder_name = "none".to_string();
                self.decoder_name = "none".to_string();
                match DirectCapture::new(source) {
                    Ok(dc) => {
                        self.pipeline = Some(TilePipeline::Direct(dc));
                    }
                    Err(e) => {
                        self.error_msg = Some(format!("Direct: {e:#}"));
                        return;
                    }
                }
            }
            PipelineMode::EncodeDecode => {
                let enc_config = VideoEncoderConfig::from_preset(self.settings.preset)
                    .resolve_for_source(
                        source.format().dimensions[0],
                        source.format().dimensions[1],
                    );
                let encoder = match self.settings.codec.create_encoder(enc_config) {
                    Ok(e) => e,
                    Err(e) => {
                        self.error_msg = Some(format!("Encoder: {e:#}"));
                        return;
                    }
                };

                self.encoder_name = encoder.name().to_string();
                let config = encoder.config();
                self.encoder_bitrate = config.bitrate;
                let (sink, pipe_source) = media_pipe(32);
                let enc = VideoEncoderPipeline::with_metrics(
                    source,
                    encoder,
                    sink,
                    Some(self.metrics.clone()),
                );

                let decode_config = DecodeConfig {
                    backend: self.settings.backend,
                    ..Default::default()
                };
                let dec = match VideoDecoderPipeline::with_clock_and_metrics::<DynamicVideoDecoder>(
                    format!("viewer-{}", self.id),
                    pipe_source,
                    &config,
                    &decode_config,
                    None,
                    Some(self.metrics.clone()),
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        self.error_msg = Some(format!("Decoder: {e:#}"));
                        return;
                    }
                };

                self.decoder_name = dec.handle.decoder_name().to_string();
                let video_track = VideoTrack::from_pipeline(dec);
                self.pipeline = Some(TilePipeline::EncodeDecode {
                    _encoder: enc,
                    video_track,
                });
            }
        }

        self.stats = Stats::default();
        self.error_msg = None;
    }

    fn aspect_ratio(&self) -> f32 {
        let (w, h) = self.settings.preset.dimensions();
        w as f32 / h as f32
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct ViewerApp {
    available_sources: Vec<DiscoveredSource>,
    source_idx: usize,
    codec: VideoCodec,
    preset: VideoPreset,
    backend: DecoderBackend,
    render_mode: RenderMode,
    pipeline_mode: PipelineMode,

    tiles: Vec<Tile>,
    next_tile_id: u64,
    pending_close: Option<usize>,
    error_msg: Option<String>,
    rt: Runtime,

    #[cfg(feature = "wgpu")]
    wgpu_render_state: Option<egui_wgpu::RenderState>,
}

impl ViewerApp {
    fn new(
        rt: Runtime,
        #[allow(unused_variables, reason = "used only with wgpu feature")]
        cc: &eframe::CreationContext<'_>,
    ) -> Self {
        let available_sources = discover_sources();
        Self {
            available_sources,
            source_idx: 0,
            codec: VideoCodec::best_available().expect("no video codec available"),
            preset: VideoPreset::P720,
            backend: DecoderBackend::Auto,
            render_mode: *RenderMode::VARIANTS.last().unwrap(),
            pipeline_mode: PipelineMode::EncodeDecode,

            tiles: Vec::new(),
            next_tile_id: 0,
            pending_close: None,
            error_msg: None,
            rt,

            #[cfg(feature = "wgpu")]
            wgpu_render_state: cc.wgpu_render_state.clone(),
        }
    }

    fn selected_source(&self) -> &DiscoveredSource {
        &self.available_sources[self.source_idx]
    }

    fn current_settings(&self) -> PipelineSettings {
        PipelineSettings {
            source: self.selected_source().clone(),
            codec: self.codec,
            preset: self.preset,
            backend: self.backend,
            render_mode: self.render_mode,
            pipeline_mode: self.pipeline_mode,
        }
    }

    fn add_tile(&mut self, settings: PipelineSettings, ctx: &egui::Context) {
        let id = self.next_tile_id;
        self.next_tile_id += 1;
        let tile = Tile::new(
            id,
            settings,
            ctx,
            &self.rt,
            #[cfg(feature = "wgpu")]
            self.wgpu_render_state.as_ref(),
        );
        self.tiles.push(tile);
    }

    fn h264_matrix(&self) -> Vec<PipelineSettings> {
        let mut codecs = Vec::new();
        for c in VideoCodec::available() {
            if c.display_name().contains("H.264") {
                codecs.push(c);
            }
        }
        if codecs.is_empty() {
            return Vec::new();
        }

        let backends = [DecoderBackend::Software, DecoderBackend::Auto];
        let render_modes: Vec<RenderMode> = RenderMode::VARIANTS.to_vec();

        let mut settings = Vec::new();
        for &codec in &codecs {
            for &backend in &backends {
                for &render_mode in &render_modes {
                    settings.push(PipelineSettings {
                        source: self.selected_source().clone(),
                        codec,
                        preset: self.preset,
                        backend,
                        render_mode,
                        pipeline_mode: PipelineMode::EncodeDecode,
                    });
                }
            }
        }
        settings
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process pending close from previous frame
        if let Some(idx) = self.pending_close.take()
            && idx < self.tiles.len()
        {
            self.tiles.remove(idx);
        }

        // --- Controls panel ---
        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("H264 matrix").clicked() {
                    let matrix = self.h264_matrix();
                    if matrix.is_empty() {
                        self.error_msg = Some("No H.264 codecs available".into());
                    } else {
                        self.tiles.clear();
                        let settings_list: Vec<_> = matrix;
                        for s in settings_list {
                            self.add_tile(s, &ctx.clone());
                        }
                    }
                }

                ui.separator();

                ui.label("Source");
                let combo = egui::ComboBox::from_id_salt("source")
                    .selected_text(self.selected_source().to_string())
                    .show_ui(ui, |ui| {
                        for (i, src) in self.available_sources.iter().enumerate() {
                            ui.selectable_value(&mut self.source_idx, i, src.to_string());
                        }
                    });
                // Refresh source list each time the dropdown opens.
                if combo.response.clicked() {
                    let prev = self.selected_source().clone();
                    self.available_sources = discover_sources();
                    // Preserve selection if the previously selected source still exists.
                    self.source_idx = self
                        .available_sources
                        .iter()
                        .position(|s| *s == prev)
                        .unwrap_or(0);
                }

                ui.label("Mode");
                egui::ComboBox::from_id_salt("mode")
                    .selected_text(self.pipeline_mode.to_string())
                    .show_ui(ui, |ui| {
                        for mode in PipelineMode::VARIANTS {
                            ui.selectable_value(&mut self.pipeline_mode, *mode, mode.to_string());
                        }
                    });

                let is_encode_decode = self.pipeline_mode == PipelineMode::EncodeDecode;

                if is_encode_decode {
                    ui.label("Encoder");
                    egui::ComboBox::from_id_salt("encoder")
                        .selected_text(self.codec.display_name())
                        .show_ui(ui, |ui| {
                            for codec in VideoCodec::available() {
                                ui.selectable_value(&mut self.codec, codec, codec.display_name());
                            }
                        });

                    ui.label("Preset");
                    egui::ComboBox::from_id_salt("preset")
                        .selected_text(self.preset.to_string())
                        .show_ui(ui, |ui| {
                            for p in VideoPreset::all() {
                                ui.selectable_value(&mut self.preset, p, p.to_string());
                            }
                        });

                    ui.label("Decoder");
                    let backend_name = match self.backend {
                        DecoderBackend::Auto => "Auto (HW > SW)",
                        DecoderBackend::Software => "Software",
                    };
                    egui::ComboBox::from_id_salt("decoder")
                        .selected_text(backend_name)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.backend,
                                DecoderBackend::Auto,
                                "Auto (HW > SW)",
                            );
                            ui.selectable_value(
                                &mut self.backend,
                                DecoderBackend::Software,
                                "Software",
                            );
                        });
                }

                ui.label("Render");
                egui::ComboBox::from_id_salt("render")
                    .selected_text(self.render_mode.to_string())
                    .show_ui(ui, |ui| {
                        for mode in RenderMode::VARIANTS {
                            ui.selectable_value(&mut self.render_mode, *mode, mode.to_string());
                        }
                    });

                ui.separator();

                if ui.button("Add").clicked() {
                    let settings = self.current_settings();
                    self.add_tile(settings, &ctx.clone());
                }
                if ui.button("Replace last").clicked() {
                    self.tiles.pop();
                    let settings = self.current_settings();
                    self.add_tile(settings, &ctx.clone());
                }

                if let Some(ref msg) = self.error_msg {
                    ui.colored_label(egui::Color32::RED, msg);
                }
            });
        });

        // --- Tile grid ---
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                if self.tiles.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label("Click Add to create a pipeline tile");
                    });
                    return;
                }

                let n = self.tiles.len();
                let cols = (n as f32).sqrt().ceil() as usize;
                let rows = n.div_ceil(cols);
                let aspect = self.tiles[0].aspect_ratio();
                let available = ui.available_size();
                let border = 1.0_f32;

                // Fit tiles into available space preserving video aspect ratio
                let total_border_x = border * (cols as f32 - 1.0).max(0.0);
                let total_border_y = border * (rows as f32 - 1.0).max(0.0);
                let tile_w_by_x = (available.x - total_border_x) / cols as f32;
                let tile_h_by_x = tile_w_by_x / aspect;
                let tile_h_by_y = (available.y - total_border_y) / rows as f32;
                let tile_w_by_y = tile_h_by_y * aspect;

                let (tile_w, tile_h) = if tile_h_by_x * rows as f32 + total_border_y <= available.y
                {
                    (tile_w_by_x, tile_h_by_x)
                } else {
                    (tile_w_by_y, tile_h_by_y)
                };

                let origin = ui.cursor().min;

                // Phase 1: update pipelines (mutable borrow)
                for tile in self.tiles.iter_mut() {
                    match tile.pipeline.as_mut() {
                        Some(TilePipeline::EncodeDecode { video_track, .. }) => {
                            if let Some(frame) = video_track.current_frame() {
                                tile.stats.update(&frame);
                                tile.video_view.render_frame(&frame);
                            }
                            video_track.set_viewport(tile_w as u32, tile_h as u32);
                        }
                        Some(TilePipeline::Direct(dc)) => {
                            if let Some(frame) = dc.current_frame() {
                                tile.stats.update(frame);
                                tile.video_view.render_frame(frame);
                            }
                        }
                        None => {}
                    }
                }

                // Phase 2: allocate close button rects and check clicks
                // (needs &mut ui, so do before painter borrow)
                let mut close_clicked = None;
                for i in 0..self.tiles.len() {
                    let col = i % cols;
                    let row = i / cols;
                    let x = origin.x + col as f32 * (tile_w + border);
                    let y = origin.y + row as f32 * (tile_h + border);
                    let close_rect = egui::Rect::from_min_size(
                        egui::pos2(x + tile_w - 18.0, y + 2.0),
                        egui::vec2(16.0, 16.0),
                    );
                    let resp = ui.allocate_rect(close_rect, egui::Sense::click());
                    if resp.clicked() {
                        close_clicked = Some(i);
                    }
                }

                // Phase 3: paint tiles and overlays
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

                for (i, tile) in self.tiles.iter_mut().enumerate() {
                    let col = i % cols;
                    let row = i / cols;
                    let x = origin.x + col as f32 * (tile_w + border);
                    let y = origin.y + row as f32 * (tile_h + border);
                    let tile_rect =
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(tile_w, tile_h));

                    let painter = ui.painter();

                    // Video fills the tile, preserving aspect ratio (letterbox/pillarbox).
                    if let Some((tex_id, tex_size)) = tile.video_view.texture_info() {
                        // Black background for letterbox bars.
                        painter.rect_filled(tile_rect, 0.0, egui::Color32::BLACK);

                        let video_rect = if tex_size.x > 0.0 && tex_size.y > 0.0 {
                            let frame_aspect = tex_size.x / tex_size.y;
                            let tile_aspect = tile_w / tile_h;
                            if frame_aspect > tile_aspect {
                                // Frame wider than tile → letterbox (bars top/bottom).
                                let h = tile_w / frame_aspect;
                                let y_off = (tile_h - h) / 2.0;
                                egui::Rect::from_min_size(
                                    egui::pos2(tile_rect.min.x, tile_rect.min.y + y_off),
                                    egui::vec2(tile_w, h),
                                )
                            } else {
                                // Frame taller than tile → pillarbox (bars left/right).
                                let w = tile_h * frame_aspect;
                                let x_off = (tile_w - w) / 2.0;
                                egui::Rect::from_min_size(
                                    egui::pos2(tile_rect.min.x + x_off, tile_rect.min.y),
                                    egui::vec2(w, tile_h),
                                )
                            }
                        } else {
                            tile_rect
                        };
                        painter.image(tex_id, video_rect, uv, egui::Color32::WHITE);
                    }

                    // Border
                    painter.rect_stroke(
                        tile_rect,
                        0.0,
                        egui::Stroke::new(border, egui::Color32::DARK_GRAY),
                        egui::StrokeKind::Inside,
                    );

                    // Debug overlay bars at bottom.
                    {
                        use moq_media::stats::*;
                        tile.metrics.set_label(LBL_ENCODER, &tile.encoder_name);
                        tile.metrics.set_label(LBL_DECODER, &tile.decoder_name);
                        tile.metrics
                            .set_label(LBL_RENDERER, tile.video_view.render_path_name());
                        if let Some(bps) = tile.encoder_bitrate {
                            tile.metrics.set_label(
                                LBL_CODEC,
                                format!(
                                    "{} {}",
                                    tile.settings.codec.display_name(),
                                    format_bitrate(bps as f64),
                                ),
                            );
                        } else {
                            tile.metrics
                                .set_label(LBL_CODEC, tile.settings.codec.display_name());
                        }
                        let snap = tile.metrics.snapshot();
                        tile.overlay.show(ui, tile_rect, &snap);
                    }

                    // Close button (X) at top-right
                    let close_rect = egui::Rect::from_min_size(
                        egui::pos2(x + tile_w - 18.0, y + 2.0),
                        egui::vec2(16.0, 16.0),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(close_rect, 2.0, egui::Color32::from_black_alpha(140));
                    painter.text(
                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "X",
                        egui::FontId::monospace(11.0),
                        egui::Color32::WHITE,
                    );
                }

                // Reserve space
                let total_h = rows as f32 * tile_h + total_border_y;
                ui.allocate_space(egui::vec2(available.x, total_h));

                if let Some(idx) = close_clicked {
                    self.pending_close = Some(idx);
                }
            });

        if !self.tiles.is_empty() {
            ctx.request_repaint();
        }
    }
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    // Create the tokio runtime *before* eframe so that capture backends and
    // other async operations find a reactor context during the event loop.
    let rt = Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    let native_options = if cfg!(feature = "wgpu") {
        #[cfg(feature = "wgpu")]
        let wgpu_config = create_egui_wgpu_config();
        #[cfg(not(feature = "wgpu"))]
        let wgpu_config = egui_wgpu::WgpuConfiguration::default();

        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: wgpu_config,
            ..Default::default()
        }
    } else {
        eframe::NativeOptions::default()
    };

    eframe::run_native(
        "moq-media viewer",
        native_options,
        Box::new(move |cc| Ok(Box::new(ViewerApp::new(rt, cc)))),
    )
}
