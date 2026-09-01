//! Shared pieces of the two egui windows: a top bar, a floating control panel,
//! cursor auto-hide, and the lifecycle helpers both windows need.

use std::time::{Duration, Instant};

use eframe::egui;
use iroh_live::Live;

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

/// Shuts the endpoint down from `on_exit`, which eframe calls on the main
/// thread outside any async context.
pub fn shutdown_live_blocking(live: &Live) {
    let live = live.clone();
    tokio::runtime::Handle::current().block_on(async move {
        live.shutdown().await;
    });
}
