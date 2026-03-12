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
use iroh_live::{
    ALPN, Live,
    media::{
        audio_backend::AudioBackend,
        codec::{AudioCodec, DefaultDecoders, DynamicVideoDecoder, VideoCodec},
        format::{
            AudioFormat, AudioPreset, DecodeConfig, DecoderBackend, PlaybackConfig, VideoPreset,
        },
        publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
        subscribe::{AudioTrack, AvRemoteTrack, SubscribeBroadcast, WatchTrack},
    },
    moq::MoqSession,
    util::StatsSmoother,
};
use moq_media_egui::{EguiVideoRenderer, create_egui_wgpu_config, format_bitrate};
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

impl iroh_live::media::traits::VideoSource for TestPatternSource {
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

impl iroh_live::media::traits::AudioSource for TestToneSource {
    fn cloned_boxed(&self) -> Box<dyn iroh_live::media::traits::AudioSource> {
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
// App state
// ---------------------------------------------------------------------------

struct SplitApp {
    rt: tokio::runtime::Runtime,

    // Publisher state
    pub_video_source: VideoSourceKind,
    pub_audio_source: AudioSourceKind,
    pub_codec: VideoCodec,
    pub_preset: VideoPreset,
    broadcast: PublishBroadcast,
    pub_video_view: Option<PubVideoView>,
    pub_router: Router,
    #[allow(dead_code, reason = "kept alive for protocol handler")]
    pub_live: Live,
    audio_ctx: AudioBackend,

    // Subscriber state
    session: MoqSession,
    sub_broadcast: SubscribeBroadcast,
    sub_video: Option<SubVideoView>,
    _sub_audio: Option<AudioTrack>,
    sub_backend: DecoderBackend,
    stats: StatsSmoother,

    // UI state
    pub_fps: FpsCounter,
    sub_fps: FpsCounter,
    needs_republish: bool,
    error_msg: Option<String>,
}

struct PubVideoView {
    track: WatchTrack,
    texture: egui::TextureHandle,
    size: egui::Vec2,
}

struct SubVideoView {
    track: WatchTrack,
    texture: egui::TextureHandle,
    size: egui::Vec2,
    egui_renderer: Option<EguiVideoRenderer>,
}

#[derive(Default)]
struct FpsCounter {
    count: u64,
    last_update: Option<Instant>,
    fps: f32,
}

impl FpsCounter {
    fn tick(&mut self) {
        self.count += 1;
        let now = Instant::now();
        let last = *self.last_update.get_or_insert(now);
        let elapsed = now.duration_since(last);
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.count as f32 / elapsed.as_secs_f32();
            self.count = 0;
            self.last_update = Some(now);
        }
    }
}

impl SplitApp {
    fn create_video_source(
        kind: VideoSourceKind,
        preset: VideoPreset,
    ) -> anyhow::Result<Box<dyn iroh_live::media::traits::VideoSource>> {
        let (w, h) = preset.dimensions();
        match kind {
            VideoSourceKind::TestPattern => Ok(Box::new(TestPatternSource::new(w, h))),
            VideoSourceKind::Camera => Ok(Box::new(CameraCapturer::new()?)),
            VideoSourceKind::Screen => Ok(Box::new(ScreenCapturer::new()?)),
        }
    }

    fn republish(&mut self) {
        self.needs_republish = false;
        self.error_msg = None;

        // Video
        let source = match Self::create_video_source(self.pub_video_source, self.pub_preset) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(format!("Video source: {e:#}"));
                return;
            }
        };
        let video = VideoRenditions::new(source, self.pub_codec, [self.pub_preset]);
        if let Err(e) = self.broadcast.set_video(Some(video)) {
            self.error_msg = Some(format!("Set video: {e:#}"));
            return;
        }

        // Audio
        let _guard = self.rt.enter();
        match self.pub_audio_source {
            AudioSourceKind::TestTone => {
                let tone = TestToneSource::new();
                let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
                if let Err(e) = self.broadcast.set_audio(Some(audio)) {
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
                if let Err(e) = self.broadcast.set_audio(Some(audio)) {
                    self.error_msg = Some(format!("Set audio: {e:#}"));
                    return;
                }
            }
            AudioSourceKind::None => {
                let _ = self.broadcast.set_audio(None::<AudioRenditions>);
            }
        }

        // Update local preview
        self.pub_video_view = None; // drop old track
        // watch_local needs the video to be set first
        // It returns a WatchTrack that shows raw capture
    }
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

async fn setup(
    audio_ctx: AudioBackend,
) -> Result<(Router, Live, PublishBroadcast, MoqSession, AvRemoteTrack)> {
    // Publisher endpoint
    let pub_endpoint = Endpoint::bind().await?;
    let pub_live = Live::new(pub_endpoint.clone());
    let pub_router = Router::builder(pub_endpoint.clone())
        .accept(ALPN, pub_live.protocol_handler())
        .spawn();

    // Create broadcast with test pattern + test tone
    let mut broadcast = PublishBroadcast::new();
    let source = TestPatternSource::new(640, 360);
    let video = VideoRenditions::new(source, VideoCodec::best_available(), [VideoPreset::P360]);
    broadcast.set_video(Some(video))?;
    let tone = TestToneSource::new();
    let audio = AudioRenditions::new(tone, AudioCodec::Opus, [AudioPreset::Hq]);
    broadcast.set_audio(Some(audio))?;

    pub_live
        .publish(BROADCAST_NAME, broadcast.producer())
        .await?;
    info!("publishing on {}", pub_endpoint.id().fmt_short());

    // Subscriber endpoint
    let pub_addr = pub_endpoint.addr();
    let sub_endpoint = Endpoint::bind().await?;
    let sub_live = Live::new(sub_endpoint.clone());
    let playback_config = PlaybackConfig::default();
    let (session, track) = sub_live
        .watch_and_listen::<DefaultDecoders>(pub_addr, BROADCAST_NAME, &audio_ctx, playback_config)
        .await?;
    info!("subscriber connected");

    Ok((pub_router, pub_live, broadcast, session, track))
}

// ---------------------------------------------------------------------------
// Video views
// ---------------------------------------------------------------------------

impl PubVideoView {
    fn new(ctx: &egui::Context, track: WatchTrack) -> Self {
        let placeholder = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
        let texture = ctx.load_texture("pub-video", placeholder, Default::default());
        Self {
            track,
            texture,
            size: egui::vec2(100.0, 100.0),
        }
    }

    fn render(&mut self, ctx: &egui::Context, available_size: egui::Vec2) -> egui::Image<'_> {
        if available_size != self.size {
            self.size = available_size;
            let ppp = ctx.pixels_per_point();
            let w = (available_size.x * ppp) as u32;
            let h = (available_size.y * ppp) as u32;
            self.track.set_viewport(w, h);
        }
        if let Some(frame) = self.track.current_frame() {
            let (w, h) = (frame.width(), frame.height());
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                frame.img().as_raw(),
            );
            self.texture.set(image, Default::default());
        }
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

impl SubVideoView {
    fn new(ctx: &egui::Context, track: WatchTrack) -> Self {
        let placeholder = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
        let texture = ctx.load_texture("sub-video", placeholder, Default::default());
        Self {
            track,
            texture,
            size: egui::vec2(100.0, 100.0),
            egui_renderer: None,
        }
    }

    fn new_wgpu(cc: &eframe::CreationContext<'_>, track: WatchTrack) -> Self {
        let placeholder = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
        let texture = cc
            .egui_ctx
            .load_texture("sub-video", placeholder, Default::default());
        let egui_renderer = cc.wgpu_render_state.as_ref().map(EguiVideoRenderer::new);
        Self {
            track,
            texture,
            size: egui::vec2(100.0, 100.0),
            egui_renderer,
        }
    }

    fn set_track(&mut self, track: WatchTrack) {
        self.track = track;
    }

    fn render(&mut self, ctx: &egui::Context, available_size: egui::Vec2) -> egui::Image<'_> {
        if available_size != self.size {
            self.size = available_size;
            let ppp = ctx.pixels_per_point();
            let w = (available_size.x * ppp) as u32;
            let h = (available_size.y * ppp) as u32;
            self.track.set_viewport(w, h);
        }

        if let Some(frame) = self.track.current_frame() {
            if let Some(ref mut r) = self.egui_renderer {
                let (id, (w, h)) = r.render(&frame);
                return egui::Image::from_texture(egui::load::SizedTexture::new(
                    id,
                    [w as f32, h as f32],
                ))
                .shrink_to_fit();
            }
            let (w, h) = (frame.width(), frame.height());
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                frame.img().as_raw(),
            );
            self.texture.set(image, Default::default());
        }

        if let Some(ref r) = self.egui_renderer
            && let Some((id, (w, h))) = r.last_texture()
        {
            return egui::Image::from_texture(egui::load::SizedTexture::new(
                id,
                [w as f32, h as f32],
            ))
            .shrink_to_fit();
        }

        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// eframe App impl
// ---------------------------------------------------------------------------

impl eframe::App for SplitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        // Handle deferred republish
        if self.needs_republish {
            self.republish();
        }

        // --- Top controls panel ---
        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                // Publisher controls
                ui.label("PUB:");
                let mut changed = false;

                ui.label("Source");
                egui::ComboBox::from_id_salt("pub_source")
                    .selected_text(self.pub_video_source.to_string())
                    .show_ui(ui, |ui| {
                        for kind in VideoSourceKind::VARIANTS {
                            if ui
                                .selectable_value(
                                    &mut self.pub_video_source,
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
                    .selected_text(self.pub_audio_source.to_string())
                    .show_ui(ui, |ui| {
                        for kind in AudioSourceKind::VARIANTS {
                            if ui
                                .selectable_value(
                                    &mut self.pub_audio_source,
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
                egui::ComboBox::from_id_salt("pub_codec")
                    .selected_text(self.pub_codec.display_name())
                    .show_ui(ui, |ui| {
                        for codec in VideoCodec::available() {
                            if ui
                                .selectable_value(&mut self.pub_codec, codec, codec.display_name())
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });

                ui.label("Preset");
                egui::ComboBox::from_id_salt("pub_preset")
                    .selected_text(self.pub_preset.to_string())
                    .show_ui(ui, |ui| {
                        for p in VideoPreset::all() {
                            if ui
                                .selectable_value(&mut self.pub_preset, p, p.to_string())
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });

                if changed || ui.button("Restart").clicked() {
                    self.needs_republish = true;
                }

                ui.separator();

                // Subscriber controls
                ui.label("SUB:");

                // Rendition selector
                let selected_rendition = self
                    .sub_video
                    .as_ref()
                    .map(|v| v.track.rendition().to_owned());
                egui::ComboBox::from_id_salt("sub_rendition")
                    .selected_text(
                        selected_rendition
                            .clone()
                            .unwrap_or_else(|| "rendition".into()),
                    )
                    .show_ui(ui, |ui| {
                        for name in self.sub_broadcast.catalog().video_renditions() {
                            if ui
                                .selectable_label(selected_rendition.as_deref() == Some(name), name)
                                .clicked()
                            {
                                let decode_config = DecodeConfig {
                                    backend: self.sub_backend,
                                    ..Default::default()
                                };
                                match self
                                    .sub_broadcast
                                    .watch_rendition::<DynamicVideoDecoder>(&decode_config, name)
                                {
                                    Ok(track) => {
                                        if let Some(v) = self.sub_video.as_mut() {
                                            v.set_track(track);
                                        }
                                    }
                                    Err(e) => {
                                        warn!("rendition switch failed: {e:#}");
                                    }
                                }
                            }
                        }
                    });

                ui.label("Decoder");
                let backend_name = match self.sub_backend {
                    DecoderBackend::Auto => "Auto",
                    DecoderBackend::Software => "SW",
                };
                egui::ComboBox::from_id_salt("sub_decoder")
                    .selected_text(backend_name)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.sub_backend, DecoderBackend::Auto, "Auto");
                        ui.selectable_value(
                            &mut self.sub_backend,
                            DecoderBackend::Software,
                            "Software",
                        );
                    });

                if let Some(ref msg) = self.error_msg {
                    ui.colored_label(egui::Color32::RED, msg);
                }
            });
        });

        // --- Central panel: split into left (pub) and right (sub) ---
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                let half_w = avail.x / 2.0;

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                    // Left: publisher preview
                    ui.vertical(|ui| {
                        ui.set_width(half_w);
                        ui.set_height(avail.y);

                        // Initialize pub video if needed
                        if self.pub_video_view.is_none()
                            && let Some(track) = self.broadcast.watch_local(DecodeConfig::default())
                        {
                            self.pub_video_view = Some(PubVideoView::new(ctx, track));
                        }

                        if let Some(ref mut view) = self.pub_video_view {
                            let bar_h = 18.0;
                            let video_h = avail.y - bar_h;
                            let img = view.render(ctx, egui::vec2(half_w, video_h));
                            ui.add_sized([half_w, video_h], img);
                            self.pub_fps.tick();
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label("No publisher video");
                            });
                        }

                        // Publisher status bar
                        let codec_name = self.pub_codec.display_name();
                        let preset = self.pub_preset.to_string();
                        let pub_text = format!(
                            "PUB  codec: {}  preset: {}  fps: {:.0}",
                            codec_name, preset, self.pub_fps.fps,
                        );
                        overlay_bar(ui, &pub_text);
                    });

                    // Divider
                    let painter = ui.painter();
                    let x = ui.cursor().min.x;
                    painter.line_segment(
                        [
                            egui::pos2(x, ui.clip_rect().top()),
                            egui::pos2(x, ui.clip_rect().bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::DARK_GRAY),
                    );

                    // Right: subscriber view
                    ui.vertical(|ui| {
                        ui.set_width(half_w);
                        ui.set_height(avail.y);

                        if let Some(ref mut view) = self.sub_video {
                            let bar_h = 36.0; // two bars
                            let video_h = avail.y - bar_h;
                            let img = view.render(ctx, egui::vec2(half_w, video_h));
                            ui.add_sized([half_w, video_h], img);
                            self.sub_fps.tick();
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label("Waiting for video...");
                            });
                        }

                        // Subscriber decode bar
                        let decoder_name = self
                            .sub_video
                            .as_ref()
                            .map(|v| v.track.decoder_name().to_owned())
                            .unwrap_or_default();
                        let rendition = self
                            .sub_video
                            .as_ref()
                            .map(|v| v.track.rendition().to_owned())
                            .unwrap_or_default();
                        let renderer = if self
                            .sub_video
                            .as_ref()
                            .is_some_and(|v| v.egui_renderer.is_some())
                        {
                            "wgpu"
                        } else {
                            "cpu"
                        };
                        let stats = self.stats.smoothed(|| {
                            let conn = self.session.conn();
                            (conn.stats(), conn.paths().get())
                        });
                        let sub_text = format!(
                            "SUB  dec: {}  rend: {}  render: {}  fps: {:.0}  bitrate: {}",
                            decoder_name,
                            rendition,
                            renderer,
                            self.sub_fps.fps,
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
                });
            });
    }

    fn on_exit(&mut self) {
        info!("shutting down");
        self.sub_broadcast.shutdown();
        self.session.close(0, b"bye");
        let router = self.pub_router.clone();
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
    let (pub_router, pub_live, broadcast, session, track) =
        rt.block_on(setup(audio_ctx.clone()))?;

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
            let sub_video = track.video.map(|video| {
                if use_wgpu {
                    SubVideoView::new_wgpu(cc, video)
                } else {
                    SubVideoView::new(&cc.egui_ctx, video)
                }
            });

            let app = SplitApp {
                rt,
                pub_video_source: VideoSourceKind::TestPattern,
                pub_audio_source: AudioSourceKind::TestTone,
                pub_codec: VideoCodec::best_available(),
                pub_preset: VideoPreset::P360,
                broadcast,
                pub_video_view: None,
                pub_router,
                pub_live,
                audio_ctx,
                session,
                sub_broadcast: track.broadcast,
                sub_video,
                _sub_audio: track.audio,
                sub_backend: DecoderBackend::Auto,
                stats: StatsSmoother::new(),
                pub_fps: FpsCounter::default(),
                sub_fps: FpsCounter::default(),
                needs_republish: false,
                error_msg: None,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
