//! Split-screen publisher/subscriber example.
//!
//! Creates two iroh endpoints connected locally. The left half publishes
//! video (+ optional audio) and the right half subscribes, decodes, and
//! renders. Useful for testing the full encode → transport → decode pipeline
//! without needing two separate processes.
//!
//! `PublishView` and `SubscribeView` are fully independent — they share
//! nothing except the publisher's endpoint address.
//!
//! ```sh
//! cargo run -p iroh-live --example split
//! cargo run -p iroh-live --example split --features "vaapi,wgpu"
//! ```

use std::time::{Duration, Instant};

use eframe::egui;
use iroh::{Endpoint, EndpointAddr, SecretKey, Watcher, protocol::Router};
use iroh_live::media::capture::{CameraCapturer, ScreenCapturer};
use iroh_live::media::traits::{AudioSource, VideoSource};
use iroh_live::{
    ALPN, Live,
    media::{
        audio_backend::AudioBackend,
        codec::{
            AudioCodec, DefaultDecoders, DynamicAudioDecoder, DynamicVideoDecoder, VideoCodec,
        },
        format::{
            AudioFormat, AudioPreset, DecodeConfig, DecoderBackend, PlaybackConfig, VideoPreset,
        },
        playout::PlayoutClock,
        publish::{AudioRenditions, LocalBroadcast, VideoRenditions},
        subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
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
// SMPTE test pattern source
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
                buf[idx] = 255;
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
// Shared helpers
// ---------------------------------------------------------------------------

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

fn fit_to_aspect(available: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let h_by_width = available.x / aspect;
    if h_by_width <= available.y {
        egui::vec2(available.x, h_by_width)
    } else {
        let w_by_height = available.y * aspect;
        egui::vec2(w_by_height, available.y)
    }
}

/// Height of a single overlay bar (text + padding).
const OVERLAY_BAR_H: f32 = 15.0;

/// Paints a translucent overlay bar with monospace text at the given rect.
///
/// Does NOT allocate egui layout space — the bar is painted over existing
/// content (typically the video).
fn overlay_bar(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let font = egui::FontId::monospace(11.0);
    let galley = painter.layout_no_wrap(text.to_string(), font, egui::Color32::WHITE);
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
    painter.galley(
        rect.min + egui::vec2(4.0, 1.0),
        galley,
        egui::Color32::WHITE,
    );
}

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

// ---------------------------------------------------------------------------
// PublishView
// ---------------------------------------------------------------------------

struct PublishView {
    router: Router,
    #[allow(dead_code, reason = "kept alive for protocol handler")]
    live: Live,
    broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,

    video_source: VideoSourceKind,
    audio_source: AudioSourceKind,
    codec: VideoCodec,
    preset: VideoPreset,

    preview: Option<VideoTrackView>,
    stats: FrameStats,
    needs_republish: bool,
    error_msg: Option<String>,
}

impl PublishView {
    async fn new(secret_key: SecretKey, audio_ctx: AudioBackend) -> Result<Self> {
        let endpoint = Endpoint::builder().secret_key(secret_key).bind().await?;
        let live = Live::new(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, live.protocol_handler())
            .spawn();

        let broadcast = LocalBroadcast::new();
        let source = TestPatternSource::new(1280, 720);
        let video = VideoRenditions::new(source, VideoCodec::best_available(), [VideoPreset::P720]);
        broadcast.video().set_renditions(video)?;
        let tone = TestToneSource::new();
        let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
        broadcast.audio().set_renditions(audio)?;

        live.publish(BROADCAST_NAME, &broadcast).await?;
        info!("publishing on {}", endpoint.id().fmt_short());

        Ok(Self {
            router,
            live,
            broadcast,
            audio_ctx,
            video_source: VideoSourceKind::TestPattern,
            audio_source: AudioSourceKind::TestTone,
            codec: VideoCodec::best_available(),
            preset: VideoPreset::P720,
            preview: None,
            stats: FrameStats::default(),
            needs_republish: false,
            error_msg: None,
        })
    }

    fn addr(&self) -> EndpointAddr {
        self.router.endpoint().addr()
    }

    fn republish(&mut self, rt: &tokio::runtime::Runtime) {
        self.needs_republish = false;
        self.error_msg = None;

        let source = match create_video_source(self.video_source, self.preset) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(format!("Video source: {e:#}"));
                return;
            }
        };
        let video = VideoRenditions::new(source, self.codec, [self.preset]);
        if let Err(e) = self.broadcast.video().set_renditions(video) {
            self.error_msg = Some(format!("Set video: {e:#}"));
            return;
        }

        let _guard = rt.enter();
        match self.audio_source {
            AudioSourceKind::TestTone => {
                let tone = TestToneSource::new();
                let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
                if let Err(e) = self.broadcast.audio().set_renditions(audio) {
                    self.error_msg = Some(format!("Set audio: {e:#}"));
                    return;
                }
            }
            AudioSourceKind::Microphone => {
                let mic = match rt.block_on(self.audio_ctx.default_input()) {
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

        self.preview = None;
    }

    fn aspect_ratio(&self) -> f32 {
        let (w, h) = self.preset.dimensions();
        w as f32 / h as f32
    }

    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Controls
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut changed = false;

            ui.label("Source");
            egui::ComboBox::from_id_salt("pub_source")
                .selected_text(self.video_source.to_string())
                .show_ui(ui, |ui| {
                    for kind in VideoSourceKind::VARIANTS {
                        if ui
                            .selectable_value(&mut self.video_source, *kind, kind.to_string())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });

            ui.label("Audio");
            egui::ComboBox::from_id_salt("pub_audio")
                .selected_text(self.audio_source.to_string())
                .show_ui(ui, |ui| {
                    for kind in AudioSourceKind::VARIANTS {
                        if ui
                            .selectable_value(&mut self.audio_source, *kind, kind.to_string())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });

            ui.label("Codec");
            egui::ComboBox::from_id_salt("local_codec")
                .selected_text(self.codec.display_name())
                .show_ui(ui, |ui| {
                    for codec in VideoCodec::available() {
                        if ui
                            .selectable_value(&mut self.codec, codec, codec.display_name())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });

            ui.label("Preset");
            egui::ComboBox::from_id_salt("local_preset")
                .selected_text(self.preset.to_string())
                .show_ui(ui, |ui| {
                    for p in VideoPreset::all() {
                        if ui
                            .selectable_value(&mut self.preset, p, p.to_string())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });

            if changed {
                info!(
                    source = %self.video_source,
                    audio = %self.audio_source,
                    codec = self.codec.display_name(),
                    preset = %self.preset,
                    "UI: publisher settings changed"
                );
                self.needs_republish = true;
            }
            if ui.button("Restart").clicked() {
                info!("UI: restart button clicked");
                self.needs_republish = true;
            }

            if let Some(ref msg) = self.error_msg {
                ui.colored_label(egui::Color32::RED, msg);
            }
        });

        ui.separator();

        // Video preview
        let avail = ui.available_size();
        let aspect = self.aspect_ratio();
        let video_size = fit_to_aspect(avail, aspect);

        if self.preview.is_none()
            && let Some(track) = self.broadcast.preview(DecodeConfig::default())
        {
            self.preview = Some(VideoTrackView::new(ctx, "pub-video", track));
        }

        // Track the video's top-left for overlay positioning.
        let x_pad = (avail.x - video_size.x) / 2.0;
        let y_pad = (avail.y - video_size.y) / 2.0;
        let video_origin = ui.cursor().min + egui::vec2(x_pad.max(0.0), y_pad.max(0.0));

        if let Some(ref mut view) = self.preview {
            let (img, frame_ts) = view.render(ctx, video_size);
            ui.add_space(y_pad.max(0.0));
            ui.horizontal(|ui| {
                ui.add_space(x_pad.max(0.0));
                ui.add_sized(video_size, img);
            });
            ui.add_space(y_pad.max(0.0));
            self.stats.tick(frame_ts);
        } else {
            ui.allocate_space(avail);
            ui.centered_and_justified(|ui| {
                ui.label("No publisher video");
            });
        }

        // Overlay bars painted on top of the video's bottom edge.
        let video_rect = egui::Rect::from_min_size(video_origin, video_size);
        let painter = ui.painter();

        let bar1_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - 2.0 * OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );
        let bar2_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );

        let pub_text = format!(
            "PUB  codec: {}  preset: {}  fps: {:.0}  delay: {:.0}ms",
            self.codec.display_name(),
            self.preset,
            self.stats.fps,
            self.stats.delay_ms,
        );
        overlay_bar(painter, bar1_rect, &pub_text);
        overlay_bar(painter, bar2_rect, "NET  (local)");
    }

    fn shutdown(&self, rt: &tokio::runtime::Runtime) {
        let router = self.router.clone();
        rt.block_on(async move {
            if let Err(e) = router.shutdown().await {
                warn!("shutdown: {e:#}");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// SubscribeView
// ---------------------------------------------------------------------------

struct SubscribeView {
    session: MoqSession,
    broadcast: RemoteBroadcast,
    catalog_watcher: n0_watcher::Direct<iroh_live::media::subscribe::CatalogSnapshot>,

    audio_ctx: AudioBackend,
    pending_video: Option<VideoTrack>,
    video: Option<VideoTrackView>,
    _audio: Option<AudioTrack>,
    backend: DecoderBackend,
    render_mode: RenderMode,

    playout_clock: PlayoutClock,
    net_stats: StatsSmoother,
    stats: FrameStats,

    #[cfg(feature = "wgpu")]
    wgpu_render_state: Option<egui_wgpu::RenderState>,
}

impl SubscribeView {
    async fn new(publisher_addr: EndpointAddr, audio_ctx: &AudioBackend) -> Result<Self> {
        let endpoint = Endpoint::bind().await?;
        let live = Live::new(endpoint);
        let (session, broadcast) = live.subscribe(publisher_addr, BROADCAST_NAME).await?;
        info!("subscriber connected");

        let playout_clock = broadcast.clock().clone();
        let catalog_watcher = broadcast.catalog_watcher();

        let playback_config = PlaybackConfig::default();
        let tracks = broadcast
            .media::<DefaultDecoders>(audio_ctx, playback_config)
            .await?;

        Ok(Self {
            session,
            broadcast: tracks.broadcast,
            catalog_watcher,
            audio_ctx: audio_ctx.clone(),
            pending_video: tracks.video,
            video: None,
            _audio: tracks.audio,
            backend: DecoderBackend::Auto,
            render_mode: *RenderMode::VARIANTS.last().unwrap(),
            playout_clock,
            net_stats: StatsSmoother::new(),
            stats: FrameStats::default(),
            #[cfg(feature = "wgpu")]
            wgpu_render_state: None,
        })
    }

    #[cfg(feature = "wgpu")]
    fn set_wgpu_render_state(&mut self, state: Option<egui_wgpu::RenderState>) {
        self.wgpu_render_state = state;
    }

    fn resubscribe(&mut self, ctx: &egui::Context) {
        self.playout_clock.reset();

        // Resubscribe video.
        let video_renditions: Vec<String> = self
            .broadcast
            .catalog()
            .video_renditions()
            .map(|s| s.to_owned())
            .collect();
        if let Some(name) = video_renditions.first() {
            let decode_config = DecodeConfig {
                backend: self.backend,
                ..Default::default()
            };
            match self
                .broadcast
                .video_rendition::<DynamicVideoDecoder>(&decode_config, name)
            {
                Ok(track) => {
                    info!(rendition = name, "subscriber: resubscribed to video");
                    self.video = Some(self.make_video_view(ctx, track));
                }
                Err(e) => {
                    warn!("video re-subscribe failed: {e:#}");
                    self.video = None;
                }
            }
        } else {
            self.video = None;
        }

        // Resubscribe audio. The old AudioTrack is dropped, stopping its
        // decoder thread, before starting a fresh one from the new catalog.
        let audio_renditions: Vec<String> = self
            .broadcast
            .catalog()
            .audio_renditions()
            .map(|s| s.to_owned())
            .collect();
        if let Some(name) = audio_renditions.first() {
            let handle = tokio::runtime::Handle::current();
            match handle.block_on(
                self.broadcast
                    .audio_rendition::<DynamicAudioDecoder>(name, &self.audio_ctx),
            ) {
                Ok(track) => {
                    info!(rendition = name, "subscriber: resubscribed to audio");
                    self._audio = Some(track);
                }
                Err(e) => {
                    warn!("audio re-subscribe failed: {e:#}");
                    self._audio = None;
                }
            }
        } else {
            self._audio = None;
        }
    }

    fn make_video_view(&self, ctx: &egui::Context, track: VideoTrack) -> VideoTrackView {
        #[cfg(feature = "wgpu")]
        if self.render_mode == RenderMode::Wgpu {
            return VideoTrackView::new_wgpu(
                ctx,
                "remote-video",
                track,
                self.wgpu_render_state.as_ref(),
            );
        }
        VideoTrackView::new(ctx, "remote-video", track)
    }

    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Convert pending video track to view (deferred until we have egui context)
        if let Some(track) = self.pending_video.take() {
            self.video = Some(self.make_video_view(ctx, track));
        }

        // Auto-detect catalog changes from publisher
        if self.catalog_watcher.update() {
            info!("subscriber: catalog changed, resubscribing");
            self.resubscribe(ctx);
        }

        // Controls
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            // Rendition selector
            let selected_rendition = self
                .video
                .as_ref()
                .map(|v| v.track().rendition().to_owned());
            egui::ComboBox::from_id_salt("sub_rendition")
                .selected_text(
                    selected_rendition
                        .clone()
                        .unwrap_or_else(|| "rendition".into()),
                )
                .show_ui(ui, |ui| {
                    for name in self.broadcast.catalog().video_renditions() {
                        if ui
                            .selectable_label(selected_rendition.as_deref() == Some(name), name)
                            .clicked()
                        {
                            info!(rendition = name, "UI: rendition switched");
                            let decode_config = DecodeConfig {
                                backend: self.backend,
                                ..Default::default()
                            };
                            match self
                                .broadcast
                                .video_rendition::<DynamicVideoDecoder>(&decode_config, name)
                            {
                                Ok(track) => {
                                    self.video = Some(self.make_video_view(ctx, track));
                                }
                                Err(e) => {
                                    warn!("rendition switch failed: {e:#}");
                                }
                            }
                        }
                    }
                });

            ui.label("Decoder");
            let backend_name = match self.backend {
                DecoderBackend::Auto => "Auto",
                DecoderBackend::Software => "SW",
            };
            egui::ComboBox::from_id_salt("sub_decoder")
                .selected_text(backend_name)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut self.backend, DecoderBackend::Auto, "Auto")
                        .changed()
                        || ui
                            .selectable_value(
                                &mut self.backend,
                                DecoderBackend::Software,
                                "Software",
                            )
                            .changed()
                    {
                        info!(backend = ?self.backend, "UI: decoder backend changed");
                        self.resubscribe(ctx);
                    }
                });

            ui.label("Render");
            egui::ComboBox::from_id_salt("sub_render")
                .selected_text(self.render_mode.to_string())
                .show_ui(ui, |ui| {
                    for mode in RenderMode::VARIANTS {
                        if ui
                            .selectable_value(&mut self.render_mode, *mode, mode.to_string())
                            .changed()
                        {
                            info!(mode = %self.render_mode, "UI: render mode changed");
                            self.resubscribe(ctx);
                        }
                    }
                });
        });

        ui.separator();

        // Video
        let avail = ui.available_size();
        let aspect = 16.0 / 9.0; // default aspect, actual comes from video
        let video_size = fit_to_aspect(avail, aspect);

        // Track the video's top-left for overlay positioning.
        let x_pad = (avail.x - video_size.x) / 2.0;
        let y_pad = (avail.y - video_size.y) / 2.0;
        let video_origin = ui.cursor().min + egui::vec2(x_pad.max(0.0), y_pad.max(0.0));

        if let Some(ref mut view) = self.video {
            let (img, frame_ts) = view.render(ctx, video_size);
            ui.add_space(y_pad.max(0.0));
            ui.horizontal(|ui| {
                ui.add_space(x_pad.max(0.0));
                ui.add_sized(video_size, img);
            });
            ui.add_space(y_pad.max(0.0));
            self.stats.tick(frame_ts);
        } else {
            ui.allocate_space(avail);
        }

        // Overlay bars painted on top of the video's bottom edge.
        let video_rect = egui::Rect::from_min_size(video_origin, video_size);
        let painter = ui.painter();

        let bar1_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - 2.0 * OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );
        let bar2_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );

        let rendition = self
            .video
            .as_ref()
            .map(|v| v.track().rendition().to_owned())
            .unwrap_or_default();
        let decoder_name = self
            .video
            .as_ref()
            .map(|v| v.track().decoder_name().to_owned())
            .unwrap_or_default();
        let renderer = if self.video.as_ref().is_some_and(|v| v.is_wgpu()) {
            "wgpu"
        } else {
            "cpu"
        };
        let buf = self.playout_clock.buffer();
        let jitter = self.playout_clock.jitter();
        let net = self.net_stats.smoothed(|| {
            let conn = self.session.conn();
            (conn.stats(), conn.paths().get())
        });
        let sub_text = format!(
            "SUB  rend: {}  dec: {}  render: {}  fps: {:.0}  delay: {:.0}ms  buf: {}ms  jitter: {:.1}ms  bitrate: {}",
            rendition,
            decoder_name,
            renderer,
            self.stats.fps,
            self.stats.delay_ms,
            buf.as_millis(),
            jitter.as_secs_f64() * 1000.0,
            format_bitrate(net.down.rate as f64),
        );
        overlay_bar(painter, bar1_rect, &sub_text);

        let conn_text = format!(
            "NET  up: {}  down: {}  rtt: {}ms",
            net.up.rate_str,
            net.down.rate_str,
            net.rtt.as_millis(),
        );
        overlay_bar(painter, bar2_rect, &conn_text);
    }

    fn shutdown(&self) {
        self.broadcast.shutdown();
        self.session.close(0, b"bye");
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct SplitApp {
    rt: tokio::runtime::Runtime,
    publish: PublishView,
    subscribe: SubscribeView,
}

impl eframe::App for SplitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        if self.publish.needs_republish {
            self.publish.republish(&self.rt);
        }

        let panel_width = ctx.input(|i| i.viewport_rect().width()) / 2.0;

        egui::SidePanel::left("pub_panel")
            .exact_width(panel_width)
            .resizable(false)
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                self.publish.ui(ctx, ui);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                self.subscribe.ui(ctx, ui);
            });
    }

    fn on_exit(&mut self) {
        info!("shutting down");
        self.subscribe.shutdown();
        self.publish.shutdown(&self.rt);
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
    let secret_key = SecretKey::generate(&mut rand::rng());

    let publish = rt.block_on(PublishView::new(secret_key, audio_ctx.clone()))?;
    let publisher_addr = publish.addr();
    #[allow(unused_mut, reason = "mut needed when wgpu feature is enabled")]
    let mut subscribe = rt.block_on(SubscribeView::new(publisher_addr, &audio_ctx))?;

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
        Box::new(move |_cc| {
            #[cfg(feature = "wgpu")]
            subscribe.set_wgpu_render_state(_cc.wgpu_render_state.clone());

            // Publish preview and subscribe video are created lazily on
            // first ui() call via pending_video / preview fields, so we
            // don't need the egui context here.

            Ok(Box::new(SplitApp {
                rt,
                publish,
                subscribe,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
