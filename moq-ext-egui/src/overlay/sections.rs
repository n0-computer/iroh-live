//! The overlay's sections: stats and status in, summary and panel lines out.
//!
//! Everything here is pure, so the tests check the lines without a UI.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use bytesize::ByteSize;
use moq_ext_player::{
    FrameTiming, MediaKind, NetworkSample, PlaybackStats, PlayerStatus, RenditionMode, SlotState,
};
use moq_ext_publisher::{PublishStats, PublishStatus, RenditionState};

use super::{COLOR_BAD, COLOR_DIM, COLOR_GOOD, COLOR_WARN, StatCategory};

/// One section of the bar: its summary and its detail panel lines.
pub(super) struct Section {
    pub(super) category: StatCategory,
    pub(super) summary: String,
    pub(super) lines: Vec<Line>,
}

impl Section {
    /// Creates a section whose summary is the category label followed by `parts`.
    pub(super) fn new(category: StatCategory, parts: Vec<String>, lines: Vec<Line>) -> Self {
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
pub(super) struct Line {
    pub(super) text: String,
    pub(super) color: egui::Color32,
    /// History key and current value, for a line with a sparkline.
    pub(super) plot: Option<(String, f64)>,
}

impl Line {
    /// Creates a line of plain information.
    pub(super) fn info(text: impl Into<String>) -> Self {
        Self::colored(text, COLOR_DIM)
    }

    /// Creates a line in `color`, with no sparkline.
    pub(super) fn colored(text: impl Into<String>, color: egui::Color32) -> Self {
        Self {
            text: text.into(),
            color,
            plot: None,
        }
    }

    /// Creates a line whose `value` is recorded under `key` and plotted.
    pub(super) fn metric(
        key: impl Into<String>,
        text: String,
        value: f64,
        color: egui::Color32,
    ) -> Self {
        Self {
            text,
            color,
            plot: Some((key.into(), value)),
        }
    }
}

/// Grades a figure where lower is better, such as a round trip.
pub(super) fn lower_is_better(value: f64, good: f64, warn: f64) -> egui::Color32 {
    if value < good {
        COLOR_GOOD
    } else if value < warn {
        COLOR_WARN
    } else {
        COLOR_BAD
    }
}

/// Grades a figure where higher is better, such as a frame rate.
pub(super) fn higher_is_better(value: f64, good: f64, warn: f64) -> egui::Color32 {
    if value > good {
        COLOR_GOOD
    } else if value > warn {
        COLOR_WARN
    } else {
        COLOR_BAD
    }
}

/// Returns `duration` in milliseconds.
pub(super) fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Returns a line for the state of the slot called `name`.
pub(super) fn slot_line(name: &str, state: &SlotState) -> Line {
    match state {
        SlotState::Off => Line::info(format!("{name}: off")),
        SlotState::Starting => Line::colored(format!("{name}: starting"), COLOR_WARN),
        SlotState::Running => Line::colored(format!("{name}: running"), COLOR_GOOD),
        SlotState::Failed(err) => Line::colored(format!("{name}: failed: {err:#}"), COLOR_BAD),
        SlotState::Ended => Line::info(format!("{name}: ended")),
    }
}

/// Describes a rendition mode in a few words.
pub(super) fn mode_text(mode: &RenditionMode) -> String {
    match mode {
        RenditionMode::Auto { max_height: None } => "auto".to_string(),
        RenditionMode::Auto {
            max_height: Some(height),
        } => format!("auto, up to {height}p"),
        RenditionMode::Pinned(name) => format!("pinned to {name}"),
        RenditionMode::Off => "off".to_string(),
    }
}

/// Builds a player's NET section.
///
/// It shows the last network sample and the link details.
pub(super) fn net_playback(network: Option<&NetworkSample>, link: &[String]) -> Section {
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
        let pct = loss * 100.0;
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
        let delivery_text = crate::format_bitrate(moq_net::bandwidth::Rate::from_bps(delivery));
        parts.push(format!("bw:{delivery_text}"));
        lines.push(Line::metric(
            "net.delivery",
            format!("delivery estimate: {delivery_text}"),
            delivery as f64,
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
pub(super) fn render_playback(stats: &PlaybackStats, status: &PlayerStatus) -> Section {
    let video = stats.video.as_ref();
    let rendition = video
        .map(|v| v.rendition.as_str())
        .filter(|r| !r.is_empty())
        .or(status.rendition.as_deref());
    let decoder = status.decoder.as_deref();

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
pub(super) fn audio_playback(stats: &PlaybackStats, status: &PlayerStatus) -> Section {
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
/// Lists every rendition either snapshot knows of. The status also names
/// audio and idle renditions. The stats only hold video renditions whose
/// encoder has started.
pub(super) fn capture_publish(stats: &PublishStats, status: &PublishStatus) -> Section {
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
                text.push_str(&format!(" failed: {err:#}"));
                COLOR_BAD
            }
            _ => egui::Color32::WHITE,
        };
        if let Some(RenditionState::Encoding { encoder }) = state {
            text.push(' ');
            text.push_str(encoder);
        }
        let Some(encode) = stats.renditions.get(name) else {
            lines.push(Line::colored(text, color));
            continue;
        };
        let mut fields: Vec<String> = Vec::new();
        fields.extend(encode.size.map(|size| size.to_string()));
        fields.extend(encode.fps.map(|fps| format!("{fps:.1}fps")));
        fields.extend(encode.bitrate.map(crate::format_bitrate));
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

/// Builds the NET section of a broadcast.
///
/// It shows the encoded video the broadcast sends, summed over its renditions.
pub(super) fn net_publish(stats: &PublishStats) -> Section {
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
    let bps: u64 = rates.iter().sum();
    let total = crate::format_bitrate(moq_net::bandwidth::Rate::from_bps(bps));
    let lines = vec![
        Line::metric(
            "net.out",
            format!("video out: {total} over {} renditions", rates.len()),
            bps as f64,
            egui::Color32::WHITE,
        ),
        Line::info(format!("video sent: {}", ByteSize(bytes).display().si())),
    ];
    Section::new(StatCategory::Net, vec![format!("out:{total}")], lines)
}

/// Returns `later - earlier` in milliseconds, negative if `later` comes first.
pub(super) fn signed_ms(later: Instant, earlier: Instant) -> f32 {
    match later.checked_duration_since(earlier) {
        Some(ahead) => ahead.as_secs_f32() * 1000.0,
        None => -(earlier.duration_since(later).as_secs_f32() * 1000.0),
    }
}

/// Returns how many milliseconds after the nearest audio a frame was presented.
///
/// The nearest audio is the one with the closest timestamp. Returns `None`
/// without audio. `audio` must be sorted by timestamp.
pub(super) fn av_offset(video: &FrameTiming, audio: &[&FrameTiming]) -> Option<f32> {
    let at = audio.partition_point(|timing| timing.pts < video.pts);
    let closest = [at.checked_sub(1), Some(at)]
        .into_iter()
        .flatten()
        .filter_map(|index| audio.get(index))
        .min_by_key(|timing| video.pts.abs_diff(timing.pts))?;
    Some(signed_ms(video.presented, closest.presented))
}

/// Grades an A/V offset: under 20 ms nobody notices, past 40 ms lips drift.
pub(super) fn av_color(offset_ms: f32) -> egui::Color32 {
    match offset_ms.abs() {
        abs if abs < 20.0 => COLOR_DIM,
        abs if abs < 40.0 => COLOR_WARN,
        _ => COLOR_BAD,
    }
}

/// Builds the TIME section of a player from its timeline.
pub(super) fn time_playback(timeline: &[FrameTiming]) -> Section {
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
            "no frames presented yet, the timeline fills while the panel is open",
        ));
    }
    Section::new(StatCategory::Time, parts, lines)
}

#[cfg(test)]
mod tests {
    use moq_ext_player::VideoPlaybackStats;
    use moq_ext_publisher::EncodeStats;

    use super::*;

    fn texts(section: &Section) -> Vec<&str> {
        section
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    /// The RENDER summary names the rendition and rate, and skipped frames turn the count amber.
    #[test]
    fn render_shows_the_rendition_and_skips() {
        let stats = PlaybackStats {
            video: Some(VideoPlaybackStats {
                rendition: "high".to_string(),
                fps: Some(30),
                frames: 90,
                skipped: 2,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut status = PlayerStatus::default();
        status.decoder = Some("openh264".to_string());
        let section = render_playback(&stats, &status);
        assert!(section.summary.starts_with(StatCategory::Render.label()));
        assert!(section.summary.contains("high"), "{}", section.summary);
        assert!(section.summary.contains("30fps"), "{}", section.summary);
        assert!(texts(&section).contains(&"decoder: openh264"));
        let frames = section
            .lines
            .iter()
            .find(|line| line.text == "frames: 90, skipped: 2")
            .expect("a frame count line");
        assert_eq!(frames.color, COLOR_WARN);
    }

    /// A rendition line names the encoder from the status and the counters from the stats.
    #[test]
    fn capture_names_the_encoder_and_counts_frames() {
        let mut stats = PublishStats::default();
        stats.renditions.insert(
            "low".to_string(),
            EncodeStats {
                frames: 12,
                ..Default::default()
            },
        );
        let mut status = PublishStatus::default();
        status.renditions.insert(
            "low".to_string(),
            RenditionState::Encoding {
                encoder: "openh264".to_string(),
            },
        );
        let section = capture_publish(&stats, &status);
        assert!(
            section.summary.contains("1/1 encoding"),
            "{}",
            section.summary
        );
        assert!(texts(&section).contains(&"low: openh264 12 frames"));
    }

    /// Without a measured bitrate the broadcast's NET section reads idle.
    #[test]
    fn net_publish_without_a_bitrate_is_idle() {
        let section = net_publish(&PublishStats::default());
        assert!(section.summary.ends_with("idle"), "{}", section.summary);
        assert_eq!(texts(&section), ["no video encoding"]);
    }

    /// The TIME section reports the hold of the last picture and its offset to the audio.
    #[test]
    fn time_reports_hold_and_av_offset() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let timing = |kind, pts_ms: u64, decoded: u64, presented: u64| FrameTiming {
            kind,
            pts: Duration::from_millis(pts_ms),
            arrived: None,
            decoded: at(decoded),
            presented: at(presented),
        };
        let timeline = [
            timing(MediaKind::Audio, 0, 0, 100),
            timing(MediaKind::Video, 0, 40, 130),
        ];
        let section = time_playback(&timeline);
        assert!(section.summary.contains("hold:90ms"), "{}", section.summary);
        assert!(section.summary.contains("av:+30ms"), "{}", section.summary);
        assert_eq!(
            texts(&time_playback(&[])),
            ["no frames presented yet, the timeline fills while the panel is open"]
        );
    }
}
