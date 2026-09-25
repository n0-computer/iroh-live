//! A broadcast's catalog, shared between the watchers of one broadcast.

use std::sync::Arc;

use hang::catalog::VideoConfig;

use crate::Error;

/// A broadcast's catalog, as hang describes it.
///
/// Cheap to clone, and derefs to [`hang::catalog::Catalog`]. Two catalogs are
/// equal only when they are the same snapshot. This lets a watcher tell an
/// update from a repeat.
#[derive(Debug, Clone, derive_more::Deref)]
#[deref(forward)]
pub struct Catalog(Arc<hang::catalog::Catalog>);

impl PartialEq for Catalog {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Catalog {}

impl From<hang::catalog::Catalog> for Catalog {
    fn from(catalog: hang::catalog::Catalog) -> Self {
        Self(Arc::new(catalog))
    }
}

impl Catalog {
    /// Returns the video renditions, largest first.
    ///
    /// Size is the coded pixel count. Between two of the same size, the higher
    /// bitrate comes first.
    pub fn ranked_video(&self) -> Vec<(&str, &VideoConfig)> {
        let mut ranked: Vec<_> = self
            .video
            .renditions
            .iter()
            .map(|(name, config)| (name.as_str(), config))
            .collect();
        let pixels = |config: &VideoConfig| {
            config.coded_width.unwrap_or(0) as u64 * config.coded_height.unwrap_or(0) as u64
        };
        ranked.sort_by(|(_, left), (_, right)| {
            pixels(right)
                .cmp(&pixels(left))
                .then(right.bitrate.cmp(&left.bitrate))
        });
        ranked
    }

    /// Returns the video rendition named `name`.
    ///
    /// Fails with [`Error::UnknownRendition`], which lists the names the
    /// catalog has.
    pub fn video_rendition(&self, name: &str) -> Result<&VideoConfig, Error> {
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
        let mut hang = hang::catalog::Catalog::default();
        let renditions = &mut hang.video.renditions;
        renditions.insert("low".into(), rendition(640, 360, 500_000));
        renditions.insert("high".into(), rendition(1920, 1080, 4_000_000));
        renditions.insert("720p-cheap".into(), rendition(1280, 720, 800_000));
        renditions.insert("720p-rich".into(), rendition(1280, 720, 3_000_000));
        let catalog = Catalog::from(hang);
        let names: Vec<&str> = catalog
            .ranked_video()
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(names, ["high", "720p-rich", "720p-cheap", "low"]);
    }

    #[test]
    fn catalogs_compare_by_snapshot() {
        let first = Catalog::from(hang::catalog::Catalog::default());
        let second = Catalog::from(hang::catalog::Catalog::default());
        assert_eq!(first, first.clone());
        assert_ne!(first, second);
    }
}
