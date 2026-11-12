//! Translucent stats bars painted over video.
//!
//! [`DebugOverlay`] draws a bar of clickable sections along the bottom of a
//! video. It shows the stats of a [`Player`] or a [`LocalBroadcast`]. The
//! stats carry only current values, so the overlay keeps its own short history
//! for the sparklines.
//!
//! [`fit_to_aspect`] sizes a video to the space it has.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use moq_ext_player::{FrameTiming, Player};
use moq_ext_publisher::LocalBroadcast;

use self::{
    sections::{
        Line, Section, audio_playback, capture_publish, net_playback, net_publish, render_playback,
        time_playback,
    },
    timeline::TIMELINE_HEIGHT,
};

mod sections;
mod timeline;

/// Height of the overlay bar, text and padding included.
const BAR_HEIGHT: f32 = 15.0;

/// Returns the largest size that fits `available` and keeps `aspect`.
///
/// The aspect is width over height.
#[must_use]
pub fn fit_to_aspect(available: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let h_by_width = available.x / aspect;
    if h_by_width <= available.y {
        egui::vec2(available.x, h_by_width)
    } else {
        let w_by_height = available.y * aspect;
        egui::vec2(w_by_height, available.y)
    }
}

/// The alpha of the translucent black behind bars and panels.
const BG_ALPHA: u8 = 200;

/// How often the overlay records history, so its length does not depend on the repaint rate.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// The points kept per series, twelve seconds at [`SAMPLE_INTERVAL`].
const HISTORY_LEN: usize = 120;

/// The width of a sparkline, in points.
const SPARK_WIDTH: f32 = 100.0;

// Colors that grade a figure, and the one for plain information.
const COLOR_GOOD: egui::Color32 = egui::Color32::from_rgb(100, 220, 100);
const COLOR_WARN: egui::Color32 = egui::Color32::from_rgb(220, 200, 80);
const COLOR_BAD: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);
const COLOR_DIM: egui::Color32 = egui::Color32::from_rgb(160, 160, 160);

/// A section of the overlay's bottom bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatCategory {
    /// The player's link figures, or the outgoing video of a broadcast.
    Net,
    /// The publisher's source, encoders, and slot states.
    Capture,
    /// The player's video decoding and rendition switching.
    Render,
    /// The player's audio and the playout latency.
    ///
    /// The latency is here because it includes the audio queued at the
    /// speaker, so it moves with the audio buffer.
    Audio,
    /// The player's timeline of presented frames over the last ten seconds.
    ///
    /// Scrolling over the panel pauses it and moves back in time. A double
    /// click returns to the live edge.
    Time,
}

impl StatCategory {
    /// Returns the short label shown in the bottom bar.
    #[must_use]
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

/// A debug overlay with a bottom bar and detail panels.
///
/// The bar shows a short summary for each enabled category. Clicking a section
/// toggles a panel above the bar with all figures and their sparklines.
/// Several panels can be open at once.
#[derive(Debug)]
pub struct DebugOverlay {
    /// Enabled categories in bar order, each with whether its panel is open.
    categories: Vec<(StatCategory, bool)>,
    history: History,
    /// Transport details such as path kind and address.
    ///
    /// The first line joins the NET summary.
    link: Vec<String>,
    /// Seconds the paused timeline is scrolled back from `timeline_paused`.
    timeline_scroll: f32,
    /// When the timeline was paused, or `None` while it follows the live edge.
    timeline_paused: Option<Instant>,
    /// Salt for this overlay's interaction ids.
    ///
    /// Without it, overlays on several tiles would share ids and highlight
    /// together on hover.
    salt: egui::Id,
}

/// Counter that gives each [`DebugOverlay`] its own id salt.
static OVERLAY_SALT: AtomicU64 = AtomicU64::new(0);

impl DebugOverlay {
    /// Creates an overlay with the given categories enabled, all collapsed.
    #[must_use]
    pub fn new(categories: &[StatCategory]) -> Self {
        Self {
            categories: categories
                .iter()
                .map(|&category| (category, false))
                .collect(),
            history: History::default(),
            link: Vec::new(),
            timeline_scroll: 0.0,
            timeline_paused: None,
            salt: egui::Id::new((
                "moq-ext-egui overlay",
                OVERLAY_SALT.fetch_add(1, Ordering::Relaxed),
            )),
        }
    }

    /// Sets the link details shown in a player's NET section.
    ///
    /// The player does not know about paths or addresses, so the caller passes
    /// them in. The first line also joins the section summary.
    pub fn set_link(&mut self, lines: Vec<String>) {
        self.link = lines;
    }

    /// Returns whether any detail panel is open.
    #[must_use]
    pub fn any_expanded(&self) -> bool {
        self.categories.iter().any(|&(_, expanded)| expanded)
    }

    /// Draws the overlay for `player` at the bottom of `video_rect`.
    ///
    /// Shows the enabled NET, RENDER, AUDIO, and TIME categories. Reads the
    /// player's timeline only while the TIME panel is open.
    pub fn show_player(&mut self, ui: &mut egui::Ui, video_rect: egui::Rect, player: &Player) {
        let stats = player.stats();
        let status = player.status().borrow().clone();
        let timeline = if self.is_expanded(StatCategory::Time) {
            player.timeline()
        } else {
            Vec::new()
        };
        let sections: Vec<Section> = self
            .categories
            .iter()
            .filter_map(|&(category, _)| match category {
                StatCategory::Net => Some(net_playback(stats.network.as_ref(), &self.link)),
                StatCategory::Render => Some(render_playback(&stats, &status)),
                StatCategory::Audio => Some(audio_playback(&stats, &status)),
                StatCategory::Time => Some(time_playback(&timeline)),
                StatCategory::Capture => None,
            })
            .collect();
        self.draw(ui, video_rect, &sections, &timeline);
    }

    /// Draws the overlay for a local broadcast at the bottom of `video_rect`.
    ///
    /// Shows the enabled CAPTURE and NET categories. The publish side has no
    /// link figures, so NET shows the encoded video bitrate summed over all
    /// renditions.
    pub fn show_broadcast(
        &mut self,
        ui: &mut egui::Ui,
        video_rect: egui::Rect,
        broadcast: &LocalBroadcast,
    ) {
        let stats = broadcast.stats();
        let status = broadcast.status().borrow().clone();
        let sections: Vec<Section> = self
            .categories
            .iter()
            .filter_map(|&(category, _)| match category {
                StatCategory::Capture => Some(capture_publish(&stats, &status)),
                StatCategory::Net => Some(net_publish(&stats)),
                StatCategory::Render | StatCategory::Audio | StatCategory::Time => None,
            })
            .collect();
        self.draw(ui, video_rect, &sections, &[]);
    }

    /// Returns whether the panel of `category` is open.
    fn is_expanded(&self, category: StatCategory) -> bool {
        self.categories
            .iter()
            .any(|&(candidate, expanded)| candidate == category && expanded)
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
        let mut y_cursor = video_rect.max.y - BAR_HEIGHT;
        for section in sections.iter().rev() {
            if section.lines.is_empty() || !self.is_expanded(section.category) {
                continue;
            }
            let height = BAR_HEIGHT * (section.lines.len() as f32 + 0.5);
            y_cursor -= height;
            let rect = egui::Rect::from_min_size(
                egui::pos2(video_rect.min.x, y_cursor),
                egui::vec2(video_rect.width(), height),
            );
            self.paint_panel(ui.painter(), rect, &section.lines, &font);
            if section.category == StatCategory::Time {
                y_cursor -= TIMELINE_HEIGHT;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(video_rect.min.x, y_cursor),
                    egui::vec2(video_rect.width(), TIMELINE_HEIGHT),
                );
                self.paint_timeline(ui, rect, timeline);
            }
        }

        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(video_rect.min.x, video_rect.max.y - BAR_HEIGHT),
            egui::vec2(video_rect.width(), BAR_HEIGHT),
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
                egui::vec2(section_width, BAR_HEIGHT),
            );

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

        for clicked in clicks {
            for (category, expanded) in &mut self.categories {
                if *category == clicked {
                    *expanded = !*expanded;
                }
            }
        }
    }

    /// Paints one detail panel.
    ///
    /// Each line with at least two points of history gets a sparkline.
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
                    egui::pos2(rect.max.x - SPARK_WIDTH - 6.0, y + 2.0),
                    egui::vec2(SPARK_WIDTH, BAR_HEIGHT - 4.0),
                );
                paint_sparkline(painter, spark_rect, values, line.color);
            }
            y += BAR_HEIGHT;
        }
    }
}

/// The overlay's own history of the figures it plots.
///
/// Each series keeps at most [`HISTORY_LEN`] points. A series that gets no
/// point for that many samples is dropped, so the history stays bounded.
#[derive(Debug, Default)]
struct History {
    /// When the history last took a sample.
    last_sample: Option<Instant>,
    /// How many samples the history has taken.
    tick: u64,
    /// The plotted figures, by key.
    series: BTreeMap<String, Series>,
}

/// The recorded points of one plotted figure.
#[derive(Debug, Default)]
struct Series {
    values: VecDeque<f64>,
    /// When each value was recorded, for the timeline.
    times: VecDeque<Instant>,
    /// The history tick of the latest point, for dropping idle series.
    last_tick: u64,
}

impl History {
    /// Appends the current value of every plotted line.
    ///
    /// Records at most once per [`SAMPLE_INTERVAL`].
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

    /// Returns the recorded points of `key` with their times, oldest first.
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
