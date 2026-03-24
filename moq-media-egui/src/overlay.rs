//! Translucent overlay bars for painting stats on top of video.
//!
//! Provides [`DebugOverlay`] — a set of collapsible stat bars (NET,
//! CAPTURE, RENDER, TIME) driven by typed stat structs. Also provides the
//! low-level [`overlay_bar`] helper for custom overlays.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use egui;
use moq_media::{
    stats::{self, Label, Metric, NetStats, PublishStats, SubscribeStats, Timeline},
    subscribe::VideoTrack,
};

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

// ── Debug overlay ───────────────────────────────────────────────────

const BG_ALPHA: u8 = 200;

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
    /// Timeline scroll offset in seconds from "now". 0.0 = live edge.
    timeline_scroll: f32,
    /// Whether the timeline auto-follows the live edge.
    timeline_live: bool,
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
            timeline_scroll: 0.0,
            timeline_live: true,
        }
    }

    /// Toggles overall visibility.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Updates labels from a [`VideoTrack`] into the stats.
    ///
    /// Call this each frame before [`show`](Self::show) to keep decoder
    /// name, rendition, and resolution in sync with the current track state.
    pub fn update_from_track(&self, stats: &SubscribeStats, track: &VideoTrack) {
        stats.render.decoder.set(track.decoder_name());
        stats.render.rendition.set(track.rendition());
    }

    /// Renders the overlay at the bottom of `video_rect` using subscribe-side stats.
    pub fn show(&mut self, ui: &mut egui::Ui, video_rect: egui::Rect, stats: &SubscribeStats) {
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
                let height = 176.0;
                y_cursor -= height;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(video_rect.min.x, y_cursor),
                    egui::vec2(video_rect.width(), height),
                );
                paint_timeline_panel(
                    ui,
                    rect,
                    &stats.timeline,
                    &stats.net,
                    &stats.timing,
                    &mut self.timeline_scroll,
                    &mut self.timeline_live,
                );
                continue;
            }

            let entries = detail_entries(cat, stats);
            let line_count = entries.len();
            if line_count == 0 {
                continue;
            }
            let height = OVERLAY_BAR_H * (line_count as f32 + 0.5);
            y_cursor -= height;
            let rect = egui::Rect::from_min_size(
                egui::pos2(video_rect.min.x, y_cursor),
                egui::vec2(video_rect.width(), height),
            );
            paint_detail_panel_entries(ui, rect, &entries, &font);
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
                let text = format_section_summary_typed(cat, stats);
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

            // Hover effect: lighter background + bottom border for click affordance.
            let id = egui::Id::new(("dbg_section", cat.label()));
            let response = ui.interact(section_rect, id, egui::Sense::click());
            if response.hovered() {
                painter.rect_filled(section_rect, 2.0, egui::Color32::from_white_alpha(30));
                painter.line_segment(
                    [section_rect.left_bottom(), section_rect.right_bottom()],
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(80)),
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

    /// Renders the overlay for publish-side stats (capture only).
    pub fn show_publish(
        &mut self,
        ui: &mut egui::Ui,
        video_rect: egui::Rect,
        stats: &PublishStats,
    ) {
        if !self.visible {
            return;
        }

        let font = egui::FontId::monospace(11.0);
        let enabled = self.enabled.clone();
        let mut clicks: Vec<StatCategory> = Vec::new();

        // Expanded detail panels.
        let mut y_cursor = video_rect.max.y - OVERLAY_BAR_H;
        for &cat in enabled.iter().rev() {
            if self.expanded.get(&cat) != Some(&true) {
                continue;
            }

            let entries = detail_entries_publish(cat, stats);
            let line_count = entries.len();
            if line_count == 0 {
                continue;
            }
            let height = OVERLAY_BAR_H * (line_count as f32 + 0.5);
            y_cursor -= height;
            let rect = egui::Rect::from_min_size(
                egui::pos2(video_rect.min.x, y_cursor),
                egui::vec2(video_rect.width(), height),
            );
            paint_detail_panel_entries(ui, rect, &entries, &font);
        }

        // Bottom bar.
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );
        let painter = ui.painter();
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

        let sections: Vec<(StatCategory, String)> = enabled
            .iter()
            .map(|&cat| {
                let text = format_section_summary_publish(cat, stats);
                (cat, text)
            })
            .collect();

        let padding = 8.0;
        let mut x = bar_rect.min.x + padding;
        for (i, (cat, text)) in sections.iter().enumerate() {
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE);
            let section_width = galley.size().x + 8.0;
            let section_rect = egui::Rect::from_min_size(
                egui::pos2(x - 4.0, bar_rect.min.y),
                egui::vec2(section_width, OVERLAY_BAR_H),
            );

            let id = egui::Id::new(("dbg_pub_section", cat.label()));
            let response = ui.interact(section_rect, id, egui::Sense::click());
            if response.hovered() {
                painter.rect_filled(section_rect, 2.0, egui::Color32::from_white_alpha(30));
                painter.line_segment(
                    [section_rect.left_bottom(), section_rect.right_bottom()],
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(80)),
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

        for cat in clicks {
            if let Some(exp) = self.expanded.get_mut(&cat) {
                *exp = !*exp;
            }
        }
    }
}

// ── Detail entry types ──────────────────────────────────────────────

enum DetailEntry<'a> {
    MetricEntry(&'a Metric),
    LabelEntry {
        name: &'static str,
        label: &'a Label,
    },
}

fn detail_entries<'a>(cat: StatCategory, stats: &'a SubscribeStats) -> Vec<DetailEntry<'a>> {
    let mut entries = Vec::new();
    match cat {
        StatCategory::Net => {
            push_label(&mut entries, "peer", &stats.net.peer);
            push_label(&mut entries, "path", &stats.net.path_type);
            push_label(&mut entries, "addr", &stats.net.path_addr);
            push_metric(&mut entries, &stats.net.rtt_ms);
            push_metric(&mut entries, &stats.net.loss_pct);
            push_metric(&mut entries, &stats.net.bw_down_mbps);
            push_metric(&mut entries, &stats.net.bw_up_mbps);
            push_metric(&mut entries, &stats.net.paths_active);
        }
        StatCategory::Capture => {
            // Subscribe side has no capture stats.
        }
        StatCategory::Render => {
            push_label(&mut entries, "decoder", &stats.render.decoder);
            push_label(&mut entries, "renderer", &stats.render.renderer);
            push_label(&mut entries, "rendition", &stats.render.rendition);
            push_metric(&mut entries, &stats.render.fps);
            push_metric(&mut entries, &stats.render.decode_ms);
        }
        StatCategory::Time => {
            push_label(&mut entries, "sync", &stats.timing.sync_state);
            push_metric(&mut entries, &stats.timing.delay_ms);
            push_metric(&mut entries, &stats.timing.audio_buffer_ms);
            push_metric(&mut entries, &stats.timing.audio_live_lag_ms);
            push_metric(&mut entries, &stats.timing.video_live_lag_ms);
            push_metric(&mut entries, &stats.timing.av_delta_ms);
            push_metric(&mut entries, &stats.timing.buf_frames);
            push_metric(&mut entries, &stats.timing.frames_skipped);
            push_metric(&mut entries, &stats.timing.late_frames_dropped);
            push_metric(&mut entries, &stats.timing.freezes);
        }
    }
    entries
}

fn detail_entries_publish<'a>(cat: StatCategory, stats: &'a PublishStats) -> Vec<DetailEntry<'a>> {
    let mut entries = Vec::new();
    match cat {
        StatCategory::Capture => {
            push_label(&mut entries, "codec", &stats.capture.codec);
            push_label(&mut entries, "encoder", &stats.capture.encoder);
            push_label(&mut entries, "resolution", &stats.capture.resolution);
            push_metric(&mut entries, &stats.capture.fps);
            push_metric(&mut entries, &stats.capture.encode_ms);
            push_metric(&mut entries, &stats.capture.bitrate_kbps);
        }
        StatCategory::Net => {
            push_label(&mut entries, "peer", &stats.net.peer);
            push_label(&mut entries, "path", &stats.net.path_type);
            push_label(&mut entries, "addr", &stats.net.path_addr);
            push_metric(&mut entries, &stats.net.rtt_ms);
            push_metric(&mut entries, &stats.net.loss_pct);
            push_metric(&mut entries, &stats.net.bw_down_mbps);
            push_metric(&mut entries, &stats.net.bw_up_mbps);
            push_metric(&mut entries, &stats.net.paths_active);
        }
        _ => {}
    }
    entries
}

fn push_metric<'a>(entries: &mut Vec<DetailEntry<'a>>, metric: &'a Metric) {
    if metric.has_samples() {
        entries.push(DetailEntry::MetricEntry(metric));
    }
}

fn push_label<'a>(entries: &mut Vec<DetailEntry<'a>>, name: &'static str, label: &'a Label) {
    let val = label.get();
    if !val.is_empty() {
        entries.push(DetailEntry::LabelEntry { name, label });
    }
}

// ── Summary formatting ──────────────────────────────────────────────

fn format_section_summary_typed(cat: StatCategory, stats: &SubscribeStats) -> String {
    let mut parts = vec![cat.label().to_string()];
    match cat {
        StatCategory::Net => {
            push_label_summary(&mut parts, &stats.net.path_type);
            push_metric_summary(&mut parts, &stats.net.rtt_ms);
            push_metric_summary(&mut parts, &stats.net.bw_down_mbps);
        }
        StatCategory::Capture => {
            // Subscribe side has no capture stats.
        }
        StatCategory::Render => {
            push_label_summary(&mut parts, &stats.render.renderer);
            push_metric_summary(&mut parts, &stats.render.fps);
            push_metric_summary(&mut parts, &stats.render.decode_ms);
        }
        StatCategory::Time => {
            push_label_summary(&mut parts, &stats.timing.sync_state);
            push_metric_summary(&mut parts, &stats.timing.delay_ms);
            push_metric_summary(&mut parts, &stats.timing.audio_live_lag_ms);
            push_metric_summary(&mut parts, &stats.timing.av_delta_ms);
        }
    }
    parts.join("")
}

fn format_section_summary_publish(cat: StatCategory, stats: &PublishStats) -> String {
    let mut parts = vec![cat.label().to_string()];
    match cat {
        StatCategory::Capture => {
            push_label_summary(&mut parts, &stats.capture.codec);
            push_metric_summary(&mut parts, &stats.capture.fps);
            push_metric_summary(&mut parts, &stats.capture.encode_ms);
        }
        StatCategory::Net => {
            push_label_summary(&mut parts, &stats.net.path_type);
            push_metric_summary(&mut parts, &stats.net.rtt_ms);
            push_metric_summary(&mut parts, &stats.net.bw_up_mbps);
        }
        _ => {}
    }
    parts.join("")
}

fn push_metric_summary(parts: &mut Vec<String>, metric: &Metric) {
    if metric.has_samples() {
        let meta = metric.meta();
        parts.push(format!(
            " {}:{:.0}{}",
            meta.label,
            metric.current(),
            meta.unit
        ));
    }
}

fn push_label_summary(parts: &mut Vec<String>, label: &Label) {
    let val = label.get();
    if !val.is_empty() {
        parts.push(format!(" {val}"));
    }
}

// ── Detail panel painting ───────────────────────────────────────────

fn paint_detail_panel_entries(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    entries: &[DetailEntry<'_>],
    font: &egui::FontId,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

    let mut y = rect.min.y + 2.0;
    let dim = egui::Color32::from_rgb(160, 160, 160);

    for entry in entries {
        match entry {
            DetailEntry::LabelEntry { name, label } => {
                let val = label.get();
                let text = format!("  {name}: {val}");
                let galley = painter.layout_no_wrap(text, font.clone(), dim);
                painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, dim);
                y += OVERLAY_BAR_H;
            }
            DetailEntry::MetricEntry(metric) => {
                let meta = metric.meta();
                let value = metric.current();
                let color = metric_color_from_thresholds(meta.thresholds, value);
                let text = format!("  {}:{:.1}{}", meta.label, value, meta.unit);
                let galley = painter.layout_no_wrap(text, font.clone(), color);
                painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, color);

                let history = metric.history();
                if history.len() >= 2 {
                    let spark_w = 100.0;
                    let spark_h = OVERLAY_BAR_H - 4.0;
                    let spark_x = rect.max.x - spark_w - 6.0;
                    let spark_rect = egui::Rect::from_min_size(
                        egui::pos2(spark_x, y + 2.0),
                        egui::vec2(spark_w, spark_h),
                    );
                    paint_sparkline(painter, spark_rect, &history, color);
                }

                y += OVERLAY_BAR_H;
            }
        }
    }
}

/// Color-codes a metric value using threshold metadata.
fn metric_color_from_thresholds(
    thresholds: Option<stats::Thresholds>,
    value: f64,
) -> egui::Color32 {
    let Some(t) = thresholds else {
        return egui::Color32::WHITE;
    };
    let good = egui::Color32::from_rgb(100, 220, 100);
    let warn = egui::Color32::from_rgb(220, 200, 80);
    let bad = egui::Color32::from_rgb(220, 80, 80);

    if t.inverted {
        // Higher is better (e.g. FPS).
        if value > t.good {
            good
        } else if value > t.warn {
            warn
        } else {
            bad
        }
    } else {
        // Lower is better (e.g. RTT, loss).
        if value < t.good {
            good
        } else if value < t.warn {
            warn
        } else {
            bad
        }
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
const LATENCY_GRAPH_H: f32 = 36.0;
const VIDEO_LANE_H: f32 = 20.0;
const AUDIO_LANE_H: f32 = 16.0;
const AV_SYNC_H: f32 = 20.0;
const DELAY_STRIP_H: f32 = 26.0;
const RTT_STRIP_H: f32 = 26.0;
const AXIS_H: f32 = 14.0;
// Total: 36+20+16+20+26+26+14 = 158px

const LATENCY_GOOD_MS: f32 = 100.0;
const LATENCY_WARN_MS: f32 = 200.0;

const COLOR_GREEN: egui::Color32 = egui::Color32::from_rgb(68, 170, 68);
const COLOR_YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 170, 0);
const COLOR_RED: egui::Color32 = egui::Color32::from_rgb(200, 68, 68);
const COLOR_BLUE: egui::Color32 = egui::Color32::from_rgb(68, 136, 204);
const COLOR_CYAN: egui::Color32 = egui::Color32::from_rgb(0, 200, 200);
const COLOR_GRAY: egui::Color32 = egui::Color32::from_rgb(160, 160, 160);
const COLOR_GRID: egui::Color32 = egui::Color32::from_rgb(50, 50, 50);

fn latency_color(ms: f32) -> egui::Color32 {
    if ms < LATENCY_GOOD_MS {
        COLOR_GREEN
    } else if ms < LATENCY_WARN_MS {
        COLOR_YELLOW
    } else {
        COLOR_RED
    }
}

/// Video frame box color: based on inter-frame gap relative to expected
/// cadence. Steady 33ms gaps at 30fps = green. 1.5× = yellow. 2× = red.
fn gap_color(gap_ms: f32, expected_ms: f32) -> egui::Color32 {
    let ratio = gap_ms / expected_ms.max(1.0);
    if ratio < 1.5 {
        COLOR_GREEN
    } else if ratio < 2.0 {
        COLOR_YELLOW
    } else {
        COLOR_RED
    }
}

/// Paints the timeline panel: latency graph, video/audio frame lanes,
/// A/V sync offset, RTT chart, and scrollable time axis.
fn paint_timeline_panel(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    timeline: &Timeline,
    net: &NetStats,
    timing: &stats::TimingStats,
    scroll: &mut f32,
    live: &mut bool,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

    let now = Instant::now();
    let t_right = if *live {
        now
    } else {
        now.checked_sub(Duration::from_secs_f32(*scroll))
            .unwrap_or(now)
    };
    let t_left = t_right
        .checked_sub(Duration::from_secs_f32(TIMELINE_WINDOW_SECS))
        .unwrap_or(t_right);
    let px_per_sec = rect.width() / TIMELINE_WINDOW_SECS;
    let time_to_x =
        |t: Instant| -> f32 { rect.min.x + t.duration_since(t_left).as_secs_f32() * px_per_sec };

    let font = egui::FontId::monospace(9.0);

    // Grid lines (every 2 seconds).
    for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
        let x = rect.min.x + sec as f32 * px_per_sec;
        painter.line_segment(
            [
                egui::pos2(x, rect.min.y),
                egui::pos2(x, rect.max.y - AXIS_H),
            ],
            egui::Stroke::new(1.0, COLOR_GRID),
        );
    }

    let frames = timeline.snapshot();
    let visible: Vec<_> = frames
        .iter()
        .filter(|f| f.render_wall >= t_left && f.render_wall <= t_right)
        .collect();
    let video_frames: Vec<_> = visible
        .iter()
        .filter(|f| f.kind == stats::FrameKind::Video)
        .copied()
        .collect();
    let audio_frames: Vec<_> = visible
        .iter()
        .filter(|f| f.kind == stats::FrameKind::Audio)
        .copied()
        .collect();

    // ── Lane 1: Latency graph ───────────────────────────────────────
    let lat_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), LATENCY_GRAPH_H));
    {
        let g = painter.layout_no_wrap("LATENCY".to_string(), font.clone(), egui::Color32::GRAY);
        painter.galley(lat_rect.min + egui::vec2(4.0, 2.0), g, egui::Color32::GRAY);

        let latencies: Vec<(f32, f32)> = video_frames
            .iter()
            .map(|f| {
                let x = time_to_x(f.render_wall);
                let lat = f.render_wall.duration_since(f.receive_wall).as_secs_f32() * 1000.0;
                (x, lat)
            })
            .collect();

        if latencies.len() >= 2 {
            let max_lat = latencies
                .iter()
                .map(|(_, l)| *l)
                .fold(0.0f32, f32::max)
                .max(50.0);
            let h = lat_rect.height() - 14.0;
            for pair in latencies.windows(2) {
                let (x1, l1) = pair[0];
                let (x2, l2) = pair[1];
                let y1 = lat_rect.max.y - (l1 / max_lat) * h;
                let y2 = lat_rect.max.y - (l2 / max_lat) * h;
                painter.line_segment(
                    [egui::pos2(x1, y1), egui::pos2(x2, y2)],
                    egui::Stroke::new(1.5, latency_color((l1 + l2) / 2.0)),
                );
            }
            if let Some((_, lat)) = latencies.last() {
                let c = latency_color(*lat);
                let g = painter.layout_no_wrap(format!("{:.0}ms", lat), font.clone(), c);
                painter.galley(
                    egui::pos2(lat_rect.max.x - g.size().x - 4.0, lat_rect.min.y + 2.0),
                    g,
                    c,
                );
            }
        }
    }

    // ── Lane 2: Video frame boxes (color = inter-frame gap) ─────────
    let video_y = rect.min.y + LATENCY_GRAPH_H;
    let video_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, video_y),
        egui::vec2(rect.width(), VIDEO_LANE_H),
    );
    {
        let g = painter.layout_no_wrap("VIDEO".to_string(), font.clone(), egui::Color32::GRAY);
        painter.galley(
            egui::pos2(video_rect.min.x + 4.0, video_rect.min.y + 1.0),
            g,
            egui::Color32::GRAY,
        );

        let box_h = VIDEO_LANE_H - 8.0;
        let box_y = video_rect.min.y + 7.0;
        // Estimate expected interval from median gap of visible frames.
        let expected_ms = if video_frames.len() >= 3 {
            let mut gaps: Vec<f32> = video_frames
                .windows(2)
                .map(|w| {
                    w[1].render_wall
                        .duration_since(w[0].render_wall)
                        .as_secs_f32()
                        * 1000.0
                })
                .collect();
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            gaps[gaps.len() / 2]
        } else {
            33.3 // assume 30fps
        };

        for (i, entry) in video_frames.iter().enumerate() {
            let x = time_to_x(entry.render_wall);
            let next_x = video_frames
                .get(i + 1)
                .map(|e| time_to_x(e.render_wall))
                .unwrap_or(x + 6.0);
            let box_w = (next_x - x - 1.0).clamp(3.0, 20.0);

            let gap_ms = if i > 0 {
                entry
                    .render_wall
                    .duration_since(video_frames[i - 1].render_wall)
                    .as_secs_f32()
                    * 1000.0
            } else {
                expected_ms
            };
            let color = gap_color(gap_ms, expected_ms);

            let r = egui::Rect::from_min_size(egui::pos2(x, box_y), egui::vec2(box_w, box_h));
            painter.rect_filled(r, 1.0, color);

            if entry.is_keyframe {
                painter.line_segment(
                    [r.left_top(), r.left_bottom()],
                    egui::Stroke::new(1.0, egui::Color32::WHITE),
                );
            }
        }
    }

    // ── Lane 3: Audio frame boxes ───────────────────────────────────
    let audio_y = rect.min.y + LATENCY_GRAPH_H + VIDEO_LANE_H;
    let audio_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, audio_y),
        egui::vec2(rect.width(), AUDIO_LANE_H),
    );
    {
        let g = painter.layout_no_wrap("AUDIO".to_string(), font.clone(), COLOR_BLUE);
        painter.galley(
            egui::pos2(audio_rect.min.x + 4.0, audio_rect.min.y + 1.0),
            g,
            COLOR_BLUE,
        );

        let box_h = AUDIO_LANE_H - 6.0;
        let box_y = audio_rect.min.y + 5.0;

        for (i, entry) in audio_frames.iter().enumerate() {
            let x = time_to_x(entry.render_wall);
            let next_x = audio_frames
                .get(i + 1)
                .map(|e| time_to_x(e.render_wall))
                .unwrap_or(x + 4.0);
            let box_w = (next_x - x - 0.5).clamp(2.0, 10.0);

            // Blue normally, red if receive-to-render > 100ms.
            let lat = entry
                .render_wall
                .duration_since(entry.receive_wall)
                .as_secs_f32()
                * 1000.0;
            let color = if lat > 100.0 { COLOR_RED } else { COLOR_BLUE };

            let r = egui::Rect::from_min_size(egui::pos2(x, box_y), egui::vec2(box_w, box_h));
            painter.rect_filled(r, 1.0, color);
        }
    }

    // ── Lane 4: A/V sync offset ─────────────────────────────────────
    let sync_y = rect.min.y + LATENCY_GRAPH_H + VIDEO_LANE_H + AUDIO_LANE_H;
    let sync_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, sync_y),
        egui::vec2(rect.width(), AV_SYNC_H),
    );
    {
        // Zero line.
        let zero_y = sync_rect.min.y + sync_rect.height() / 2.0;
        painter.line_segment(
            [
                egui::pos2(sync_rect.min.x, zero_y),
                egui::pos2(sync_rect.max.x, zero_y),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
        );

        let g = painter.layout_no_wrap("A/V".to_string(), font.clone(), COLOR_GRAY);
        painter.galley(
            egui::pos2(sync_rect.min.x + 4.0, sync_rect.min.y + 1.0),
            g,
            COLOR_GRAY,
        );

        // For each video frame, find the closest audio frame by PTS and
        // compute the render-time offset: video_render - audio_render.
        // Positive = video renders later than audio.
        const SYNC_RANGE_MS: f32 = 80.0;
        let half_h = sync_rect.height() / 2.0 - 2.0;

        let mut sync_points: Vec<(f32, f32)> = Vec::new();
        for vf in &video_frames {
            let closest_audio = audio_frames
                .iter()
                .min_by_key(|af| vf.pts.abs_diff(af.pts).as_millis());
            if let Some(af) = closest_audio {
                let signed_offset = if vf.render_wall >= af.render_wall {
                    vf.render_wall.duration_since(af.render_wall).as_secs_f32() * 1000.0
                } else {
                    -(af.render_wall.duration_since(vf.render_wall).as_secs_f32() * 1000.0)
                };
                sync_points.push((time_to_x(vf.render_wall), signed_offset));
            }
        }

        if sync_points.len() >= 2 {
            for pair in sync_points.windows(2) {
                let (x1, o1) = pair[0];
                let (x2, o2) = pair[1];
                let y1 = zero_y - (o1 / SYNC_RANGE_MS).clamp(-1.0, 1.0) * half_h;
                let y2 = zero_y - (o2 / SYNC_RANGE_MS).clamp(-1.0, 1.0) * half_h;
                let avg = (o1.abs() + o2.abs()) / 2.0;
                let color = if avg < 20.0 {
                    COLOR_GRAY
                } else if avg < 40.0 {
                    COLOR_YELLOW
                } else {
                    COLOR_RED
                };
                painter.line_segment(
                    [egui::pos2(x1, y1), egui::pos2(x2, y2)],
                    egui::Stroke::new(1.0, color),
                );
            }
            if let Some((_, offset)) = sync_points.last() {
                let c = if offset.abs() < 20.0 {
                    COLOR_GRAY
                } else if offset.abs() < 40.0 {
                    COLOR_YELLOW
                } else {
                    COLOR_RED
                };
                let g = painter.layout_no_wrap(format!("{:+.0}ms", offset), font.clone(), c);
                painter.galley(
                    egui::pos2(sync_rect.max.x - g.size().x - 4.0, sync_rect.min.y + 1.0),
                    g,
                    c,
                );
            }
        }
    }

    // ── Lane 5: Delay sparkline ──────────────────────────────────────
    let delay_y = rect.min.y + LATENCY_GRAPH_H + VIDEO_LANE_H + AUDIO_LANE_H + AV_SYNC_H;
    let delay_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, delay_y),
        egui::vec2(rect.width(), DELAY_STRIP_H),
    );
    {
        let hist = timing.delay_ms.history();
        let vis: Vec<_> = hist
            .iter()
            .filter(|(t, _)| *t >= t_left && *t <= t_right)
            .collect();
        if vis.len() >= 2 {
            let max_delay = vis.iter().map(|(_, v)| *v).fold(0.0f64, f64::max).max(50.0);
            let points: Vec<egui::Pos2> = vis
                .iter()
                .map(|(t, v)| {
                    let x = time_to_x(*t);
                    let y =
                        delay_rect.max.y - (*v / max_delay) as f32 * (delay_rect.height() - 4.0);
                    egui::pos2(x, y)
                })
                .collect();
            for pair in points.windows(2) {
                let color = if pair[1].y > delay_rect.max.y - 4.0 {
                    COLOR_GREEN
                } else {
                    let v = (delay_rect.max.y - pair[1].y) / (delay_rect.height() - 4.0)
                        * max_delay as f32;
                    if v < 100.0 {
                        COLOR_GREEN
                    } else if v < 300.0 {
                        COLOR_YELLOW
                    } else {
                        COLOR_RED
                    }
                };
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5, color));
            }
        }
        let g = painter.layout_no_wrap(
            format!(
                "Delay {:.0}ms  AudioBuf {:.0}ms  AudioLag {:.0}ms  VideoLag {:.0}ms  {}",
                timing.delay_ms.current(),
                timing.audio_buffer_ms.current(),
                timing.audio_live_lag_ms.current(),
                timing.video_live_lag_ms.current(),
                timing.sync_state.get()
            ),
            font.clone(),
            if timing.delay_ms.current() < 100.0 {
                COLOR_GREEN
            } else if timing.delay_ms.current() < 300.0 {
                COLOR_YELLOW
            } else {
                COLOR_RED
            },
        );
        painter.galley(delay_rect.min + egui::vec2(4.0, 2.0), g, COLOR_GREEN);
    }

    // ── Lane 6: RTT sparkline ───────────────────────────────────────
    let rtt_y =
        rect.min.y + LATENCY_GRAPH_H + VIDEO_LANE_H + AUDIO_LANE_H + AV_SYNC_H + DELAY_STRIP_H;
    let rtt_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rtt_y),
        egui::vec2(rect.width(), RTT_STRIP_H),
    );
    {
        let rtt = net.rtt_ms.history();
        let vis: Vec<_> = rtt
            .iter()
            .filter(|(t, _)| *t >= t_left && *t <= t_right)
            .collect();
        if vis.len() >= 2 {
            let max_rtt = vis.iter().map(|(_, v)| *v).fold(0.0f64, f64::max).max(1.0);
            let points: Vec<egui::Pos2> = vis
                .iter()
                .map(|(t, v)| {
                    let x = time_to_x(*t);
                    let y = rtt_rect.max.y - (*v / max_rtt) as f32 * (rtt_rect.height() - 4.0);
                    egui::pos2(x, y)
                })
                .collect();
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5, COLOR_CYAN));
            }
        }
        let g = painter.layout_no_wrap(
            format!("RTT {:.0}ms", net.rtt_ms.current()),
            font.clone(),
            COLOR_CYAN,
        );
        painter.galley(rtt_rect.min + egui::vec2(4.0, 2.0), g, COLOR_CYAN);
    }

    // ── Time axis ───────────────────────────────────────────────────
    let axis_y = rect.max.y - AXIS_H;
    let axis_color = egui::Color32::from_rgb(120, 120, 120);
    let offset_secs = if *live { 0.0 } else { *scroll };
    for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
        let x = rect.min.x + sec as f32 * px_per_sec;
        let t = TIMELINE_WINDOW_SECS - sec as f32 + offset_secs;
        let g = painter.layout_no_wrap(format!("-{:.0}s", t), font.clone(), axis_color);
        painter.galley(egui::pos2(x + 2.0, axis_y), g, axis_color);
    }

    // Live/paused indicator.
    let indicator = if *live { "LIVE" } else { "PAUSED" };
    let ind_color = if *live { COLOR_GREEN } else { COLOR_YELLOW };
    let g = painter.layout_no_wrap(indicator.to_string(), font.clone(), ind_color);
    painter.galley(
        egui::pos2(rect.max.x - g.size().x - 4.0, axis_y),
        g,
        ind_color,
    );

    // ── Scroll handling ─────────────────────────────────────────────
    let id = egui::Id::new("timeline_scroll");
    let response = ui.interact(rect, id, egui::Sense::click().union(egui::Sense::hover()));
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta.y);
        if scroll_delta.abs() > 0.1 {
            *live = false;
            *scroll = (*scroll + scroll_delta * 0.5).max(0.0);
        }
    }
    if response.double_clicked() {
        *live = true;
        *scroll = 0.0;
    }
}
