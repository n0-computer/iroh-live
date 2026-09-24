//! What a broadcast carries, in our types.
//!
//! On the wire the catalog is hang's JSON document, extended with the sections
//! iroh-live adds ([`IrohLiveExt`]). Applications read [`Catalog`] instead:
//! renditions as [`VideoRenditionInfo`] and [`AudioRenditionInfo`], and the
//! publisher's [`Metadata`]. hang's own shape stays behind
//! [`Catalog::as_hang`], so a change to it reaches only the callers that asked
//! for it.

use std::sync::Arc;

use moq_mux::catalog::hang::CatalogExt;
use serde::{Deserialize, Serialize};

use crate::{Bitrate, video};

/// hang's catalog with the iroh-live sections, as it travels.
pub(crate) type HangCatalog = moq_mux::catalog::hang::Catalog<IrohLiveExt>;

/// The catalog producer for an iroh-live broadcast.
pub(crate) type CatalogProducer = moq_mux::catalog::Producer<IrohLiveExt>;

/// A broadcast's catalog: its renditions and its metadata.
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
    metadata: Metadata,
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
        let metadata = Metadata::from_ext(&hang.ext);
        Self {
            inner: Arc::new(Inner {
                hang,
                video,
                audio,
                metadata,
            }),
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

    /// Returns the publisher's metadata.
    pub fn metadata(&self) -> &Metadata {
        &self.inner.metadata
    }

    /// Returns hang's catalog, with the iroh-live sections.
    ///
    /// An integration point: its shape follows hang's versioning, not ours.
    pub fn as_hang(&self) -> &hang::catalog::Catalog<IrohLiveExt> {
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

    /// The coded height, if the size is declared.
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

/// What the publisher says about the broadcast, apart from its media.
///
/// Every field is what the publisher chose to write and none of it is
/// verified: the endpoint a broadcast arrived from is the authenticated half,
/// and that lives outside the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Metadata {
    /// The name a viewer shows next to the picture.
    pub display_name: Option<String>,
    /// The track carrying chat messages, if the publisher opened one on the
    /// broadcast.
    ///
    /// The track itself is the application's, created on
    /// [`LocalBroadcast::as_moq`](crate::LocalBroadcast::as_moq); this only
    /// says where it is.
    pub chat: Option<TrackRef>,
}

impl Metadata {
    /// Returns the metadata with a display name.
    #[must_use]
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Returns the metadata pointing at a chat track.
    #[must_use]
    pub fn with_chat(mut self, track: TrackRef) -> Self {
        self.chat = Some(track);
        self
    }

    fn from_ext(ext: &IrohLiveExt) -> Self {
        Self {
            display_name: ext.user.as_ref().and_then(|user| user.name.clone()),
            chat: ext.chat.as_ref().and_then(|chat| chat.message.clone()),
        }
    }

    /// Writes this metadata into the catalog's iroh-live sections.
    pub(crate) fn apply(&self, ext: &mut IrohLiveExt) {
        ext.user = self.display_name.as_ref().map(|name| User {
            name: Some(name.clone()),
            ..User::default()
        });
        ext.chat = self.chat.as_ref().map(|track| Chat {
            message: Some(track.clone()),
            typing: None,
        });
    }
}

/// The sections iroh-live adds to hang's catalog, flattened beside `video` and
/// `audio`.
///
/// A consumer that knows only hang's schema ignores them. Public because
/// [`Catalog::as_hang`] names it; applications read [`Metadata`] instead.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct IrohLiveExt {
    /// The tracks carrying chat, if the publisher opened any.
    pub chat: Option<Chat>,
    /// Who is publishing, if they said.
    pub user: Option<User>,
}

impl CatalogExt for IrohLiveExt {}

/// A reference to a track on the broadcast, as the catalog carries it.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct TrackRef {
    /// The track name on the broadcast.
    pub name: String,
    /// The publisher's priority for the track.
    pub priority: u8,
}

impl TrackRef {
    /// Creates a reference to the track `name`, at `priority`.
    pub fn new(name: impl Into<String>, priority: u8) -> Self {
        Self {
            name: name.into(),
            priority,
        }
    }
}

/// The chat section: which tracks carry messages and typing indicators.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Chat {
    /// The track carrying chat messages.
    pub message: Option<TrackRef>,
    /// The track carrying typing indicators, if the publisher sends any.
    pub typing: Option<TrackRef>,
}

/// The publisher's description of itself, on the wire.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct User {
    /// An application-defined identifier, stable across sessions.
    pub id: Option<String>,
    /// The display name.
    pub name: Option<String>,
    /// A URL for an avatar image.
    pub avatar: Option<String>,
    /// An accent color, as a CSS color string.
    pub color: Option<String>,
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
        assert_eq!(catalog.video()[0].codec, "avc1.64001f");
        assert_eq!(catalog.video()[0].height(), Some(1080));
    }

    #[test]
    fn metadata_round_trips_through_the_user_section() {
        let mut ext = IrohLiveExt::default();
        Metadata::default().with_display_name("ada").apply(&mut ext);
        let json = serde_json::to_string(&ext).expect("serialize");
        assert!(json.contains("\"user\""), "{json}");
        let mut hang = HangCatalog::default();
        hang.ext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            Catalog::new(hang).metadata().display_name.as_deref(),
            Some("ada")
        );
    }

    /// A catalog as `@moq/hang` publishes it from a browser. Our extension
    /// flattens into the same object, so a bug in how it is read shows up as a
    /// dropped media field rather than as an error.
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
