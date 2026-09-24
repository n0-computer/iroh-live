//! Translucent stat bars painted over video.
//!
//! [`DebugOverlay`] draws a bottom bar of collapsible sections (NET, CAPTURE,
//! RENDER, AUDIO, TIME) from the snapshots a
//! [`Player`](iroh_live_media::Player) and a
//! [`LocalBroadcast`](iroh_live_media::LocalBroadcast) hand out. Those
//! snapshots carry only current values, so the overlay keeps the short history
//! behind its sparklines itself; the TIME panel draws the player's own
//! [`timeline`](iroh_live_media::Player::timeline) of presented frames. [`overlay_bar`] and [`fit_to_aspect`] are the
//! building blocks for callers that draw their own overlays.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use iroh_live_media::{
    FrameTiming, MediaKind, NetworkSample, PlaybackStats, PlayerStatus, PublishStats,
    PublishStatus, RenditionMode, RenditionState, SlotState,
};

/// Height of a single overlay bar (text + padding).
pub const OVERLAY_BAR_H: f32 = 15.0;

/// Paints a translucent overlay bar with monospace text at the given rect.
///
/// Does **not** allocate egui layout space: the bar is painted over existing
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

const BG_ALPHA: u8 = 200;

/// How often the overlay records a point for its sparklines. The overlay is
/// drawn every frame, and recording at the frame rate would make the length of
/// the visible history depend on how fast the window repaints.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// Points kept per sparkline, twelve seconds at [`SAMPLE_INTERVAL`].
const HISTORY_LEN: usize = 120;

const SPARK_W: f32 = 100.0;

const COLOR_GOOD: egui::Color32 = egui::Color32::from_rgb(100, 220, 100);
const COLOR_WARN: egui::Color32 = egui::Color32::from_rgb(220, 200, 80);
const COLOR_BAD: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);
const COLOR_DIM: egui::Color32 = egui::Color32::from_rgb(160, 160, 160);

/// A section of the overlay's bottom bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatCategory {
    /// The link: round trip, loss and the sender's delivery estimate on a
    /// player; the encoded video leaving a broadcast on the publish side,
    /// which has no link figures of its own.
    Net,
    /// The publish side: the source frame rate, each rendition's encoder, the
    /// audio encoder, and the state of every slot.
    Capture,
    /// The player's video: rendition mode, rendition, decoder, frame rate and
    /// decode time, plus the reason the last switch failed, if one did.
    Render,
    /// The player's audio and the playout latency.
    ///
    /// The latency sits here rather than under [`Render`](Self::Render)
    /// because it is the jitter allowance plus the audio queued at the
    /// speaker, so it moves with the audio buffer shown next to it.
    Audio,
    /// The player's timeline: how long each picture was held between its
    /// decoder and the screen, the cadence of pictures and audio, their sync,
    /// the audio buffer and the round trip, over the last ten seconds.
    ///
    /// Scrolling over the panel pauses it and moves back in time; a double
    /// click returns to the live edge.
    Time,
}

impl StatCategory {
    /// Returns the short label shown in the bottom bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Net => "NET",
            Self::Capture => "CAPTURE",
            Self::Render => "RENDER",
            Self::Audio => "AUDIO",
            Self::Time => "TIME",
        }
    }
}

/// Consolidated debug overlay with a persistent bottom bar and click-to-expand
/// detail panels.
///
/// The bottom bar shows every enabled section that has something to say, each
/// with a few key figures. Clicking a section toggles a detail panel above the
/// bar with every figure, and a sparkline next to those that change over time.
/// Several panels can be open at once.
#[derive(Debug)]
pub struct DebugOverlay {
    /// The enabled categories in bar order, each with whether its panel is
    /// open.
    categories: Vec<(StatCategory, bool)>,
    visible: bool,
    history: History,
    /// What the transport says about the link, beyond the network signals:
    /// the path's kind and address, say. The first line joins the summary.
    link: Vec<String>,
    /// How far back from now the timeline shows, in seconds, while paused.
    timeline_scroll: f32,
    /// When the timeline was paused, which its right edge is measured back
    /// from; `None` while it follows the live edge.
    timeline_paused: Option<Instant>,
    /// Salts every interactive id this overlay claims, so a grid of tiles does
    /// not share them. Two overlays under one id are one widget as far as egui
    /// is concerned, and hovering a section on one tile would light the same
    /// section on every other.
    salt: egui::Id,
}

/// Hands each [`DebugOverlay`] an id nothing else uses.
static OVERLAY_SALT: AtomicU64 = AtomicU64::new(0);

impl DebugOverlay {
    /// Creates a new overlay with the given categories enabled, all collapsed.
    pub fn new(categories: &[StatCategory]) -> Self {
        Self {
            categories: categories.iter().map(|&cat| (cat, false)).collect(),
            visible: true,
            history: History::default(),
            link: Vec::new(),
            timeline_scroll: 0.0,
            timeline_paused: None,
            salt: egui::Id::new((
                "iroh-live-egui overlay",
                OVERLAY_SALT.fetch_add(1, Ordering::Relaxed),
            )),
        }
    }

    /// Toggles overall visibility.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Sets what the transport says about the link, shown in the NET section
    /// of a player: the media crate knows nothing of paths or addresses, so a
    /// caller that does hands them in here. The first line also joins the
    /// section's summary.
    pub fn set_link(&mut self, lines: Vec<String>) {
        self.link = lines;
    }

    /// Returns true if any detail panel is currently expanded.
    pub fn any_expanded(&self) -> bool {
        self.categories.iter().any(|&(_, expanded)| expanded)
    }

    /// Draws the overlay for a player at the bottom of `video_rect`.
    ///
    /// Uses the NET, RENDER, AUDIO and TIME categories; CAPTURE has nothing to
    /// show for a player and is left out of the bar. `timeline` is what
    /// [`Player::timeline`](iroh_live_media::Player::timeline) returns, read
    /// only when the TIME panel is open, so a caller can pass an empty slice
    /// otherwise.
    pub fn show_playback(
        &mut self,
        ui: &mut egui::Ui,
        video_rect: egui::Rect,
        stats: &PlaybackStats,
        status: &PlayerStatus,
        timeline: &[FrameTiming],
    ) {
        if !self.visible {
            return;
        }
        let sections: Vec<Section> = self
            .categories
            .iter()
            .filter_map(|&(cat, _)| match cat {
                StatCategory::Net => Some(net_playback(stats.network.as_ref(), &self.link)),
                StatCategory::Render => Some(render_playback(stats, status)),
                StatCategory::Audio => Some(audio_playback(stats, status)),
                StatCategory::Time => Some(time_playback(timeline)),
                StatCategory::Capture => None,
            })
            .collect();
        self.draw(ui, video_rect, &sections, timeline);
    }

    /// Reports whether the TIME panel is open, so a caller knows whether to
    /// read the player's timeline for [`show_playback`](Self::show_playback).
    pub fn timeline_open(&self) -> bool {
        self.visible && self.is_expanded(StatCategory::Time)
    }

    /// Draws the overlay for a local broadcast at the bottom of `video_rect`.
    ///
    /// Uses the CAPTURE and NET categories. NET shows the encoded video the
    /// broadcast sends, summed over its renditions, since the publish side
    /// carries no link figures. RENDER and AUDIO are left out of the bar.
    pub fn show_publish(
        &mut self,
        ui: &mut egui::Ui,
        video_rect: egui::Rect,
        stats: &PublishStats,
        status: &PublishStatus,
    ) {
        if !self.visible {
            return;
        }
        let sections: Vec<Section> = self
            .categories
            .iter()
            .filter_map(|&(cat, _)| match cat {
                StatCategory::Capture => Some(capture_publish(stats, status)),
                StatCategory::Net => Some(net_publish(stats)),
                StatCategory::Render | StatCategory::Audio | StatCategory::Time => None,
            })
            .collect();
        self.draw(ui, video_rect, &sections, &[]);
    }

    fn is_expanded(&self, cat: StatCategory) -> bool {
        self.categories
            .iter()
            .any(|&(c, expanded)| c == cat && expanded)
    }

    /// Records the history, then paints the open panels and the bottom bar.
    fn draw(
        &mut self,
        ui: &mut egui::Ui,
        video_rect: egui::Rect,
        sections: &[Section],
        timeline: &[FrameTiming],
    ) {
        self.history.record(sections, Instant::now());
        let font = egui::FontId::monospace(11.0);

        // Open panels stack upward from the bar. Walking the sections in
        // reverse puts the first category's panel on top.
        let mut y_cursor = video_rect.max.y - OVERLAY_BAR_H;
        for section in sections.iter().rev() {
            if section.lines.is_empty() || !self.is_expanded(section.category) {
                continue;
            }
            let height = OVERLAY_BAR_H * (section.lines.len() as f32 + 0.5);
            y_cursor -= height;
            let rect = egui::Rect::from_min_size(
                egui::pos2(video_rect.min.x, y_cursor),
                egui::vec2(video_rect.width(), height),
            );
            self.paint_panel(ui.painter(), rect, &section.lines, &font);
            // The timeline sits above the TIME section's own figures.
            if section.category == StatCategory::Time {
                y_cursor -= TIMELINE_H;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(video_rect.min.x, y_cursor),
                    egui::vec2(video_rect.width(), TIMELINE_H),
                );
                self.paint_timeline(ui, rect, timeline);
            }
        }

        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - OVERLAY_BAR_H),
            egui::vec2(video_rect.width(), OVERLAY_BAR_H),
        );
        let painter = ui.painter();
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));

        let mut clicks = Vec::new();
        let mut x = bar_rect.min.x + 8.0;
        for (i, section) in sections.iter().enumerate() {
            let galley =
                painter.layout_no_wrap(section.summary.clone(), font.clone(), egui::Color32::WHITE);
            let section_width = galley.size().x + 8.0;
            let section_rect = egui::Rect::from_min_size(
                egui::pos2(x - 4.0, bar_rect.min.y),
                egui::vec2(section_width, OVERLAY_BAR_H),
            );

            // A lighter background and an underline on hover show that the
            // section can be clicked.
            let id = self.salt.with(("dbg_section", section.category.label()));
            let response = ui.interact(section_rect, id, egui::Sense::click());
            if response.hovered() {
                painter.rect_filled(section_rect, 2.0, egui::Color32::from_white_alpha(30));
                painter.line_segment(
                    [section_rect.left_bottom(), section_rect.right_bottom()],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(80)),
                );
            }
            if response.clicked() {
                clicks.push(section.category);
            }

            painter.galley(
                egui::pos2(x, bar_rect.min.y + 1.0),
                galley,
                egui::Color32::WHITE,
            );
            x += section_width;

            if i + 1 < sections.len() {
                let sep_x = x + 2.0;
                painter.line_segment(
                    [
                        egui::pos2(sep_x, bar_rect.min.y + 3.0),
                        egui::pos2(sep_x, bar_rect.max.y - 3.0),
                    ],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(40)),
                );
                x += 12.0;
            }
        }

        for cat in clicks {
            for (c, expanded) in &mut self.categories {
                if *c == cat {
                    *expanded = !*expanded;
                }
            }
        }
    }

    /// Paints one detail panel, with a sparkline beside every line that has a
    /// history of at least two points.
    fn paint_panel(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        lines: &[Line],
        font: &egui::FontId,
    ) {
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));
        let mut y = rect.min.y + 2.0;
        for line in lines {
            let galley =
                painter.layout_no_wrap(format!("  {}", line.text), font.clone(), line.color);
            painter.galley(egui::pos2(rect.min.x + 6.0, y), galley, line.color);

            let history = line
                .plot
                .as_ref()
                .and_then(|(key, _)| self.history.get(key))
                .filter(|values| values.len() >= 2);
            if let Some(values) = history {
                let spark_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.max.x - SPARK_W - 6.0, y + 2.0),
                    egui::vec2(SPARK_W, OVERLAY_BAR_H - 4.0),
                );
                paint_sparkline(painter, spark_rect, values, line.color);
            }
            y += OVERLAY_BAR_H;
        }
    }
}

/// One section of the bar: its summary, and the lines of its detail panel.
struct Section {
    category: StatCategory,
    summary: String,
    lines: Vec<Line>,
}

impl Section {
    /// Creates a section whose summary joins the category label and `parts`.
    fn new(category: StatCategory, parts: Vec<String>, lines: Vec<Line>) -> Self {
        let mut summary = category.label().to_string();
        for part in parts {
            summary.push(' ');
            summary.push_str(&part);
        }
        Self {
            category,
            summary,
            lines,
        }
    }
}

/// One line of a detail panel.
struct Line {
    text: String,
    color: egui::Color32,
    /// The history key and the current value, for a line with a sparkline.
    plot: Option<(String, f64)>,
}

impl Line {
    /// Creates a line of plain information.
    fn info(text: impl Into<String>) -> Self {
        Self::colored(text, COLOR_DIM)
    }

    /// Creates a line in `color`, with no sparkline.
    fn colored(text: impl Into<String>, color: egui::Color32) -> Self {
        Self {
            text: text.into(),
            color,
            plot: None,
        }
    }

    /// Creates a line whose `value` is recorded under `key` and plotted.
    fn metric(key: impl Into<String>, text: String, value: f64, color: egui::Color32) -> Self {
        Self {
            text,
            color,
            plot: Some((key.into(), value)),
        }
    }
}

/// The overlay's own record of the figures it plots.
///
/// Every series is a ring of at most [`HISTORY_LEN`] points, and a series not
/// fed for that many samples is dropped, so the history stays bounded however
/// many figures come and go.
#[derive(Debug, Default)]
struct History {
    last_sample: Option<Instant>,
    tick: u64,
    series: BTreeMap<String, Series>,
}

#[derive(Debug, Default)]
struct Series {
    values: VecDeque<f64>,
    /// When each value was recorded, for the timeline's strips.
    times: VecDeque<Instant>,
    last_tick: u64,
}

impl History {
    /// Appends the current value of every plotted line, at most once per
    /// [`SAMPLE_INTERVAL`].
    fn record(&mut self, sections: &[Section], now: Instant) {
        if self
            .last_sample
            .is_some_and(|last| now.duration_since(last) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_sample = Some(now);
        self.tick += 1;
        let tick = self.tick;
        let points = sections
            .iter()
            .flat_map(|section| &section.lines)
            .filter_map(|line| line.plot.as_ref());
        for (key, value) in points {
            let series = self.series.entry(key.clone()).or_default();
            if series.values.len() >= HISTORY_LEN {
                series.values.pop_front();
                series.times.pop_front();
            }
            series.values.push_back(*value);
            series.times.push_back(now);
            series.last_tick = tick;
        }
        self.series
            .retain(|_, series| tick - series.last_tick < HISTORY_LEN as u64);
    }

    /// Returns the recorded points of `key`, oldest first.
    fn get(&self, key: &str) -> Option<&VecDeque<f64>> {
        self.series.get(key).map(|series| &series.values)
    }

    /// Returns the recorded points of `key` with when each was recorded,
    /// oldest first.
    fn timed(&self, key: &str) -> Vec<(Instant, f64)> {
        self.series
            .get(key)
            .map(|series| {
                series
                    .times
                    .iter()
                    .copied()
                    .zip(series.values.iter().copied())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Draws a small line graph of `values`, scaled to their own range.
fn paint_sparkline(
    painter: &egui::Painter,
    rect: egui::Rect,
    values: &VecDeque<f64>,
    color: egui::Color32,
) {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = (max - min).max(0.001);
    let last = (values.len() - 1).max(1) as f32;
    let points: Vec<egui::Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.min.x + (i as f32 / last) * rect.width();
            let y = rect.max.y - ((v - min) / range) as f32 * rect.height();
            egui::pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.0_f32, color.linear_multiply(0.7)),
    ));
}

/// Grades a figure where lower is better, such as a round trip.
fn lower_is_better(value: f64, good: f64, warn: f64) -> egui::Color32 {
    if value < good {
        COLOR_GOOD
    } else if value < warn {
        COLOR_WARN
    } else {
        COLOR_BAD
    }
}

/// Grades a figure where higher is better, such as a frame rate.
fn higher_is_better(value: f64, good: f64, warn: f64) -> egui::Color32 {
    if value > good {
        COLOR_GOOD
    } else if value > warn {
        COLOR_WARN
    } else {
        COLOR_BAD
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Formats a byte count in megabytes, or kilobytes below one.
fn format_bytes(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1_000_000.0 {
        format!("{:.1} MB", bytes / 1_000_000.0)
    } else {
        format!("{:.0} kB", bytes / 1_000.0)
    }
}

/// Returns a line for the state of the slot called `name`.
fn slot_line(name: &str, state: &SlotState) -> Line {
    match state {
        SlotState::Off => Line::info(format!("{name}: off")),
        SlotState::Starting => Line::colored(format!("{name}: starting"), COLOR_WARN),
        SlotState::Running => Line::colored(format!("{name}: running"), COLOR_GOOD),
        SlotState::Failed(err) => Line::colored(format!("{name}: failed: {err}"), COLOR_BAD),
        SlotState::Ended => Line::info(format!("{name}: ended")),
        other => Line::info(format!("{name}: {other:?}")),
    }
}

/// Describes a rendition mode in a few words.
fn mode_text(mode: &RenditionMode) -> String {
    match mode {
        RenditionMode::Auto { max_height: None } => "auto".to_string(),
        RenditionMode::Auto {
            max_height: Some(height),
        } => format!("auto, up to {height}p"),
        RenditionMode::Pinned(name) => format!("pinned to {name}"),
        RenditionMode::Off => "off".to_string(),
        other => format!("{other:?}"),
    }
}

/// Builds the NET section of a player from the last link reading and what
/// the transport said about the link.
fn net_playback(network: Option<&NetworkSample>, link: &[String]) -> Section {
    let mut parts: Vec<String> = link.first().cloned().into_iter().collect();
    let mut lines: Vec<Line> = link.iter().map(|line| Line::info(line.clone())).collect();
    let Some(net) = network else {
        if parts.is_empty() {
            parts.push("no link data".to_string());
        }
        lines.push(Line::info(
            "no link data: the broadcast carries no network signals",
        ));
        return Section::new(StatCategory::Net, parts, lines);
    };
    if let Some(rtt) = net.rtt {
        let ms = millis(rtt);
        parts.push(format!("rtt:{ms:.0}ms"));
        let color = lower_is_better(ms, 100.0, 200.0);
        lines.push(Line::metric(
            "net.rtt",
            format!("rtt: {ms:.1} ms"),
            ms,
            color,
        ));
    }
    if let Some(min_rtt) = net.min_rtt {
        lines.push(Line::info(format!("min rtt: {:.1} ms", millis(min_rtt))));
    }
    if let Some(loss) = net.loss {
        let pct = f64::from(loss) * 100.0;
        parts.push(format!("loss:{pct:.1}%"));
        let color = lower_is_better(pct, 2.0, 10.0);
        lines.push(Line::metric(
            "net.loss",
            format!("loss: {pct:.1}%"),
            pct,
            color,
        ));
    }
    if let Some(delivery) = net.delivery {
        parts.push(format!("bw:{delivery}"));
        lines.push(Line::metric(
            "net.delivery",
            format!("delivery estimate: {delivery}"),
            delivery.as_bps() as f64,
            egui::Color32::WHITE,
        ));
    }
    lines.push(Line::info(format!(
        "path generation: {}",
        net.path_generation
    )));
    Section::new(StatCategory::Net, parts, lines)
}

/// Builds the RENDER section of a player.
fn render_playback(stats: &PlaybackStats, status: &PlayerStatus) -> Section {
    let video = stats.video.as_ref();
    let rendition = video
        .map(|v| v.rendition.as_str())
        .filter(|r| !r.is_empty())
        .or(status.rendition.as_deref());
    let decoder = video
        .map(|v| v.decoder.as_str())
        .filter(|d| !d.is_empty())
        .or(status.decoder.as_deref());

    let mut parts = Vec::new();
    let mut lines = vec![
        Line::info(format!("mode: {}", mode_text(&status.mode))),
        slot_line("video", &status.video),
    ];
    if matches!(status.mode, RenditionMode::Off) {
        parts.push("off".to_string());
    }
    if let Some(rendition) = rendition {
        parts.push(rendition.to_string());
        let text = match &status.switching_to {
            Some(next) => format!("rendition: {rendition} -> {next} (warming up)"),
            None => format!("rendition: {rendition}"),
        };
        lines.push(Line::info(text));
    }
    if let Some(err) = &status.switch_error {
        parts.push("switch failed".to_string());
        lines.push(Line::colored(format!("switch error: {err}"), COLOR_BAD));
    }
    if let Some(decoder) = decoder {
        lines.push(Line::info(format!("decoder: {decoder}")));
    }
    if let Some(video) = video {
        if let Some(size) = video.size {
            lines.push(Line::info(format!("size: {size}")));
        }
        if let Some(fps) = video.fps {
            let fps = f64::from(fps);
            parts.push(format!("{fps:.0}fps"));
            let color = higher_is_better(fps, 24.0, 15.0);
            lines.push(Line::metric(
                "render.fps",
                format!("fps: {fps:.0}"),
                fps,
                color,
            ));
        }
        if let Some(decode) = video.decode_time {
            let ms = millis(decode);
            parts.push(format!("dec:{ms:.1}ms"));
            let color = lower_is_better(ms, 16.0, 33.0);
            lines.push(Line::metric(
                "render.decode",
                format!("decode: {ms:.1} ms"),
                ms,
                color,
            ));
        }
        let color = if video.skipped > 0 {
            COLOR_WARN
        } else {
            COLOR_DIM
        };
        lines.push(Line::colored(
            format!("frames: {}, skipped: {}", video.frames, video.skipped),
            color,
        ));
    }
    Section::new(StatCategory::Render, parts, lines)
}

/// Builds the AUDIO section of a player, latency included.
fn audio_playback(stats: &PlaybackStats, status: &PlayerStatus) -> Section {
    let mut parts = Vec::new();
    let mut lines = vec![slot_line("audio", &status.audio)];
    if let Some(audio) = &stats.audio {
        if !audio.rendition.is_empty() {
            lines.push(Line::info(format!("rendition: {}", audio.rendition)));
        }
        let buffered = millis(audio.buffered);
        parts.push(format!("buf:{buffered:.0}ms"));
        lines.push(Line::metric(
            "audio.buffered",
            format!("buffered: {buffered:.0} ms"),
            buffered,
            higher_is_better(buffered, 40.0, 15.0),
        ));
        let peak = f64::from(audio.peak);
        let level = if peak > 0.0 {
            format!("{:.1} dBFS", 20.0 * peak.log10())
        } else {
            "silent".to_string()
        };
        lines.push(Line::metric(
            "audio.peak",
            format!("peak: {level}"),
            peak,
            egui::Color32::WHITE,
        ));
        lines.push(Line::info(format!("frames: {}", audio.frames)));
    }
    let latency = millis(stats.latency);
    parts.push(format!("lat:{latency:.0}ms"));
    lines.push(Line::metric(
        "audio.latency",
        format!("latency: {latency:.0} ms"),
        latency,
        lower_is_better(latency, 100.0, 200.0),
    ));
    Section::new(StatCategory::Audio, parts, lines)
}

/// Builds the CAPTURE section of a broadcast.
///
/// Lists every rendition either snapshot knows of: the status names audio
/// renditions and idle video ones too, while the stats hold an entry only for
/// a video rendition whose encoder has started.
fn capture_publish(stats: &PublishStats, status: &PublishStatus) -> Section {
    let mut parts = Vec::new();
    let mut lines = vec![slot_line("video", &status.video)];
    if let Some(fps) = stats.source_fps {
        let fps = f64::from(fps);
        parts.push(format!("src:{fps:.0}fps"));
        lines.push(Line::metric(
            "capture.source_fps",
            format!("source: {fps:.1} fps"),
            fps,
            higher_is_better(fps, 24.0, 15.0),
        ));
    }

    let names: BTreeSet<&String> = status
        .renditions
        .keys()
        .chain(stats.renditions.keys())
        .collect();
    let encoding = status
        .renditions
        .values()
        .filter(|state| matches!(state, RenditionState::Encoding { .. }))
        .count();
    if !names.is_empty() {
        parts.push(format!("{encoding}/{} encoding", names.len()));
    }
    for name in names {
        let state = status.renditions.get(name);
        let mut text = format!("{name}:");
        let color = match state {
            Some(RenditionState::Idle) => {
                text.push_str(" idle");
                COLOR_DIM
            }
            Some(RenditionState::Failed(err)) => {
                text.push_str(&format!(" failed: {err}"));
                COLOR_BAD
            }
            _ => egui::Color32::WHITE,
        };
        let Some(encode) = stats.renditions.get(name) else {
            if let Some(RenditionState::Encoding { encoder }) = state {
                text.push(' ');
                text.push_str(encoder);
            }
            lines.push(Line::colored(text, color));
            continue;
        };
        let encoder = encode.encoder.as_deref().or(match state {
            Some(RenditionState::Encoding { encoder }) => Some(encoder.as_str()),
            _ => None,
        });
        let mut fields: Vec<String> = Vec::new();
        fields.extend(encoder.map(str::to_string));
        fields.extend(encode.size.map(|size| size.to_string()));
        fields.extend(encode.fps.map(|fps| format!("{fps:.1}fps")));
        fields.extend(encode.bitrate.map(|bitrate| bitrate.to_string()));
        fields.extend(
            encode
                .encode_time
                .map(|time| format!("enc:{:.1}ms", millis(time))),
        );
        fields.push(format!("{} frames", encode.frames));
        for field in fields {
            text.push(' ');
            text.push_str(&field);
        }
        match encode.bitrate {
            Some(bitrate) => lines.push(Line::metric(
                format!("capture.{name}.bitrate"),
                text,
                bitrate.as_bps() as f64,
                color,
            )),
            None => lines.push(Line::colored(text, color)),
        }
    }

    lines.push(slot_line("audio", &status.audio));
    if let Some(audio) = &stats.audio {
        let codec = audio.codec.as_deref().unwrap_or("audio");
        let color = if audio.dropped > 0 {
            COLOR_WARN
        } else {
            COLOR_DIM
        };
        lines.push(Line::colored(
            format!(
                "{codec}: {} frames, {} dropped",
                audio.frames, audio.dropped
            ),
            color,
        ));
        if audio.dropped > 0 {
            parts.push(format!("audio dropped:{}", audio.dropped));
        }
    }
    Section::new(StatCategory::Capture, parts, lines)
}

/// Builds the NET section of a broadcast: the encoded video it sends, summed
/// over its renditions.
fn net_publish(stats: &PublishStats) -> Section {
    let rates: Vec<u64> = stats
        .renditions
        .values()
        .filter_map(|encode| encode.bitrate.map(|bitrate| bitrate.as_bps()))
        .collect();
    let bytes: u64 = stats.renditions.values().map(|encode| encode.bytes).sum();
    if rates.is_empty() {
        return Section::new(
            StatCategory::Net,
            vec!["idle".to_string()],
            vec![Line::info("no video encoding")],
        );
    }
    let total = iroh_live_media::Bitrate::from_bps(rates.iter().sum());
    let lines = vec![
        Line::metric(
            "net.out",
            format!("video out: {total} over {} renditions", rates.len()),
            total.as_bps() as f64,
            egui::Color32::WHITE,
        ),
        Line::info(format!("video sent: {}", format_bytes(bytes))),
    ];
    Section::new(StatCategory::Net, vec![format!("out:{total}")], lines)
}

/// Returns `later - earlier` in milliseconds, negative when `later` is the
/// earlier of the two.
fn signed_ms(later: Instant, earlier: Instant) -> f32 {
    match later.checked_duration_since(earlier) {
        Some(ahead) => ahead.as_secs_f32() * 1000.0,
        None => -(earlier.duration_since(later).as_secs_f32() * 1000.0),
    }
}

/// How much later a picture was presented than the audio with the closest
/// timestamp, in milliseconds, or `None` without audio.
///
/// `audio` is sorted by timestamp, so the closest is found by a binary search
/// rather than a scan per picture.
fn av_offset(video: &FrameTiming, audio: &[&FrameTiming]) -> Option<f32> {
    let at = audio.partition_point(|timing| timing.pts < video.pts);
    let closest = [at.checked_sub(1), Some(at)]
        .into_iter()
        .flatten()
        .filter_map(|index| audio.get(index))
        .min_by_key(|timing| video.pts.abs_diff(timing.pts))?;
    Some(signed_ms(video.presented, closest.presented))
}

/// Grades an A/V offset: within 20 ms nobody sees it, past 40 ms lips drift.
fn av_color(offset_ms: f32) -> egui::Color32 {
    match offset_ms.abs() {
        abs if abs < 20.0 => COLOR_DIM,
        abs if abs < 40.0 => COLOR_WARN,
        _ => COLOR_BAD,
    }
}

/// Builds the TIME section of a player from its timeline.
fn time_playback(timeline: &[FrameTiming]) -> Section {
    let video = timeline
        .iter()
        .rev()
        .find(|timing| timing.kind == MediaKind::Video);
    let mut audio: Vec<&FrameTiming> = timeline
        .iter()
        .filter(|timing| timing.kind == MediaKind::Audio)
        .collect();
    audio.sort_by_key(|timing| timing.pts);
    let mut parts = Vec::new();
    let mut lines = Vec::new();
    if let Some(video) = video {
        let hold = signed_ms(video.presented, video.decoded);
        parts.push(format!("hold:{hold:.0}ms"));
        lines.push(Line::metric(
            "time.hold",
            format!("decode to screen: {hold:.0} ms"),
            f64::from(hold),
            lower_is_better(f64::from(hold), 100.0, 200.0),
        ));
        if let Some(offset) = av_offset(video, &audio) {
            parts.push(format!("av:{offset:+.0}ms"));
            lines.push(Line::metric(
                "time.av",
                format!("a/v: {offset:+.0} ms (positive: the picture is late)"),
                f64::from(offset),
                av_color(offset),
            ));
        }
    }
    if lines.is_empty() {
        lines.push(Line::info(
            "no frames presented yet; the timeline fills while the panel is open",
        ));
    }
    Section::new(StatCategory::Time, parts, lines)
}

/// Seconds of history the timeline spans.
const TIMELINE_WINDOW_SECS: f32 = 10.0;

/// How far back the paused timeline scrolls, in seconds: about as far as the
/// player's timeline and the overlay's own history reach.
const TIMELINE_SCROLL_MAX: f32 = 12.0;
const HOLD_LANE_H: f32 = 36.0;
const VIDEO_LANE_H: f32 = 20.0;
const AUDIO_LANE_H: f32 = 16.0;
const AV_LANE_H: f32 = 20.0;
const BUFFER_LANE_H: f32 = 26.0;
const RTT_LANE_H: f32 = 26.0;
const AXIS_H: f32 = 14.0;
/// The timeline's height: every lane and the axis, plus a little air.
const TIMELINE_H: f32 = HOLD_LANE_H
    + VIDEO_LANE_H
    + AUDIO_LANE_H
    + AV_LANE_H
    + BUFFER_LANE_H
    + RTT_LANE_H
    + AXIS_H
    + 4.0;

const COLOR_AUDIO: egui::Color32 = egui::Color32::from_rgb(68, 136, 204);
const COLOR_RTT: egui::Color32 = egui::Color32::from_rgb(0, 200, 200);
const COLOR_GRID: egui::Color32 = egui::Color32::from_rgb(50, 50, 50);

/// Grades a picture's gap to the one before against the usual gap: steady is
/// good, half again as long is a hiccup, twice as long is a dropped frame.
fn gap_color(gap_ms: f32, expected_ms: f32) -> egui::Color32 {
    let ratio = gap_ms / expected_ms.max(1.0);
    if ratio < 1.5 {
        COLOR_GOOD
    } else if ratio < 2.0 {
        COLOR_WARN
    } else {
        COLOR_BAD
    }
}

/// Where a timeline lane is, and how times map onto it.
struct Lanes {
    rect: egui::Rect,
    left: Instant,
    right: Instant,
}

impl Lanes {
    /// The x coordinate of `at`.
    fn x(&self, at: Instant) -> f32 {
        let px_per_sec = self.rect.width() / TIMELINE_WINDOW_SECS;
        self.rect.min.x + signed_ms(at, self.left) / 1000.0 * px_per_sec
    }

    /// Whether `at` is inside the window.
    fn shows(&self, at: Instant) -> bool {
        at >= self.left && at <= self.right
    }

    /// The lane `height` tall starting `top` below the timeline's top.
    fn lane(&self, top: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(self.rect.min.x, self.rect.min.y + top),
            egui::vec2(self.rect.width(), height),
        )
    }
}

impl DebugOverlay {
    /// Paints the timeline: how long each picture was held, picture and audio
    /// cadence, A/V offset, the audio buffer and the round trip, over a
    /// scrollable time axis.
    fn paint_timeline(&mut self, ui: &mut egui::Ui, rect: egui::Rect, timeline: &[FrameTiming]) {
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));
        let font = egui::FontId::monospace(9.0);

        let now = Instant::now();
        // Paused, the right edge stays where it was put rather than sliding
        // along with the clock.
        let right = match self.timeline_paused {
            None => now,
            Some(at) => at
                .checked_sub(Duration::from_secs_f32(self.timeline_scroll))
                .unwrap_or(at),
        };
        let left = right
            .checked_sub(Duration::from_secs_f32(TIMELINE_WINDOW_SECS))
            .unwrap_or(right);
        let lanes = Lanes { rect, left, right };
        let px_per_sec = rect.width() / TIMELINE_WINDOW_SECS;

        // A grid line every two seconds.
        for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
            let x = rect.min.x + sec as f32 * px_per_sec;
            painter.line_segment(
                [
                    egui::pos2(x, rect.min.y),
                    egui::pos2(x, rect.max.y - AXIS_H),
                ],
                egui::Stroke::new(1.0_f32, COLOR_GRID),
            );
        }

        let visible: Vec<&FrameTiming> = timeline
            .iter()
            .filter(|timing| lanes.shows(timing.presented))
            .collect();
        let video: Vec<&FrameTiming> = visible
            .iter()
            .copied()
            .filter(|timing| timing.kind == MediaKind::Video)
            .collect();
        let audio: Vec<&FrameTiming> = visible
            .iter()
            .copied()
            .filter(|timing| timing.kind == MediaKind::Audio)
            .collect();
        let mut audio_by_pts = audio.clone();
        audio_by_pts.sort_by_key(|timing| timing.pts);
        let label = |rect: egui::Rect, text: &str, color: egui::Color32| {
            let galley = painter.layout_no_wrap(text.to_string(), font.clone(), color);
            painter.galley(rect.min + egui::vec2(4.0, 1.0), galley, color);
        };
        let value = |rect: egui::Rect, text: String, color: egui::Color32| {
            let galley = painter.layout_no_wrap(text, font.clone(), color);
            let at = egui::pos2(rect.max.x - galley.size().x - 4.0, rect.min.y + 1.0);
            painter.galley(at, galley, color);
        };

        // How long each picture was held between its decoder and the screen.
        let mut top = 0.0;
        let hold_rect = lanes.lane(top, HOLD_LANE_H);
        label(hold_rect, "HOLD", COLOR_DIM);
        let holds: Vec<(f32, f32)> = video
            .iter()
            .map(|timing| {
                (
                    lanes.x(timing.presented),
                    signed_ms(timing.presented, timing.decoded),
                )
            })
            .collect();
        if holds.len() >= 2 {
            let max = holds
                .iter()
                .map(|&(_, hold)| hold)
                .fold(0.0f32, f32::max)
                .max(50.0);
            let height = hold_rect.height() - 14.0;
            for pair in holds.windows(2) {
                let (x1, h1) = pair[0];
                let (x2, h2) = pair[1];
                let y1 = hold_rect.max.y - (h1 / max) * height;
                let y2 = hold_rect.max.y - (h2 / max) * height;
                let color = lower_is_better(f64::from(h1 + h2) / 2.0, 100.0, 200.0);
                painter.line_segment(
                    [egui::pos2(x1, y1), egui::pos2(x2, y2)],
                    egui::Stroke::new(1.5_f32, color),
                );
            }
        }
        if let Some(&(_, hold)) = holds.last() {
            let color = lower_is_better(f64::from(hold), 100.0, 200.0);
            value(hold_rect, format!("{hold:.0}ms"), color);
        }
        top += HOLD_LANE_H;

        // One box per picture, coloured by its gap to the one before.
        let video_rect = lanes.lane(top, VIDEO_LANE_H);
        label(video_rect, "VIDEO", COLOR_DIM);
        let mut gaps: Vec<f32> = video
            .windows(2)
            .map(|pair| signed_ms(pair[1].presented, pair[0].presented))
            .collect();
        let expected = match gaps.len() >= 2 {
            true => {
                gaps.sort_by(f32::total_cmp);
                gaps[gaps.len() / 2]
            }
            false => 1000.0 / 30.0,
        };
        for (index, timing) in video.iter().enumerate() {
            let x = lanes.x(timing.presented);
            let next = video
                .get(index + 1)
                .map_or(x + 6.0, |next| lanes.x(next.presented));
            let width = (next - x - 1.0).clamp(3.0, 20.0);
            let gap = match index {
                0 => expected,
                _ => signed_ms(timing.presented, video[index - 1].presented),
            };
            let frame = egui::Rect::from_min_size(
                egui::pos2(x, video_rect.min.y + 7.0),
                egui::vec2(width, VIDEO_LANE_H - 8.0),
            );
            painter.rect_filled(frame, 1.0, gap_color(gap, expected));
        }
        top += VIDEO_LANE_H;

        // One box per block of audio, red when it reached the speaker more
        // than 100 ms after it decoded.
        let audio_rect = lanes.lane(top, AUDIO_LANE_H);
        label(audio_rect, "AUDIO", COLOR_AUDIO);
        for (index, timing) in audio.iter().enumerate() {
            let x = lanes.x(timing.presented);
            let next = audio
                .get(index + 1)
                .map_or(x + 4.0, |next| lanes.x(next.presented));
            let width = (next - x - 0.5).clamp(2.0, 10.0);
            let color = match signed_ms(timing.presented, timing.decoded) > 100.0 {
                true => COLOR_BAD,
                false => COLOR_AUDIO,
            };
            let block = egui::Rect::from_min_size(
                egui::pos2(x, audio_rect.min.y + 5.0),
                egui::vec2(width, AUDIO_LANE_H - 6.0),
            );
            painter.rect_filled(block, 1.0, color);
        }
        top += AUDIO_LANE_H;

        // How much later each picture was presented than its audio.
        let av_rect = lanes.lane(top, AV_LANE_H);
        let zero = av_rect.center().y;
        painter.line_segment(
            [
                egui::pos2(av_rect.min.x, zero),
                egui::pos2(av_rect.max.x, zero),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(60, 60, 60)),
        );
        label(av_rect, "A/V", COLOR_DIM);
        // The offset at which the line reaches the lane's edge.
        const AV_RANGE_MS: f32 = 80.0;
        let half = av_rect.height() / 2.0 - 2.0;
        let offsets: Vec<(f32, f32)> = video
            .iter()
            .filter_map(|timing| {
                Some((lanes.x(timing.presented), av_offset(timing, &audio_by_pts)?))
            })
            .collect();
        for pair in offsets.windows(2) {
            let (x1, o1) = pair[0];
            let (x2, o2) = pair[1];
            let y1 = zero - (o1 / AV_RANGE_MS).clamp(-1.0, 1.0) * half;
            let y2 = zero - (o2 / AV_RANGE_MS).clamp(-1.0, 1.0) * half;
            painter.line_segment(
                [egui::pos2(x1, y1), egui::pos2(x2, y2)],
                egui::Stroke::new(1.0_f32, av_color((o1.abs() + o2.abs()) / 2.0)),
            );
        }
        if let Some(&(_, offset)) = offsets.last() {
            value(av_rect, format!("{offset:+.0}ms"), av_color(offset));
        }
        top += AV_LANE_H;

        // The audio queued at the speaker, and the round trip, from the
        // overlay's own history of the AUDIO and NET figures.
        for (key, name, height, color) in [
            ("audio.buffered", "BUFFER", BUFFER_LANE_H, COLOR_AUDIO),
            ("net.rtt", "RTT", RTT_LANE_H, COLOR_RTT),
        ] {
            let strip = lanes.lane(top, height);
            let points: Vec<(Instant, f64)> = self
                .history
                .timed(key)
                .into_iter()
                .filter(|&(at, _)| lanes.shows(at))
                .collect();
            let max = points
                .iter()
                .map(|&(_, value)| value)
                .fold(0.0f64, f64::max)
                .max(1.0);
            let line: Vec<egui::Pos2> = points
                .iter()
                .map(|&(at, value)| {
                    let y = strip.max.y - (value / max) as f32 * (strip.height() - 4.0);
                    egui::pos2(lanes.x(at), y)
                })
                .collect();
            if line.len() >= 2 {
                painter.add(egui::Shape::line(line, egui::Stroke::new(1.5_f32, color)));
            }
            let text = match points.last() {
                Some(&(_, current)) => format!("{name} {current:.0}ms"),
                None => format!("{name} -"),
            };
            label(strip, &text, color);
            top += height;
        }

        // The axis, in seconds before the right edge.
        let axis_y = rect.max.y - AXIS_H;
        let axis_color = egui::Color32::from_rgb(120, 120, 120);
        let offset = now.saturating_duration_since(right).as_secs_f32();
        for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
            let x = rect.min.x + sec as f32 * px_per_sec;
            let ago = TIMELINE_WINDOW_SECS - sec as f32 + offset;
            let galley = painter.layout_no_wrap(format!("-{ago:.0}s"), font.clone(), axis_color);
            painter.galley(egui::pos2(x + 2.0, axis_y), galley, axis_color);
        }
        let (indicator, color) = match self.timeline_paused {
            None => ("LIVE", COLOR_GOOD),
            Some(_) => ("PAUSED", COLOR_WARN),
        };
        let galley = painter.layout_no_wrap(indicator.to_string(), font.clone(), color);
        painter.galley(
            egui::pos2(rect.max.x - galley.size().x - 4.0, axis_y),
            galley,
            color,
        );

        // Scrolling pauses and moves back in time; a double click resumes.
        let id = self.salt.with("timeline_scroll");
        let response = ui.interact(rect, id, egui::Sense::click().union(egui::Sense::hover()));
        if response.hovered() {
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            if delta.abs() > 0.1 {
                self.timeline_paused.get_or_insert(now);
                // No further back than the timeline keeps.
                self.timeline_scroll =
                    (self.timeline_scroll + delta * 0.5).clamp(0.0, TIMELINE_SCROLL_MAX);
            }
        }
        if response.double_clicked() {
            self.timeline_paused = None;
            self.timeline_scroll = 0.0;
        }
    }
}
