//! Reading a hang catalog the way a player does.
//!
//! A broadcast's catalog is [`hang::catalog::Catalog`], shared as an `Arc`
//! between the watchers of one broadcast. Its `video.ranked()` orders the
//! renditions, and [`video_rendition`] looks one up with an error that names
//! the renditions the catalog has.

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

/// Returns the video renditions as a ladder, best first.
///
/// The order is hang's: enabled first, then by preference, picture size and
/// bitrate.
pub(crate) fn rungs(catalog: &Catalog) -> Vec<Rung> {
    catalog
        .video
        .ranked()
        .map(|(name, config)| Rung {
            name: name.clone(),
            bitrate: config.bitrate,
            height: config.coded_height,
            stalled: !config.enabled,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec};

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
    fn preference_reorders_renditions_of_one_size() {
        let mut catalog = Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("rich".into(), rendition(1280, 720, 3_000_000));
        let mut preferred = rendition(1280, 720, 800_000);
        preferred.preference = 1;
        renditions.insert("preferred".into(), preferred);
        assert_eq!(names(&catalog), ["preferred", "rich"]);
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
}
