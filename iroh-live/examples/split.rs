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
//! With `--patchbay` (Linux only), endpoints run in separate patchbay
//! network namespaces with a simulated link. Sliders let you impair the
//! link (rate, latency, loss) at runtime.
//!
//! ```sh
//! cargo run -p iroh-live --example split
//! cargo run -p iroh-live --example split --features "vaapi,wgpu"
//! cargo run -p iroh-live --example split -- --patchbay
//! ```

use std::time::Duration;

use clap::Parser;
use eframe::egui;
use iroh::{Endpoint, EndpointAddr, SecretKey, protocol::Router};
use iroh_live::media::capture::{CameraCapturer, CaptureBackend, ScreenCapturer};
use iroh_live::media::test_sources::{TestPatternSource, TestToneSource};
use iroh_live::media::traits::VideoSource;
use iroh_live::{
    ALPN, Live,
    media::{
        audio_backend::AudioBackend,
        codec::{AudioCodec, DefaultDecoders, DynamicVideoDecoder, VideoCodec},
        format::{AudioPreset, DecodeConfig, DecoderBackend, PlaybackConfig, VideoPreset},
        playout::PlayoutClock,
        publish::{LocalBroadcast, VideoInput},
        subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
    },
    moq::MoqSession,
};
use moq_media_egui::{
    VideoTrackView, create_egui_wgpu_config,
    overlay::{DebugOverlay, StatCategory, fit_to_aspect},
};
use n0_error::{Result, anyerr};
use strum::VariantArray;
use tracing::{info, warn};

mod common;

#[derive(Parser)]
struct Args {
    /// Run in patchbay mode with network impairment sliders (Linux only).
    #[arg(long)]
    patchbay: bool,
}

const BROADCAST_NAME: &str = "split";

// ---------------------------------------------------------------------------
// Source selection
// ---------------------------------------------------------------------------

/// A video source option discovered at runtime from available backends.
#[derive(Debug, Clone, PartialEq)]
enum DiscoveredVideoSource {
    TestPattern,
    Camera {
        backend: CaptureBackend,
        name: String,
    },
    Screen {
        backend: CaptureBackend,
    },
}

impl std::fmt::Display for DiscoveredVideoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TestPattern => write!(f, "Test Pattern"),
            Self::Camera { backend, name } => write!(f, "{name} ({backend})"),
            Self::Screen { backend } => write!(f, "Screen ({backend})"),
        }
    }
}

impl DiscoveredVideoSource {
    fn create(&self, preset: VideoPreset) -> anyhow::Result<Box<dyn VideoSource>> {
        let (w, h) = preset.dimensions();
        match self {
            Self::TestPattern => Ok(Box::new(TestPatternSource::new(w, h))),
            Self::Camera { backend, .. } => {
                let config = iroh_live::media::capture::CameraConfig::default();
                Ok(Box::new(CameraCapturer::with_backend(*backend, &config)?))
            }
            Self::Screen { backend } => {
                let config = iroh_live::media::capture::ScreenConfig::default();
                Ok(Box::new(ScreenCapturer::with_backend(*backend, &config)?))
            }
        }
    }
}

/// Discovers all available video sources across all compiled backends.
fn discover_video_sources() -> Vec<DiscoveredVideoSource> {
    let mut sources = vec![DiscoveredVideoSource::TestPattern];

    for backend in ScreenCapturer::list_backends() {
        sources.push(DiscoveredVideoSource::Screen { backend });
    }

    for cam in CameraCapturer::list().unwrap_or_default() {
        sources.push(DiscoveredVideoSource::Camera {
            backend: cam.backend,
            name: cam.name,
        });
    }

    sources
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
// Patchbay (Linux only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
struct PatchbayState {
    lab: patchbay::Lab,
    pub_node: patchbay::NodeId,
    sub_node: patchbay::NodeId,
    router_node: patchbay::NodeId,
    rate_kbit: u32,
    latency_ms: u32,
    loss_pct: f32,
    dirty: bool,
}

#[cfg(target_os = "linux")]
impl PatchbayState {
    fn new(
        lab: patchbay::Lab,
        pub_node: patchbay::NodeId,
        sub_node: patchbay::NodeId,
        router_node: patchbay::NodeId,
    ) -> Self {
        Self {
            lab,
            pub_node,
            sub_node,
            router_node,
            rate_kbit: 100_000,
            latency_ms: 0,
            loss_pct: 0.0,
            dirty: false,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Link:");
            let mut changed = false;
            changed |= ui
                .add(egui::Slider::new(&mut self.rate_kbit, 100..=100_000).text("kbit/s"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut self.latency_ms, 0..=1000).text("ms lat"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut self.loss_pct, 0.0..=30.0).text("% loss"))
                .changed();
            if changed {
                self.dirty = true;
            }
        });
    }

    /// Applies impairment to both pub↔router and sub↔router links so the
    /// simulated delay, loss, and rate limit affect traffic in both directions.
    fn apply(&mut self, rt: &tokio::runtime::Runtime) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let cond = Some(patchbay::LinkCondition::Manual(patchbay::LinkLimits {
            rate_kbit: self.rate_kbit,
            latency_ms: self.latency_ms,
            jitter_ms: self.latency_ms / 5,
            loss_pct: self.loss_pct,
            ..Default::default()
        }));
        let lab = self.lab.clone();
        let pub_n = self.pub_node;
        let sub_n = self.sub_node;
        let router_n = self.router_node;
        rt.spawn(async move {
            if let Err(e) = lab.set_link_condition(pub_n, router_n, cond).await {
                warn!("pub link condition: {e:#}");
            }
            if let Err(e) = lab.set_link_condition(sub_n, router_n, cond).await {
                warn!("sub link condition: {e:#}");
            }
        });
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

    available_sources: Vec<DiscoveredVideoSource>,
    video_source_idx: usize,
    audio_source: AudioSourceKind,
    codec: VideoCodec,
    preset: VideoPreset,

    preview: Option<VideoTrackView>,
    overlay: DebugOverlay,
    needs_republish: bool,
    error_msg: Option<String>,
}

impl PublishView {
    async fn new(endpoint: Endpoint, audio_ctx: AudioBackend) -> Result<Self> {
        let live = Live::new(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, live.protocol_handler())
            .spawn();

        let broadcast = LocalBroadcast::new();
        broadcast.video().set(VideoInput::new(
            TestPatternSource::new(1280, 720),
            VideoCodec::best_available().expect("no video codec available"),
            [VideoPreset::P720],
        ))?;
        broadcast
            .audio()
            .set(TestToneSource::new(), AudioCodec::Opus, [AudioPreset::Hq])?;

        live.publish(BROADCAST_NAME, &broadcast).await?;
        info!("publishing on {}", endpoint.id().fmt_short());

        Ok(Self {
            router,
            live,
            broadcast,
            audio_ctx,
            available_sources: discover_video_sources(),
            video_source_idx: 0,
            audio_source: AudioSourceKind::TestTone,
            codec: VideoCodec::best_available().expect("no video codec available"),
            preset: VideoPreset::P720,
            preview: None,
            overlay: DebugOverlay::new(&[StatCategory::Capture]),
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

        let source = match self.available_sources[self.video_source_idx].create(self.preset) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(format!("Video source: {e:#}"));
                return;
            }
        };
        if let Err(e) =
            self.broadcast
                .video()
                .set(VideoInput::new(source, self.codec, [self.preset]))
        {
            self.error_msg = Some(format!("Set video: {e:#}"));
            return;
        }

        let _guard = rt.enter();
        match self.audio_source {
            AudioSourceKind::TestTone => {
                if let Err(e) = self.broadcast.audio().set(
                    TestToneSource::new(),
                    AudioCodec::Opus,
                    [AudioPreset::Hq],
                ) {
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
                if let Err(e) = self
                    .broadcast
                    .audio()
                    .set(mic, AudioCodec::Opus, [AudioPreset::Hq])
                {
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
                .selected_text(self.available_sources[self.video_source_idx].to_string())
                .show_ui(ui, |ui| {
                    for (i, src) in self.available_sources.iter().enumerate() {
                        if ui
                            .selectable_value(&mut self.video_source_idx, i, src.to_string())
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
                    source = %self.available_sources[self.video_source_idx],
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
            let (img, _frame_ts) = view.render(ctx, video_size);
            ui.add_space(y_pad.max(0.0));
            ui.horizontal(|ui| {
                ui.add_space(x_pad.max(0.0));
                ui.add_sized(video_size, img);
            });
            ui.add_space(y_pad.max(0.0));
        } else {
            ui.allocate_space(avail);
            ui.centered_and_justified(|ui| {
                ui.label("No publisher video");
            });
        }

        let video_rect = egui::Rect::from_min_size(video_origin, video_size);
        let stats = self.broadcast.stats();
        stats.capture.codec.set(self.codec.display_name());
        self.overlay.show_publish(ui, video_rect, stats);
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
    audio_ctx: AudioBackend,
    pending_video: Option<VideoTrack>,
    video: Option<VideoTrackView>,
    _audio: Option<AudioTrack>,
    backend: DecoderBackend,
    render_mode: RenderMode,
    _live: Live,

    playout_clock: PlayoutClock,
    overlay: DebugOverlay,
    skip_threshold_ms: u32,

    #[cfg(feature = "wgpu")]
    wgpu_render_state: Option<egui_wgpu::RenderState>,
}

impl SubscribeView {
    async fn new(
        endpoint: Endpoint,
        publisher_addr: EndpointAddr,
        audio_ctx: &AudioBackend,
    ) -> Result<Self> {
        let live = Live::new(endpoint);
        let (session, broadcast) = live.subscribe(publisher_addr, BROADCAST_NAME).await?;
        info!("subscriber connected");

        let playout_clock = broadcast.clock().clone();

        iroh_live::util::spawn_stats_recorder(
            session.conn(),
            broadcast.stats().net.clone(),
            broadcast.shutdown_token(),
        );

        let playback_config = PlaybackConfig::default();
        let tracks = broadcast
            .media::<DefaultDecoders>(audio_ctx, playback_config)
            .await?;

        Ok(Self {
            session,
            broadcast: tracks.broadcast,
            audio_ctx: audio_ctx.clone(),
            pending_video: tracks.video,
            video: None,
            _audio: tracks.audio,
            backend: DecoderBackend::Auto,
            render_mode: *RenderMode::VARIANTS.last().unwrap(),
            playout_clock,
            skip_threshold_ms: 500,
            overlay: DebugOverlay::new(&[
                StatCategory::Net,
                StatCategory::Render,
                StatCategory::Time,
            ]),
            #[cfg(feature = "wgpu")]
            wgpu_render_state: None,
            _live: live,
        })
    }

    #[cfg(feature = "wgpu")]
    fn set_wgpu_render_state(&mut self, state: Option<egui_wgpu::RenderState>) {
        self.wgpu_render_state = state;
    }

    fn resubscribe(&mut self, ctx: &egui::Context) {
        self.playout_clock.reset();

        // Resubscribe video with default quality selection.
        match self.broadcast.video() {
            Ok(track) => {
                info!(
                    rendition = track.rendition(),
                    "subscriber: resubscribed to video"
                );
                self.video = Some(self.make_video_view(ctx, track));
            }
            Err(e) => {
                warn!("video re-subscribe failed: {e:#}");
                self.video = None;
            }
        }

        // Resubscribe audio. The old AudioTrack is dropped, stopping its
        // decoder thread, before starting a fresh one from the new catalog.
        let handle = tokio::runtime::Handle::current();
        match handle.block_on(self.broadcast.audio(&self.audio_ctx)) {
            Ok(track) => {
                info!("subscriber: resubscribed to audio");
                self._audio = Some(track);
            }
            Err(e) => {
                warn!("audio re-subscribe failed: {e:#}");
                self._audio = None;
            }
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

        // When the publisher switches source, the old track closes before
        // the new one is ready. Wait for the close, then resubscribe from
        // the current catalog (which already reflects the new source).
        let video_closed = self.video.as_ref().is_some_and(|v| v.track().is_closed());
        if video_closed {
            info!("subscriber: video track closed, resubscribing from current catalog");
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

            if ui
                .add(
                    egui::Slider::new(&mut self.skip_threshold_ms, 200..=5000)
                        .text("skip ms")
                        .logarithmic(true),
                )
                .changed()
            {
                self.broadcast
                    .set_skip_threshold(Duration::from_millis(self.skip_threshold_ms as u64));
            }
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
            let _ = frame_ts;
        } else {
            ui.allocate_space(avail);
        }

        let video_rect = egui::Rect::from_min_size(video_origin, video_size);
        let stats = self.broadcast.stats();
        if let Some(v) = &self.video {
            self.overlay.update_from_track(stats, v.track());
            stats.render.renderer.set(if v.is_wgpu() {
                v.render_path_name()
            } else {
                "cpu"
            });
        }
        self.overlay.show(ui, video_rect, stats);
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
    #[cfg(target_os = "linux")]
    patchbay: Option<PatchbayState>,
}

impl eframe::App for SplitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        if self.publish.needs_republish {
            self.publish.republish(&self.rt);
        }

        #[cfg(target_os = "linux")]
        if let Some(ref mut pb) = self.patchbay {
            pb.apply(&self.rt);
            egui::TopBottomPanel::top("impairment").show(ctx, |ui| pb.ui(ui));
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

/// Creates both endpoints directly (no network simulation).
async fn setup_direct(audio_ctx: &AudioBackend) -> Result<(PublishView, SubscribeView)> {
    let secret_key = SecretKey::generate(&mut rand::rng());
    let pub_endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .bind()
        .await?;
    let publish = PublishView::new(pub_endpoint, audio_ctx.clone()).await?;
    info!(addr = ?publish.addr(), "publish side ready");

    let sub_endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
    let subscribe = SubscribeView::new(sub_endpoint, publish.addr(), audio_ctx).await?;
    info!("subscribe side ready");

    Ok((publish, subscribe))
}

/// Creates endpoints inside patchbay network namespaces with a simulated link.
#[cfg(target_os = "linux")]
async fn setup_patchbay(
    audio_ctx: &AudioBackend,
) -> Result<(PublishView, SubscribeView, PatchbayState)> {
    let lab = patchbay::Lab::new()
        .await
        .map_err(|e| anyerr!("patchbay lab: {e:#}"))?;
    let router = lab
        .add_router("r1")
        .build()
        .await
        .map_err(|e| anyerr!("router: {e:#}"))?;
    let router_id = router.id();

    let pub_device = lab
        .add_device("publisher")
        .iface("eth0", router_id, None)
        .build()
        .await
        .map_err(|e| anyerr!("pub device: {e:#}"))?;
    let pub_id = pub_device.id();

    let sub_device = lab
        .add_device("subscriber")
        .iface("eth0", router_id, None)
        .build()
        .await
        .map_err(|e| anyerr!("sub device: {e:#}"))?;
    let sub_id = sub_device.id();

    info!("patchbay lab ready");

    let secret_key = SecretKey::generate(&mut rand::rng());
    let pub_endpoint = pub_device
        .spawn({
            let secret_key = secret_key.clone();
            |_dev| async move {
                Endpoint::builder(iroh::endpoint::presets::N0)
                    .secret_key(secret_key)
                    .bind()
                    .await
                    .map_err(|e| anyhow::anyhow!("pub endpoint: {e:#}"))
            }
        })
        .map_err(|e| anyerr!("pub spawn: {e:#}"))?
        .await
        .map_err(|e| anyerr!("pub join: {e:#}"))??;

    let sub_endpoint = sub_device
        .spawn(|_dev| async move {
            Endpoint::bind(iroh::endpoint::presets::N0)
                .await
                .map_err(|e| anyhow::anyhow!("sub endpoint: {e:#}"))
        })
        .map_err(|e| anyerr!("sub spawn: {e:#}"))?
        .await
        .map_err(|e| anyerr!("sub join: {e:#}"))??;

    let publish = PublishView::new(pub_endpoint, audio_ctx.clone()).await?;
    info!(addr = ?publish.addr(), "publish side ready (patchbay)");

    let subscribe = SubscribeView::new(sub_endpoint, publish.addr(), audio_ctx).await?;
    info!("subscribe side ready (patchbay)");

    Ok((
        publish,
        subscribe,
        PatchbayState::new(lab, pub_id, sub_id, router_id),
    ))
}

fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt::init();

    #[cfg(target_os = "linux")]
    if args.patchbay {
        patchbay::init_userns().map_err(|e| anyerr!("patchbay init_userns: {e:#}"))?;
    }
    #[cfg(not(target_os = "linux"))]
    if args.patchbay {
        return Err(anyerr!("--patchbay requires Linux"));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let audio_ctx = AudioBackend::default();

    // Branch on patchbay for setup; cfg gates keep patchbay types off non-Linux.
    #[cfg(target_os = "linux")]
    let (publish, mut subscribe, patchbay_state) = if args.patchbay {
        let (p, s, pb) = rt.block_on(setup_patchbay(&audio_ctx))?;
        (p, s, Some(pb))
    } else {
        let (p, s) = rt.block_on(setup_direct(&audio_ctx))?;
        (p, s, None)
    };
    #[cfg(not(target_os = "linux"))]
    let (publish, mut subscribe) = rt.block_on(setup_direct(&audio_ctx))?;

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

    let title = if args.patchbay {
        "iroh-live split (patchbay)"
    } else {
        "iroh-live split"
    };

    eframe::run_native(
        title,
        native_options,
        Box::new(move |_cc| {
            #[cfg(feature = "wgpu")]
            subscribe.set_wgpu_render_state(_cc.wgpu_render_state.clone());

            Ok(Box::new(SplitApp {
                rt,
                publish,
                subscribe,
                #[cfg(target_os = "linux")]
                patchbay: patchbay_state,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
