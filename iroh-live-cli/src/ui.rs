//! Widgets and lifecycle helpers shared by the egui windows.

use std::time::{Duration, Instant};

use eframe::egui;
use iroh_live::{
    Live,
    media::{AudioOutput, LocalBroadcast, Player, PlayerConfig, RenditionMode},
};
use iroh_live_egui::{
    VideoView,
    overlay::{DebugOverlay, StatCategory},
};
use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watcher as _;
use tracing::{info, warn};

use crate::{args::PlaybackArgs, backend::Backend};

/// Returns a player config from `--decoder` and `--latency`, with audio to `output`.
pub fn player_config(args: &PlaybackArgs, output: Option<&AudioOutput>) -> PlayerConfig {
    PlayerConfig {
        decoder: args.decoder.into(),
        latency: args.latency.latency(),
        audio: output.cloned(),
        ..PlayerConfig::default()
    }
}

/// Height of the top bar, in points.
const TOP_BAR_HEIGHT: f32 = 24.0;

/// How long the pointer must sit still before the overlay fades out.
const CURSOR_IDLE: Duration = Duration::from_secs(2);

/// Draws the top bar with `text` and a fullscreen toggle.
///
/// Clicking the bar copies `text` to the clipboard.
pub fn top_bar(ui: &mut egui::Ui, ctx: &egui::Context, text: &str) {
    let content = ctx.content_rect();
    let bar = egui::Rect::from_min_size(content.min, egui::vec2(content.width(), TOP_BAR_HEIGHT));

    let painter = ui.painter_at(bar);
    painter.rect_filled(bar, 0.0, egui::Color32::from_black_alpha(160));
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::monospace(12.0),
        egui::Color32::WHITE,
    );
    painter.galley(bar.min + egui::vec2(8.0, 4.0), galley, egui::Color32::WHITE);

    let response = ui.interact(bar, egui::Id::new("top-bar"), egui::Sense::click());
    if response.clicked() {
        ctx.copy_text(text.to_string());
    }
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    fullscreen_button(ui, ctx, bar);
}

/// Draws the fullscreen toggle at the right end of the top bar.
fn fullscreen_button(ui: &mut egui::Ui, ctx: &egui::Context, bar: egui::Rect) {
    let size = egui::vec2(20.0, 16.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(bar.right() - size.x - 8.0, bar.min.y + 4.0),
        size,
    );
    let response = ui.interact(rect, egui::Id::new("fullscreen"), egui::Sense::click());
    let color = if response.hovered() {
        egui::Color32::from_white_alpha(200)
    } else {
        egui::Color32::from_white_alpha(140)
    };
    ui.painter_at(bar).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "[ ]",
        egui::FontId::proportional(12.0),
        color,
    );
    if response.clicked() {
        let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
    }
}

/// Draws `contents` in a translucent panel pinned under the top bar.
pub fn control_panel(ctx: &egui::Context, id: &str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::LEFT_TOP, [8.0, TOP_BAR_HEIGHT + 4.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180))
                .corner_radius(3.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        contents(ui);
                    });
                });
        });
}

/// Spacing between the items of a [`dialog`], in points.
const DIALOG_SPACING: f32 = 8.0;

/// Draws `contents` in a centred translucent panel, `width` points wide.
///
/// The caller sets the width because an [`egui::Area`] is bounded by the
/// window. Centred content would otherwise stretch the panel across the screen.
pub fn dialog(ctx: &egui::Context, id: &str, width: f32, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200))
                .corner_radius(6.0)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::Vec2::splat(DIALOG_SPACING);
                    ui.set_max_width(width);
                    ui.vertical_centered(contents);
                });
        });
}

/// Hides the overlay and the pointer once the pointer has been still for a while.
#[derive(Debug)]
pub struct CursorIdle {
    visible: bool,
    since: Instant,
}

impl Default for CursorIdle {
    fn default() -> Self {
        Self {
            visible: true,
            since: Instant::now(),
        }
    }
}

impl CursorIdle {
    /// Returns whether to draw the overlay this frame.
    ///
    /// `pinned` keeps it visible, for example while a stats panel is expanded.
    pub fn update(&mut self, ctx: &egui::Context, pinned: bool) -> bool {
        if pinned || ctx.input(|input| input.pointer.delta().length_sq() > 0.0) {
            self.visible = true;
            self.since = Instant::now();
        } else if self.since.elapsed() > CURSOR_IDLE {
            self.visible = false;
        }
        if !self.visible {
            // egui resets the cursor every frame, so set it on every pass.
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        self.visible
    }
}

/// Leaves full screen when Escape is pressed.
///
/// Escape does not close the window, because it is easy to hit by accident.
pub fn escape_leaves_fullscreen(ctx: &egui::Context) {
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
}

/// Closes the egui viewport on Ctrl-C.
///
/// Call this from the eframe creation closure. The task handle is dropped on
/// purpose: an abort-on-drop guard would cancel it when the closure returns.
pub fn spawn_ctrl_c_handler(ctx: &egui::Context) {
    let ctx = ctx.clone();
    tokio::runtime::Handle::current().spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    });
}

/// Repaints the window every `period` while the returned handle is held.
///
/// eframe stops running passes for an unfocused, occluded, or minimized window,
/// because repaints requested inside a pass are never delivered. Windows with
/// off-screen work, such as a call waiting to be answered, need this.
#[must_use = "the heartbeat stops when the handle is dropped"]
pub fn spawn_heartbeat(ctx: &egui::Context, period: Duration) -> AbortOnDropHandle<()> {
    let ctx = ctx.clone();
    AbortOnDropHandle::new(spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            ctx.request_repaint();
        }
    }))
}

/// Shuts the endpoint down from eframe's `on_exit`, outside any async context.
pub fn shutdown_live_blocking(live: &Live) {
    let live = live.clone();
    tokio::runtime::Handle::current().block_on(async move {
        live.shutdown().await;
    });
}

/// Closes `broadcast`, then shuts the endpoint down, from outside async context.
pub fn shutdown_publish_blocking(live: &Live, broadcast: &LocalBroadcast) {
    let live = live.clone();
    let broadcast = broadcast.clone();
    tokio::runtime::Handle::current().block_on(async move {
        broadcast.close();
        broadcast.closed().await;
        live.shutdown().await;
    });
}

/// Returns the window options for a media window.
///
/// Video frames arrive as `wgpu::Texture`s, so the window needs eframe's wgpu
/// renderer. The glow backend cannot draw them.
pub fn native_options(fullscreen: bool) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: iroh_live_egui::create_egui_wgpu_config(),
        viewport: egui::ViewportBuilder::default().with_fullscreen(fullscreen),
        ..Default::default()
    }
}

/// A remote broadcast on screen, with its player and stats overlay.
///
/// Used for the remote of a call and for each tile of a room. Dropping it
/// stops the decoders.
#[derive(Debug)]
pub struct RemoteView {
    player: Player,
    video: VideoView,
    overlay: DebugOverlay,
    /// The decoder the picker last chose, which may differ from the one running.
    decoder: Backend,
    /// The output gain the slider last set.
    volume: f32,
    /// The subscription the broadcast arrives over, for the overlay.
    link: Option<Link>,
}

/// The source of the overlay's transport lines.
#[derive(Debug)]
struct Link {
    subscription: iroh_live::Subscription,
    /// When the lines were last refreshed.
    refreshed: Option<Instant>,
}

/// How often the overlay's link lines are refreshed.
const LINK_REFRESH: Duration = Duration::from_millis(500);

impl Link {
    /// Returns the lines if they are due for a refresh.
    fn refresh(&mut self, now: Instant) -> Option<Vec<String>> {
        if self
            .refreshed
            .is_some_and(|at| now.duration_since(at) < LINK_REFRESH)
        {
            return None;
        }
        self.refreshed = Some(now);
        Some(self.lines())
    }

    /// Returns the overlay's NET lines: the path kind and the arriving bitrate.
    fn lines(&self) -> Vec<String> {
        let Some(serving) = self.subscription.link() else {
            return vec!["no link serves the broadcast".to_string()];
        };
        let link = serving.sample;
        let mut lines = vec![match (serving.kind, link.relayed) {
            (iroh_live::moq::LinkKind::Relay, _) => "via a relay link".to_string(),
            (_, true) => "relayed".to_string(),
            (_, false) => "direct".to_string(),
        }];
        if let Some(bps) = link.goodput_bps {
            lines.push(format!(
                "arriving: {}",
                iroh_live_egui::format_bitrate(iroh_live::media::Bitrate::from_bps(bps))
            ));
        }
        lines
    }
}

impl RemoteView {
    /// Creates a view onto `player`, drawing through `render_state`.
    ///
    /// `name` salts the texture and widget ids, so each tile needs its own.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        player: Player,
        decoder: Backend,
        render_state: Option<&iroh_live_egui::egui_wgpu::RenderState>,
    ) -> Self {
        let video = VideoView::new(ctx, name, player.video(), render_state);
        Self {
            player,
            video,
            overlay: DebugOverlay::new(&[
                StatCategory::Net,
                StatCategory::Render,
                StatCategory::Audio,
                StatCategory::Time,
            ]),
            decoder,
            volume: 1.0,
            link: None,
        }
    }

    /// Adds link lines for `subscription` to the overlay.
    pub fn with_link(mut self, subscription: iroh_live::Subscription) -> Self {
        self.link = Some(Link {
            subscription,
            refreshed: None,
        });
        self
    }

    /// Returns whether the stats overlay is expanded.
    pub fn overlay_expanded(&self) -> bool {
        self.overlay.any_expanded()
    }

    /// Sets the rendition mode.
    pub fn set_rendition(&mut self, mode: RenditionMode) {
        info!(?mode, "rendition mode");
        self.player.set_rendition(mode);
    }

    /// Switches the video decoder to `choice`.
    ///
    /// The old decoder keeps the picture up until the new one catches up.
    pub fn set_decoder(&mut self, choice: Backend) {
        self.decoder = choice;
        info!(decoder = %choice, "decoder selected");
        self.player.set_decoder(choice.into());
    }

    /// Draws the picture at `size`, or a placeholder when there is no video.
    ///
    /// Pass the rect of the response to [`draw_overlay`](Self::draw_overlay).
    pub fn draw(&mut self, ui: &mut egui::Ui, size: egui::Vec2) -> egui::Response {
        ui.add_sized(size, self.video.render())
    }

    /// Draws the stats overlay over `rect`.
    pub fn draw_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let stats = self.player.stats();
        let status = self.player.status().get();
        if let Some(lines) = self
            .link
            .as_mut()
            .and_then(|link| link.refresh(Instant::now()))
        {
            self.overlay.set_link(lines);
        }
        // Copy the timeline only while the TIME panel is open.
        let timeline = if self.overlay.timeline_open() {
            self.player.timeline()
        } else {
            Vec::new()
        };
        self.overlay
            .show_playback(ui, rect, &stats, &status, &timeline);
    }

    /// Draws the rendition and decoder pickers and the volume slider.
    ///
    /// `id` salts the widget ids, so each tile needs its own.
    pub fn controls(&mut self, ui: &mut egui::Ui, id: &str) {
        let status = self.player.status().get();
        let catalog = self.player.broadcast().catalog().get();
        let Some(catalog) = catalog.filter(|catalog| !catalog.video.renditions.is_empty()) else {
            ui.label("no video");
            return;
        };
        let rendition = status.rendition.clone().unwrap_or_default();
        let running = status.decoder.clone().unwrap_or_default();

        ui.label("Rendition");
        let label = match &status.mode {
            RenditionMode::Pinned(name) => name.clone(),
            _ => format!("Auto ({rendition})"),
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(format!("{id}-rendition"))
            .selected_text(label)
            .show_ui(ui, |ui| {
                let auto = matches!(status.mode, RenditionMode::Auto { .. });
                if ui.selectable_label(auto, "Auto").clicked() {
                    chosen = Some(RenditionMode::auto());
                }
                for (name, config) in catalog.ranked_video() {
                    let pinned = status.mode == RenditionMode::pinned(name);
                    let text = config.label.as_deref().unwrap_or(name);
                    if ui.selectable_label(pinned, text).clicked() {
                        chosen = Some(RenditionMode::pinned(name));
                    }
                }
            });
        if let Some(mode) = chosen {
            self.set_rendition(mode);
        }

        ui.label("Decoder");
        // Show the running backend next to the choice when they differ, as
        // with `Auto` or a named backend that failed to open.
        let label = if self.decoder.to_string() == running {
            running
        } else {
            format!("{} ({running})", self.decoder)
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(format!("{id}-decoder"))
            .selected_text(label)
            .show_ui(ui, |ui| {
                for candidate in Backend::decoders() {
                    let selected = self.decoder == candidate;
                    if ui
                        .selectable_label(selected, candidate.to_string())
                        .clicked()
                    {
                        chosen = Some(candidate);
                    }
                }
            });
        if let Some(choice) = chosen {
            self.set_decoder(choice);
        }

        if self.player.stats().audio.is_some() {
            ui.label("Volume");
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false))
                .changed()
            {
                self.player.set_volume(self.volume);
            }
        }
    }
}

/// Quiet zone around a rendered QR code, in modules.
///
/// The QR standard requires four. Without it, a camera finds the code but
/// cannot read it.
const QR_QUIET: usize = 4;

/// A ticket drawn as a QR code, for a peer to scan off this screen.
///
/// The texture holds one pixel per module and is magnified nearest-neighbour,
/// so edges stay hard at any size. It is black on white regardless of theme,
/// because QR codes only read dark on light.
#[derive(derive_more::Debug)]
pub struct TicketQr {
    #[debug(skip)]
    texture: egui::TextureHandle,
}

impl TicketQr {
    /// Renders `ticket` as a QR code.
    ///
    /// `id` names the texture, so each code in a window needs its own. Returns
    /// `None` and logs a warning if the ticket does not fit in a QR code.
    pub fn new(ctx: &egui::Context, id: &str, ticket: &str) -> Option<Self> {
        let pixels = QrPixels::render(ticket)
            .inspect_err(|err| warn!(error = %err, "could not render the ticket QR code"))
            .ok()?;
        let image = egui::ColorImage::from_gray([pixels.side, pixels.side], &pixels.gray);
        Some(Self {
            texture: ctx.load_texture(id, image, egui::TextureOptions::NEAREST),
        })
    }

    /// Returns the image of the code, to draw at any square size.
    pub fn image(&self) -> egui::Image<'_> {
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

/// A QR code as grayscale pixels, one per module, `side` by `side`, rows packed.
#[derive(Debug)]
struct QrPixels {
    side: usize,
    gray: Vec<u8>,
}

impl QrPixels {
    /// Renders `text` as a QR code with the standard quiet zone around it.
    ///
    /// Fails if `text` does not fit in the largest QR version.
    fn render(text: &str) -> Result<Self, qrcode::types::QrError> {
        let code = qrcode::QrCode::new(text)?;
        let modules = code.width();
        let side = modules + 2 * QR_QUIET;
        let colors = code.to_colors();

        let mut gray = vec![u8::MAX; side * side];
        for row in 0..modules {
            for column in 0..modules {
                if colors[row * modules + column] == qrcode::Color::Dark {
                    gray[(row + QR_QUIET) * side + column + QR_QUIET] = 0;
                }
            }
        }
        Ok(Self { side, gray })
    }
}

#[cfg(test)]
mod tests {
    use iroh_live::BroadcastTicket;

    use super::{QR_QUIET, QrPixels};

    /// Pixels per module in the upscaled test image.
    ///
    /// About what a camera sees of a code filling a third of a 720p frame.
    const MODULE_PIXELS: usize = 8;

    /// Scales `pixels` up by [`MODULE_PIXELS`], as the texture does on screen.
    fn upscale(pixels: &QrPixels) -> QrPixels {
        let side = pixels.side * MODULE_PIXELS;
        let mut gray = vec![u8::MAX; side * side];
        for (index, value) in pixels.gray.iter().enumerate() {
            let top = index / pixels.side * MODULE_PIXELS;
            let left = index % pixels.side * MODULE_PIXELS;
            for y in top..top + MODULE_PIXELS {
                gray[y * side + left..y * side + left + MODULE_PIXELS].fill(*value);
            }
        }
        QrPixels { side, gray }
    }

    /// Decodes the first QR code in `pixels` with `rqrr`.
    fn decode(pixels: &QrPixels) -> Option<String> {
        let side = pixels.side;
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
            pixels.gray[y * side + x]
        });
        prepared
            .detect_grids()
            .into_iter()
            .find_map(|grid| grid.decode().ok())
            .map(|(_meta, text)| text)
    }

    /// A rendered ticket decodes back to the same ticket.
    #[test]
    fn a_call_ticket_survives_the_round_trip_through_the_rendered_code() {
        let id = iroh::SecretKey::generate().public();
        let ticket = BroadcastTicket::new(id, "call");
        let pixels = QrPixels::render(&ticket.to_string()).expect("a ticket fits in a QR code");
        let text = decode(&upscale(&pixels)).expect("the code is there to be found");
        assert_eq!(
            text.parse::<BroadcastTicket>()
                .expect("it decoded as rendered"),
            ticket
        );
    }

    /// A rendered code keeps a white quiet zone around it.
    #[test]
    fn a_rendered_code_keeps_the_quiet_zone_a_decoder_relies_on() {
        let pixels = QrPixels::render("iroh-live:hello").expect("the text fits");
        for row in 0..pixels.side {
            for column in 0..pixels.side {
                let inside = (QR_QUIET..pixels.side - QR_QUIET).contains(&row)
                    && (QR_QUIET..pixels.side - QR_QUIET).contains(&column);
                if inside {
                    continue;
                }
                assert_eq!(
                    pixels.gray[row * pixels.side + column],
                    u8::MAX,
                    "row {row}, column {column} is inside the quiet zone"
                );
            }
        }
    }
}
