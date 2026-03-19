//! 1:1 video call example.
//!
//! Two modes: **dial** (caller) and **accept** (callee). The caller gets a
//! ticket printed to the terminal; paste it into the callee's UI to connect.
//!
//! Uses [`Call`] which is sugar over MoQ primitives — everything here could
//! be done directly with [`Live`] + [`LocalBroadcast`] + [`RemoteBroadcast`].
//!
//! ```sh
//! # Terminal 1 — start as callee, displays a ticket
//! cargo run -p iroh-live --example call
//!
//! # Terminal 2 — dial with the ticket
//! cargo run -p iroh-live --example call -- <ticket>
//! ```

use std::time::Duration;

use eframe::egui::{self, Id};
use iroh_live::{
    Call, CallError, CallTicket, Live,
    media::{
        audio_backend::{AudioBackend, AudioDevice},
        capture::{CameraCapturer, CaptureBackend, ScreenCapturer},
        codec::{AudioCodec, DefaultDecoders, DynamicVideoDecoder, VideoCodec},
        format::{AudioPreset, DecodeConfig, PlaybackConfig, VideoPreset},
        publish::LocalBroadcast,
        subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
        test_sources::{TestPatternSource, TestToneSource},
        traits::VideoSource,
    },
    moq::MoqSession,
};
use moq_media_egui::{
    VideoTrackView,
    overlay::{DebugOverlay, StatCategory, fit_to_aspect},
};
use n0_error::{Result, anyerr};
use strum::VariantArray;
use tracing::{info, warn};

mod common;

// ---------------------------------------------------------------------------
// Source discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum DiscoveredVideoSource {
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
enum AudioInputKind {
    #[strum(serialize = "Test Tone")]
    TestTone,
    Microphone,
    None,
}

// ---------------------------------------------------------------------------
// Shared device-selection state
// ---------------------------------------------------------------------------

struct DeviceSelectors {
    video_sources: Vec<DiscoveredVideoSource>,
    video_source_idx: usize,
    audio_input: AudioInputKind,
    audio_outputs: Vec<AudioDevice>,
    audio_output_idx: usize,
    codec: VideoCodec,
    preset: VideoPreset,
    video_dirty: bool,
    audio_in_dirty: bool,
    audio_out_dirty: bool,
    error_msg: Option<String>,
}

impl DeviceSelectors {
    fn new() -> Self {
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

    fn is_dirty(&self) -> bool {
        self.video_dirty || self.audio_in_dirty || self.audio_out_dirty
    }

    /// Draws device selector combo boxes. Returns `true` if anything changed.
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut video_changed = false;
        let mut audio_in_changed = false;
        let mut audio_out_changed = false;

        ui.label("Video");
        egui::ComboBox::from_id_salt("call_video_src")
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
        egui::ComboBox::from_id_salt("call_audio_in")
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
        egui::ComboBox::from_id_salt("call_audio_out")
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
        egui::ComboBox::from_id_salt("call_codec")
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
        egui::ComboBox::from_id_salt("call_preset")
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

    /// Applies only the changed device selections to the broadcast.
    fn apply(
        &mut self,
        broadcast: &LocalBroadcast,
        audio_ctx: &AudioBackend,
        rt: &tokio::runtime::Runtime,
    ) {
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
            if let Err(e) = broadcast.video().set(source, self.codec, [self.preset]) {
                self.error_msg = Some(format!("Set video: {e:#}"));
                return;
            }
        }

        if self.audio_in_dirty {
            self.audio_in_dirty = false;
            let _guard = rt.enter();
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
                    let mic = match rt.block_on(audio_ctx.default_input()) {
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
                let _guard = rt.enter();
                if let Err(e) = rt.block_on(audio_ctx.switch_output(Some(id))) {
                    self.error_msg = Some(format!("Audio output: {e:#}"));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App states
// ---------------------------------------------------------------------------

enum AppState {
    Setup(Box<SetupState>),
    InCall(Box<InCallState>),
}

struct SetupState {
    live: Live,
    broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,
    devices: DeviceSelectors,

    our_ticket: String,
    remote_ticket: String,
    error: Option<String>,
    connecting: bool,

    preview: Option<VideoTrackView>,
}

struct InCallState {
    session: MoqSession,
    #[allow(dead_code, reason = "kept alive to sustain the call")]
    call: Call,
    broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,
    devices: DeviceSelectors,

    // Local preview
    preview: Option<VideoTrackView>,

    // Remote
    remote: RemoteBroadcast,
    pending_video: Option<VideoTrack>,
    remote_video: Option<VideoTrackView>,
    #[allow(dead_code, reason = "kept alive to sustain audio playout")]
    remote_audio: Option<AudioTrack>,

    overlay: DebugOverlay,
}

// ---------------------------------------------------------------------------
// Setup UI
// ---------------------------------------------------------------------------

impl SetupState {
    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime) -> bool {
        ctx.request_repaint_after(Duration::from_millis(16));
        let mut dial_clicked = false;

        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.heading("iroh-live call");
            ui.add_space(10.0);

            // Device selectors
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.devices.ui(ui);
            });

            if self.devices.is_dirty() {
                self.devices.apply(&self.broadcast, &self.audio_ctx, rt);
                self.preview = None;
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            // Ticket
            ui.label("Your ticket (share with caller):");
            ui.horizontal(|ui| {
                let ticket_display = if self.our_ticket.len() > 60 {
                    format!("{}...", &self.our_ticket[..60])
                } else {
                    self.our_ticket.clone()
                };
                ui.monospace(&ticket_display);
                if ui.button("Copy").clicked() {
                    ctx.copy_text(self.our_ticket.clone());
                }
            });

            ui.add_space(10.0);

            // Dial
            ui.label("Paste a remote ticket to dial:");
            ui.horizontal(|ui| {
                let response = ui.text_edit_singleline(&mut self.remote_ticket);
                let can_dial = !self.remote_ticket.trim().is_empty() && !self.connecting;
                if ui
                    .add_enabled(can_dial, egui::Button::new("Dial"))
                    .clicked()
                    || (can_dial
                        && response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    dial_clicked = true;
                }
            });

            if let Some(ref err) = self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            if self.connecting {
                ui.spinner();
                ui.label("Connecting...");
            }

            ui.add_space(5.0);
            ui.label("Or wait for someone to dial your ticket.");
        });

        // Local preview fills remaining space
        ui.separator();
        self.preview_ui(ctx, ui);

        dial_clicked
    }

    fn preview_ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        if self.preview.is_none()
            && let Some(track) = self.broadcast.preview(DecodeConfig::default())
        {
            self.preview = Some(VideoTrackView::new(ctx, "setup-preview", track));
        }

        let avail = ui.available_size();
        if let Some(ref mut view) = self.preview {
            let video_size = fit_to_aspect(avail, 16.0 / 9.0);
            let (img, _) = view.render(ctx, video_size);
            ui.centered_and_justified(|ui| ui.add_sized(video_size, img));
        } else {
            ui.allocate_space(avail);
        }
    }
}

// ---------------------------------------------------------------------------
// InCall UI — fullscreen remote, PiP local, overlay with selectors
// ---------------------------------------------------------------------------

const PIP_WIDTH: f32 = 240.0;
const PIP_ASPECT: f32 = 16.0 / 9.0;

impl InCallState {
    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime) {
        ctx.request_repaint_after(Duration::from_millis(16));

        // Apply pending device changes
        if self.devices.is_dirty() {
            self.devices.apply(&self.broadcast, &self.audio_ctx, rt);
            self.preview = None;
        }

        // When the remote publisher switches source, the old track closes.
        // Resubscribe from the current catalog to pick up the new source.
        let remote_closed = self
            .remote_video
            .as_ref()
            .is_some_and(|v| v.track().is_closed());
        if remote_closed {
            info!("remote video track closed, resubscribing from current catalog");
            self.resubscribe_remote(ctx, rt);
        }

        // Remote video — fills the entire panel
        self.remote_video_ui(ctx, ui);

        // Local preview — PiP overlay in bottom-right
        self.pip_ui(ctx);

        // Stats + selectors overlay in bottom-left
        self.overlay_ui(ctx);
    }

    fn remote_video_ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        if let Some(track) = self.pending_video.take() {
            self.remote_video = Some(VideoTrackView::new(ctx, "remote-video", track));
        }

        let avail = ui.available_size();
        if let Some(ref mut view) = self.remote_video {
            let (img, _frame_ts) = view.render(ctx, avail);
            ui.add_sized(avail, img);
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Waiting for remote video...");
            });
        }
    }

    fn pip_ui(&mut self, ctx: &egui::Context) {
        if self.preview.is_none()
            && let Some(track) = self.broadcast.preview(DecodeConfig::default())
        {
            self.preview = Some(VideoTrackView::new(ctx, "local-preview", track));
        }

        let pip_size = egui::vec2(PIP_WIDTH, PIP_WIDTH / PIP_ASPECT);

        egui::Area::new(Id::new("pip"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-10.0, -10.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::BLACK)
                    .corner_radius(4.0)
                    .inner_margin(2.0)
                    .show(ui, |ui| {
                        if let Some(ref mut view) = self.preview {
                            let (img, _frame_ts) = view.render(ctx, pip_size);
                            ui.add_sized(pip_size, img);
                        } else {
                            ui.allocate_exact_size(pip_size, egui::Sense::hover());
                        }
                    });
            });
    }

    fn overlay_ui(&mut self, ctx: &egui::Context) {
        egui::Area::new(Id::new("call-overlay"))
            .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180))
                    .corner_radius(3.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        ui.set_min_width(200.0);
                        self.overlay_contents(ctx, ui);
                    });
            });
    }

    fn resubscribe_remote(&mut self, ctx: &egui::Context, rt: &tokio::runtime::Runtime) {
        match self.remote.video() {
            Ok(track) => {
                info!(
                    rendition = track.rendition(),
                    "resubscribed to remote video"
                );
                self.remote_video = Some(VideoTrackView::new(ctx, "remote-video", track));
            }
            Err(e) => {
                warn!("remote video resubscribe failed: {e:#}");
                self.remote_video = None;
            }
        }

        let _guard = rt.enter();
        match rt.block_on(self.remote.audio(&self.audio_ctx)) {
            Ok(track) => {
                info!("resubscribed to remote audio");
                self.remote_audio = Some(track);
            }
            Err(e) => {
                warn!("remote audio resubscribe failed: {e:#}");
                self.remote_audio = None;
            }
        }
    }

    fn overlay_contents(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Rendition selector
        let selected_rendition = self
            .remote_video
            .as_ref()
            .map(|v| v.track().rendition().to_owned());
        ui.horizontal(|ui| {
            ui.label("Rendition");
            egui::ComboBox::from_id_salt("call_rendition")
                .selected_text(selected_rendition.clone().unwrap_or_else(|| "—".into()))
                .show_ui(ui, |ui| {
                    for name in self.remote.catalog().video_renditions() {
                        if ui
                            .selectable_label(selected_rendition.as_deref() == Some(name), name)
                            .clicked()
                        {
                            info!(rendition = name, "rendition switched");
                            match self
                                .remote
                                .video_rendition::<DynamicVideoDecoder>(&Default::default(), name)
                            {
                                Ok(track) => {
                                    self.remote_video =
                                        Some(VideoTrackView::new(ctx, "remote-video", track));
                                }
                                Err(e) => warn!("rendition switch failed: {e:#}"),
                            }
                        }
                    }
                });
        });

        // Device selectors
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            self.devices.ui(ui);
        });

        ui.separator();

        // Update labels from remote track.
        let stats = self.remote.stats();
        if let Some(v) = &self.remote_video {
            self.overlay.update_from_track(stats, v.track());
        }
        // Stats overlay on the available rect below the separator.
        let stats_rect =
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 100.0));
        self.overlay.show(ui, stats_rect, stats);
        ui.allocate_space(egui::vec2(ui.available_width(), 80.0));
    }
}

// ---------------------------------------------------------------------------
// CallApp
// ---------------------------------------------------------------------------

struct CallApp {
    rt: tokio::runtime::Runtime,
    state: AppState,
    call_rx: Option<tokio::sync::oneshot::Receiver<std::result::Result<CallResult, String>>>,
    /// Accept task handle — aborted when the user dials to prevent the
    /// background accept from racing with the dial on the same session.
    accept_task: Option<tokio::task::JoinHandle<()>>,
}

struct CallResult {
    call: Call,
    remote: RemoteBroadcast,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

impl CallApp {
    fn transition_to_call(&mut self, result: CallResult, devices: DeviceSelectors) {
        let session = result.call.session().clone();
        let broadcast = result.call.local().clone();
        // Retrieve audio_ctx from setup state before overwriting.
        let audio_ctx = match &self.state {
            AppState::Setup(s) => s.audio_ctx.clone(),
            AppState::InCall(s) => s.audio_ctx.clone(),
        };
        let remote = result.remote;
        iroh_live::util::spawn_stats_recorder(
            session.conn(),
            remote.stats().net.clone(),
            remote.shutdown_token(),
        );

        self.state = AppState::InCall(Box::new(InCallState {
            session,
            call: result.call,
            broadcast,
            audio_ctx,
            devices,
            preview: None,
            remote,
            pending_video: result.video,
            remote_video: None,
            remote_audio: result.audio,
            overlay: DebugOverlay::new(&[
                StatCategory::Net,
                StatCategory::Render,
                StatCategory::Time,
            ]),
        }));
    }
}

impl eframe::App for CallApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check if an async dial/accept completed
        if let Some(ref mut rx) = self.call_rx
            && let Ok(result) = rx.try_recv()
        {
            self.call_rx = None;
            match result {
                Ok(call_result) => {
                    let devices = match &mut self.state {
                        AppState::Setup(s) => {
                            std::mem::replace(&mut s.devices, DeviceSelectors::new())
                        }
                        _ => DeviceSelectors::new(),
                    };
                    self.transition_to_call(call_result, devices);
                }
                Err(err) => {
                    if let AppState::Setup(ref mut setup) = self.state {
                        setup.error = Some(err);
                        setup.connecting = false;
                    }
                }
            }
        }

        match &mut self.state {
            AppState::Setup(setup) => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if setup.ui(ctx, ui, &self.rt) {
                        let ticket_str = setup.remote_ticket.trim().to_string();
                        match CallTicket::deserialize(&ticket_str) {
                            Ok(ticket) => {
                                // Abort the background accept task so it doesn't
                                // race with dial on the same session.
                                if let Some(handle) = self.accept_task.take() {
                                    handle.abort();
                                }
                                setup.connecting = true;
                                setup.error = None;
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                self.call_rx = Some(rx);
                                let live = setup.live.clone();
                                let broadcast = setup.broadcast.clone();
                                let audio_ctx = setup.audio_ctx.clone();
                                self.rt.spawn(async move {
                                    let result =
                                        dial_call(&live, ticket, broadcast, &audio_ctx).await;
                                    let _ = tx.send(result.map_err(|e| format!("{e:#}")));
                                });
                            }
                            Err(e) => {
                                setup.error = Some(format!("Invalid ticket: {e:#}"));
                            }
                        }
                    }
                });
            }
            AppState::InCall(call_state) => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        call_state.ui(ctx, ui, &self.rt);
                    });
            }
        }
    }

    fn on_exit(&mut self) {
        if let AppState::InCall(ref state) = self.state {
            state.call.close();
        }
    }
}

// ---------------------------------------------------------------------------
// Async helpers
// ---------------------------------------------------------------------------

async fn dial_call(
    live: &Live,
    ticket: CallTicket,
    broadcast: LocalBroadcast,
    audio_ctx: &AudioBackend,
) -> std::result::Result<CallResult, CallError> {
    info!(remote = %ticket, "dialing");
    let call = Call::dial(live, ticket, broadcast).await?;
    subscribe_call(call, audio_ctx).await
}

async fn accept_call(
    live: &Live,
    broadcast: LocalBroadcast,
    audio_ctx: &AudioBackend,
) -> std::result::Result<CallResult, String> {
    let mut incoming = live.transport().incoming_sessions();
    let session = incoming
        .next()
        .await
        .ok_or_else(|| "transport shut down".to_string())?;
    info!(remote = %session.remote_id().fmt_short(), "incoming call");
    let session = session.accept();
    let call = Call::accept(session, broadcast)
        .await
        .map_err(|e| format!("accept failed: {e:#}"))?;
    subscribe_call(call, audio_ctx)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Subscribes to a call's remote media and returns everything needed for the UI.
async fn subscribe_call(
    call: Call,
    audio_ctx: &AudioBackend,
) -> std::result::Result<CallResult, CallError> {
    let tracks = call
        .remote()
        .media::<DefaultDecoders>(audio_ctx, PlaybackConfig::default())
        .await
        .map_err(CallError::ConnectionFailed)?;
    Ok(CallResult {
        remote: tracks.broadcast,
        video: tracks.video,
        audio: tracks.audio,
        call,
    })
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

    let ticket_arg = std::env::args().nth(1);
    let audio_ctx = AudioBackend::default();

    // Create Live instance and broadcast with default test sources.
    // The broadcast is passed to Call::dial/accept which publishes it
    // on the session — we do NOT call live.publish() to avoid a
    // double-publish conflict.
    let (live, broadcast) = rt.block_on(async {
        let live = Live::from_env().await?;

        let broadcast = LocalBroadcast::new();
        broadcast.video().set(
            TestPatternSource::new(1280, 720),
            VideoCodec::best_available().expect("no video codec available"),
            [VideoPreset::P720],
        )?;
        broadcast
            .audio()
            .set(TestToneSource::new(), AudioCodec::Opus, [AudioPreset::Hq])?;

        n0_error::Ok((live, broadcast))
    })?;

    let our_ticket = rt.block_on(async {
        let addr = live.endpoint().addr();
        CallTicket::new(addr)
    });
    let our_ticket_str = our_ticket.to_string();
    println!("Your ticket: {our_ticket_str}");

    // If a ticket was passed on the command line, dial immediately.
    // Otherwise, start accepting incoming calls in the background.
    // The accept task is tracked so it can be aborted when the user
    // dials via the UI — otherwise it races with dial on the same session.
    let (initial_call_rx, initial_accept_task) = if let Some(ref ticket_str) = ticket_arg {
        let ticket = CallTicket::deserialize(ticket_str.trim())
            .map_err(|e| anyerr!("invalid ticket: {e:#}"))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let live_clone = live.clone();
        let broadcast_clone = broadcast.clone();
        let audio_clone = audio_ctx.clone();
        rt.spawn(async move {
            let result = dial_call(&live_clone, ticket, broadcast_clone, &audio_clone).await;
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
        (Some(rx), None)
    } else {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let live_clone = live.clone();
        let broadcast_clone = broadcast.clone();
        let audio_clone = audio_ctx.clone();
        let handle = rt.spawn(async move {
            let result = accept_call(&live_clone, broadcast_clone, &audio_clone).await;
            let _ = tx.send(result);
        });
        (Some(rx), Some(handle))
    };

    let _guard = rt.enter();

    eframe::run_native(
        "iroh-live call",
        eframe::NativeOptions::default(),
        Box::new(move |_cc| {
            Ok(Box::new(CallApp {
                rt,
                state: AppState::Setup(Box::new(SetupState {
                    live,
                    broadcast,
                    audio_ctx,
                    devices: DeviceSelectors::new(),
                    our_ticket: our_ticket_str,
                    remote_ticket: String::new(),
                    error: None,
                    connecting: initial_call_rx.is_some() && ticket_arg.is_some(),
                    preview: None,
                })),
                call_rx: initial_call_rx,
                accept_task: initial_accept_task,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
