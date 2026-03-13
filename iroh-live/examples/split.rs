//! Split-screen publisher/subscriber example.
//!
//! Creates two iroh endpoints connected locally. The left half publishes
//! video (+ optional audio) and the right half subscribes, decodes, and
//! renders. Useful for testing the full encode → transport → decode pipeline
//! without needing two separate processes.
//!
//! ```sh
//! cargo run -p iroh-live --example split
//! cargo run -p iroh-live --example split --features "vaapi,wgpu"
//! ```

use std::time::{Duration, Instant};

use eframe::egui;
use iroh::{Endpoint, Watcher, protocol::Router};
use iroh_live::media::capture::{CameraCapturer, ScreenCapturer};
use iroh_live::media::traits::{AudioSource, VideoSource};
use iroh_live::{
    ALPN, Live,
    media::{
        audio_backend::AudioBackend,
        codec::{AudioCodec, DefaultDecoders, DynamicVideoDecoder, VideoCodec},
        format::{
            AudioFormat, AudioPreset, DecodeConfig, DecoderBackend, PlaybackConfig, VideoPreset,
        },
        playout::{PlayoutClock, PlayoutMode},
        publish::{AudioRenditions, LocalBroadcast, VideoRenditions},
        subscribe::{AudioTrack, MediaTracks, RemoteBroadcast, VideoTrack},
    },
    moq::MoqSession,
    util::StatsSmoother,
};
use moq_media_egui::{VideoTrackView, create_egui_wgpu_config, format_bitrate};
use n0_error::{Result, anyerr};
use strum::VariantArray;
use tracing::{info, warn};

mod common;

const BROADCAST_NAME: &str = "split";

// ---------------------------------------------------------------------------
// Source selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum VideoSourceKind {
    #[strum(serialize = "Test Pattern")]
    TestPattern,
    Camera,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum AudioSourceKind {
    #[strum(serialize = "Test Tone")]
    TestTone,
    Microphone,
    None,
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
// SMPTE test pattern source (reused from viewer)
// ---------------------------------------------------------------------------

const SMPTE_BARS: [[u8; 3]; 7] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
];

const BALL_RADIUS: u32 = 15;
const BALL_BORDER: u32 = 3;

struct TestPatternSource {
    format: iroh_live::media::format::VideoFormat,
    frame_index: u64,
    started: bool,
    buffer: Vec<u8>,
    background: Vec<u8>,
    start_time: Instant,
}

impl TestPatternSource {
    fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        let background = Self::render_background(width, height);
        Self {
            format: iroh_live::media::format::VideoFormat {
                pixel_format: iroh_live::media::format::PixelFormat::Rgba,
                dimensions: [width, height],
            },
            frame_index: 0,
            started: false,
            buffer: vec![0u8; size],
            background,
            start_time: Instant::now(),
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

    /// Stamps a flash indicator in the bottom-right corner, synced to the beep cadence.
    fn stamp_beep_indicator(buf: &mut [u8], w: u32, h: u32, beep_active: bool) {
        if !beep_active {
            return;
        }
        let size = 20u32.min(w / 8).min(h / 8);
        let x0 = w.saturating_sub(size + 10);
        let y0 = h.saturating_sub(size + 10);
        for y in y0..h.min(y0 + size) {
            for x in x0..w.min(x0 + size) {
                let idx = ((y * w + x) * 4) as usize;
                buf[idx] = 255; // bright yellow flash
                buf[idx + 1] = 255;
                buf[idx + 2] = 0;
                buf[idx + 3] = 255;
            }
        }
    }
}

impl VideoSource for TestPatternSource {
    fn name(&self) -> &str {
        "test-pattern"
    }
    fn format(&self) -> iroh_live::media::format::VideoFormat {
        self.format.clone()
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.started = true;
        self.frame_index = 0;
        self.start_time = Instant::now();
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.started = false;
        Ok(())
    }
    fn pop_frame(&mut self) -> anyhow::Result<Option<iroh_live::media::format::VideoFrame>> {
        if !self.started {
            return Ok(None);
        }
        let [w, h] = self.format.dimensions;
        self.buffer.copy_from_slice(&self.background);
        Self::stamp_ball(&mut self.buffer, w, h, self.frame_index * 4);

        // Flash indicator synced to beep cadence (first 100ms of each second)
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let beep_active = (elapsed % 1.0) < 0.1;
        Self::stamp_beep_indicator(&mut self.buffer, w, h, beep_active);

        self.frame_index += 1;

        Ok(Some(iroh_live::media::format::VideoFrame::new_rgba(
            bytes::Bytes::copy_from_slice(&self.buffer),
            w,
            h,
            Duration::ZERO,
        )))
    }
}

// ---------------------------------------------------------------------------
// Test tone audio source (880 Hz beep, 100ms every second)
// ---------------------------------------------------------------------------

struct TestToneSource {
    format: AudioFormat,
    phase: f64,
    sample_index: u64,
}

impl TestToneSource {
    fn new() -> Self {
        Self {
            format: AudioFormat::mono_48k(),
            phase: 0.0,
            sample_index: 0,
        }
    }
}

impl AudioSource for TestToneSource {
    fn cloned_boxed(&self) -> Box<dyn AudioSource> {
        Box::new(Self {
            format: self.format,
            phase: 0.0,
            sample_index: 0,
        })
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> anyhow::Result<Option<usize>> {
        let sample_rate = self.format.sample_rate as f64;
        let channels = self.format.channel_count as usize;
        let frames = buf.len() / channels;

        for i in 0..frames {
            let t = self.sample_index as f64 / sample_rate;
            let in_beep = (t % 1.0) < 0.1;
            let sample = if in_beep {
                (self.phase * std::f64::consts::TAU).sin() as f32 * 0.3
            } else {
                0.0
            };
            if in_beep {
                self.phase += 880.0 / sample_rate;
            } else {
                self.phase = 0.0;
            }
            for ch in 0..channels {
                buf[i * channels + ch] = sample;
            }
            self.sample_index += 1;
        }
        Ok(Some(frames))
    }
}

// ---------------------------------------------------------------------------
// Playout mode UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum PlayoutModeKind {
    Live,
    Reliable,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct SplitApp {
    rt: tokio::runtime::Runtime,

    // Local (publisher) state
    local_video_source: VideoSourceKind,
    local_audio_source: AudioSourceKind,
    local_codec: VideoCodec,
    local_preset: VideoPreset,
    broadcast: LocalBroadcast,
    local_video_view: Option<VideoTrackView>,
    router: Router,
    #[allow(dead_code, reason = "kept alive for protocol handler")]
    live: Live,
    audio_ctx: AudioBackend,

    // Remote (subscriber) state
    session: MoqSession,
    remote_broadcast: RemoteBroadcast,
    remote_video: Option<VideoTrackView>,
    _remote_audio: Option<AudioTrack>,
    remote_backend: DecoderBackend,
    remote_render_mode: RenderMode,
    stats: StatsSmoother,

    // Playout state
    playout_clock: PlayoutClock,
    playout_mode_kind: PlayoutModeKind,
    /// Live: display buffer offset in ms
    live_buffer_ms: f32,
    /// Live: hang group-skip ceiling in ms
    live_max_latency_ms: f32,

    // UI state
    local_stats: FrameStats,
    remote_stats: FrameStats,
    needs_republish: bool,
    error_msg: Option<String>,

    #[cfg(feature = "wgpu")]
    wgpu_render_state: Option<egui_wgpu::RenderState>,
}

#[derive(Default)]
struct FrameStats {
    count: u64,
    last_update: Option<Instant>,
    fps: f32,
    delay_ms: f32,
    baseline: Option<(Instant, Duration)>,
}

impl FrameStats {
    fn tick(&mut self, frame_ts: Option<Duration>) {
        self.count += 1;
        let now = Instant::now();
        let last = *self.last_update.get_or_insert(now);
        let elapsed = now.duration_since(last);
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.count as f32 / elapsed.as_secs_f32();
            self.count = 0;
            self.last_update = Some(now);
        }
        if let Some(ts) = frame_ts {
            let (base_wall, base_pts) = *self.baseline.get_or_insert((now, ts));
            let wall_delta = now.duration_since(base_wall);
            let pts_delta = ts.saturating_sub(base_pts);
            self.delay_ms = wall_delta.saturating_sub(pts_delta).as_secs_f32() * 1000.0;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fits a rectangle of the given aspect ratio into `available`, preserving aspect.
fn fit_to_aspect(available: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let h_by_width = available.x / aspect;
    if h_by_width <= available.y {
        egui::vec2(available.x, h_by_width)
    } else {
        let w_by_height = available.y * aspect;
        egui::vec2(w_by_height, available.y)
    }
}

fn overlay_bar(ui: &mut egui::Ui, text: &str) {
    let font = egui::FontId::monospace(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE);
    let bar_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), galley.size().y + 4.0),
    );
    ui.painter()
        .rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(180));
    ui.painter().galley(
        bar_rect.min + egui::vec2(4.0, 2.0),
        galley,
        egui::Color32::WHITE,
    );
    ui.allocate_space(bar_rect.size());
}

impl SplitApp {
    fn create_video_source(
        kind: VideoSourceKind,
        preset: VideoPreset,
    ) -> anyhow::Result<Box<dyn VideoSource>> {
        let (w, h) = preset.dimensions();
        match kind {
            VideoSourceKind::TestPattern => Ok(Box::new(TestPatternSource::new(w, h))),
            VideoSourceKind::Camera => Ok(Box::new(CameraCapturer::new()?)),
            VideoSourceKind::Screen => Ok(Box::new(ScreenCapturer::new()?)),
        }
    }

    fn republish(&mut self, ctx: &egui::Context) {
        self.needs_republish = false;
        self.error_msg = None;

        // Video
        let source = match Self::create_video_source(self.local_video_source, self.local_preset) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(format!("Video source: {e:#}"));
                return;
            }
        };
        let video = VideoRenditions::new(source, self.local_codec, [self.local_preset]);
        if let Err(e) = self.broadcast.video().set_renditions(video) {
            self.error_msg = Some(format!("Set video: {e:#}"));
            return;
        }

        // Audio
        let _guard = self.rt.enter();
        match self.local_audio_source {
            AudioSourceKind::TestTone => {
                let tone = TestToneSource::new();
                let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
                if let Err(e) = self.broadcast.audio().set_renditions(audio) {
                    self.error_msg = Some(format!("Set audio: {e:#}"));
                    return;
                }
            }
            AudioSourceKind::Microphone => {
                let mic = match self.rt.block_on(self.audio_ctx.default_input()) {
                    Ok(m) => m,
                    Err(e) => {
                        self.error_msg = Some(format!("Microphone: {e:#}"));
                        return;
                    }
                };
                let audio = AudioRenditions::new(mic, AudioCodec::Opus, [AudioPreset::Hq]);
                if let Err(e) = self.broadcast.audio().set_renditions(audio) {
                    self.error_msg = Some(format!("Set audio: {e:#}"));
                    return;
                }
            }
            AudioSourceKind::None => {
                self.broadcast.audio().clear();
            }
        }

        // Reset pub preview — will be re-created from preview next frame
        self.local_video_view = None;

        // Re-subscribe on the sub side: the old encoder tracks were torn down,
        // so the subscriber needs a fresh VideoTrack from the new catalog.
        self.resubscribe_video(ctx);
    }

    fn resubscribe_video(&mut self, ctx: &egui::Context) {
        let renditions: Vec<String> = self
            .remote_broadcast
            .catalog()
            .video_renditions()
            .map(|s| s.to_owned())
            .collect();
        if let Some(name) = renditions.first() {
            let decode_config = DecodeConfig {
                backend: self.remote_backend,
                ..Default::default()
            };
            match self
                .remote_broadcast
                .video_rendition::<DynamicVideoDecoder>(&decode_config, name)
            {
                Ok(track) => {
                    self.remote_video = Some(self.make_remote_video_view(ctx, track));
                }
                Err(e) => {
                    warn!("re-subscribe failed: {e:#}");
                }
            }
        }
    }

    fn make_remote_video_view(&self, ctx: &egui::Context, track: VideoTrack) -> VideoTrackView {
        #[cfg(feature = "wgpu")]
        if self.remote_render_mode == RenderMode::Wgpu {
            return VideoTrackView::new_wgpu(
                ctx,
                "remote-video",
                track,
                self.wgpu_render_state.as_ref(),
            );
        }
        VideoTrackView::new(ctx, "remote-video", track)
    }

    fn local_aspect_ratio(&self) -> f32 {
        let (w, h) = self.local_preset.dimensions();
        w as f32 / h as f32
    }
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

async fn setup(
    audio_ctx: AudioBackend,
) -> Result<(Router, Live, LocalBroadcast, MoqSession, MediaTracks)> {
    // Local (publisher) endpoint
    let local_endpoint = Endpoint::bind().await?;
    let live = Live::new(local_endpoint.clone());
    let router = Router::builder(local_endpoint.clone())
        .accept(ALPN, live.protocol_handler())
        .spawn();

    // Create broadcast with test pattern + test tone
    let broadcast = LocalBroadcast::new();
    let source = TestPatternSource::new(1280, 720);
    let video = VideoRenditions::new(source, VideoCodec::best_available(), [VideoPreset::P720]);
    broadcast.video().set_renditions(video)?;
    let tone = TestToneSource::new();
    let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
    broadcast.audio().set_renditions(audio)?;

    live.publish(BROADCAST_NAME, &broadcast).await?;
    info!("publishing on {}", local_endpoint.id().fmt_short());

    // Remote (subscriber) endpoint
    let local_addr = local_endpoint.addr();
    let remote_endpoint = Endpoint::bind().await?;
    let remote_live = Live::new(remote_endpoint.clone());
    let playback_config = PlaybackConfig::default();
    let (session, track) = remote_live
        .media::<DefaultDecoders>(local_addr, BROADCAST_NAME, &audio_ctx, playback_config)
        .await?;
    info!("subscriber connected");

    Ok((router, live, broadcast, session, track))
}

// ---------------------------------------------------------------------------
// eframe App impl
// ---------------------------------------------------------------------------

impl eframe::App for SplitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        // Handle deferred republish
        if self.needs_republish {
            self.republish(ctx);
        }

        let panel_width = ctx.input(|i| i.viewport_rect().width()) / 2.0;

        // =====================================================================
        // LEFT HALF: Publisher
        // =====================================================================
        egui::SidePanel::left("pub_panel")
            .exact_width(panel_width)
            .resizable(false)
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                // --- Local controls ---
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let mut changed = false;

                    ui.label("Source");
                    egui::ComboBox::from_id_salt("pub_source")
                        .selected_text(self.local_video_source.to_string())
                        .show_ui(ui, |ui| {
                            for kind in VideoSourceKind::VARIANTS {
                                if ui
                                    .selectable_value(
                                        &mut self.local_video_source,
                                        *kind,
                                        kind.to_string(),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        });

                    ui.label("Audio");
                    egui::ComboBox::from_id_salt("pub_audio")
                        .selected_text(self.local_audio_source.to_string())
                        .show_ui(ui, |ui| {
                            for kind in AudioSourceKind::VARIANTS {
                                if ui
                                    .selectable_value(
                                        &mut self.local_audio_source,
                                        *kind,
                                        kind.to_string(),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        });

                    ui.label("Codec");
                    egui::ComboBox::from_id_salt("local_codec")
                        .selected_text(self.local_codec.display_name())
                        .show_ui(ui, |ui| {
                            for codec in VideoCodec::available() {
                                if ui
                                    .selectable_value(
                                        &mut self.local_codec,
                                        codec,
                                        codec.display_name(),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        });

                    ui.label("Preset");
                    egui::ComboBox::from_id_salt("local_preset")
                        .selected_text(self.local_preset.to_string())
                        .show_ui(ui, |ui| {
                            for p in VideoPreset::all() {
                                if ui
                                    .selectable_value(&mut self.local_preset, p, p.to_string())
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        });

                    if changed || ui.button("Restart").clicked() {
                        self.needs_republish = true;
                    }

                    if let Some(ref msg) = self.error_msg {
                        ui.colored_label(egui::Color32::RED, msg);
                    }
                });

                ui.separator();

                // --- Local video preview ---
                let avail = ui.available_size();
                let bar_h = 36.0; // two status bars
                let video_avail = egui::vec2(avail.x, (avail.y - bar_h).max(1.0));
                let aspect = self.local_aspect_ratio();
                let video_size = fit_to_aspect(video_avail, aspect);

                // Initialize pub video if needed
                if self.local_video_view.is_none()
                    && let Some(track) = self.broadcast.preview(DecodeConfig::default())
                {
                    self.local_video_view = Some(VideoTrackView::new(ctx, "pub-video", track));
                }

                if let Some(ref mut view) = self.local_video_view {
                    let (img, frame_ts) = view.render(ctx, video_size);
                    // Center the video in available space
                    let x_pad = (video_avail.x - video_size.x) / 2.0;
                    let y_pad = (video_avail.y - video_size.y) / 2.0;
                    ui.add_space(y_pad.max(0.0));
                    ui.horizontal(|ui| {
                        ui.add_space(x_pad.max(0.0));
                        ui.add_sized(video_size, img);
                    });
                    ui.add_space(y_pad.max(0.0));
                    self.local_stats.tick(frame_ts);
                } else {
                    ui.allocate_space(video_avail);
                    ui.centered_and_justified(|ui| {
                        ui.label("No publisher video");
                    });
                }

                // Publisher status bar
                let enc_bitrate = self
                    .remote_broadcast
                    .catalog()
                    .video
                    .renditions
                    .values()
                    .next()
                    .and_then(|c| c.bitrate)
                    .map(|b| format_bitrate(b as f64))
                    .unwrap_or_default();
                let pub_text = format!(
                    "PUB  codec: {}  preset: {}  fps: {:.0}  delay: {:.0}ms  bitrate: {}",
                    self.local_codec.display_name(),
                    self.local_preset,
                    self.local_stats.fps,
                    self.local_stats.delay_ms,
                    enc_bitrate,
                );
                overlay_bar(ui, &pub_text);

                // Placeholder NET bar (no pub-side connection stats yet)
                overlay_bar(ui, "NET  (local)");
            });

        // =====================================================================
        // RIGHT HALF: Subscriber
        // =====================================================================
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                // --- Remote controls ---
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    // Rendition selector
                    let selected_rendition = self
                        .remote_video
                        .as_ref()
                        .map(|v| v.track().rendition().to_owned());
                    egui::ComboBox::from_id_salt("sub_rendition")
                        .selected_text(
                            selected_rendition
                                .clone()
                                .unwrap_or_else(|| "rendition".into()),
                        )
                        .show_ui(ui, |ui| {
                            for name in self.remote_broadcast.catalog().video_renditions() {
                                if ui
                                    .selectable_label(
                                        selected_rendition.as_deref() == Some(name),
                                        name,
                                    )
                                    .clicked()
                                {
                                    let decode_config = DecodeConfig {
                                        backend: self.remote_backend,
                                        ..Default::default()
                                    };
                                    match self
                                        .remote_broadcast
                                        .video_rendition::<DynamicVideoDecoder>(
                                            &decode_config,
                                            name,
                                        ) {
                                        Ok(track) => {
                                            self.remote_video =
                                                Some(self.make_remote_video_view(ctx, track));
                                        }
                                        Err(e) => {
                                            warn!("rendition switch failed: {e:#}");
                                        }
                                    }
                                }
                            }
                        });

                    ui.label("Decoder");
                    let backend_name = match self.remote_backend {
                        DecoderBackend::Auto => "Auto",
                        DecoderBackend::Software => "SW",
                    };
                    egui::ComboBox::from_id_salt("sub_decoder")
                        .selected_text(backend_name)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(
                                    &mut self.remote_backend,
                                    DecoderBackend::Auto,
                                    "Auto",
                                )
                                .changed()
                                || ui
                                    .selectable_value(
                                        &mut self.remote_backend,
                                        DecoderBackend::Software,
                                        "Software",
                                    )
                                    .changed()
                            {
                                self.resubscribe_video(ctx);
                            }
                        });

                    ui.label("Render");
                    egui::ComboBox::from_id_salt("sub_render")
                        .selected_text(self.remote_render_mode.to_string())
                        .show_ui(ui, |ui| {
                            for mode in RenderMode::VARIANTS {
                                if ui
                                    .selectable_value(
                                        &mut self.remote_render_mode,
                                        *mode,
                                        mode.to_string(),
                                    )
                                    .changed()
                                {
                                    // Recreate sub view with new render mode
                                    self.resubscribe_video(ctx);
                                }
                            }
                        });
                });

                // --- Playout mode controls ---
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let mut mode_changed = false;

                    ui.label("Playout");
                    egui::ComboBox::from_id_salt("playout_mode")
                        .selected_text(self.playout_mode_kind.to_string())
                        .show_ui(ui, |ui| {
                            for kind in PlayoutModeKind::VARIANTS {
                                if ui
                                    .selectable_value(
                                        &mut self.playout_mode_kind,
                                        *kind,
                                        kind.to_string(),
                                    )
                                    .changed()
                                {
                                    mode_changed = true;
                                }
                            }
                        });

                    match self.playout_mode_kind {
                        PlayoutModeKind::Live => {
                            ui.label("buffer");
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.live_buffer_ms, 0.0..=200.0)
                                        .suffix("ms")
                                        .integer(),
                                )
                                .changed()
                            {
                                mode_changed = true;
                            }
                            ui.label("max");
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.live_max_latency_ms, 20.0..=500.0)
                                        .suffix("ms")
                                        .integer(),
                                )
                                .changed()
                            {
                                mode_changed = true;
                            }
                        }
                        PlayoutModeKind::Reliable => {
                            ui.label("(no latency controls)");
                        }
                    }

                    if mode_changed {
                        let new_mode = match self.playout_mode_kind {
                            PlayoutModeKind::Live => PlayoutMode::Live {
                                buffer: Duration::from_millis(self.live_buffer_ms as u64),
                                max_latency: Duration::from_millis(self.live_max_latency_ms as u64),
                            },
                            PlayoutModeKind::Reliable => PlayoutMode::Reliable,
                        };
                        self.playout_clock.set_mode(new_mode);
                    }

                    // Show buffer + jitter diagnostics.
                    let buf = self.playout_clock.buffer();
                    let jitter = self.playout_clock.jitter();
                    ui.label(format!(
                        "buf: {}ms  jitter: {:.1}ms",
                        buf.as_millis(),
                        jitter.as_secs_f64() * 1000.0,
                    ));
                });

                ui.separator();

                // --- Remote video ---
                let avail = ui.available_size();
                let bar_h = 36.0; // two status bars
                let video_avail = egui::vec2(avail.x, (avail.y - bar_h).max(1.0));

                // Use preset aspect ratio (same source dimensions)
                let aspect = self.local_aspect_ratio();
                let video_size = fit_to_aspect(video_avail, aspect);

                if let Some(ref mut view) = self.remote_video {
                    let (img, frame_ts) = view.render(ctx, video_size);
                    let x_pad = (video_avail.x - video_size.x) / 2.0;
                    let y_pad = (video_avail.y - video_size.y) / 2.0;
                    ui.add_space(y_pad.max(0.0));
                    ui.horizontal(|ui| {
                        ui.add_space(x_pad.max(0.0));
                        ui.add_sized(video_size, img);
                    });
                    ui.add_space(y_pad.max(0.0));
                    self.remote_stats.tick(frame_ts);
                } else {
                    ui.allocate_space(video_avail);
                }

                // Subscriber decode bar (rend first, then dec)
                let rendition = self
                    .remote_video
                    .as_ref()
                    .map(|v| v.track().rendition().to_owned())
                    .unwrap_or_default();
                let decoder_name = self
                    .remote_video
                    .as_ref()
                    .map(|v| v.track().decoder_name().to_owned())
                    .unwrap_or_default();
                let renderer = if self.remote_video.as_ref().is_some_and(|v| v.is_wgpu()) {
                    "wgpu"
                } else {
                    "cpu"
                };
                let stats = self.stats.smoothed(|| {
                    let conn = self.session.conn();
                    (conn.stats(), conn.paths().get())
                });
                let sub_text = format!(
                    "SUB  rend: {}  dec: {}  render: {}  fps: {:.0}  delay: {:.0}ms  bitrate: {}",
                    rendition,
                    decoder_name,
                    renderer,
                    self.remote_stats.fps,
                    self.remote_stats.delay_ms,
                    format_bitrate(stats.down.rate as f64),
                );
                overlay_bar(ui, &sub_text);

                // Connection stats bar
                let conn_text = format!(
                    "NET  up: {}  down: {}  rtt: {}ms",
                    stats.up.rate_str,
                    stats.down.rate_str,
                    stats.rtt.as_millis(),
                );
                overlay_bar(ui, &conn_text);
            });
    }

    fn on_exit(&mut self) {
        info!("shutting down");
        self.remote_broadcast.shutdown();
        self.session.close(0, b"bye");
        let router = self.router.clone();
        self.rt.block_on(async move {
            if let Err(e) = router.shutdown().await {
                warn!("shutdown: {e:#}");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let audio_ctx = AudioBackend::default();
    let (router, live, broadcast, session, track) = rt.block_on(setup(audio_ctx.clone()))?;

    let _guard = rt.enter();

    let use_wgpu = cfg!(feature = "wgpu");
    let native_options = if use_wgpu {
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: create_egui_wgpu_config(),
            ..Default::default()
        }
    } else {
        eframe::NativeOptions::default()
    };

    eframe::run_native(
        "iroh-live split",
        native_options,
        Box::new(move |cc| {
            let remote_render_mode = *RenderMode::VARIANTS.last().unwrap();

            #[cfg(feature = "wgpu")]
            let wgpu_render_state = cc.wgpu_render_state.clone();

            let playout_clock = track.clock().clone();
            let remote_video = track.video.map(|video| {
                #[cfg(feature = "wgpu")]
                if remote_render_mode == RenderMode::Wgpu {
                    return VideoTrackView::new_wgpu(
                        &cc.egui_ctx,
                        "remote-video",
                        video,
                        cc.wgpu_render_state.as_ref(),
                    );
                }
                VideoTrackView::new(&cc.egui_ctx, "remote-video", video)
            });
            let app = SplitApp {
                rt,
                local_video_source: VideoSourceKind::TestPattern,
                local_audio_source: AudioSourceKind::TestTone,
                local_codec: VideoCodec::best_available(),
                local_preset: VideoPreset::P720,
                broadcast,
                local_video_view: None,
                router,
                live,
                audio_ctx,
                session,
                remote_broadcast: track.broadcast,
                remote_video,
                _remote_audio: track.audio,
                remote_backend: DecoderBackend::Auto,
                remote_render_mode,
                stats: StatsSmoother::new(),
                playout_clock,
                playout_mode_kind: PlayoutModeKind::Live,
                live_buffer_ms: 80.0,
                live_max_latency_ms: 150.0,
                local_stats: FrameStats::default(),
                remote_stats: FrameStats::default(),
                needs_republish: false,
                error_msg: None,
                #[cfg(feature = "wgpu")]
                wgpu_render_state,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
