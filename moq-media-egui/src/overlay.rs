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
use moq_media::stats::{self, MetricsCollector, MetricsSnapshot};
use moq_media::subscribe::VideoTrack;

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

const BG_ALPHA: u8 = 200;
const BG_HOVER_ALPHA: u8 = 220;

/// Stat bar category with associated metrics and labels.
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

    /// Key metrics shown in the bottom bar summary (up to 3).
    fn summary_metrics(self) -> &'static [&'static str] {
        match self {
            Self::Net => &[stats::NET_RTT_MS, stats::NET_BW_DOWN_MBPS],
            Self::Capture => &[stats::CAP_FPS, stats::CAP_ENCODE_MS],
            Self::Render => &[stats::RND_FPS, stats::RND_DECODE_MS],
            Self::Time => &[stats::TMG_JITTER_MS, stats::TMG_DELAY_MS],
        }
    }

    /// Key labels shown in the bottom bar summary.
    fn summary_labels(self) -> &'static [&'static str] {
        match self {
            Self::Net => &[stats::LBL_PATH_TYPE],
            Self::Capture => &[stats::LBL_CODEC],
            Self::Render => &[stats::LBL_RENDERER],
            Self::Time => &[],
        }
    }

    /// All metrics for the expanded detail panel.
    fn detail_metrics(self) -> &'static [&'static str] {
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
                stats::TMG_BUF_FRAMES,
                stats::TMG_FRAMES_SKIPPED,
                stats::TMG_FREEZES,
            ],
        }
    }

    /// All labels for the expanded detail panel.
    fn detail_labels(self) -> &'static [&'static str] {
        match self {
            Self::Net => &[stats::LBL_PEER, stats::LBL_PATH_TYPE, stats::LBL_PATH_ADDR],
            Self::Capture => &[stats::LBL_CODEC, stats::LBL_ENCODER, stats::LBL_RESOLUTION],
            Self::Render => &[
                stats::LBL_DECODER,
                stats::LBL_RENDERER,
                stats::LBL_RENDITION,
                stats::LBL_RESOLUTION,
            ],
            Self::Time => &[],
        }
    }
}

/// Consolidated debug overlay with a persistent bottom bar and
/// click-to-expand detail panels.
///
/// The bottom bar always shows all enabled sections side by side, each
/// with 1-3 key metrics and labels. Clicking a section toggles a detail
/// panel above the bar with all metrics (including sparklines) and
/// labels. Multiple sections can be expanded simultaneously.
#[derive(Debug)]
pub struct DebugOverlay {
    expanded: HashMap<StatCategory, bool>,
    enabled: Vec<StatCategory>,
    visible: bool,
}

impl DebugOverlay {
    /// Creates a new overlay with the given categories enabled.
    pub fn new(categories: &[StatCategory]) -> Self {
        let mut expanded = HashMap::new();
        for &cat in categories {
            expanded.insert(cat, false);
        }
        Self {
            expanded,
            enabled: categories.to_vec(),
            visible: true,
        }
    }

    /// Toggles overall visibility.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Updates labels from a [`VideoTrack`] into the collector.
    ///
    /// Call this each frame before [`show`](Self::show) to keep decoder
    /// name, rendition, and resolution in sync with the current track state.
    pub fn update_from_track(&self, metrics: &MetricsCollector, track: &VideoTrack) {
        metrics.set_label(stats::LBL_DECODER, track.decoder_name());
        metrics.set_label(stats::LBL_RENDITION, track.rendition());
    }

    /// Renders the overlay at the bottom of `video_rect`.
    ///
    /// For labels that can be derived from a `VideoTrack`, call
    /// [`update_from_track`](Self::update_from_track) before this. For other
    /// labels (renderer, peer, relay), use [`MetricsCollector::set_label`].
    pub fn show(&mut self, ui: &mut egui::Ui, video_rect: egui::Rect, snap: &MetricsSnapshot) {
        if !self.visible {
            return;
        }

        let font = egui::FontId::monospace(11.0);
        let enabled = self.enabled.clone();
        let mut clicks: Vec<StatCategory> = Vec::new();

        // Expanded detail panels stacked upward from the bottom bar.
        let mut y_cursor = video_rect.max.y - OVERLAY_BAR_H;
        for &cat in enabled.iter().rev() {
            if self.expanded.get(&cat) != Some(&true) {
                continue;
            }

            // TIME section shows the timeline panel instead of metrics.
            if cat == StatCategory::Time {
                let height = 150.0;
                y_cursor -= height;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(video_rect.min.x, y_cursor),
                    egui::vec2(video_rect.width(), height),
                );
                paint_timeline_panel(ui, rect, snap);
                continue;
            }

            let label_count = cat
                .detail_labels()
                .iter()
                .filter(|l| snap.label(l).is_some())
                .count();
            let metric_count = cat
                .detail_metrics()
                .iter()
                .filter(|m| snap.series(m).is_some())
                .count();
            let line_count = label_count + metric_count;
            if line_count == 0 {
                continue;
            }
            let height = OVERLAY_BAR_H * (line_count as f32 + 0.5);
            y_cursor -= height;
            let rect = egui::Rect::from_min_size(
                egui::pos2(video_rect.min.x, y_cursor),
                egui::vec2(video_rect.width(), height),
            );
            paint_detail_panel(ui, rect, cat, snap, &font);
        }

        // Bottom bar: all sections side by side.
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );
        let painter = ui.painter();
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

        // Measure section widths, then draw.
        let sections: Vec<(StatCategory, String)> = enabled
            .iter()
            .map(|&cat| {
                let text = format_section_summary(cat, snap);
                (cat, text)
            })
            .collect();

        let total_text_width: f32 = sections
            .iter()
            .map(|(_, text)| {
                painter
                    .layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
            .sum();
        let separator_width = 12.0 * (sections.len().saturating_sub(1)) as f32;
        let padding = 8.0;
        let _total_width = total_text_width + separator_width + padding * 2.0;

        let mut x = bar_rect.min.x + padding;
        for (i, (cat, text)) in sections.iter().enumerate() {
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE);
            let section_width = galley.size().x + 8.0;
            let section_rect = egui::Rect::from_min_size(
                egui::pos2(x - 4.0, bar_rect.min.y),
                egui::vec2(section_width, OVERLAY_BAR_H),
            );

            // Hover effect.
            let id = egui::Id::new(("dbg_section", cat.label()));
            let response = ui.interact(section_rect, id, egui::Sense::click());
            if response.hovered() {
                painter.rect_filled(
                    section_rect,
                    0.0,
                    egui::Color32::from_black_alpha(BG_HOVER_ALPHA),
                );
            }
            if response.clicked() {
                clicks.push(*cat);
            }

            painter.galley(
                egui::pos2(x, bar_rect.min.y + 1.0),
                galley,
                egui::Color32::WHITE,
            );

            x += section_width;

            // Separator.
            if i < sections.len() - 1 {
                let sep_x = x + 2.0;
                painter.line_segment(
                    [
                        egui::pos2(sep_x, bar_rect.min.y + 3.0),
                        egui::pos2(sep_x, bar_rect.max.y - 3.0),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
                );
                x += 12.0;
            }
        }

        // Apply clicks (toggle expanded).
        for cat in clicks {
            if let Some(exp) = self.expanded.get_mut(&cat) {
                *exp = !*exp;
            }
        }
    }
}

/// Formats the summary text for a section in the bottom bar.
fn format_section_summary(cat: StatCategory, snap: &MetricsSnapshot) -> String {
    let mut parts = vec![cat.label().to_string()];

    // Labels first (e.g. "wgpu/dmabuf").
    for &lbl_name in cat.summary_labels() {
        if let Some(val) = snap.label(lbl_name) {
            parts.push(format!(" {val}"));
        }
    }

    // Then key metrics.
    for &name in cat.summary_metrics() {
        if let Some(ts) = snap.series(name) {
            parts.push(format!(" {}:{:.0}{}", ts.label, ts.current, ts.unit));
        }
    }

    parts.join("")
}

/// Paints the expanded detail panel for a category.
fn paint_detail_panel(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cat: StatCategory,
    snap: &MetricsSnapshot,
    font: &egui::FontId,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

    let mut y = rect.min.y + 2.0;
    let dim = egui::Color32::from_rgb(160, 160, 160);

    // Labels.
    for &lbl_name in cat.detail_labels() {
        if let Some(val) = snap.label(lbl_name) {
            // Use the part after "lbl." as the display name.
            let display_name = lbl_name.strip_prefix("lbl.").unwrap_or(lbl_name);
            let text = format!("  {display_name}: {val}");
            let galley = painter.layout_no_wrap(text, font.clone(), dim);
            painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, dim);
            y += OVERLAY_BAR_H;
        }
    }

    // Metrics with sparklines.
    for &name in cat.detail_metrics() {
        if let Some(ts) = snap.series(name) {
            let color = metric_color(name, ts.current);
            let text = format!("  {}:{:.1}{}", ts.label, ts.current, ts.unit);
            let galley = painter.layout_no_wrap(text, font.clone(), color);
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

// ── Timeline panel ──────────────────────────────────────────────────

const TIMELINE_WINDOW_SECS: f32 = 10.0;
const RTT_STRIP_H: f32 = 35.0;
const FRAME_STRIP_H: f32 = 50.0;
const AXIS_H: f32 = 14.0;
const RTT_COLOR: egui::Color32 = egui::Color32::from_rgb(0, 200, 200);
const FRAME_GOOD: egui::Color32 = egui::Color32::from_rgb(68, 170, 68);
const FRAME_WARN: egui::Color32 = egui::Color32::from_rgb(200, 170, 0);
const FRAME_BAD: egui::Color32 = egui::Color32::from_rgb(200, 68, 68);
const GRID_COLOR: egui::Color32 = egui::Color32::from_rgb(50, 50, 50);

/// Paints the timeline panel: RTT line chart + video frame boxes.
fn paint_timeline_panel(ui: &mut egui::Ui, rect: egui::Rect, snap: &MetricsSnapshot) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

    let now = Instant::now();
    let window = Duration::from_secs_f32(TIMELINE_WINDOW_SECS);
    let t_min = now.checked_sub(window).unwrap_or(now);
    let px_per_sec = rect.width() / TIMELINE_WINDOW_SECS;

    let time_to_x = |t: Instant| -> f32 {
        let dt = t.duration_since(t_min).as_secs_f32();
        rect.min.x + dt * px_per_sec
    };

    let font = egui::FontId::monospace(9.0);

    // Grid lines (every 2 seconds).
    for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
        let x = rect.min.x + sec as f32 * px_per_sec;
        painter.line_segment(
            [
                egui::pos2(x, rect.min.y),
                egui::pos2(x, rect.max.y - AXIS_H),
            ],
            egui::Stroke::new(1.0, GRID_COLOR),
        );
    }

    // --- RTT strip (top) ---
    let rtt_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), RTT_STRIP_H));

    let rtt = &snap.timeline.rtt_samples;
    if rtt.len() >= 2 {
        // Filter to visible window.
        let visible: Vec<_> = rtt.iter().filter(|(t, _)| *t >= t_min).collect();
        if visible.len() >= 2 {
            let max_rtt = visible
                .iter()
                .map(|(_, v)| *v)
                .fold(0.0f64, f64::max)
                .max(1.0);

            let points: Vec<egui::Pos2> = visible
                .iter()
                .map(|(t, v)| {
                    let x = time_to_x(*t);
                    let y = rtt_rect.max.y - (*v / max_rtt) as f32 * (rtt_rect.height() - 4.0);
                    egui::pos2(x, y)
                })
                .collect();

            let stroke = egui::Stroke::new(1.5, RTT_COLOR);
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], stroke);
            }

            // RTT label.
            let label = format!(
                "RTT {:.0}ms",
                visible.last().map(|(_, v)| *v).unwrap_or(0.0)
            );
            let galley = painter.layout_no_wrap(label, font.clone(), RTT_COLOR);
            painter.galley(rtt_rect.min + egui::vec2(4.0, 2.0), galley, RTT_COLOR);
        }
    }

    // --- Video frame strip (middle) ---
    let frame_y = rect.min.y + RTT_STRIP_H;
    let frame_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, frame_y),
        egui::vec2(rect.width(), FRAME_STRIP_H),
    );

    let frames = &snap.timeline.video_frames;
    if !frames.is_empty() {
        // Label.
        let galley = painter.layout_no_wrap("VIDEO".to_string(), font.clone(), egui::Color32::GRAY);
        painter.galley(
            egui::pos2(frame_rect.min.x + 4.0, frame_rect.min.y + 2.0),
            galley,
            egui::Color32::GRAY,
        );

        let box_h = frame_rect.height() - 16.0;
        let box_y = frame_rect.min.y + 14.0;

        for (i, entry) in frames.iter().enumerate() {
            let Some(render_wall) = entry.render_wall else {
                continue;
            };
            if render_wall < t_min {
                continue;
            }

            let x = time_to_x(render_wall);

            // Box width: gap to next frame, or 3px minimum.
            let next_x = frames
                .get(i + 1)
                .and_then(|e| e.render_wall)
                .map(|t| time_to_x(t))
                .unwrap_or(x + 6.0);
            let box_w = (next_x - x - 1.0).max(3.0).min(20.0);

            // Color based on decode-to-render latency.
            let color = if let Some(decode_end) = entry.decode_end {
                let latency_ms = render_wall.duration_since(decode_end).as_secs_f32() * 1000.0;
                if latency_ms < 5.0 {
                    FRAME_GOOD
                } else if latency_ms < 15.0 {
                    FRAME_WARN
                } else {
                    FRAME_BAD
                }
            } else {
                FRAME_GOOD
            };

            let box_rect =
                egui::Rect::from_min_size(egui::pos2(x, box_y), egui::vec2(box_w, box_h));
            painter.rect_filled(box_rect, 1.0, color);
        }
    }

    // --- Time axis (bottom) ---
    let axis_y = rect.max.y - AXIS_H;
    let axis_color = egui::Color32::from_rgb(120, 120, 120);
    for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
        let x = rect.min.x + sec as f32 * px_per_sec;
        let label = format!("-{}s", TIMELINE_WINDOW_SECS as i32 - sec);
        let galley = painter.layout_no_wrap(label, font.clone(), axis_color);
        painter.galley(egui::pos2(x + 2.0, axis_y), galley, axis_color);
    }
}
