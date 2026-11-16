//! What the call window draws on each screen.

use eframe::egui;
use iroh::EndpointId;
use iroh_live::{BroadcastTicket, calls::Call};
use moq_ext_egui::overlay::fit_to_aspect;
use tracing::info;

use super::window::{Action, CallApp};
use crate::{
    scan::ScanView,
    transport::Subscribed,
    ui::{BUTTON, RemoteView},
};

/// Width of the local picture-in-picture, in points.
pub(super) const PIP_WIDTH: f32 = 240.0;

/// Aspect ratio of the local preview and the picture-in-picture.
pub(super) const ASPECT: f32 = 16.0 / 9.0;

/// Width of the calling screen's dialog, in points.
pub(super) const DIALOG_WIDTH: f32 = 280.0;

/// Fraction of the window's shorter side the ticket QR code takes.
pub(super) const QR_FRACTION: f32 = 0.5;

/// Smallest side of the ticket QR code, in points.
///
/// The buttons under the code set the column width, so a narrower code
/// would leave gaps beside it.
pub(super) const QR_MIN: f32 = BUTTON.x;

/// Largest side of the ticket QR code, in points.
///
/// A camera reads the code well below this size.
pub(super) const QR_MAX: f32 = 280.0;

/// What the window is showing.
pub(super) enum Screen {
    /// Nobody on the line: this node's ticket and a box for the peer's.
    Waiting(Waiting),
    /// The camera, looking for the peer's ticket.
    Scanning(Box<ScanView>),
    /// A dial this window started, not yet answered.
    Calling(Calling),
    /// A call in progress.
    InCall(Box<InCall>),
}

impl Screen {
    /// Returns an empty waiting screen.
    pub(super) fn waiting() -> Self {
        Self::Waiting(Waiting::default())
    }

    /// Returns the waiting screen showing `message`.
    pub(super) fn reporting(message: String) -> Self {
        Self::Waiting(Waiting {
            input: String::new(),
            message: Some(message),
        })
    }
}

/// The waiting screen's state.
#[derive(Debug, Default)]
pub(super) struct Waiting {
    /// The ticket being typed.
    pub(super) input: String,
    /// The outcome of the last attempt, if any.
    pub(super) message: Option<String>,
}

/// A dial in flight, as the calling screen sees it.
#[derive(Debug)]
pub(super) struct Calling {
    /// Who is being called. The attempt itself is in [`CallApp::pending`].
    pub(super) peer: String,
}

/// A connected call. Dropping it hangs up.
pub(super) struct InCall {
    pub(super) peer: EndpointId,
    /// Hangs up when dropped.
    pub(super) _call: Call,
    pub(super) sub: Subscribed,
    pub(super) remote: RemoteView,
}

impl InCall {
    /// Reports whether the peer hung up or its session ended.
    ///
    /// The peer withdrawing its offer ends the broadcast at once.
    pub(super) fn ended(&self) -> bool {
        self.sub.subscription.broadcast().is_closed() || self.sub.broadcast.is_closed()
    }
}

impl CallApp {
    /// Draws the local picture behind the screens without remote video.
    pub(super) fn draw_backdrop(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let size = fit_to_aspect(available, ASPECT);
        let image = self.preview.render();
        ui.centered_and_justified(|ui| ui.add_sized(size, image));
    }

    /// Draws the waiting screen.
    pub(super) fn waiting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.draw_backdrop(ui);
        crate::ui::top_bar(ui, ctx, &self.ticket);

        let answering = self.pending.is_some();
        let side = qr_side(ctx.content_rect().size());
        let Self {
            screen, qr, ticket, ..
        } = self;
        let Screen::Waiting(waiting) = screen else {
            return;
        };

        let mut action = None;
        crate::ui::dialog(ctx, "call-waiting", side, |ui| {
            ui.label("Have them scan this, or send them the ticket:");
            if let Some(qr) = qr {
                ui.add_sized(egui::Vec2::splat(side), qr.image());
            }
            if ui
                .add_sized(BUTTON, egui::Button::new("Scan theirs"))
                .clicked()
            {
                action = Some(Action::Scan);
            }
            if ui
                .add_sized(BUTTON, egui::Button::new("Copy ticket"))
                .clicked()
            {
                ctx.copy_text(ticket.clone());
            }
            if let Some(text) = paste_row(ui, side, waiting) {
                action = Some(Action::Dial(text));
            }
            // An incoming attempt only shows a spinner. The session may
            // not be a caller.
            if answering {
                ui.spinner();
            }
            if let Some(message) = &waiting.message {
                ui.colored_label(egui::Color32::LIGHT_YELLOW, message);
            }
        });

        match action {
            Some(Action::Scan) => self.enter_scan(ctx),
            Some(Action::Dial(text)) => match text.parse::<BroadcastTicket>() {
                Ok(ticket) => self.dial(ctx, ticket),
                Err(err) => self.report(format!("that is not a ticket: {err}")),
            },
            None => {}
        }
    }

    /// Draws the scan screen and its cancel button.
    pub(super) fn scan_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Screen::Scanning(view) = &mut self.screen {
            view.draw(ui);
        }
        crate::ui::top_bar(ui, ctx, &self.ticket);

        let mut cancel = false;
        crate::ui::control_panel(ctx, "call-scan-controls", |ui| {
            cancel = ui.add_sized(BUTTON, egui::Button::new("Cancel")).clicked();
        });
        if cancel {
            info!("the scan was cancelled");
            self.leave_scan();
        }
    }

    /// Draws the calling screen and its cancel button.
    pub(super) fn calling_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.draw_backdrop(ui);
        crate::ui::top_bar(ui, ctx, &self.ticket);

        let Screen::Calling(calling) = &self.screen else {
            return;
        };
        let peer = calling.peer.clone();

        let mut cancel = false;
        crate::ui::dialog(ctx, "call-calling", DIALOG_WIDTH, |ui| {
            ui.heading(format!("calling {peer} ..."));
            ui.spinner();
            cancel = ui.add_sized(BUTTON, egui::Button::new("Cancel")).clicked();
        });
        if cancel {
            self.cancel(&peer);
        }
    }

    /// Draws the call: the peer, this node in a corner, and the overlay.
    pub(super) fn in_call_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let expanded = match &self.screen {
            Screen::InCall(call) => call.remote.overlay_expanded(),
            Screen::Waiting(_) | Screen::Scanning(_) | Screen::Calling(_) => false,
        };
        let show_overlay = self.cursor.update(ctx, expanded);

        let Self {
            screen,
            preview,
            ticket,
            ..
        } = self;
        let Screen::InCall(call) = screen else {
            return;
        };

        let available = ui.available_size();
        let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        call.remote.draw(ui, available);

        let pip = egui::vec2(PIP_WIDTH, PIP_WIDTH / ASPECT);
        egui::Area::new(egui::Id::unique("call-pip"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-10.0, -10.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::BLACK)
                    .corner_radius(4.0)
                    .inner_margin(2.0)
                    .show(ui, |ui| ui.add_sized(pip, preview.render()));
            });

        if !show_overlay {
            return;
        }
        crate::ui::top_bar(ui, ctx, ticket);
        call.remote.draw_overlay(ui, video_rect);

        let mut hang_up = false;
        crate::ui::control_panel(ctx, "call-controls", |ui| {
            call.remote.controls(ui, "call");
            hang_up = ui
                .button("Hang up")
                .on_hover_text("End the call and go back to the ticket")
                .clicked();
        });
        if hang_up {
            info!("hanging up");
            self.hang_up("you hung up");
        }
    }
}

/// Draws the ticket box and the call button, and returns an entered ticket.
pub(super) fn paste_row(ui: &mut egui::Ui, width: f32, waiting: &mut Waiting) -> Option<String> {
    let text = egui::TextEdit::singleline(&mut waiting.input).hint_text("Their ticket");
    let response = ui.add_sized(egui::vec2(width, BUTTON.y), text);
    let ready = !waiting.input.trim().is_empty();
    let clicked = ui
        .add_enabled(ready, egui::Button::new("Call").min_size(BUTTON))
        .clicked();
    let submitted =
        ready && response.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter));
    if clicked || submitted {
        Some(waiting.input.trim().to_string())
    } else {
        None
    }
}

/// Returns the ticket QR code's side, in points, for a window of `content` size.
///
/// Scales with the shorter side, so the buttons under the code still fit
/// on a small touchscreen.
pub(super) fn qr_side(content: egui::Vec2) -> f32 {
    (content.x.min(content.y) * QR_FRACTION).clamp(QR_MIN, QR_MAX)
}

#[cfg(test)]
mod tests {
    use super::{QR_MAX, QR_MIN, egui, qr_side};

    /// A small window draws the code below [`QR_MAX`], so the buttons fit.
    #[test]
    fn a_small_window_draws_the_code_smaller() {
        let side = qr_side(egui::vec2(480.0, 320.0));
        assert!(side < QR_MAX, "unexpected: {side}");
        assert!(side >= QR_MIN, "unexpected: {side}");
    }

    /// The code stops growing at [`QR_MAX`].
    #[test]
    fn a_large_window_stops_growing_the_code() {
        assert_eq!(qr_side(egui::vec2(2560.0, 1440.0)), QR_MAX);
    }

    /// A tiny window still draws the code at [`QR_MIN`].
    #[test]
    fn a_window_with_no_room_still_draws_a_whole_code() {
        assert_eq!(qr_side(egui::vec2(40.0, 20.0)), QR_MIN);
    }
}
