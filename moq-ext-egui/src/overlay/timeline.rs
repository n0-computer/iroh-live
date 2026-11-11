//! The TIME panel's timeline: when each frame was decoded and presented.

use std::time::{Duration, Instant};

use moq_ext_player::{FrameTiming, MediaKind};

use super::{
    BG_ALPHA, COLOR_BAD, COLOR_DIM, COLOR_GOOD, COLOR_WARN, DebugOverlay,
    sections::{av_color, av_offset, lower_is_better, signed_ms},
};

/// Seconds of history the timeline spans.
const TIMELINE_WINDOW_SECS: f32 = 10.0;

/// How far back the paused timeline scrolls, in seconds.
///
/// About as far as the recorded history reaches: [`HISTORY_LEN`] samples at
/// [`SAMPLE_INTERVAL`].
///
/// [`HISTORY_LEN`]: super::HISTORY_LEN
/// [`SAMPLE_INTERVAL`]: super::SAMPLE_INTERVAL
const TIMELINE_SCROLL_MAX: f32 = 12.0;

// The heights of the timeline's lanes and its axis.
const HOLD_LANE_HEIGHT: f32 = 36.0;
const VIDEO_LANE_HEIGHT: f32 = 20.0;
const AUDIO_LANE_HEIGHT: f32 = 16.0;
const AV_LANE_HEIGHT: f32 = 20.0;
const BUFFER_LANE_HEIGHT: f32 = 26.0;
const RTT_LANE_HEIGHT: f32 = 26.0;
const AXIS_HEIGHT: f32 = 14.0;
/// The timeline's height: every lane and the axis, plus a small margin.
pub(super) const TIMELINE_HEIGHT: f32 = HOLD_LANE_HEIGHT
    + VIDEO_LANE_HEIGHT
    + AUDIO_LANE_HEIGHT
    + AV_LANE_HEIGHT
    + BUFFER_LANE_HEIGHT
    + RTT_LANE_HEIGHT
    + AXIS_HEIGHT
    + 4.0;

// Colors of the timeline's audio lanes, RTT lane and grid.
const COLOR_AUDIO: egui::Color32 = egui::Color32::from_rgb(68, 136, 204);
const COLOR_RTT: egui::Color32 = egui::Color32::from_rgb(0, 200, 200);
const COLOR_GRID: egui::Color32 = egui::Color32::from_rgb(50, 50, 50);

/// Grades a frame's gap to the previous one against the expected gap.
///
/// Under 1.5 times the expected gap is good. Twice as long means a dropped
/// frame.
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

/// Maps times onto the timeline's x axis.
struct Lanes {
    rect: egui::Rect,
    left: Instant,
    right: Instant,
}

impl Lanes {
    /// Returns the x coordinate of `at`.
    fn x(&self, at: Instant) -> f32 {
        let px_per_sec = self.rect.width() / TIMELINE_WINDOW_SECS;
        self.rect.min.x + signed_ms(at, self.left) / 1000.0 * px_per_sec
    }

    /// Returns whether `at` is inside the visible window.
    fn shows(&self, at: Instant) -> bool {
        at >= self.left && at <= self.right
    }

    /// Returns the lane `height` tall that starts `top` below the timeline's top.
    fn lane(&self, top: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(self.rect.min.x, self.rect.min.y + top),
            egui::vec2(self.rect.width(), height),
        )
    }
}

impl DebugOverlay {
    /// Paints the timeline panel and handles scrolling through it.
    pub(super) fn paint_timeline(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        timeline: &[FrameTiming],
    ) {
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(BG_ALPHA));
        let font = egui::FontId::monospace(9.0);

        let now = Instant::now();
        // While paused, the right edge stays fixed.
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

        for sec in (0..=TIMELINE_WINDOW_SECS as i32).step_by(2) {
            let x = rect.min.x + sec as f32 * px_per_sec;
            painter.line_segment(
                [
                    egui::pos2(x, rect.min.y),
                    egui::pos2(x, rect.max.y - AXIS_HEIGHT),
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

        // HOLD: time from decode to screen, per frame.
        let mut top = 0.0;
        let hold_rect = lanes.lane(top, HOLD_LANE_HEIGHT);
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
        top += HOLD_LANE_HEIGHT;

        // VIDEO: one box per frame, colored by its gap to the previous frame.
        let video_rect = lanes.lane(top, VIDEO_LANE_HEIGHT);
        label(video_rect, "VIDEO", COLOR_DIM);
        let mut gaps: Vec<f32> = video
            .windows(2)
            .map(|pair| signed_ms(pair[1].presented, pair[0].presented))
            .collect();
        let expected = if gaps.len() >= 2 {
            gaps.sort_by(f32::total_cmp);
            gaps[gaps.len() / 2]
        } else {
            1000.0 / 30.0
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
                egui::vec2(width, VIDEO_LANE_HEIGHT - 8.0),
            );
            painter.rect_filled(frame, 1.0, gap_color(gap, expected));
        }
        top += VIDEO_LANE_HEIGHT;

        // AUDIO: one box per audio block, red if it played more than 100 ms
        // after it was decoded.
        let audio_rect = lanes.lane(top, AUDIO_LANE_HEIGHT);
        label(audio_rect, "AUDIO", COLOR_AUDIO);
        for (index, timing) in audio.iter().enumerate() {
            let x = lanes.x(timing.presented);
            let next = audio
                .get(index + 1)
                .map_or(x + 4.0, |next| lanes.x(next.presented));
            let width = (next - x - 0.5).clamp(2.0, 10.0);
            let color = if signed_ms(timing.presented, timing.decoded) > 100.0 {
                COLOR_BAD
            } else {
                COLOR_AUDIO
            };
            let block = egui::Rect::from_min_size(
                egui::pos2(x, audio_rect.min.y + 5.0),
                egui::vec2(width, AUDIO_LANE_HEIGHT - 6.0),
            );
            painter.rect_filled(block, 1.0, color);
        }
        top += AUDIO_LANE_HEIGHT;

        // A/V: how much later each frame was presented than its audio.
        let av_rect = lanes.lane(top, AV_LANE_HEIGHT);
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
        top += AV_LANE_HEIGHT;

        // BUFFER and RTT come from the overlay's history of the AUDIO and NET
        // figures.
        for (key, name, height, color) in [
            ("audio.buffered", "BUFFER", BUFFER_LANE_HEIGHT, COLOR_AUDIO),
            ("net.rtt", "RTT", RTT_LANE_HEIGHT, COLOR_RTT),
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

        // Axis labels show seconds before now.
        let axis_y = rect.max.y - AXIS_HEIGHT;
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

        // Scrolling pauses the timeline and moves back in time. A double click
        // resumes.
        let id = self.salt.with("timeline_scroll");
        let response = ui.interact(rect, id, egui::Sense::click().union(egui::Sense::hover()));
        if response.hovered() {
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            if delta.abs() > 0.1 {
                self.timeline_paused.get_or_insert(now);
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
