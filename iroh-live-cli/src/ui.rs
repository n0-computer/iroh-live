//! Shared pieces of the egui windows: a top bar, a floating control panel,
//! cursor auto-hide, the local preview and remote-broadcast widgets, and the
//! lifecycle helpers every window needs.

use std::time::{Duration, Instant};

use eframe::egui;
use iroh_live::{
    Live,
    media::{
        net::NetworkSignals,
        publish::LocalBroadcast,
        subscribe::{AudioTrack, MediaTracks, RemoteBroadcast},
    },
};
use moq_media_egui::{
    FrameView, VideoTrackView,
    overlay::{DebugOverlay, StatCategory},
};
use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::watch;
use tracing::info;

/// Asks `broadcast` for GPU-resident frames, which is right for every window in
/// this CLI: they all draw what they decode and none of them reads a pixel.
///
/// Call it before opening the video track. A decoder reads the policy when it is
/// built rather than watching it afterwards, so setting it later has no effect
/// until a rendition switch rebuilds the decoder.
///
/// What it buys is the download: a hardware decoder that can share its decode
/// surface hands one over and the renderer imports it, so a full frame is not
/// copied out of the GPU and straight back in. `irl record` is the other side of
/// the choice and does not ask, since it never decodes at all.
pub fn draw_without_downloading(broadcast: &RemoteBroadcast) {
    broadcast.set_playback_policy(broadcast.playback_policy().with_gpu_frames(true));
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

/// Hides the overlay once the pointer has been still for a while.
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
        self.visible
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

/// The window options every media window here wants.
///
/// eframe's wgpu renderer, configured the way `moq-media-egui`'s video
/// renderer needs it: a video frame arrives as a `wgpu::Texture` and there is
/// no path that draws one through the glow backend.
pub fn native_options(fullscreen: bool) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: moq_media_egui::create_egui_wgpu_config(),
        viewport: egui::ViewportBuilder::default().with_fullscreen(fullscreen),
        ..Default::default()
    }
}

/// The publisher's own picture, drawn from the frames already on their way to
/// the encoders.
///
/// Costs no extra decode: [`LocalBroadcast::preview`] taps the capture output
/// before the encoder sees it. The tap is replaced whenever the source is, so
/// [`update`](Self::update) reads it fresh every frame rather than holding a
/// receiver that a source switch would silently orphan.
#[derive(Debug)]
pub struct LocalPreview {
    view: FrameView,
}

impl LocalPreview {
    /// Creates a preview that draws through `render_state`, if one is
    /// available.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        render_state: Option<&moq_media_egui::egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            view: FrameView::new_wgpu(ctx, name, render_state),
        }
    }

    /// Draws the newest captured frame, if one arrived since the last call.
    ///
    /// Requests a repaint when it did, so the picture advances without waiting
    /// for the next input event.
    pub fn update(&mut self, ctx: &egui::Context, broadcast: &LocalBroadcast) {
        if let Some(frames) = broadcast.preview()
            && let Some(frame) = frames.take()
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

/// The rendition a viewer asked for, as distinct from the one decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenditionChoice {
    /// Follow the downlink: the transport signals drive `moq-media`'s
    /// adaptation, which swaps renditions without the picture going blank.
    Auto,
    /// Hold one rendition whatever the downlink does.
    Pinned(String),
}

/// One remote broadcast on screen.
///
/// Owns the decoded picture, the audio track that keeps playing while the
/// window draws, and the stats overlay drawn over the frame. Both the single
/// remote of a call and every tile of a room grid are one of these.
///
/// Dropping it stops the decoders; [`shutdown`](Self::shutdown) also ends the
/// subscription, which is what a window closing wants.
#[derive(Debug)]
pub struct RemoteView {
    broadcast: RemoteBroadcast,
    video: Option<VideoTrackView>,
    audio: Option<AudioTrack>,
    overlay: DebugOverlay,
    signals: watch::Receiver<NetworkSignals>,
    choice: RenditionChoice,
    /// The output gain the slider last set. Only a build with `playback` has a
    /// sink to apply it to.
    #[cfg(feature = "playback")]
    volume: f32,
}

impl RemoteView {
    /// Opens a view onto `broadcast`, drawing `tracks` through `render_state`.
    ///
    /// `name` salts the texture and the widget ids, so a grid of these needs a
    /// distinct one per tile. The video track starts on
    /// [`RenditionChoice::Auto`]; [`set_rendition`](Self::set_rendition) pins
    /// one instead.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        broadcast: RemoteBroadcast,
        tracks: MediaTracks,
        signals: watch::Receiver<NetworkSignals>,
        render_state: Option<&moq_media_egui::egui_wgpu::RenderState>,
    ) -> Self {
        let MediaTracks { video, audio } = tracks;
        let video = video.map(|track| VideoTrackView::new_wgpu(ctx, name, track, render_state));
        let view = Self {
            broadcast,
            video,
            audio,
            overlay: DebugOverlay::new(&[
                StatCategory::Net,
                StatCategory::Render,
                StatCategory::Time,
            ]),
            signals,
            choice: RenditionChoice::Auto,
            #[cfg(feature = "playback")]
            volume: 1.0,
        };
        view.apply_rendition();
        view
    }

    /// Reports whether the stats overlay is expanded, which keeps the
    /// controls up while it is being read.
    pub fn overlay_expanded(&self) -> bool {
        self.overlay.any_expanded()
    }

    /// Points the video track at `choice`.
    pub fn set_rendition(&mut self, choice: RenditionChoice) {
        self.choice = choice;
        self.apply_rendition();
    }

    /// Tells the video track to follow the downlink or hold one rendition,
    /// whichever [`RenditionChoice`] is currently selected.
    fn apply_rendition(&self) {
        let Some(view) = self.video.as_ref() else {
            return;
        };
        let track = view.track();
        match &self.choice {
            RenditionChoice::Auto => {
                track.enable_adaptation(self.signals.clone());
                info!(rendition = track.rendition(), "following the downlink");
            }
            RenditionChoice::Pinned(name) => {
                track.disable_adaptation();
                track.set_rendition(name.clone());
                info!(rendition = %name, "rendition pinned");
            }
        }
    }

    /// Draws the picture at `size`, or a placeholder while the peer sends no
    /// video.
    ///
    /// Returns the response of whatever was drawn, whose rect is what
    /// [`draw_overlay`](Self::draw_overlay) wants.
    pub fn draw(&mut self, ui: &mut egui::Ui, size: egui::Vec2) -> egui::Response {
        let ctx = ui.ctx().clone();
        match self.video.as_mut() {
            Some(view) => {
                let (image, _) = view.render(&ctx, size);
                ui.add_sized(size, image)
            }
            None => ui.add_sized(size, egui::Label::new("no video")),
        }
    }

    /// Draws the stats overlay over `rect`.
    pub fn draw_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if let Some(view) = self.video.as_ref() {
            self.overlay
                .update_from_track(self.broadcast.stats(), view.track());
        }
        self.overlay.show(ui, rect, self.broadcast.stats());
    }

    /// Draws the rendition picker and the volume slider.
    ///
    /// `id` salts the widget ids, so a grid of these needs a distinct one per
    /// tile.
    pub fn controls(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(view) = self.video.as_ref() else {
            ui.label("no video");
            return;
        };
        let rendition = view.track().rendition();

        ui.label("Rendition");
        let label = match &self.choice {
            RenditionChoice::Auto => format!("Auto ({rendition})"),
            RenditionChoice::Pinned(name) => name.clone(),
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(format!("{id}-rendition"))
            .selected_text(label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.choice == RenditionChoice::Auto, "Auto")
                    .clicked()
                {
                    chosen = Some(RenditionChoice::Auto);
                }
                for name in self.broadcast.catalog().video().keys() {
                    let pinned = self.choice == RenditionChoice::Pinned(name.clone());
                    if ui.selectable_label(pinned, name).clicked() {
                        chosen = Some(RenditionChoice::Pinned(name.clone()));
                    }
                }
            });
        if let Some(choice) = chosen {
            self.set_rendition(choice);
        }

        #[cfg(feature = "playback")]
        if let Some(audio) = self.audio.as_ref() {
            ui.label("Volume");
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false))
                .changed()
            {
                audio.set_volume(self.volume);
            }
        }
    }

    /// Drops the decoders and ends the subscription.
    ///
    /// The session itself belongs to whoever opened it, so this leaves it
    /// alone: a room keeps one session per peer and several views can ride on
    /// it.
    pub fn shutdown(&mut self) {
        self.video = None;
        self.audio = None;
        self.broadcast.shutdown();
    }
}
