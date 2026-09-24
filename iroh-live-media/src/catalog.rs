//! What a broadcast carries, in our types.
//!
//! On the wire the catalog is hang's JSON document. Applications read
//! [`Catalog`] instead: renditions as [`VideoRenditionInfo`] and
//! [`AudioRenditionInfo`]. hang's own shape stays behind [`Catalog::as_hang`],
//! so a change to it reaches only the callers that asked for it.

use std::sync::Arc;

use crate::{Bitrate, video};

/// hang's catalog, as it travels.
pub(crate) type HangCatalog = moq_mux::catalog::hang::Catalog;

/// The catalog producer for an iroh-live broadcast.
pub(crate) type CatalogProducer = moq_mux::catalog::Producer;

/// A broadcast's catalog: its renditions.
///
/// Cheap to clone. Two catalogs compare equal only when they are the same
/// snapshot, which is what a watcher needs to tell an update from a repeat:
/// every update the publisher sends is a new snapshot.
#[derive(Debug, Clone)]
pub struct Catalog {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    hang: HangCatalog,
    video: Vec<VideoRenditionInfo>,
    audio: Vec<AudioRenditionInfo>,
}

impl PartialEq for Catalog {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Catalog {}

impl Catalog {
    /// Wraps a catalog read off the wire.
    pub(crate) fn new(hang: HangCatalog) -> Self {
        let mut video: Vec<VideoRenditionInfo> = hang
            .video
            .renditions
            .iter()
            .map(|(name, config)| VideoRenditionInfo::from_hang(name, config))
            .collect();
        video.sort_by(|left, right| {
            right
                .pixels()
                .cmp(&left.pixels())
                .then(right.bitrate.cmp(&left.bitrate))
        });
        let audio = hang
            .audio
            .renditions
            .iter()
            .map(|(name, config)| AudioRenditionInfo::from_hang(name, config))
            .collect();
        Self {
            inner: Arc::new(Inner { hang, video, audio }),
        }
    }

    /// Returns the video renditions, largest first.
    ///
    /// Largest by pixel count, and between two of the same size by the higher
    /// bitrate.
    pub fn video(&self) -> &[VideoRenditionInfo] {
        &self.inner.video
    }

    /// Returns the audio renditions.
    pub fn audio(&self) -> &[AudioRenditionInfo] {
        &self.inner.audio
    }

    /// Returns the video rendition named `name`, if the catalog has one.
    pub fn video_rendition(&self, name: &str) -> Option<&VideoRenditionInfo> {
        self.inner.video.iter().find(|info| info.name == name)
    }

    /// Returns hang's catalog.
    ///
    /// An integration point: its shape follows hang's versioning, not ours.
    pub fn as_hang(&self) -> &hang::catalog::Catalog {
        &self.inner.hang
    }

    /// Returns hang's configuration for the video rendition `name`.
    pub(crate) fn hang_video(&self, name: &str) -> Option<&hang::catalog::VideoConfig> {
        self.inner.hang.video.renditions.get(name)
    }

    /// Returns hang's configuration for the audio rendition `name`.
    pub(crate) fn hang_audio(&self, name: &str) -> Option<&hang::catalog::AudioConfig> {
        self.inner.hang.audio.renditions.get(name)
    }
}

/// One video rendition a broadcast offers.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct VideoRenditionInfo {
    /// The rendition's name, which is its track name.
    pub name: String,
    /// The coded picture size, if the publisher declared it.
    pub size: Option<video::Size>,
    /// The frame rate, if the publisher declared it.
    pub rate: Option<video::Rate>,
    /// The bitrate the publisher advertises, if it declared one.
    ///
    /// A ceiling handed to the encoder rather than what arrives: a software
    /// encoder spends well under it on most pictures.
    pub bitrate: Option<Bitrate>,
    /// The codec string, as `avc1.64001f`.
    pub codec: String,
    /// The label a picker shows, if the publisher set one.
    pub label: Option<String>,
    /// Whether the publisher recommends avoiding this rendition for now.
    pub stalled: bool,
}

impl VideoRenditionInfo {
    fn from_hang(name: &str, config: &hang::catalog::VideoConfig) -> Self {
        Self {
            name: name.to_string(),
            size: config
                .coded_width
                .zip(config.coded_height)
                .map(|(width, height)| video::Size::new(width, height)),
            rate: config
                .framerate
                .and_then(|rate| video::Rate::from_f64(rate).ok()),
            bitrate: config.bitrate.map(Bitrate::from_bps),
            codec: config.codec.to_string(),
            label: config.label.clone(),
            stalled: config.stalled.unwrap_or(false),
        }
    }

    /// The pixel count, or zero where the size is not declared.
    pub(crate) fn pixels(&self) -> u64 {
        self.size.map_or(0, |size| size.pixels())
    }

    /// Returns the coded height, if the size is declared.
    pub fn height(&self) -> Option<u32> {
        self.size.map(|size| size.height)
    }
}

/// One audio rendition a broadcast offers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioRenditionInfo {
    /// The rendition's name, which is its track name.
    pub name: String,
    /// The codec string, as `opus`.
    pub codec: String,
    /// The sample rate in hertz.
    pub sample_rate: u32,
    /// The number of channels.
    pub channels: u32,
    /// The bitrate the publisher advertises, if it declared one.
    pub bitrate: Option<Bitrate>,
    /// The label a picker shows, if the publisher set one.
    pub label: Option<String>,
}

impl AudioRenditionInfo {
    fn from_hang(name: &str, config: &hang::catalog::AudioConfig) -> Self {
        Self {
            name: name.to_string(),
            codec: config.codec.to_string(),
            sample_rate: config.sample_rate,
            channels: config.channel_count,
            bitrate: config.bitrate.map(Bitrate::from_bps),
            label: config.label.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec, VideoConfig};

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
        let mut hang = HangCatalog::default();
        hang.video
            .renditions
            .insert("low".into(), rendition(640, 360, 500_000));
        hang.video
            .renditions
            .insert("high".into(), rendition(1920, 1080, 4_000_000));
        hang.video
            .renditions
            .insert("720p-cheap".into(), rendition(1280, 720, 800_000));
        hang.video
            .renditions
            .insert("720p-rich".into(), rendition(1280, 720, 3_000_000));
        let catalog = Catalog::new(hang);
        let names: Vec<&str> = catalog.video().iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["high", "720p-rich", "720p-cheap", "low"]);
        // Inline parameter sets make it `avc3`, as the rendition above says.
        assert_eq!(catalog.video()[0].codec, "avc3.64001f");
        assert_eq!(catalog.video()[0].height(), Some(1080));
    }

    /// A catalog as `@moq/hang` publishes it from a browser.
    #[test]
    fn a_browser_catalog_keeps_its_avcc_description() {
        const BROWSER_CATALOG: &str = r#"{
            "video": { "renditions": { "360p": {
                "codec": "avc1.42e01e",
                "description": "0142e01effe1001a6742e01e",
                "codedWidth": 640, "codedHeight": 360,
                "bitrate": 1200000, "framerate": 30
            } } },
            "audio": { "renditions": {} }
        }"#;
        let hang: HangCatalog = serde_json::from_str(BROWSER_CATALOG).expect("parses");
        assert!(
            hang.video.renditions["360p"].description.is_some(),
            "an avc1 track without its description cannot be decoded",
        );
        let catalog = Catalog::new(hang);
        let info = &catalog.video()[0];
        assert_eq!(info.size, Some(video::Size::new(640, 360)));
        assert_eq!(info.bitrate, Some(Bitrate::from_bps(1_200_000)));
        assert_eq!(info.rate, Some(video::Rate::new(30, 1).expect("valid")));
    }

    #[test]
    fn catalogs_compare_by_snapshot() {
        let first = Catalog::new(HangCatalog::default());
        let second = Catalog::new(HangCatalog::default());
        assert_eq!(first, first.clone());
        assert_ne!(first, second);
    }
}
