//! Reading a hang catalog the way a player does.
//!
//! A broadcast's catalog is [`hang::catalog::Catalog`], shared as an `Arc`
//! between the watchers of one broadcast. Its `video.ranked()` orders the
//! renditions by the publisher's preference, the adaptation ladder orders
//! them by cost, and [`video_rendition`] looks one up with an error that
//! names the renditions the catalog has.
//!
//! The player also reads one root section of its own, `rate`, which a
//! publisher with rate control adds. [`split`] takes it off the catalog.

use std::cmp::Reverse;

use hang::catalog::{Catalog, VideoConfig};
use moq_ext_adaptive::Rung;
use serde::{Deserialize, Serialize};

use crate::Error;

/// The root sections of a catalog the player reads beyond hang's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sections {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate: Option<RateSection>,
}

impl moq_mux::catalog::hang::CatalogExt for Sections {}

/// The `rate` section, `{"probe": true}` from a publisher with rate control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RateSection {
    #[serde(default)]
    probe: bool,
}

/// Whether a broadcast's publisher pauses renditions it cannot fund.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Pausing {
    /// Every rendition runs at its configured bitrate, and the estimate the
    /// publisher reports may read well above the link.
    #[default]
    Never,
    /// The publisher probes its estimate, and pauses a rendition whose share
    /// falls below its band boundary.
    AtBoundary,
}

/// Splits the sections the player reads off `catalog`.
///
/// Moves each section hang defines into a catalog without application
/// sections. hang marks the catalog non-exhaustive, so a section it adds
/// later is dropped here until it is listed.
pub(crate) fn split(catalog: Catalog<Sections>) -> (Catalog, Pausing) {
    let hang::catalog::Catalog {
        video,
        audio,
        media,
        text,
        json,
        binary,
        m2ts,
        stats,
        echo,
        include,
        ext,
        ..
    } = catalog;
    let mut base = Catalog::default();
    base.video = video;
    base.audio = audio;
    base.media = media;
    base.text = text;
    base.json = json;
    base.binary = binary;
    base.m2ts = m2ts;
    base.stats = stats;
    base.echo = echo;
    base.include = include;
    let pausing = match ext.rate {
        Some(RateSection { probe: true }) => Pausing::AtBoundary,
        _ => Pausing::Never,
    };
    (base, pausing)
}

/// Returns the video rendition of `catalog` named `name`.
///
/// # Errors
///
/// Fails with [`Error::UnknownRendition`], which lists the names the catalog
/// has, best first.
pub fn video_rendition<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a VideoConfig, Error> {
    catalog.video.renditions.get(name).ok_or_else(|| {
        n0_error::e!(Error::UnknownRendition {
            name: name.to_string(),
            offered: catalog
                .video
                .ranked()
                .map(|(name, _)| name.clone())
                .collect(),
        })
    })
}

/// Returns the video renditions as a ladder, most expensive first.
///
/// Adaptation steps down this ladder to save bandwidth, so it is ordered by
/// bitrate, then by picture size. hang's ranking puts a publisher's
/// preference first, and a preferred rendition can cost more than a less
/// preferred one, so here it only breaks ties. A rendition without a bitrate
/// sorts below those with one. When the publisher pauses renditions, each rung
/// carries the boundary it pauses at.
pub(crate) fn rungs(catalog: &Catalog, pausing: Pausing) -> Vec<Rung> {
    let mut ladder: Vec<_> = catalog.video.ranked().collect();
    // Stable, so hang's ranking keeps the order among equals.
    ladder.sort_by_key(|(_, config)| {
        let area = u64::from(config.coded_width.unwrap_or(0))
            * u64::from(config.coded_height.unwrap_or(0));
        Reverse((config.bitrate, area))
    });
    let mut rungs: Vec<Rung> = ladder
        .into_iter()
        .map(|(name, config)| Rung {
            name: name.clone(),
            bitrate: config.bitrate,
            boundary: None,
            stalled: !config.enabled,
        })
        .collect();
    if pausing == Pausing::AtBoundary {
        moq_ext_adaptive::set_boundaries(&mut rungs);
    }
    rungs
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use hang::catalog::{H264, VideoCodec};
    use moq_ext_adaptive::{
        Adaptation, Bound, Constraints, NetworkSample, RenditionMode, step_down,
    };

    use super::*;

    fn rendition(width: u32, height: u32, bitrate: u64) -> VideoConfig {
        let mut config = VideoConfig::new(VideoCodec::H264(H264 {
            inline: true,
            profile: 0x64,
            constraints: 0,
            level: 0x1f,
        }));
        config.coded_width = Some(width);
        config.coded_height = Some(height);
        config.bitrate = Some(bitrate);
        config
    }

    fn names(catalog: &Catalog) -> Vec<String> {
        rungs(catalog, Pausing::Never)
            .into_iter()
            .map(|rung| rung.name)
            .collect()
    }

    #[test]
    fn video_renditions_come_largest_first() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("low".into(), rendition(640, 360, 500_000));
        renditions.insert("high".into(), rendition(1920, 1080, 4_000_000));
        renditions.insert("720p-cheap".into(), rendition(1280, 720, 800_000));
        renditions.insert("720p-rich".into(), rendition(1280, 720, 3_000_000));
        assert_eq!(names(&catalog), ["high", "720p-rich", "720p-cheap", "low"]);
    }

    #[test]
    fn preference_only_breaks_a_tie() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("a-plain".into(), rendition(1280, 720, 800_000));
        let mut preferred = rendition(1280, 720, 800_000);
        preferred.preference = 1;
        renditions.insert("b-preferred".into(), preferred);
        let mut cheap = rendition(1280, 720, 500_000);
        cheap.preference = 2;
        renditions.insert("c-cheap".into(), cheap);
        assert_eq!(names(&catalog), ["b-preferred", "a-plain", "c-cheap"]);
    }

    /// Returns hang's own example of a preferred source and a costlier fallback.
    ///
    /// From `ranked_orders_by_preference_before_picture`: a 360p source at
    /// 500 kb/s is preferred over a 1080p fallback at 6 Mb/s.
    fn preferred_source() -> Catalog {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("source".into(), rendition(640, 360, 500_000));
        let mut fallback = rendition(1920, 1080, 6_000_000);
        fallback.preference = -1;
        renditions.insert("fallback".into(), fallback);
        catalog
    }

    #[test]
    fn the_ladder_steps_down_to_the_cheaper_rung_whatever_the_preference() {
        let catalog = preferred_source();
        let ranked: Vec<_> = catalog
            .video
            .ranked()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            ranked,
            ["source", "fallback"],
            "hang ranks the source first"
        );
        let ladder = rungs(&catalog, Pausing::Never);
        assert_eq!(names(&catalog), ["fallback", "source"]);

        let now = Instant::now();
        let constraints = Constraints::default();
        let short = NetworkSample {
            loss: Some(0.0),
            delivery: Some(400_000),
            ..NetworkSample::default()
        };
        let mut bound = Bound::new(Adaptation::default());
        let chosen = bound.decide(&ladder, None, None, &constraints, &short, now);
        assert_eq!(
            chosen.as_deref(),
            Some("source"),
            "a shortfall plays the cheaper rung"
        );

        let emergency = NetworkSample {
            loss: Some(0.5),
            ..NetworkSample::default()
        };
        let mut bound = Bound::new(Adaptation::default());
        let chosen = bound.decide(
            &ladder,
            Some("fallback"),
            Some("fallback"),
            &constraints,
            &emergency,
            now,
        );
        assert_eq!(
            chosen.as_deref(),
            Some("source"),
            "emergency loss drops to the cheaper rung"
        );

        assert!(!step_down(
            &RenditionMode::Auto,
            &ladder,
            Some("fallback"),
            Some("source")
        ));
        assert!(step_down(
            &RenditionMode::Auto,
            &ladder,
            Some("source"),
            Some("fallback")
        ));
    }

    #[test]
    fn an_unknown_rendition_names_the_offered_ones() {
        let mut catalog = Catalog::default();
        catalog
            .video
            .renditions
            .insert("hd".into(), rendition(1280, 720, 3_000_000));
        let err = video_rendition(&catalog, "4k").expect_err("no such rendition");
        assert!(err.to_string().contains("hd"), "{err}");
    }

    #[test]
    fn a_rendition_without_a_bitrate_sorts_last() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        let mut unknown = rendition(1920, 1080, 0);
        unknown.bitrate = None;
        renditions.insert("unknown".into(), unknown);
        renditions.insert("low".into(), rendition(640, 360, 500_000));
        assert_eq!(names(&catalog), ["low", "unknown"]);
        assert_eq!(rungs(&catalog, Pausing::Never)[1].bitrate, None);
    }

    #[test]
    fn a_disabled_rendition_is_a_stalled_rung() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        let mut off = rendition(1920, 1080, 4_000_000);
        off.enabled = false;
        renditions.insert("off".into(), off);
        renditions.insert("on".into(), rendition(640, 360, 500_000));
        let ladder = rungs(&catalog, Pausing::Never);
        assert_eq!(names(&catalog), ["off", "on"]);
        assert!(ladder[0].stalled);
        assert!(!ladder[1].stalled);
        assert_eq!(
            moq_ext_adaptive::best(&ladder, &Constraints::default()).as_deref(),
            Some("on")
        );
    }

    /// Reads `json` as a catalog with the player's sections.
    fn parse(json: &str) -> (Catalog, Pausing) {
        split(json.parse::<Catalog<Sections>>().expect("a valid catalog"))
    }

    /// The wire form the publisher writes, which `moq-ext-publisher` pins too.
    #[test]
    fn the_rate_section_turns_on_the_boundaries() {
        let (catalog, pausing) = parse(r#"{"rate":{"probe":true}}"#);
        assert_eq!(pausing, Pausing::AtBoundary);
        assert_eq!(catalog, Catalog::default());
        assert_eq!(parse("{}").1, Pausing::Never);
        assert_eq!(parse(r#"{"rate":{}}"#).1, Pausing::Never);
        assert_eq!(parse(r#"{"rate":{"probe":false}}"#).1, Pausing::Never);
    }

    /// Splitting keeps every section hang defines.
    #[test]
    fn splitting_keeps_the_hang_sections() {
        let mut catalog = Catalog::<Sections>::default();
        catalog
            .video
            .renditions
            .insert("high".into(), rendition(1280, 720, 1_800_000));
        catalog.stats = Some(hang::catalog::Stats::new("stats"));
        let json = catalog.to_json().expect("serializes");
        let (split, _) = parse(&json);
        assert_eq!(split.to_json().expect("serializes"), json);
    }

    #[test]
    fn a_publisher_that_pauses_gives_each_rung_its_boundary() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("720p".into(), rendition(1280, 720, 1_800_000));
        renditions.insert("360p".into(), rendition(640, 360, 500_000));
        let boundaries = |pausing| -> Vec<_> {
            rungs(&catalog, pausing)
                .iter()
                .map(|rung| rung.boundary)
                .collect()
        };
        assert_eq!(boundaries(Pausing::Never), [None, None]);
        assert_eq!(boundaries(Pausing::AtBoundary), [Some(933_333), None]);
    }
}
