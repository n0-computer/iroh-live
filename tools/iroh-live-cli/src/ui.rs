//! Shared UI components: top bar, device selectors, remote playback controls,
//! auto-hide logic.

use std::time::{Duration, Instant};

use eframe::egui;
use iroh_live::media::{
    AudioBackend,
    adaptive::RenditionMode,
    audio_backend::AudioDevice,
    capture::{CameraCapturer, CaptureBackend, ScreenCapturer},
    codec::{AudioCodec, DynamicVideoDecoder, VideoCodec},
    format::{AudioPreset, DecodeConfig, DecoderBackend, VideoPreset},
    net::NetworkSignals,
    publish::LocalBroadcast,
    subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
    test_sources::{TestPatternSource, TestToneSource},
    traits::VideoSource,
};
use moq_media_egui::{
    VideoTrackView,
    overlay::{DebugOverlay, StatCategory},
};
use strum::VariantArray;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

const TOP_BAR_H: f32 = 24.0;

/// Draws the top bar overlay with ticket text (click to copy) and fullscreen toggle.
pub fn top_bar(ui: &mut egui::Ui, ctx: &egui::Context, ticket_text: &str) {
    let content_rect = ctx.content_rect();
    let bar_rect = egui::Rect::from_min_size(
        content_rect.min,
        egui::vec2(content_rect.width(), TOP_BAR_H),
    );

    let painter = ui.painter_at(bar_rect);
    painter.rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(160));

    let text_pos = bar_rect.min + egui::vec2(8.0, 4.0);
    let galley = painter.layout_no_wrap(
        ticket_text.to_string(),
        egui::FontId::monospace(12.0),
        egui::Color32::WHITE,
    );
    painter.galley(text_pos, galley, egui::Color32::WHITE);

    let bar_response = ui.interact(
        bar_rect,
        egui::Id::new("top_bar_click"),
        egui::Sense::click(),
    );
    if bar_response.clicked() {
        ctx.copy_text(ticket_text.to_string());
    }
    if bar_response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    fullscreen_button(ui, ctx, bar_rect);
}

fn fullscreen_button(ui: &mut egui::Ui, ctx: &egui::Context, bar_rect: egui::Rect) {
    let btn_size = egui::vec2(20.0, 16.0);
    let btn_pos = egui::pos2(bar_rect.right() - btn_size.x - 8.0, bar_rect.min.y + 4.0);
    let btn_rect = egui::Rect::from_min_size(btn_pos, btn_size);
    let response = ui.interact(
        btn_rect,
        egui::Id::new("fullscreen_btn"),
        egui::Sense::click(),
    );
    let color = if response.hovered() {
        egui::Color32::from_white_alpha(200)
    } else {
        egui::Color32::from_white_alpha(140)
    };
    ui.painter_at(bar_rect).text(
        btn_rect.center(),
        egui::Align2::CENTER_CENTER,
        "⛶",
        egui::FontId::proportional(14.0),
        color,
    );
    if response.clicked() {
        let is_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen));
    }
}

// ---------------------------------------------------------------------------
// Mouse auto-hide (disabled when overlay is expanded)
// ---------------------------------------------------------------------------

pub struct MouseHide {
    pub active: bool,
    timer: Instant,
}

impl Default for MouseHide {
    fn default() -> Self {
        Self {
            active: true,
            timer: Instant::now(),
        }
    }
}

const MOUSE_HIDE_DELAY: Duration = Duration::from_secs(2);

impl MouseHide {
    /// Returns whether the overlay should be visible. Stays visible when
    /// `force_visible` is true (e.g. debug overlay expanded).
    pub fn update(&mut self, ctx: &egui::Context, force_visible: bool) -> bool {
        if force_visible {
            self.active = true;
            self.timer = Instant::now();
            return true;
        }
        if ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
            self.active = true;
            self.timer = Instant::now();
        } else if self.timer.elapsed() > MOUSE_HIDE_DELAY {
            self.active = false;
        }
        self.active
    }
}

// ---------------------------------------------------------------------------
// Device selectors (shared by call, room, publish --preview)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveredVideoSource {
    Camera {
        backend: CaptureBackend,
        name: String,
    },
    Screen {
        backend: CaptureBackend,
    },
    TestPattern,
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
    pub fn create(&self, preset: VideoPreset) -> anyhow::Result<Box<dyn VideoSource>> {
        let (w, h) = preset.dimensions();
        match self {
            Self::TestPattern => Ok(Box::new(TestPatternSource::new(w, h))),
            Self::Camera { backend, .. } => Ok(Box::new(CameraCapturer::open(
                Some(*backend),
                None,
                &iroh_live::media::capture::CameraConfig::default(),
            )?)),
            Self::Screen { backend } => Ok(Box::new(ScreenCapturer::open(
                Some(*backend),
                None,
                &iroh_live::media::capture::ScreenConfig::default(),
            )?)),
        }
    }
}

pub fn discover_video_sources() -> Vec<DiscoveredVideoSource> {
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
pub enum AudioInputKind {
    #[strum(serialize = "Test Tone")]
    TestTone,
    Microphone,
    None,
}

pub struct DeviceSelectors {
    pub video_sources: Vec<DiscoveredVideoSource>,
    pub video_source_idx: usize,
    pub audio_input: AudioInputKind,
    pub audio_outputs: Vec<AudioDevice>,
    pub audio_output_idx: usize,
    pub codec: VideoCodec,
    pub preset: VideoPreset,
    video_dirty: bool,
    audio_in_dirty: bool,
    audio_out_dirty: bool,
    pub error_msg: Option<String>,
}

impl DeviceSelectors {
    pub fn new() -> Self {
        Self {
            video_sources: discover_video_sources(),
            video_source_idx: 0,
            audio_input: AudioInputKind::TestTone,
            audio_outputs: AudioBackend::list_outputs(),
            audio_output_idx: 0,
            codec: VideoCodec::best_available().expect("no video codec available"),
            preset: VideoPreset::P720,
            video_dirty: false,
            audio_in_dirty: false,
            audio_out_dirty: false,
            error_msg: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.video_dirty || self.audio_in_dirty || self.audio_out_dirty
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, id_salt: &str) -> bool {
        let mut video_changed = false;
        let mut audio_in_changed = false;
        let mut audio_out_changed = false;

        ui.label("Video");
        egui::ComboBox::from_id_salt(format!("{id_salt}_video_src"))
            .selected_text(self.video_sources[self.video_source_idx].to_string())
            .show_ui(ui, |ui| {
                for (i, src) in self.video_sources.iter().enumerate() {
                    if ui
                        .selectable_value(&mut self.video_source_idx, i, src.to_string())
                        .changed()
                    {
                        video_changed = true;
                    }
                }
            });

        ui.label("Audio In");
        egui::ComboBox::from_id_salt(format!("{id_salt}_audio_in"))
            .selected_text(self.audio_input.to_string())
            .show_ui(ui, |ui| {
                for kind in AudioInputKind::VARIANTS {
                    if ui
                        .selectable_value(&mut self.audio_input, *kind, kind.to_string())
                        .changed()
                    {
                        audio_in_changed = true;
                    }
                }
            });

        ui.label("Audio Out");
        let out_text = self
            .audio_outputs
            .get(self.audio_output_idx)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Default".into());
        egui::ComboBox::from_id_salt(format!("{id_salt}_audio_out"))
            .selected_text(out_text)
            .show_ui(ui, |ui| {
                for (i, dev) in self.audio_outputs.iter().enumerate() {
                    if ui
                        .selectable_value(&mut self.audio_output_idx, i, dev.to_string())
                        .changed()
                    {
                        audio_out_changed = true;
                    }
                }
            });

        ui.label("Codec");
        egui::ComboBox::from_id_salt(format!("{id_salt}_codec"))
            .selected_text(self.codec.display_name())
            .show_ui(ui, |ui| {
                for codec in VideoCodec::available() {
                    if ui
                        .selectable_value(&mut self.codec, codec, codec.display_name())
                        .changed()
                    {
                        video_changed = true;
                    }
                }
            });

        ui.label("Preset");
        egui::ComboBox::from_id_salt(format!("{id_salt}_preset"))
            .selected_text(self.preset.to_string())
            .show_ui(ui, |ui| {
                for p in VideoPreset::all() {
                    if ui
                        .selectable_value(&mut self.preset, p, p.to_string())
                        .changed()
                    {
                        video_changed = true;
                    }
                }
            });

        if let Some(ref msg) = self.error_msg {
            ui.colored_label(egui::Color32::RED, msg);
        }

        self.video_dirty |= video_changed;
        self.audio_in_dirty |= audio_in_changed;
        self.audio_out_dirty |= audio_out_changed;

        video_changed || audio_in_changed || audio_out_changed
    }

    pub fn apply(&mut self, broadcast: &LocalBroadcast, audio_ctx: &AudioBackend) {
        self.error_msg = None;

        if self.video_dirty {
            self.video_dirty = false;
            let source = match self.video_sources[self.video_source_idx].create(self.preset) {
                Ok(s) => s,
                Err(e) => {
                    self.error_msg = Some(format!("Video source: {e:#}"));
                    return;
                }
            };
            if let Err(e) = broadcast
                .video()
                .set_source(source, self.codec, [self.preset])
            {
                self.error_msg = Some(format!("Set video: {e:#}"));
                return;
            }
        }

        if self.audio_in_dirty {
            self.audio_in_dirty = false;
            match self.audio_input {
                AudioInputKind::TestTone => {
                    if let Err(e) = broadcast.audio().set(
                        TestToneSource::new(),
                        AudioCodec::Opus,
                        [AudioPreset::Hq],
                    ) {
                        self.error_msg = Some(format!("Set audio: {e:#}"));
                        return;
                    }
                }
                AudioInputKind::Microphone => {
                    let mic = match audio_ctx.default_input_blocking() {
                        Ok(m) => m,
                        Err(e) => {
                            self.error_msg = Some(format!("Microphone: {e:#}"));
                            return;
                        }
                    };
                    if let Err(e) = broadcast
                        .audio()
                        .set(mic, AudioCodec::Opus, [AudioPreset::Hq])
                    {
                        self.error_msg = Some(format!("Set audio: {e:#}"));
                        return;
                    }
                }
                AudioInputKind::None => {
                    broadcast.audio().clear();
                }
            }
        }

        if self.audio_out_dirty {
            self.audio_out_dirty = false;
            if let Some(dev) = self.audio_outputs.get(self.audio_output_idx) {
                let id = dev.id.clone();
                if let Err(e) = audio_ctx.switch_output_blocking(Some(id)) {
                    self.error_msg = Some(format!("Audio output: {e:#}"));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sync mode for UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
pub enum SyncModeChoice {
    /// Audio-master sync (default interactive).
    #[strum(serialize = "Synced")]
    AudioMaster,
    /// Unmanaged — no A/V sync, frames delivered as decoded.
    Unmanaged,
}

// ---------------------------------------------------------------------------
// Remote playback controls (shared by play, call, room)
// ---------------------------------------------------------------------------

/// Manages a remote broadcast's video rendering, rendition selection (auto/fixed),
/// decoder backend, sync mode, and debug overlay. Shared across play/call/room.
pub struct RemoteControls {
    pub video: Option<VideoTrackView>,
    pub audio: Option<AudioTrack>,
    pub broadcast: RemoteBroadcast,
    pub overlay: DebugOverlay,
    audio_ctx: AudioBackend,
    #[allow(dead_code, reason = "reserved for AdaptiveVideoTrack wiring")]
    signals: tokio::sync::watch::Receiver<NetworkSignals>,

    // Control state
    pub decoder_backend: DecoderBackend,
    pub sync_mode: SyncModeChoice,
    pub rendition_mode: RenditionMode,
    stale_duration_ms: u32,
}

impl RemoteControls {
    #[allow(
        clippy::too_many_arguments,
        reason = "constructor gathers all dependencies"
    )]
    pub fn new(
        broadcast: RemoteBroadcast,
        video: Option<VideoTrack>,
        audio: Option<AudioTrack>,
        audio_ctx: AudioBackend,
        signals: tokio::sync::watch::Receiver<NetworkSignals>,
        ctx: &egui::Context,
        view_id: &str,
        categories: &[StatCategory],
    ) -> Self {
        let video_view = video.map(|v| VideoTrackView::new(ctx, view_id, v));
        Self {
            video: video_view,
            audio,
            broadcast,
            overlay: DebugOverlay::new(categories),
            audio_ctx,
            signals,
            decoder_backend: DecoderBackend::Auto,
            sync_mode: SyncModeChoice::AudioMaster,
            rendition_mode: RenditionMode::Auto,
            stale_duration_ms: 500,
        }
    }

    /// Resubscribes to video and audio from the current catalog.
    pub fn resubscribe(&mut self, ctx: &egui::Context, view_id: &str) {
        self.broadcast.reset_sync();

        let decode_config = DecodeConfig {
            backend: self.decoder_backend,
            ..Default::default()
        };

        // Video
        match self.broadcast.video_with(
            iroh_live::media::subscribe::VideoOptions::default().playback(decode_config.clone()),
        ) {
            Ok(track) => {
                info!(rendition = track.rendition(), "resubscribed to video");
                self.video = Some(VideoTrackView::new(ctx, view_id, track));
            }
            Err(e) => {
                warn!("video resubscribe failed: {e:#}");
                self.video = None;
            }
        }

        // Audio
        match tokio::runtime::Handle::current().block_on(self.broadcast.audio(&self.audio_ctx)) {
            Ok(track) => {
                info!("resubscribed to audio");
                self.audio = Some(track);
            }
            Err(e) => {
                warn!("audio resubscribe failed: {e:#}");
                self.audio = None;
            }
        }
    }

    /// Returns true if the video track has closed (source switch).
    pub fn video_closed(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.track().is_closed())
    }

    /// Updates debug overlay stats from the current video track.
    pub fn update_overlay(&mut self) {
        let stats = self.broadcast.stats();
        if let Some(v) = &self.video {
            self.overlay.update_from_track(stats, v.track());
            stats.render.renderer.set(if v.is_wgpu() {
                v.render_path_name()
            } else {
                "cpu"
            });
        }
    }

    /// Draws the playback control bar: rendition (auto/fixed), decoder, sync mode,
    /// stale duration slider. Returns true if anything changed that requires resubscribe.
    pub fn controls_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, id_salt: &str) -> bool {
        let mut needs_resubscribe = false;

        // Rendition: Auto or fixed by name
        let rendition_label = match &self.rendition_mode {
            RenditionMode::Auto => "Auto".to_string(),
            RenditionMode::Fixed(name) => name.clone(),
        };
        egui::ComboBox::from_id_salt(format!("{id_salt}_rendition"))
            .selected_text(&rendition_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.rendition_mode == RenditionMode::Auto, "Auto")
                    .clicked()
                {
                    self.rendition_mode = RenditionMode::Auto;
                    needs_resubscribe = true;
                }
                for name in self.broadcast.catalog().video_renditions() {
                    let is_selected =
                        matches!(&self.rendition_mode, RenditionMode::Fixed(n) if n == name);
                    if ui.selectable_label(is_selected, name).clicked() {
                        info!(rendition = name, "rendition pinned");
                        self.rendition_mode = RenditionMode::Fixed(name.to_string());
                        let decode_config = DecodeConfig {
                            backend: self.decoder_backend,
                            ..Default::default()
                        };
                        match self
                            .broadcast
                            .video_rendition::<DynamicVideoDecoder>(&decode_config, name)
                        {
                            Ok(track) => {
                                self.video = Some(VideoTrackView::new(ctx, id_salt, track));
                            }
                            Err(e) => warn!("rendition switch failed: {e:#}"),
                        }
                    }
                }
            });

        // Decoder backend
        let backend_label = match self.decoder_backend {
            DecoderBackend::Auto => "Auto",
            DecoderBackend::Software => "SW",
        };
        egui::ComboBox::from_id_salt(format!("{id_salt}_decoder"))
            .selected_text(backend_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(&mut self.decoder_backend, DecoderBackend::Auto, "Auto")
                    .changed()
                    || ui
                        .selectable_value(
                            &mut self.decoder_backend,
                            DecoderBackend::Software,
                            "Software",
                        )
                        .changed()
                {
                    info!(backend = ?self.decoder_backend, "decoder backend changed");
                    needs_resubscribe = true;
                }
            });

        // Sync mode
        egui::ComboBox::from_id_salt(format!("{id_salt}_sync"))
            .selected_text(self.sync_mode.to_string())
            .show_ui(ui, |ui| {
                for mode in SyncModeChoice::VARIANTS {
                    if ui
                        .selectable_value(&mut self.sync_mode, *mode, mode.to_string())
                        .changed()
                    {
                        info!(mode = %self.sync_mode, "sync mode changed");
                        needs_resubscribe = true;
                    }
                }
            });

        // Stale duration slider
        if ui
            .add(
                egui::Slider::new(&mut self.stale_duration_ms, 200..=5000)
                    .text("stale ms")
                    .logarithmic(true),
            )
            .changed()
        {
            self.broadcast
                .set_max_stale_duration(Duration::from_millis(self.stale_duration_ms as u64));
        }

        if needs_resubscribe {
            self.resubscribe(ctx, id_salt);
        }

        needs_resubscribe
    }
}
