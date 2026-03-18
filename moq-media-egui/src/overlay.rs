//! Translucent overlay bars for painting stats on top of video.
//!
//! Provides [`DebugOverlay`] — a set of collapsible stat bars (NET,
//! CAPTURE, RENDER, TIME) driven by [`MetricsSnapshot`]. Also provides the
//! low-level [`overlay_bar`] helper for custom overlays.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use egui;
use moq_media::stats::{self, MetricsSnapshot};

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

// ── Debug overlay ───────────────────────────────────────────────────

/// Stat bar category with color and associated metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatCategory {
    Net,
    Capture,
    Render,
    Time,
}

impl StatCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Net => "NET",
            Self::Capture => "CAPTURE",
            Self::Render => "RENDER",
            Self::Time => "TIME",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Net => egui::Color32::from_rgb(80, 140, 255),
            Self::Capture => egui::Color32::from_rgb(80, 200, 120),
            Self::Render => egui::Color32::from_rgb(230, 160, 60),
            Self::Time => egui::Color32::from_rgb(200, 100, 200),
        }
    }

    /// The key metric shown when collapsed.
    fn key_metric(self) -> &'static str {
        match self {
            Self::Net => stats::NET_RTT_MS,
            Self::Capture => stats::CAP_FPS,
            Self::Render => stats::RND_FPS,
            Self::Time => stats::TMG_JITTER_MS,
        }
    }

    /// All metrics belonging to this category.
    fn metrics(self) -> &'static [&'static str] {
        match self {
            Self::Net => &[
                stats::NET_RTT_MS,
                stats::NET_LOSS_PCT,
                stats::NET_BW_DOWN_MBPS,
                stats::NET_BW_UP_MBPS,
            ],
            Self::Capture => &[
                stats::CAP_FPS,
                stats::CAP_ENCODE_MS,
                stats::CAP_BITRATE_KBPS,
            ],
            Self::Render => &[stats::RND_FPS, stats::RND_DECODE_MS],
            Self::Time => &[
                stats::TMG_JITTER_MS,
                stats::TMG_DELAY_MS,
                stats::TMG_DRIFT_MS,
                stats::TMG_REANCHOR_COUNT,
                stats::TMG_BUF_FRAMES,
            ],
        }
    }
}

/// Display state for a stat bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarState {
    Collapsed,
    Expanded,
    Full,
}

impl BarState {
    fn next(self) -> Self {
        match self {
            Self::Collapsed => Self::Expanded,
            Self::Expanded => Self::Full,
            Self::Full => Self::Collapsed,
        }
    }
}

/// Consolidated debug overlay with collapsible stat bars.
///
/// Renders stats from a [`MetricsSnapshot`] as painted overlay bars on
/// top of video. Each category (NET, CAPTURE, RENDER, TIME) has three
/// states cycled by clicking: collapsed (key metric), expanded (all
/// metrics), full (multi-line with sparklines).
///
/// Only categories present in `enabled` are shown. For example, a pure
/// viewer with no capture has no CAPTURE bar.
#[derive(Debug)]
pub struct DebugOverlay {
    states: HashMap<StatCategory, BarState>,
    enabled: Vec<StatCategory>,
    visible: bool,
    /// Extra text line for net bar expanded view (e.g. path type, address).
    pub net_extra: Option<String>,
}

impl DebugOverlay {
    /// Creates a new overlay with the given categories enabled.
    pub fn new(categories: &[StatCategory]) -> Self {
        let mut states = HashMap::new();
        for &cat in categories {
            states.insert(cat, BarState::Collapsed);
        }
        Self {
            states,
            enabled: categories.to_vec(),
            visible: true,
            net_extra: None,
        }
    }

    /// Toggles overall visibility.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Renders the overlay at the bottom of `video_rect`.
    ///
    /// Draws over existing content — does not allocate egui layout space.
    /// Collapsed bars stack left-to-right at the bottom. Expanded/full bars
    /// stack upward from the bottom, pushing collapsed bars up.
    pub fn show(&mut self, ui: &mut egui::Ui, video_rect: egui::Rect, snap: &MetricsSnapshot) {
        if !self.visible {
            return;
        }

        let mut y_cursor = video_rect.max.y;
        let font = egui::FontId::monospace(11.0);
        let enabled = self.enabled.clone();
        let net_extra = self.net_extra.clone();
        let mut clicks: Vec<StatCategory> = Vec::new();

        // First pass: expanded/full bars stacked upward from bottom.
        for &cat in enabled.iter().rev() {
            let state = self
                .states
                .get(&cat)
                .copied()
                .unwrap_or(BarState::Collapsed);
            match state {
                BarState::Expanded => {
                    y_cursor -= OVERLAY_BAR_H;
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(video_rect.min.x, y_cursor),
                        egui::vec2(video_rect.width(), OVERLAY_BAR_H),
                    );
                    paint_bar_expanded(ui, rect, cat, snap, &font, &net_extra, &mut clicks);
                }
                BarState::Full => {
                    let extra_lines = usize::from(cat == StatCategory::Net && net_extra.is_some());
                    let line_count = cat.metrics().len() + extra_lines;
                    let height = OVERLAY_BAR_H * (line_count as f32 + 1.0);
                    y_cursor -= height;
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(video_rect.min.x, y_cursor),
                        egui::vec2(video_rect.width(), height),
                    );
                    paint_bar_full(ui, rect, cat, snap, &font, &net_extra, &mut clicks);
                }
                BarState::Collapsed => {}
            }
        }

        // Second pass: collapsed bars tile left-to-right.
        y_cursor -= OVERLAY_BAR_H;
        let mut collapsed_x = video_rect.min.x;
        for &cat in &enabled {
            if self.states.get(&cat) == Some(&BarState::Collapsed) {
                let width = 120.0;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(collapsed_x, y_cursor),
                    egui::vec2(width, OVERLAY_BAR_H),
                );
                paint_bar_collapsed(ui, rect, cat, snap, &font, &mut clicks);
                collapsed_x += width + 2.0;
            }
        }

        // Apply collected clicks.
        for cat in clicks {
            if let Some(state) = self.states.get_mut(&cat) {
                *state = state.next();
            }
        }
    }
}

fn bar_click(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cat: StatCategory,
    clicks: &mut Vec<StatCategory>,
) {
    let id = egui::Id::new(("debug_overlay", cat.label()));
    let response = ui.interact(rect, id, egui::Sense::click());
    if response.clicked() {
        clicks.push(cat);
    }
}

fn paint_bar_collapsed(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cat: StatCategory,
    snap: &MetricsSnapshot,
    font: &egui::FontId,
    clicks: &mut Vec<StatCategory>,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(180));
    let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    painter.rect_filled(stripe, 0.0, cat.color());

    let key = cat.key_metric();
    let value = snap.get(key).unwrap_or(0.0);
    let unit = snap.series(key).map(|s| s.unit).unwrap_or("");
    let text = format!("{} {:.0}{}", cat.label(), value, unit);
    let color = metric_color(key, value);
    let galley = painter.layout_no_wrap(text, font.clone(), color);
    painter.galley(rect.min + egui::vec2(6.0, 1.0), galley, color);

    bar_click(ui, rect, cat, clicks);
}

fn paint_bar_expanded(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cat: StatCategory,
    snap: &MetricsSnapshot,
    font: &egui::FontId,
    net_extra: &Option<String>,
    clicks: &mut Vec<StatCategory>,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(180));
    let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    painter.rect_filled(stripe, 0.0, cat.color());

    let mut parts = vec![cat.label().to_string()];
    for &name in cat.metrics() {
        if let Some(ts) = snap.series(name) {
            parts.push(format!(" {}:{:.0}{}", ts.label, ts.current, ts.unit));
        }
    }
    if cat == StatCategory::Net
        && let Some(extra) = net_extra
    {
        parts.push(format!(" {extra}"));
    }

    let text = parts.join("");
    let galley = painter.layout_no_wrap(text, font.clone(), egui::Color32::WHITE);
    painter.galley(
        rect.min + egui::vec2(6.0, 1.0),
        galley,
        egui::Color32::WHITE,
    );

    bar_click(ui, rect, cat, clicks);
}

fn paint_bar_full(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cat: StatCategory,
    snap: &MetricsSnapshot,
    font: &egui::FontId,
    net_extra: &Option<String>,
    clicks: &mut Vec<StatCategory>,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(180));
    let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    painter.rect_filled(stripe, 0.0, cat.color());

    // Header line.
    let galley = painter.layout_no_wrap(cat.label().to_string(), font.clone(), cat.color());
    painter.galley(rect.min + egui::vec2(6.0, 1.0), galley, cat.color());

    // One line per metric with inline sparkline.
    let mut y = rect.min.y + OVERLAY_BAR_H;
    for &name in cat.metrics() {
        if let Some(ts) = snap.series(name) {
            let color = metric_color(name, ts.current);
            let label = format!("  {}:{:.1}{}", ts.label, ts.current, ts.unit);
            let galley = painter.layout_no_wrap(label, font.clone(), color);
            painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, color);

            if ts.history.len() >= 2 {
                let spark_w = 60.0;
                let spark_h = OVERLAY_BAR_H - 4.0;
                let spark_x = rect.max.x - spark_w - 6.0;
                let spark_rect = egui::Rect::from_min_size(
                    egui::pos2(spark_x, y + 2.0),
                    egui::vec2(spark_w, spark_h),
                );
                paint_sparkline(painter, spark_rect, &ts.history, color);
            }

            y += OVERLAY_BAR_H;
        }
    }

    if cat == StatCategory::Net
        && let Some(extra) = net_extra
    {
        let galley =
            painter.layout_no_wrap(format!("  {extra}"), font.clone(), egui::Color32::GRAY);
        painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, egui::Color32::GRAY);
    }

    bar_click(ui, rect, cat, clicks);
}

/// Color-codes a metric value: green (good), yellow (warn), red (bad).
fn metric_color(name: &str, value: f64) -> egui::Color32 {
    match name {
        stats::NET_RTT_MS => {
            if value < 30.0 {
                egui::Color32::from_rgb(100, 220, 100)
            } else if value < 100.0 {
                egui::Color32::from_rgb(220, 200, 80)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            }
        }
        stats::NET_LOSS_PCT => {
            if value < 2.0 {
                egui::Color32::from_rgb(100, 220, 100)
            } else if value < 10.0 {
                egui::Color32::from_rgb(220, 200, 80)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            }
        }
        stats::RND_FPS | stats::CAP_FPS => {
            if value > 25.0 {
                egui::Color32::from_rgb(100, 220, 100)
            } else if value > 15.0 {
                egui::Color32::from_rgb(220, 200, 80)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            }
        }
        stats::TMG_JITTER_MS => {
            if value < 10.0 {
                egui::Color32::from_rgb(100, 220, 100)
            } else if value < 30.0 {
                egui::Color32::from_rgb(220, 200, 80)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            }
        }
        _ => egui::Color32::WHITE,
    }
}

/// Draws a tiny sparkline graph from time-series history.
fn paint_sparkline(
    painter: &egui::Painter,
    rect: egui::Rect,
    history: &[(Instant, f64)],
    color: egui::Color32,
) {
    if history.len() < 2 {
        return;
    }
    let min_val = history
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::INFINITY, f64::min);
    let max_val = history
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max);
    let range = (max_val - min_val).max(0.001);

    let points: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, (_, v))| {
            let x = rect.min.x + (i as f32 / (history.len() - 1) as f32) * rect.width();
            let y = rect.max.y - ((v - min_val) / range) as f32 * rect.height();
            egui::pos2(x, y)
        })
        .collect();

    let stroke = egui::Stroke::new(1.0, color.linear_multiply(0.7));
    for pair in points.windows(2) {
        painter.line_segment([pair[0], pair[1]], stroke);
    }
}
