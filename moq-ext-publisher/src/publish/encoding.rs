//! How a broadcast encodes its sources: renditions and their presets.

use std::collections::BTreeSet;

use moq_ext_sources::{AudioFormat, Error};
use moq_net::bandwidth::Rate;

/// The frame durations Opus accepts, in microseconds.
///
/// Another one would fail later in the publication, after `set_audio` has
/// returned.
const OPUS_FRAMES_MICROS: [u128; 6] = [2_500, 5_000, 10_000, 20_000, 40_000, 60_000];

/// How a video source is encoded: one or more renditions of the same picture.
#[derive(Debug, Clone)]
pub struct VideoEncoding {
    /// The renditions, which a subscriber chooses among.
    pub renditions: Vec<VideoRendition>,
}

impl VideoEncoding {
    /// Encodes one rendition.
    #[must_use]
    pub fn single(rendition: VideoRendition) -> Self {
        Self::ladder([rendition])
    }

    /// Encodes a simulcast ladder: several renditions of the same picture.
    #[must_use]
    pub fn ladder(renditions: impl IntoIterator<Item = VideoRendition>) -> Self {
        Self {
            renditions: renditions.into_iter().collect(),
        }
    }

    /// Checks everything that can be checked without opening an encoder.
    pub(crate) fn validate(&self, source: moq_video::FrameRate) -> Result<(), Error> {
        if self.renditions.is_empty() {
            return Err(Error::invalid(
                "a video encoding needs at least one rendition",
            ));
        }
        let mut seen = BTreeSet::new();
        for rendition in &self.renditions {
            if rendition.name.is_empty() {
                return Err(Error::invalid("a rendition name cannot be empty"));
            }
            if !seen.insert(rendition.name.as_str()) {
                return Err(Error::invalid(format!(
                    "two renditions are named {}",
                    rendition.name
                )));
            }
            if let Some(size) = rendition.size
                && (size.width == 0
                    || size.height == 0
                    || size.width % 2 == 1
                    || size.height % 2 == 1)
            {
                return Err(Error::invalid(format!(
                    "rendition {} has size {size}, but both sides must be even and non-zero",
                    rendition.name
                )));
            }
            if let Some(rate) = rendition.rate
                && rate.as_f64() > source.as_f64()
            {
                return Err(Error::invalid(format!(
                    "rendition {} asks for {rate} fps from a source that runs at {source}",
                    rendition.name
                )));
            }
            if rendition.config.join.is_zero() {
                return Err(Error::invalid(format!(
                    "rendition {} has a zero keyframe interval",
                    rendition.name
                )));
            }
            // OpenH264 is the one encoder every build carries, and it only
            // encodes H.264. A software-only H.265 rendition can never open.
            if rendition.config.codec == moq_video::encode::Codec::H265
                && rendition.config.kind == moq_video::encode::Kind::Software
            {
                return Err(n0_error::e!(Error::NoEncoder {
                    codec: "H.265 in software".to_string()
                }));
            }
        }
        Ok(())
    }
}

/// One encoding of a broadcast's picture.
///
/// The encoder settings are moq-video's own [`Config`]. Its `width`, `height`
/// and `framerate` are filled in from [`size`](Self::size), [`rate`](Self::rate)
/// and the source when the rendition opens, and so is its `color` unless set.
/// Its `join` is the keyframe interval: a subscriber cannot draw anything
/// before a keyframe, so it sets how long a new viewer waits for a picture and
/// how long a rendition switch takes.
///
/// [`Config`]: moq_video::encode::Config
#[derive(Debug, Clone)]
pub struct VideoRendition {
    /// The track name, which a subscriber picks the rendition by.
    pub name: String,
    /// The encoded size. `None` encodes at the source's own size.
    pub size: Option<moq_video::Size>,
    /// The frame rate. `None` follows the source. It cannot exceed the source's rate.
    pub rate: Option<moq_video::FrameRate>,
    /// The encoder settings: bitrate, keyframe interval, codec, backend, preset.
    pub config: moq_video::encode::Config,
}

impl VideoRendition {
    /// Creates a rendition at the source's size with moq-video's default settings.
    ///
    /// The defaults are H.264 from any backend, a keyframe every two seconds,
    /// and a bitrate derived from the size and rate.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let rate = moq_video::FrameRate::new(30, 1).expect("30 fps is a valid rate");
        Self {
            name: name.into(),
            size: None,
            rate: None,
            // The size and rate are placeholders until the rendition opens.
            config: moq_video::encode::Config::new(0, 0, rate),
        }
    }

    /// Returns `360p`: 640x360 at 500 kbit/s.
    #[must_use]
    pub fn p360() -> Self {
        Self::preset("360p", 640, 360, 500)
    }

    /// Returns `720p`: 1280x720 at 1.8 Mbit/s.
    #[must_use]
    pub fn p720() -> Self {
        Self::preset("720p", 1280, 720, 1_800)
    }

    /// Creates a 16:9 preset with a bitrate chosen for 30 fps camera content.
    ///
    /// The bitrate is a little above what the encoder derives on its own. A
    /// rendition that starves at its ceiling looks worse than one that spends a
    /// bit more.
    fn preset(name: &str, width: u32, height: u32, kbps: u64) -> Self {
        let mut rendition = Self::new(name);
        rendition.size = Some(moq_video::Size::new(width, height));
        rendition.config.bitrate = Some(Rate::from_kbps(kbps));
        rendition
    }

    /// Returns the encoder config for this rendition of a source of `size` at `rate`.
    pub(crate) fn encode_config(
        &self,
        size: moq_video::Size,
        rate: moq_video::FrameRate,
        color: Option<moq_video::Color>,
    ) -> moq_video::encode::Config {
        let size = self.size.unwrap_or(size);
        let mut config = self.config.clone();
        config.width = size.width;
        config.height = size.height;
        config.framerate = self.rate.unwrap_or(rate);
        config.color = config.color.or(color);
        config
    }
}

/// Returns Opus mono at 32 kbit/s in 20 ms frames, for speech.
#[must_use]
pub fn voice() -> moq_audio::encode::Config {
    opus(moq_audio::Layout::Mono, 32)
}

/// Returns Opus stereo at 128 kbit/s in 20 ms frames, for music.
#[must_use]
pub fn music() -> moq_audio::encode::Config {
    opus(moq_audio::Layout::Stereo, 128)
}

/// Returns uncompressed PCM in 20 ms frames at the rate and layout of `format`.
///
/// Pass the source's own format, so the samples go out as they come in. A
/// microphone's format is known only once it opens, and is resampled to
/// whatever is passed here.
#[must_use]
pub fn pcm(format: AudioFormat) -> moq_audio::encode::Config {
    let input = moq_audio::encode::Input::new(format.sample_rate, format.layout);
    moq_audio::encode::Config::from_input(moq_audio::encode::Codec::Pcm, &input)
}

/// Returns Opus at 48 kHz in `layout` at `kbps`.
fn opus(layout: moq_audio::Layout, kbps: u64) -> moq_audio::encode::Config {
    let mut config = moq_audio::encode::Config::new(48_000, layout);
    config.codec = moq_audio::encode::Codec::Opus;
    config.bitrate = Some(Rate::from_kbps(kbps));
    config
}

/// Returns the track name an audio encoding publishes under.
pub(crate) fn audio_track_name(config: &moq_audio::encode::Config) -> String {
    config.codec.to_string()
}

/// Checks everything about an audio encoding that can be checked without opening an encoder.
pub(crate) fn validate_audio(config: &moq_audio::encode::Config) -> Result<(), Error> {
    if config.codec == moq_audio::encode::Codec::Pcm && config.bitrate.is_some() {
        return Err(Error::invalid(
            "PCM's bitrate follows from its rate and layout and cannot be set",
        ));
    }
    if config.frame_duration.is_zero() {
        return Err(Error::invalid("an audio frame duration cannot be zero"));
    }
    if config.codec == moq_audio::encode::Codec::Opus
        && !OPUS_FRAMES_MICROS.contains(&config.frame_duration.as_micros())
    {
        return Err(Error::invalid(format!(
            "Opus encodes frames of 2.5, 5, 10, 20, 40 or 60 ms, not {:?}",
            config.frame_duration
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn fps(n: u32) -> moq_video::FrameRate {
        moq_video::FrameRate::new(n, 1).expect("valid")
    }

    #[test]
    fn an_empty_ladder_is_refused() {
        let encoding = VideoEncoding::ladder([]);
        assert!(matches!(
            encoding.validate(fps(30)),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn duplicate_names_are_refused() {
        let encoding = VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p360()]);
        assert!(matches!(
            encoding.validate(fps(30)),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn a_rate_above_the_source_is_refused() {
        let encoding = VideoEncoding::single(VideoRendition {
            rate: Some(fps(60)),
            ..VideoRendition::p360()
        });
        assert!(encoding.validate(fps(30)).is_err());
        assert!(encoding.validate(fps(60)).is_ok());
    }

    #[test]
    fn software_h265_has_no_encoder() {
        let mut rendition = VideoRendition::p360();
        rendition.config.codec = moq_video::encode::Codec::H265;
        rendition.config.kind = moq_video::encode::Kind::Software;
        assert!(matches!(
            VideoEncoding::single(rendition).validate(fps(30)),
            Err(Error::NoEncoder { .. })
        ));
    }

    #[test]
    fn the_keyframe_interval_sets_the_encoder_join() {
        let mut rendition = VideoRendition::new("video");
        rendition.config.join = Duration::from_secs(1);
        let size = moq_video::Size::new(1280, 720);
        for rate in [fps(30), fps(15)] {
            let config = rendition.encode_config(size, rate, None);
            assert_eq!(config.join, Duration::from_secs(1));
            assert_eq!(config.effective_join(), Duration::from_secs(1));
        }
    }

    #[test]
    fn the_rendition_size_and_rate_override_the_source() {
        let rendition = VideoRendition {
            rate: Some(fps(15)),
            ..VideoRendition::p360()
        };
        let config = rendition.encode_config(moq_video::Size::new(1280, 720), fps(30), None);
        assert_eq!(config.size(), moq_video::Size::new(640, 360));
        assert_eq!(config.framerate, fps(15));
        assert_eq!(config.bitrate, Some(Rate::from_kbps(500)));
    }

    #[test]
    fn voice_is_mono_opus() {
        let config = voice();
        assert_eq!(config.layout, moq_audio::Layout::Mono);
        assert_eq!(config.codec, moq_audio::encode::Codec::Opus);
        assert!(validate_audio(&config).is_ok());
    }

    #[test]
    fn pcm_follows_the_source() {
        let mut config = pcm(AudioFormat {
            sample_rate: 44_100,
            layout: moq_audio::Layout::Stereo,
        });
        assert_eq!(config.sample_rate, 44_100);
        assert_eq!(config.layout, moq_audio::Layout::Stereo);
        assert!(validate_audio(&config).is_ok());
        config.bitrate = Some(Rate::from_kbps(1));
        assert!(validate_audio(&config).is_err());
    }
}
