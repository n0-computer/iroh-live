//! Split-screen with patchbay network impairment.
//!
//! Like `split.rs`, but the publisher and subscriber endpoints run in
//! separate patchbay network namespaces with a simulated link between
//! them. Sliders in the UI let you impair the link (rate, latency,
//! packet loss) at runtime to observe how the pipeline reacts.
//!
//! Requires Linux with unprivileged user namespaces enabled.
//!
//! ```sh
//! cargo run -p iroh-live --example split-patchbay
//! cargo run -p iroh-live --example split-patchbay --features "vaapi,wgpu"
//! ```
#![cfg(target_os = "linux")]

use std::time::Duration;

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
use patchbay::{Device, Lab, LinkCondition, LinkLimits};
use strum::VariantArray;
use tracing::{info, warn};

mod common;

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
    /// Patchbay device for the subscriber side — impairment is applied here.
    sub_device: Device,
    /// Current impairment sliders.
    rate_kbit: u32,
    latency_ms: u32,
    loss_pct: f32,
    /// Whether impairment is dirty (needs to be applied).
    impairment_dirty: bool,
}

impl SplitApp {
    fn impairment_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Link:");
            let mut changed = false;
            changed |= ui
                .add(egui::Slider::new(&mut self.rate_kbit, 100..=100_000).text("kbit/s"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut self.latency_ms, 0..=500).text("ms lat"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut self.loss_pct, 0.0..=30.0).text("% loss"))
                .changed();
            if changed {
                self.impairment_dirty = true;
            }
        });
    }

    fn apply_impairment(&mut self) {
        if !self.impairment_dirty {
            return;
        }
        self.impairment_dirty = false;
        let limits = LinkLimits {
            rate_kbit: self.rate_kbit,
            latency_ms: self.latency_ms,
            jitter_ms: self.latency_ms / 5,
            loss_pct: self.loss_pct,
            ..Default::default()
        };
        let device = self.sub_device.clone();
        self.rt.spawn(async move {
            if let Err(e) = device
                .set_link_condition("eth0", Some(LinkCondition::Manual(limits)))
                .await
            {
                tracing::warn!("failed to set link condition: {e:#}");
            }
        });
    }
}

impl eframe::App for SplitApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        if self.publish.needs_republish {
            self.publish.republish(&self.rt);
        }

        self.apply_impairment();

        // Top bar with impairment sliders.
        egui::TopBottomPanel::top("impairment").show(ctx, |ui| {
            self.impairment_ui(ui);
        });

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

    // Initialize patchbay user namespace (must be before tokio).
    patchbay::init_userns().map_err(|e| anyerr!("patchbay init_userns: {e:#}"))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let audio_ctx = AudioBackend::default();
    let secret_key = SecretKey::generate(&mut rand::rng());

    // Set up patchbay lab with a router and two devices.
    let (publish, subscribe, sub_device) = rt.block_on(async {
        let lab = Lab::new()
            .await
            .map_err(|e| anyerr!("patchbay lab: {e:#}"))?;
        let router = lab
            .add_router("r1")
            .build()
            .await
            .map_err(|e| anyerr!("router: {e:#}"))?;

        let pub_device = lab
            .add_device("publisher")
            .iface("eth0", router.id(), None)
            .build()
            .await
            .map_err(|e| anyerr!("pub device: {e:#}"))?;

        let sub_device = lab
            .add_device("subscriber")
            .iface("eth0", router.id(), None)
            .build()
            .await
            .map_err(|e| anyerr!("sub device: {e:#}"))?;

        info!("patchbay lab ready");

        // Create publisher endpoint inside pub_device's namespace.
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

        // Create subscriber endpoint inside sub_device's namespace.
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
        info!(addr=?publish.addr(), "Publish side ready");
        let publisher_addr = publish.addr();

        let subscribe = SubscribeView::new(sub_endpoint, publisher_addr, &audio_ctx).await?;
        info!("Subscribe side ready");

        n0_error::Ok((publish, subscribe, sub_device))
    })?;

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

    #[allow(unused_mut, reason = "mut needed when wgpu feature is enabled")]
    let mut subscribe = subscribe;

    eframe::run_native(
        "iroh-live split-patchbay",
        native_options,
        Box::new(move |_cc| {
            #[cfg(feature = "wgpu")]
            subscribe.set_wgpu_render_state(_cc.wgpu_render_state.clone());

            Ok(Box::new(SplitApp {
                rt,
                publish,
                subscribe,
                sub_device,
                rate_kbit: 100_000,
                latency_ms: 0,
                loss_pct: 0.0,
                impairment_dirty: false,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
