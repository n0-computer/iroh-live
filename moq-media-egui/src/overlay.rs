//! Translucent overlay bars for painting stats on top of video.
//!
//! Paints monospace text on a semi-transparent black background. The bars
//! do not allocate egui layout space — they are drawn over existing content
//! via the [`Painter`](egui::Painter).

use std::time::{Duration, Instant};

use egui;

/// Height of a single overlay bar (text + padding).
pub const OVERLAY_BAR_H: f32 = 15.0;

/// Paints a translucent overlay bar with monospace text at the given rect.
///
/// Does **not** allocate egui layout space — the bar is painted over existing
/// content (typically video). Use [`OVERLAY_BAR_H`] for positioning.
pub fn overlay_bar(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let font = egui::FontId::monospace(11.0);
    let galley = painter.layout_no_wrap(text.to_string(), font, egui::Color32::WHITE);
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
    painter.galley(
        rect.min + egui::vec2(4.0, 1.0),
        galley,
        egui::Color32::WHITE,
    );
}

/// Computes the largest size that fits `available` while preserving `aspect` (width / height).
pub fn fit_to_aspect(available: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let h_by_width = available.x / aspect;
    if h_by_width <= available.y {
        egui::vec2(available.x, h_by_width)
    } else {
        let w_by_height = available.y * aspect;
        egui::vec2(w_by_height, available.y)
    }
}

/// Rolling frame-rate and delay tracker.
///
/// Call [`tick`](Self::tick) once per rendered frame. Computes fps over
/// 1-second windows and measures wall-clock delay relative to frame PTS.
#[derive(Debug, Default)]
pub struct FrameStats {
    count: u64,
    last_update: Option<Instant>,
    /// Current measured frames per second.
    pub fps: f32,
    /// Current estimated display delay in milliseconds.
    pub delay_ms: f32,
    baseline: Option<(Instant, Duration)>,
}

impl FrameStats {
    /// Records one frame tick, optionally with a presentation timestamp.
    pub fn tick(&mut self, frame_ts: Option<Duration>) {
        self.count += 1;
        let now = Instant::now();
        let last = *self.last_update.get_or_insert(now);
        let elapsed = now.duration_since(last);
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.count as f32 / elapsed.as_secs_f32();
            self.count = 0;
            self.last_update = Some(now);
        }
        if let Some(ts) = frame_ts {
            let (base_wall, base_pts) = *self.baseline.get_or_insert((now, ts));
            let wall_delta = now.duration_since(base_wall);
            let pts_delta = ts.saturating_sub(base_pts);
            self.delay_ms = wall_delta.saturating_sub(pts_delta).as_secs_f32() * 1000.0;
        }
    }
}
