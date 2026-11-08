//! Reading a hang catalog the way a player does.
//!
//! A broadcast's catalog is [`hang::catalog::Catalog`], shared as an `Arc`
//! between the watchers of one broadcast. [`VideoRenditions`] adds the ranking,
//! and a lookup whose error names the renditions the catalog has.

use hang::catalog::{Catalog, VideoConfig};
use moq_ext_adaptive::Rung;

use crate::Error;

/// The video renditions of a [`Catalog`], ranked.
pub trait VideoRenditions {
    /// Returns the video renditions, largest first.
    ///
    /// Size is the coded pixel count. Between two of the same size, the higher
    /// bitrate comes first.
    fn ranked_video(&self) -> Vec<(&str, &VideoConfig)>;

    /// Returns the video rendition named `name`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::UnknownRendition`], which lists the names the
    /// catalog has.
    fn video_rendition(&self, name: &str) -> Result<&VideoConfig, Error>;
}

impl VideoRenditions for Catalog {
    fn ranked_video(&self) -> Vec<(&str, &VideoConfig)> {
        let mut ranked: Vec<_> = self
            .video
            .renditions
            .iter()
            .map(|(name, config)| (name.as_str(), config))
            .collect();
        moq_ext_adaptive::rank(&mut ranked, |(_, config)| {
            let pixels =
                config.coded_width.unwrap_or(0) as u64 * config.coded_height.unwrap_or(0) as u64;
            (pixels, config.bitrate)
        });
        ranked
    }

    fn video_rendition(&self, name: &str) -> Result<&VideoConfig, Error> {
        self.video.renditions.get(name).ok_or_else(|| {
            n0_error::e!(Error::UnknownRendition {
                name: name.to_string(),
                offered: self
                    .ranked_video()
                    .into_iter()
                    .map(|(name, _)| name.to_string())
                    .collect(),
            })
        })
    }
}

/// Returns the video renditions as a ladder, largest first.
pub(crate) fn rungs(catalog: &Catalog) -> Vec<Rung> {
    catalog
        .ranked_video()
        .into_iter()
        .map(|(name, config)| Rung {
            name: name.to_string(),
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

    #[test]
    fn video_renditions_come_largest_first() {
        let mut catalog = hang::catalog::Catalog::default();
        let renditions = &mut catalog.video.renditions;
        renditions.insert("low".into(), rendition(640, 360, 500_000));
        renditions.insert("high".into(), rendition(1920, 1080, 4_000_000));
        renditions.insert("720p-cheap".into(), rendition(1280, 720, 800_000));
        renditions.insert("720p-rich".into(), rendition(1280, 720, 3_000_000));
        let names: Vec<&str> = catalog
            .ranked_video()
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(names, ["high", "720p-rich", "720p-cheap", "low"]);
    }
}
