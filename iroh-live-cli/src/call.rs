//! `irl call` — 1:1 bidirectional video call.

use std::time::Duration;

use eframe::egui::{self, Id};
use iroh_live::{
    Call, CallError, Live,
    media::{
        AudioBackend,
        format::PlaybackConfig,
        publish::LocalBroadcast,
        subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
    },
    ticket::LiveTicket,
};
use moq_media_egui::{
    VideoTrackView,
    overlay::{StatCategory, fit_to_aspect},
};
use n0_error::{Result, anyerr};
use tracing::info;

use crate::{
    args::CallArgs,
    ui::{DeviceSelectors, RemoteControls},
};

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
    live: Live,
    #[allow(
        dead_code,
        reason = "kept alive — owns session, stats, and signal tasks"
    )]
    call: Call,
    broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,
    devices: DeviceSelectors,
    preview: Option<VideoTrackView>,
    remote: RemoteControls,
}

// ---------------------------------------------------------------------------
// Setup UI
// ---------------------------------------------------------------------------

impl SetupState {
    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> bool {
        ctx.request_repaint_after(Duration::from_millis(16));
        let mut dial_clicked = false;

        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.heading("irl call");
            ui.add_space(10.0);

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.devices.ui(ui, "setup");
            });

            if self.devices.is_dirty() {
                self.devices.apply(&self.broadcast, &self.audio_ctx);
                self.preview = None;
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            ui.label("Your ticket (share with caller):");
            ui.horizontal(|ui| {
                let display = if self.our_ticket.len() > 60 {
                    format!("{}...", &self.our_ticket[..60])
                } else {
                    self.our_ticket.clone()
                };
                ui.monospace(&display);
                if ui.button("Copy").clicked() {
                    ctx.copy_text(self.our_ticket.clone());
                }
            });

            ui.add_space(10.0);
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

        ui.separator();
        if self.preview.is_none()
            && let Some(track) = self.broadcast.preview()
        {
            self.preview = Some(VideoTrackView::new(ctx, "setup-preview", track));
        }
        let avail = ui.available_size();
        if let Some(view) = self.preview.as_mut() {
            let video_size = fit_to_aspect(avail, 16.0 / 9.0);
            let (img, _) = view.render(ctx, video_size);
            ui.centered_and_justified(|ui| ui.add_sized(video_size, img));
        } else {
            ui.allocate_space(avail);
        }

        dial_clicked
    }
}

// ---------------------------------------------------------------------------
// InCall UI
// ---------------------------------------------------------------------------

const PIP_WIDTH: f32 = 240.0;
const PIP_ASPECT: f32 = 16.0 / 9.0;

impl InCallState {
    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ctx.request_repaint_after(Duration::from_millis(16));

        if self.devices.is_dirty() {
            self.devices.apply(&self.broadcast, &self.audio_ctx);
            self.preview = None;
        }

        if self.remote.video_closed() {
            info!("remote video track closed, resubscribing");
            self.remote.resubscribe(ctx, "remote-video");
        }

        // Remote video — fills panel.
        let avail = ui.available_size();
        if let Some(video) = self.remote.video.as_mut() {
            let (img, _) = video.render(ctx, avail);
            ui.add_sized(avail, img);
        } else {
            ui.centered_and_justified(|ui| ui.label("Waiting for remote video..."));
        }

        // PiP local preview.
        if self.preview.is_none()
            && let Some(track) = self.broadcast.preview()
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
                        if let Some(view) = self.preview.as_mut() {
                            let (img, _) = view.render(ctx, pip_size);
                            ui.add_sized(pip_size, img);
                        } else {
                            ui.allocate_exact_size(pip_size, egui::Sense::hover());
                        }
                    });
            });

        // Overlay with controls.
        egui::Area::new(Id::new("call-overlay"))
            .anchor(egui::Align2::LEFT_TOP, [8.0, 28.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180))
                    .corner_radius(3.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        ui.set_min_width(200.0);

                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            self.remote.controls_ui(ui, ctx, "call");
                        });

                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            self.devices.ui(ui, "call");
                        });

                        ui.separator();

                        self.remote.update_overlay();
                        let stats_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(ui.available_width(), 100.0),
                        );
                        self.remote
                            .overlay
                            .show(ui, stats_rect, self.remote.broadcast.stats());
                        ui.allocate_space(egui::vec2(ui.available_width(), 80.0));
                    });
            });
    }
}

// ---------------------------------------------------------------------------
// CallApp
// ---------------------------------------------------------------------------

struct CallApp {
    state: AppState,
    call_rx: Option<tokio::sync::oneshot::Receiver<std::result::Result<CallResult, String>>>,
    accept_task: Option<tokio::task::JoinHandle<()>>,
    wgpu_render_state: Option<moq_media_egui::egui_wgpu::RenderState>,
}

struct CallResult {
    call: Call,
    remote: RemoteBroadcast,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

impl CallApp {
    fn transition_to_call(
        &mut self,
        result: CallResult,
        devices: DeviceSelectors,
        ctx: &egui::Context,
    ) {
        let broadcast = result.call.local().clone();
        let (live, audio_ctx) = match &self.state {
            AppState::Setup(s) => (s.live.clone(), s.audio_ctx.clone()),
            AppState::InCall(s) => (s.live.clone(), s.audio_ctx.clone()),
        };

        // Stats recording and signal production are auto-wired by Call::setup,
        // so we just clone the signals receiver for the UI controls.
        let signals = Some(result.call.signals().clone());

        let remote = RemoteControls::new(
            result.remote,
            result.video,
            result.audio,
            audio_ctx.clone(),
            signals,
            ctx,
            "remote-video",
            &[StatCategory::Net, StatCategory::Render, StatCategory::Time],
            self.wgpu_render_state.clone(),
        );

        self.state = AppState::InCall(Box::new(InCallState {
            live,
            call: result.call,
            broadcast,
            audio_ctx,
            devices,
            preview: None,
            remote,
        }));
    }
}

impl eframe::App for CallApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check async dial/accept completion.
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
                    self.transition_to_call(call_result, devices, ctx);
                }
                Err(err) => {
                    if let AppState::Setup(setup) = &mut self.state {
                        setup.error = Some(err);
                        setup.connecting = false;
                    }
                }
            }
        }

        match &mut self.state {
            AppState::Setup(setup) => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if setup.ui(ctx, ui) {
                        let ticket_str = setup.remote_ticket.trim().to_string();
                        match LiveTicket::deserialize(&ticket_str) {
                            Ok(ticket) => {
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
                                tokio::runtime::Handle::current().spawn(async move {
                                    let result =
                                        dial_call(&live, ticket, broadcast, &audio_ctx).await;
                                    let _ = tx.send(result.map_err(|e| format!("{e:#}")));
                                });
                            }
                            Err(e) => setup.error = Some(format!("Invalid ticket: {e:#}")),
                        }
                    }
                });
            }
            AppState::InCall(state) => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        state.ui(ctx, ui);
                    });
            }
        }
    }

    fn on_exit(&mut self) {
        info!("exit");
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        let live = match &self.state {
            AppState::Setup(s) => &s.live,
            AppState::InCall(s) => &s.live,
        };
        crate::ui::shutdown_live_blocking(live);
    }
}

// ---------------------------------------------------------------------------
// Async helpers
// ---------------------------------------------------------------------------

async fn dial_call(
    live: &Live,
    ticket: LiveTicket,
    broadcast: LocalBroadcast,
    audio_ctx: &AudioBackend,
) -> std::result::Result<CallResult, CallError> {
    info!(remote = %ticket, "dialing");
    let call = Call::dial(live, ticket.endpoint, broadcast).await?;
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

async fn subscribe_call(
    call: Call,
    audio_ctx: &AudioBackend,
) -> std::result::Result<CallResult, CallError> {
    let tracks = call
        .remote()
        .media(audio_ctx, PlaybackConfig::default())
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
// Entry point
// ---------------------------------------------------------------------------

pub fn run(args: CallArgs, rt: &tokio::runtime::Runtime) -> Result<()> {
    let ticket_arg = args.ticket.clone();

    let (live, broadcast, audio_ctx, our_ticket_str, initial_call_rx, initial_accept_task) = rt
        .block_on(async {
            let audio_ctx = AudioBackend::default();

            let live = Live::from_env().await?.with_router().spawn();
            let broadcast = LocalBroadcast::new();

            let video_sources = args.capture.video_sources().map_err(|e| anyerr!("{e}"))?;
            let audio_sources = args.capture.audio_sources().map_err(|e| anyerr!("{e}"))?;
            let codec = args.capture.video_codec()?;
            let presets = args.capture.presets()?;
            let audio_preset = args.capture.audio_preset_parsed()?;

            crate::source::setup_video(&broadcast, &video_sources, codec, &presets)?;
            crate::source::setup_audio(&broadcast, &audio_sources, &audio_ctx, audio_preset)
                .await?;

            let our_ticket = LiveTicket::new(live.endpoint().addr(), "call");
            let our_ticket_str = our_ticket.to_string();
            println!("Your ticket: {our_ticket_str}");

            let (initial_call_rx, initial_accept_task) = if let Some(ref ticket_str) = ticket_arg {
                let ticket = LiveTicket::deserialize(ticket_str.trim())
                    .map_err(|e| anyerr!("invalid ticket: {e:#}"))?;
                let (tx, rx) = tokio::sync::oneshot::channel();
                let live_c = live.clone();
                let bc_c = broadcast.clone();
                let ac_c = audio_ctx.clone();
                tokio::spawn(async move {
                    let result = dial_call(&live_c, ticket, bc_c, &ac_c).await;
                    let _ = tx.send(result.map_err(|e| format!("{e:#}")));
                });
                (Some(rx), None)
            } else {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let live_c = live.clone();
                let bc_c = broadcast.clone();
                let ac_c = audio_ctx.clone();
                let handle = tokio::spawn(async move {
                    let result = accept_call(&live_c, bc_c, &ac_c).await;
                    let _ = tx.send(result);
                });
                (Some(rx), Some(handle))
            };

            n0_error::Ok((
                live,
                broadcast,
                audio_ctx,
                our_ticket_str,
                initial_call_rx,
                initial_accept_task,
            ))
        })?;

    // eframe runs outside block_on — Handle::current().block_on() in egui
    // callbacks won't nest.
    let _guard = rt.enter();
    eframe::run_native(
        "irl call",
        eframe::NativeOptions::default(),
        Box::new(move |cc| {
            crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);

            Ok(Box::new(CallApp {
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
                wgpu_render_state: cc.wgpu_render_state.clone(),
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}
