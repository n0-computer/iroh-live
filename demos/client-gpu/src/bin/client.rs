//! The client: publishes video to the gpu node and shows what comes back.
//!
//! The window shows the local video next to the returned video, the controls
//! for the gpu filter and the transport, and the latency from capture to the
//! returned picture being drawn here.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use clap::Parser;
use client_gpu_demo::{
    filter::Filter,
    latency::LatencyStats,
    pipeline::{Preset, Source},
    session::{Congestion, Session, Settings, Wake},
};
use eframe::egui;
use iroh::EndpointId;
use iroh_live_egui::FrameView;
use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::{runtime::Handle, sync::oneshot};

/// Command line of the client.
#[derive(Debug, Parser)]
struct Args {
    /// The endpoint id the gpu node printed.
    gpu: EndpointId,
    /// Runs without a window for this many seconds and prints latency statistics.
    ///
    /// The latency then ends when the returned frame is read, not drawn.
    #[arg(long, value_name = "SECONDS")]
    headless: Option<u64>,
    /// The initial gpu filter: none, invert or blur.
    #[arg(long, default_value = "none", value_parser = parse_filter)]
    filter: Filter,
}

fn parse_filter(name: &str) -> Result<Filter, &'static str> {
    Filter::parse(name).ok_or("expected none, invert or blur")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    match args.headless {
        Some(seconds) => runtime.block_on(headless(
            args.gpu,
            args.filter,
            Duration::from_secs(seconds),
        )),
        None => window(runtime.handle().clone(), args.gpu, args.filter),
    }
}

/// Connects, and prints the latency statistics once a second.
async fn headless(gpu: EndpointId, filter: Filter, duration: Duration) -> Result<()> {
    let settings = Settings {
        filter,
        ..Settings::default()
    };
    let mut session = Session::connect(gpu, &settings, Arc::new(|| {})).await?;
    let mut stats = LatencyStats::default();
    let (start, mut printed) = (Instant::now(), Instant::now());
    while start.elapsed() < duration {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if let Some(frame) = session.next_returned()
            && let Some(latency) = session.latency_of(&frame)
        {
            stats.push(latency);
        }
        if printed.elapsed() >= Duration::from_secs(1) {
            println!("{}", stats.summary());
            printed = Instant::now();
        }
    }
    session.shutdown().await;
    Ok(())
}

fn window(runtime: Handle, gpu: EndpointId, filter: Filter) -> Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: iroh_live_egui::create_egui_wgpu_config(),
        ..Default::default()
    };
    eframe::run_native(
        "client-gpu",
        options,
        Box::new(move |cc| {
            let mut app = App {
                runtime,
                gpu,
                ctx: cc.egui_ctx.clone(),
                settings: Settings {
                    filter,
                    ..Settings::default()
                },
                state: State::Failed(String::new()),
                local: FrameView::new(&cc.egui_ctx, "local", cc.wgpu_render_state.as_ref()),
                returned: FrameView::new(&cc.egui_ctx, "returned", cc.wgpu_render_state.as_ref()),
                stats: LatencyStats::default(),
                notice: None,
            };
            app.connect();
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow!("eframe failed: {err:#}"))
}

/// Where the connection stands.
enum State {
    Connecting {
        rx: oneshot::Receiver<Result<Session>>,
        _task: AbortOnDropHandle<()>,
    },
    Running(Box<Session>),
    Failed(String),
}

struct App {
    runtime: Handle,
    gpu: EndpointId,
    ctx: egui::Context,
    /// The values the controls edit.
    settings: Settings,
    state: State,
    local: FrameView,
    returned: FrameView,
    stats: LatencyStats,
    notice: Option<String>,
}

impl App {
    /// Starts a session with the current settings, replacing any running one.
    fn connect(&mut self) {
        if let State::Running(session) =
            std::mem::replace(&mut self.state, State::Failed(String::new()))
        {
            self.runtime.spawn(session.shutdown());
        }
        self.stats = LatencyStats::default();
        let (tx, rx) = oneshot::channel();
        let (gpu, settings, ctx) = (self.gpu, self.settings.clone(), self.ctx.clone());
        let task = spawn(async move {
            let wake: Wake = {
                let ctx = ctx.clone();
                Arc::new(move || ctx.request_repaint())
            };
            tx.send(Session::connect(gpu, &settings, wake).await).ok();
            ctx.request_repaint();
        });
        self.state = State::Connecting {
            rx,
            _task: AbortOnDropHandle::new(task),
        };
    }

    /// Takes in what arrived: the connection, new pictures, latency samples.
    fn poll(&mut self) {
        match &mut self.state {
            State::Connecting { rx, .. } => {
                if let Ok(result) = rx.try_recv() {
                    self.state = match result {
                        Ok(session) => State::Running(Box::new(session)),
                        Err(err) => State::Failed(format!("{err:#}")),
                    };
                }
            }
            State::Running(session) => {
                if let Some(err) = session.poll() {
                    self.notice = Some(err);
                }
                if let Some(frame) = session.next_local() {
                    self.local.render_frame(&frame);
                }
                if let Some(frame) = session.next_returned() {
                    self.returned.render_frame(&frame);
                    // The frame is on the GPU as far as this process can tell.
                    if let Some(latency) = session.latency_of(&frame) {
                        self.stats.push(latency);
                    }
                }
            }
            State::Failed(_) => {}
        }
    }

    fn session(&mut self) -> Option<&mut Session> {
        match &mut self.state {
            State::Running(session) => Some(session),
            _ => None,
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("client-gpu");
        ui.label(format!("gpu node {}", self.gpu.fmt_short()));
        match &self.state {
            State::Connecting { .. } => ui.label("connecting ..."),
            State::Running(session) => {
                ui.label(format!("connected as {}", session.id().fmt_short()))
            }
            State::Failed(err) => ui.colored_label(egui::Color32::LIGHT_RED, err),
        };
        if let Some(notice) = self.notice.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::YELLOW, notice);
                if ui.small_button("x").clicked() {
                    self.notice = None;
                }
            });
        }
        let mut settings = self.settings.clone();

        ui.separator();
        ui.strong("GPU filter (live)");
        ui.horizontal(|ui| {
            for filter in Filter::ALL {
                ui.radio_value(&mut settings.filter, filter, filter.name());
            }
        });
        if settings.filter != self.settings.filter
            && let Some(session) = self.session()
        {
            session.set_filter(settings.filter);
        }

        ui.separator();
        ui.strong("Video (live, restarts the source)");
        egui::ComboBox::from_label("source")
            .selected_text(settings.video.source.label())
            .show_ui(ui, |ui| {
                for source in Source::ALL {
                    ui.selectable_value(&mut settings.video.source, source, source.label());
                }
            });
        let preset = settings.video.preset;
        egui::ComboBox::from_label("rendition")
            .selected_text(preset.label())
            .show_ui(ui, |ui| {
                for preset in Preset::ALL {
                    ui.selectable_value(&mut settings.video.preset, preset, preset.label());
                }
            });
        if settings.video.preset != preset {
            settings.video.kbps = settings.video.preset.size_and_kbps().1;
        }
        ui.add(
            egui::DragValue::new(&mut settings.video.kbps)
                .range(50..=20_000)
                .suffix(" kbit/s"),
        );
        let apply_video = ui
            .add_enabled(
                matches!(self.state, State::Running(_)),
                egui::Button::new("Apply video"),
            )
            .clicked();

        ui.separator();
        ui.strong("Returned video latency (live)");
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut settings.latency_min_ms)
                    .range(0..=2000)
                    .prefix("min "),
            );
            ui.add(
                egui::DragValue::new(&mut settings.latency_max_ms)
                    .range(0..=5000)
                    .prefix("max "),
            );
            ui.label("ms");
        });
        let apply_latency = ui.button("Apply latency").clicked();

        ui.separator();
        ui.strong("Congestion controller (reconnects)");
        egui::ComboBox::from_label("controller")
            .selected_text(settings.congestion.label())
            .show_ui(ui, |ui| {
                for cc in Congestion::ALL {
                    ui.selectable_value(&mut settings.congestion, cc, cc.label());
                }
            });
        let reconnect = ui.button("Reconnect").clicked();

        self.settings = settings.clone();
        if apply_video && let Some(session) = self.session() {
            session.restart_video(settings.video.clone());
        }
        if apply_latency
            && let Some(session) = self.session()
            && let Err(err) = session.set_latency(&settings)
        {
            self.notice = Some(format!("{err:#}"));
        }
        if reconnect {
            self.connect();
        }
    }

    fn videos(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |columns| {
            for (column, title, view) in [
                (0, "LOCAL (as sent, with frame id)", &self.local),
                (1, "RETURNED (filtered by the gpu node)", &self.returned),
            ] {
                let ui = &mut columns[column];
                ui.label(title);
                let width = ui.available_width();
                ui.add(
                    view.image()
                        .fit_to_exact_size(egui::vec2(width, width * 9.0 / 16.0)),
                );
            }
        });
        ui.separator();
        ui.label(format!(
            "capture to returned frame drawn: {}",
            self.stats.summary()
        ));
        sparkline(ui, &self.stats);
    }
}

/// Draws the samples as a line, with the median and the 95th percentile.
fn sparkline(ui: &mut egui::Ui, stats: &LatencyStats) {
    let size = egui::vec2(ui.available_width(), 120.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(24));
    let top = stats.samples().fold(50.0f32, f32::max);
    let y = |ms: f32| rect.bottom() - ms / top * rect.height();
    let n = stats.len();
    let points: Vec<egui::Pos2> = stats
        .samples()
        .enumerate()
        .map(|(i, ms)| {
            let x = rect.left() + i as f32 / (n - 1).max(1) as f32 * rect.width();
            egui::pos2(x, y(ms))
        })
        .collect();
    for (percentile, color) in [(50.0, egui::Color32::GRAY), (95.0, egui::Color32::ORANGE)] {
        if let Some(ms) = stats.percentile(percentile) {
            let stroke = egui::Stroke::new(1.0, color);
            painter.hline(rect.x_range(), y(ms), stroke);
        }
    }
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, egui::Color32::LIGHT_GREEN),
    ));
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        format!("{top:.0} ms   gray p50, orange p95"),
        egui::FontId::monospace(11.0),
        egui::Color32::WHITE,
    );
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Frames and sessions wake the window. This keeps the status fresh.
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        self.poll();
        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(260.0)
            .show(ui, |ui| self.controls(ui));
        egui::CentralPanel::default().show(ui, |ui| self.videos(ui));
    }

    fn on_exit(&mut self) {
        if let State::Running(session) =
            std::mem::replace(&mut self.state, State::Failed(String::new()))
        {
            self.runtime.block_on(session.shutdown());
        }
    }
}
