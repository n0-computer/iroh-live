//! Shared pieces of the egui windows: a top bar, a floating control panel,
//! cursor auto-hide, the local preview and remote-broadcast widgets, and the
//! lifecycle helpers every window needs.

use std::time::{Duration, Instant};

use clap::ValueEnum;
use eframe::egui;
use iroh_live::{
    Live,
    media::{
        AudioOutput, Latency, LocalBroadcast, Player, PlayerConfig, RenditionMode, VideoFrames,
    },
};
use iroh_live_egui::{
    FrameView, VideoView,
    overlay::{DebugOverlay, StatCategory},
};
use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watcher as _;
use tracing::{info, warn};

use crate::{args::PlaybackArgs, backend::DecoderArg};

/// The player config a window wants: the decoder `--decoder` asked for, the
/// latency `--latency` names, and audio through `output`.
pub fn player_config(args: &PlaybackArgs, output: Option<&AudioOutput>) -> PlayerConfig {
    let mut config = PlayerConfig::default()
        .with_decoder(args.decoder.into())
        .with_latency(Latency::range(
            args.latency.jitter(),
            args.latency.max_latency(),
        ));
    if let Some(output) = output {
        config = config.with_audio(output);
    }
    config
}

/// Height of the top bar, in points.
const TOP_BAR_HEIGHT: f32 = 24.0;

/// How long the pointer must sit still before the overlay fades out.
const CURSOR_IDLE: Duration = Duration::from_secs(2);

/// Draws the top bar: the ticket, which copies to the clipboard when clicked,
/// and a fullscreen toggle.
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
    let color = match response.hovered() {
        true => egui::Color32::from_white_alpha(200),
        false => egui::Color32::from_white_alpha(140),
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

/// Draws `contents` in a column `width` points wide, in a translucent panel in
/// the middle of the window.
///
/// The counterpart of [`control_panel`] for a screen with nothing yet to
/// control: what a window says while it waits for a connection, drawn over
/// whatever picture is behind it rather than in place of one.
///
/// The width is the caller's because an [`egui::Area`] is bounded by the window
/// rather than by its own contents, and a column centred inside that would be
/// one the panel stretched across the screen to hold.
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

/// Hides the overlay, and the pointer with it, once the pointer has been still
/// for a while.
///
/// The pointer goes too because a still mouse arrow sitting over a picture is
/// the thing every video player learned to hide, and leaving it there while the
/// controls fade out looks like the controls broke rather than withdrew.
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
    /// Reports whether the overlay should be drawn this frame.
    ///
    /// `pinned` keeps it up regardless, which is what an expanded stats panel
    /// wants: it would otherwise vanish while being read.
    pub fn update(&mut self, ctx: &egui::Context, pinned: bool) -> bool {
        if pinned || ctx.input(|input| input.pointer.delta().length_sq() > 0.0) {
            self.visible = true;
            self.since = Instant::now();
        } else if self.since.elapsed() > CURSOR_IDLE {
            self.visible = false;
        }
        if !self.visible {
            // Set every pass rather than once on the way out: egui resolves the
            // cursor from what this frame asked for, so a single call would be
            // undone by the next frame that asked for nothing.
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        self.visible
    }
}

/// Leaves full screen when Escape is pressed, and does nothing otherwise.
///
/// Only leaves. Escape is what a full-screen picture trains people to press,
/// but a window is not something to close on it: a player that quit on Escape
/// would throw away a stream that took a ticket to reach, and the key is easy
/// to hit by accident.
///
/// The command goes out without asking whether the window is full screen
/// already. Leaving a window that is not full screen does nothing, and reading
/// the state first would only add a way to be wrong about it.
pub fn escape_leaves_fullscreen(ctx: &egui::Context) {
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
}

/// Closes the egui viewport on Ctrl-C.
///
/// Call this from the eframe creation closure. The task ends when the signal
/// fires, so its handle is deliberately dropped rather than held: an
/// abort-on-drop guard would cancel it as the closure returns.
pub fn spawn_ctrl_c_handler(ctx: &egui::Context) {
    let ctx = ctx.clone();
    tokio::runtime::Handle::current().spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    });
}

/// Wakes the window on a fixed interval for as long as the returned handle is
/// held.
///
/// eframe runs a pass only when something asks it to, and a window that is
/// unfocused, occluded, or minimized stops asking: a repaint requested from
/// inside a pass never comes back around, so the pass that would have asked
/// again never happens. A window whose work continues off screen, such as a
/// call waiting to be answered, needs the loop to keep turning regardless of
/// what the compositor thinks.
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

/// Shuts the endpoint down from `on_exit`, which eframe calls on the main
/// thread outside any async context.
pub fn shutdown_live_blocking(live: &Live) {
    let live = live.clone();
    tokio::runtime::Handle::current().block_on(async move {
        live.shutdown().await;
    });
}

/// Finishes a local publication before shutting its transport down.
pub fn shutdown_publish_blocking(live: &Live, broadcast: &LocalBroadcast) {
    let live = live.clone();
    let broadcast = broadcast.clone();
    tokio::runtime::Handle::current().block_on(async move {
        broadcast.close();
        broadcast.closed().await;
        live.shutdown().await;
    });
}

/// The window options every media window here wants.
///
/// eframe's wgpu renderer, configured the way `iroh-live-egui`'s video
/// renderer needs it: a video frame arrives as a `wgpu::Texture` and there is
/// no path that draws one through the glow backend.
pub fn native_options(fullscreen: bool) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: iroh_live_egui::create_egui_wgpu_config(),
        viewport: egui::ViewportBuilder::default().with_fullscreen(fullscreen),
        ..Default::default()
    }
}

/// The publisher's own picture, drawn from the frames its source captures.
///
/// Costs no extra decode: the frames are the source's own, read through a
/// handle of their own. A source switch hands the preview the new source's
/// frames with [`set_frames`](Self::set_frames).
#[derive(Debug)]
pub struct LocalPreview {
    view: FrameView,
    frames: Option<VideoFrames>,
    /// The window to wake, kept so [`set_frames`](Self::set_frames) can wake
    /// it for the replacement.
    ctx: egui::Context,
    /// Wakes the window when a captured frame arrives, so the preview keeps
    /// moving in a window that has nothing else to repaint for.
    _wake: Option<AbortOnDropHandle<()>>,
}

impl LocalPreview {
    /// Creates a preview of `frames` that draws through `render_state`, if one
    /// is available.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        frames: Option<VideoFrames>,
        render_state: Option<&iroh_live_egui::egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            view: FrameView::new_wgpu(ctx, name, render_state),
            _wake: wake_on_frame(ctx, frames.as_ref()),
            frames,
            ctx: ctx.clone(),
        }
    }

    /// Points the preview at another source's frames, or at nothing.
    pub fn set_frames(&mut self, frames: Option<VideoFrames>) {
        self._wake = wake_on_frame(&self.ctx, frames.as_ref());
        self.frames = frames;
    }

    /// Draws the newest captured frame, if one arrived since the last call.
    ///
    /// Requests a repaint when it did, so the picture advances without waiting
    /// for the next input event.
    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(frames) = self.frames.as_mut()
            && let Some(frame) = frames.try_next()
        {
            self.view.render_frame(&frame);
            ctx.request_repaint();
        }
    }

    /// Returns the image for whatever frame was drawn last.
    pub fn image(&self) -> egui::Image<'_> {
        self.view.image()
    }
}

/// Asks the window to draw whenever `frames` has a new picture.
///
/// Reads through a handle of its own, so it never takes a picture from the
/// drawing pass.
fn wake_on_frame(
    ctx: &egui::Context,
    frames: Option<&VideoFrames>,
) -> Option<AbortOnDropHandle<()>> {
    let ctx = ctx.clone();
    let mut frames = frames?.clone();
    Some(AbortOnDropHandle::new(spawn(async move {
        while frames.next().await.is_some() {
            ctx.request_repaint();
        }
    })))
}

/// One remote broadcast on screen.
///
/// Owns the player, which decodes the picture and plays the sound, and the
/// stats overlay drawn over the frame. Both the single remote of a call and
/// every tile of a room grid are one of these.
///
/// Dropping it stops the decoders.
#[derive(Debug)]
pub struct RemoteView {
    player: Player,
    video: VideoView,
    overlay: DebugOverlay,
    /// The decoder the picker last asked for, which is not necessarily the one
    /// running: `Auto` names a strategy, and a backend that fails to open leaves
    /// the incumbent playing.
    decoder: DecoderArg,
    /// The output gain the slider last set.
    volume: f32,
    /// The subscription the broadcast arrives over, for the overlay's link
    /// lines.
    link: Option<Link>,
}

/// What the overlay says about the transport, read off the serving link.
#[derive(Debug)]
struct Link {
    subscription: iroh_live::Subscription,
    /// When the lines were last read off the link.
    refreshed: Option<Instant>,
}

/// How often the overlay's link lines are read off the link: they change
/// with the path, not with every frame drawn.
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

    /// The selected path's kind and address, the number of paths, and the
    /// bytes arriving, as the overlay's NET lines.
    ///
    /// Follows the route: a relay link has no iroh path to describe, so it
    /// says which link serves and what arrives over it.
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
        if let Some(addr) = &link.remote_addr {
            lines.push(format!("address: {addr}"));
        }
        if link.paths > 0 {
            lines.push(format!("paths: {}", link.paths));
        }
        if let Some(bps) = link.goodput_bps {
            lines.push(format!(
                "arriving: {}",
                iroh_live_egui::format_bitrate(bps as f64)
            ));
        }
        lines
    }
}

impl RemoteView {
    /// Opens a view onto `player`, drawing through `render_state`.
    ///
    /// `name` salts the texture and the widget ids, so a grid of these needs a
    /// distinct one per tile.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        player: Player,
        decoder: DecoderArg,
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

    /// Returns the view with the overlay describing the path of whichever
    /// session serves `subscription`, and the bytes arriving over it.
    pub fn with_link(mut self, subscription: iroh_live::Subscription) -> Self {
        self.link = Some(Link {
            subscription,
            refreshed: None,
        });
        self
    }

    /// Reports whether the stats overlay is expanded, which keeps the
    /// controls up while it is being read.
    pub fn overlay_expanded(&self) -> bool {
        self.overlay.any_expanded()
    }

    /// Chooses how the rendition is picked.
    pub fn set_rendition(&mut self, mode: RenditionMode) {
        info!(?mode, "rendition mode");
        self.player.set_rendition(mode);
    }

    /// Points the video decoder at `choice`.
    ///
    /// The replacement opens alongside the incumbent and takes over once it has
    /// caught up, leaving the picture up across the change.
    pub fn set_decoder(&mut self, choice: DecoderArg) {
        self.decoder = choice;
        info!(decoder = %choice, "decoder selected");
        self.player.set_decoder(choice.into());
    }

    /// Draws the picture at `size`, or a placeholder while the peer sends no
    /// video.
    ///
    /// Returns the response of whatever was drawn, whose rect is what
    /// [`draw_overlay`](Self::draw_overlay) wants.
    pub fn draw(&mut self, ui: &mut egui::Ui, size: egui::Vec2) -> egui::Response {
        let (image, _) = self.video.render(size);
        ui.add_sized(size, image)
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
        // Copied out only while the TIME panel is open to draw it.
        let timeline = match self.overlay.timeline_open() {
            true => self.player.timeline(),
            false => Vec::new(),
        };
        self.overlay
            .show_playback(ui, rect, &stats, &status, &timeline);
    }

    /// Draws the rendition and decoder pickers and the volume slider.
    ///
    /// `id` salts the widget ids, so a grid of these needs a distinct one per
    /// tile.
    pub fn controls(&mut self, ui: &mut egui::Ui, id: &str) {
        let status = self.player.status().get();
        let catalog = self.player.broadcast().catalog().get();
        let Some(catalog) = catalog.filter(|catalog| !catalog.video().is_empty()) else {
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
                for info in catalog.video() {
                    let pinned = status.mode == RenditionMode::pinned(info.name.clone());
                    let text = info.label.clone().unwrap_or_else(|| info.name.clone());
                    if ui.selectable_label(pinned, text).clicked() {
                        chosen = Some(RenditionMode::pinned(info.name.clone()));
                    }
                }
            });
        if let Some(mode) = chosen {
            self.set_rendition(mode);
        }

        ui.label("Decoder");
        // The backend that opened, next to the choice that asked for it: `Auto`
        // never names one, and a named backend that would not open leaves a
        // different one running, which is the case worth seeing.
        let label = match self.decoder.to_string() == running {
            true => running,
            false => format!("{} ({running})", self.decoder),
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(format!("{id}-decoder"))
            .selected_text(label)
            .show_ui(ui, |ui| {
                for candidate in DecoderArg::value_variants() {
                    let selected = self.decoder == *candidate;
                    if ui
                        .selectable_label(selected, candidate.to_string())
                        .clicked()
                    {
                        chosen = Some(*candidate);
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
/// Four is what the QR standard asks for, and a decoder is entitled to rely on
/// it. A code drawn flush against whatever is behind it is one a camera finds
/// the grid of and then cannot read.
const QR_QUIET: usize = 4;

/// A ticket drawn as a QR code, for a peer to read off this screen.
///
/// The texture holds one pixel per module and is magnified nearest-neighbour,
/// so the code has hard edges at whatever size it is drawn and resizing the
/// window costs no re-render. Its pixels are opaque black and white rather than
/// themed: a QR code reads dark on light and nothing else.
pub struct TicketQr {
    texture: egui::TextureHandle,
}

impl std::fmt::Debug for TicketQr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketQr")
            .field("modules", &self.texture.size())
            .finish()
    }
}

impl TicketQr {
    /// Renders `ticket` as a QR code.
    ///
    /// `id` names the texture, so two of these in one window need distinct
    /// ones.
    ///
    /// Returns `None` if the text does not fit in a QR code, which for a ticket
    /// means something has gone wrong upstream: the largest version holds
    /// nearly three kilobytes and a ticket is around a hundred bytes. A window
    /// without a code to show is still a window, so this is reported in the log
    /// rather than to the caller.
    pub fn new(ctx: &egui::Context, id: &str, ticket: &str) -> Option<Self> {
        let pixels = QrPixels::render(ticket)
            .inspect_err(|err| warn!(error = %err, "could not render the ticket QR code"))
            .ok()?;
        let image = egui::ColorImage::from_gray([pixels.side, pixels.side], &pixels.gray);
        Some(Self {
            texture: ctx.load_texture(id, image, egui::TextureOptions::NEAREST),
        })
    }

    /// Returns the image for the code, which the caller draws at whatever
    /// square size it has room for.
    pub fn image(&self) -> egui::Image<'_> {
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

/// A QR code as grayscale pixels: `side` by `side`, one byte per pixel, rows
/// tightly packed.
///
/// One pixel per module, because that is all the information there is. What
/// scales it up to something a camera can read is the texture sampler, which
/// magnifies nearest-neighbour and so draws every module as a hard-edged
/// square.
#[derive(Debug)]
struct QrPixels {
    side: usize,
    gray: Vec<u8>,
}

impl QrPixels {
    /// Renders `text` as a QR code with the standard quiet zone around it.
    ///
    /// # Errors
    ///
    /// Fails if `text` is longer than the largest QR version holds.
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
    /// Roughly what a camera sees of a code filling a third of a 720p frame,
    /// and enough that `rqrr`'s binarization has clean edges to lock onto.
    const MODULE_PIXELS: usize = 8;

    /// Repeats every pixel of `pixels` [`MODULE_PIXELS`] times in both
    /// directions, which is what the texture sampler does to the code on its
    /// way to the screen.
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

    /// Reads the first QR code in `pixels`, the way the scan camera does.
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

    /// The whole point of drawing the code: the peer's `irl call --scan`
    /// equivalent reads it back off a camera. A call ticket is the longest one
    /// this CLI shows, because its broadcast name is an endpoint id.
    #[test]
    fn a_call_ticket_survives_the_round_trip_through_the_rendered_code() {
        let id = iroh::SecretKey::generate().public();
        let ticket = BroadcastTicket::new(id, format!("calls/{id}"));
        let pixels = QrPixels::render(&ticket.to_string()).expect("a ticket fits in a QR code");
        let text = decode(&upscale(&pixels)).expect("the code is there to be found");
        assert_eq!(
            text.parse::<BroadcastTicket>()
                .expect("it decoded as rendered"),
            ticket
        );
    }

    /// A code whose quiet zone is drawn over is one a camera locates and then
    /// fails to read, which looks exactly like pointing it at nothing.
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
