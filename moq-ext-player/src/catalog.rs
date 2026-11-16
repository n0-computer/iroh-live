//! Reading a hang catalog the way a player does.
//!
//! A broadcast's catalog is [`hang::catalog::Catalog`], shared as an `Arc`
//! between the watchers of one broadcast. Its `video.ranked()` orders the
//! renditions by the publisher's preference, the adaptation ladder orders
//! them by cost, and [`video_rendition`] looks one up with an error that
//! names the renditions the catalog has.

use std::cmp::Reverse;

use hang::catalog::{Catalog, VideoConfig};
use moq_ext_adaptive::Rung;

use crate::Error;

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
/// sorts below those with one.
pub(crate) fn rungs(catalog: &Catalog) -> Vec<Rung> {
    let mut ladder: Vec<_> = catalog.video.ranked().collect();
    // Stable, so hang's ranking keeps the order among equals.
    ladder.sort_by_key(|(_, config)| {
        let area = u64::from(config.coded_width.unwrap_or(0))
            * u64::from(config.coded_height.unwrap_or(0));
        Reverse((config.bitrate, area))
    });
    ladder
        .into_iter()
        .map(|(name, config)| Rung {
            name: name.clone(),
            bitrate: config.bitrate,
            stalled: !config.enabled,
        })
        .collect()
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
        rungs(catalog).into_iter().map(|rung| rung.name).collect()
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
        let ladder = rungs(&catalog);
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
        assert_eq!(rungs(&catalog)[1].bitrate, None);
    }

    #[test]
    fn a_disabled_rendition_is_a_stalled_rung() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        let mut off = rendition(1920, 1080, 4_000_000);
        off.enabled = false;
        renditions.insert("off".into(), off);
        renditions.insert("on".into(), rendition(640, 360, 500_000));
        let ladder = rungs(&catalog);
        assert_eq!(names(&catalog), ["off", "on"]);
        assert!(ladder[0].stalled);
        assert!(!ladder[1].stalled);
        assert_eq!(
            moq_ext_adaptive::best(&ladder, &Constraints::default()).as_deref(),
            Some("on")
        );
    }
}
