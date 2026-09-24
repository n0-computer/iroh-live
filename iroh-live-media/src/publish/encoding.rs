//! How a broadcast encodes its sources: renditions and their presets.

use std::time::Duration;

use crate::{AudioFormat, Bitrate, audio, error::Error, video};

/// How a video source is encoded: one or more renditions of the same picture.
#[derive(Debug, Clone)]
pub struct VideoEncoding {
    /// The renditions, which a subscriber chooses among.
    pub renditions: Vec<VideoRendition>,
    /// Try hardware encoders first, falling back to software once per
    /// rendition if the hardware one fails to open or fails mid-stream.
    /// Default: true.
    pub prefer_hardware: bool,
}

impl VideoEncoding {
    /// Encodes one rendition.
    pub fn single(rendition: VideoRendition) -> Self {
        Self::ladder([rendition])
    }

    /// Encodes a simulcast ladder: several renditions of the same picture.
    pub fn ladder(renditions: impl IntoIterator<Item = VideoRendition>) -> Self {
        Self {
            renditions: renditions.into_iter().collect(),
            prefer_hardware: true,
        }
    }

    /// Checks everything that can be checked without opening an encoder.
    pub(crate) fn validate(&self, source: video::Rate) -> Result<(), Error> {
        if self.renditions.is_empty() {
            return Err(Error::invalid(
                "a video encoding needs at least one rendition",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
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
                    "rendition {} has size {size}; both sides must be even and non-zero",
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
            if rendition.keyframe_interval.is_zero() {
                return Err(Error::invalid(format!(
                    "rendition {} has a zero keyframe interval",
                    rendition.name
                )));
            }
            // The one encoder every build carries is OpenH264, which speaks
            // H.264 alone, so a software-only H.265 rendition can never open.
            let software_only =
                !self.prefer_hardware || rendition.encoder == video::encode::Kind::Software;
            if rendition.codec == video::encode::Codec::H265 && software_only {
                return Err(n0_error::e!(Error::NoEncoder {
                    codec: "H.265 in software".to_string()
                }));
            }
        }
        Ok(())
    }
}

/// One encoding of a broadcast's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRendition {
    /// The name, which is its track name and what a subscriber picks by.
    pub name: String,
    /// The encoded size. `None` encodes at the source's own size.
    pub size: Option<video::Size>,
    /// The target bitrate. `None` derives one from the size and rate.
    pub bitrate: Option<Bitrate>,
    /// The frame rate. `None` follows the source; at most the source's rate.
    pub rate: Option<video::Rate>,
    /// How often the encoder inserts a keyframe.
    ///
    /// A subscriber cannot draw anything until the next keyframe, so this is
    /// join latency far more than it is bitrate: how long somebody who just
    /// scanned a code waits for a picture, and how long a rendition switch
    /// takes to land.
    pub keyframe_interval: Duration,
    /// Which codec to encode.
    pub codec: video::encode::Codec,
    /// Which encoder backend to use.
    pub encoder: video::encode::Kind,
}

impl VideoRendition {
    /// Creates a rendition at the source's own size, with a keyframe every two
    /// seconds.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: None,
            bitrate: None,
            rate: None,
            keyframe_interval: Duration::from_secs(2),
            codec: video::encode::Codec::default(),
            encoder: video::encode::Kind::default(),
        }
    }

    /// Returns `180p`: 320x180 at 150 kbit/s.
    pub fn p180() -> Self {
        Self::preset("180p", 320, 180, 150)
    }

    /// Returns `360p`: 640x360 at 500 kbit/s.
    pub fn p360() -> Self {
        Self::preset("360p", 640, 360, 500)
    }

    /// Returns `720p`: 1280x720 at 1.8 Mbit/s.
    pub fn p720() -> Self {
        Self::preset("720p", 1280, 720, 1_800)
    }

    /// Returns `1080p`: 1920x1080 at 4 Mbit/s.
    pub fn p1080() -> Self {
        Self::preset("1080p", 1920, 1080, 4_000)
    }

    /// A preset at 16:9, with a bitrate reviewed for 30 fps camera content:
    /// about what the encoder derives on its own, rounded up a little, since
    /// a ladder rung that starves at its own ceiling is worse than one that
    /// spends a bit more.
    fn preset(name: &str, width: u32, height: u32, kbps: u64) -> Self {
        Self {
            size: Some(video::Size::new(width, height)),
            bitrate: Some(Bitrate::from_kbps(kbps)),
            ..Self::new(name)
        }
    }

    /// The encoder config for this rendition of a source of `size` at `rate`.
    pub(crate) fn encode_config(
        &self,
        size: video::Size,
        rate: video::Rate,
        color: Option<video::Color>,
        prefer_hardware: bool,
    ) -> video::encode::Config {
        let size = self.size.unwrap_or(size);
        let rate = self.rate.unwrap_or(rate);
        let mut config = video::encode::Config::new(size.width, size.height, rate);
        config.bitrate = self.bitrate;
        config.codec = self.codec;
        config.kind = match (&self.encoder, prefer_hardware) {
            (video::encode::Kind::Auto, false) => video::encode::Kind::Software,
            (kind, _) => kind.clone(),
        };
        config.color = color;
        config.gop = video::encode::Gop::keyframe_every(self.keyframe_interval, rate);
        config
    }
}

/// How an audio source is encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioEncoding {
    /// Which codec to encode.
    pub codec: audio::encode::Codec,
    /// The target bitrate. `None` lets the codec pick, and PCM always does.
    pub bitrate: Option<Bitrate>,
    /// The channel layout. `None` follows the source.
    pub layout: Option<audio::Layout>,
    /// The duration of one encoded frame. Opus takes 2.5, 5, 10, 20, 40 or 60
    /// ms.
    pub frame_duration: Duration,
}

impl AudioEncoding {
    /// Returns Opus mono at 32 kbit/s, for speech.
    pub fn voice() -> Self {
        Self {
            codec: audio::encode::Codec::Opus,
            bitrate: Some(Bitrate::from_kbps(32)),
            layout: Some(audio::Layout::Mono),
            frame_duration: Duration::from_millis(20),
        }
    }

    /// Returns Opus stereo at 128 kbit/s, for music.
    pub fn music() -> Self {
        Self {
            codec: audio::encode::Codec::Opus,
            bitrate: Some(Bitrate::from_kbps(128)),
            layout: Some(audio::Layout::Stereo),
            frame_duration: Duration::from_millis(20),
        }
    }

    /// Returns uncompressed PCM at the source's own rate and layout.
    pub fn pcm() -> Self {
        Self {
            codec: audio::encode::Codec::Pcm,
            bitrate: None,
            layout: None,
            frame_duration: Duration::from_millis(20),
        }
    }

    /// The track name the encoding publishes under.
    pub(crate) fn track_name(&self) -> String {
        self.codec.to_string()
    }

    /// Checks everything that can be checked without opening an encoder.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.codec == audio::encode::Codec::Pcm && self.bitrate.is_some() {
            return Err(Error::invalid(
                "PCM's bitrate follows from its rate and layout and cannot be set",
            ));
        }
        if self.frame_duration.is_zero() {
            return Err(Error::invalid("an audio frame duration cannot be zero"));
        }
        // Opus encodes only these frame sizes; another one would fail in the
        // publication, long after the call that asked for it returned.
        const OPUS_FRAMES_MICROS: [u128; 6] = [2_500, 5_000, 10_000, 20_000, 40_000, 60_000];
        if self.codec == audio::encode::Codec::Opus
            && !OPUS_FRAMES_MICROS.contains(&self.frame_duration.as_micros())
        {
            return Err(Error::invalid(format!(
                "Opus encodes frames of 2.5, 5, 10, 20, 40 or 60 ms, not {:?}",
                self.frame_duration
            )));
        }
        Ok(())
    }

    /// The codec settings for a source in `format`, or at the codec's default
    /// rate where the source is not described yet.
    pub(crate) fn settings(&self, format: Option<AudioFormat>) -> audio::encode::Settings {
        let input = match format {
            Some(format) => audio::encode::Input::new(format.sample_rate, format.layout),
            None => audio::encode::Input::default(),
        };
        let mut settings = audio::encode::Settings::from_input(self.codec, &input);
        if let Some(layout) = self.layout {
            settings.layout = layout;
        }
        settings.bitrate = self.bitrate;
        settings.frame_duration = self.frame_duration;
        settings
    }
}

impl Default for AudioEncoding {
    fn default() -> Self {
        Self::voice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps(n: u32) -> video::Rate {
        video::Rate::new(n, 1).expect("valid")
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
        let encoding = VideoEncoding {
            prefer_hardware: false,
            ..VideoEncoding::single(VideoRendition {
                codec: video::encode::Codec::H265,
                ..VideoRendition::p360()
            })
        };
        assert!(matches!(
            encoding.validate(fps(30)),
            Err(Error::NoEncoder { .. })
        ));
    }

    #[test]
    fn the_keyframe_interval_reaches_the_encoder_config() {
        let rendition = VideoRendition {
            keyframe_interval: Duration::from_secs(1),
            ..VideoRendition::new("video")
        };
        let size = video::Size::new(1280, 720);
        assert_eq!(
            rendition.encode_config(size, fps(30), None, true).gop,
            video::encode::Gop::Keyframe { interval: 30 }
        );
        assert_eq!(
            rendition.encode_config(size, fps(15), None, true).gop,
            video::encode::Gop::Keyframe { interval: 15 }
        );
    }

    #[test]
    fn preferring_software_forces_it_for_auto_only() {
        let size = video::Size::new(640, 360);
        let auto = VideoRendition::p360().encode_config(size, fps(30), None, false);
        assert_eq!(auto.kind, video::encode::Kind::Software);
        let named = VideoRendition {
            encoder: video::encode::Kind::Named("vaapi".into()),
            ..VideoRendition::p360()
        }
        .encode_config(size, fps(30), None, false);
        assert_eq!(named.kind, video::encode::Kind::Named("vaapi".into()));
    }

    #[test]
    fn voice_is_mono_opus() {
        let settings = AudioEncoding::voice().settings(Some(AudioFormat {
            sample_rate: 48_000,
            layout: audio::Layout::Stereo,
        }));
        assert_eq!(settings.layout, audio::Layout::Mono);
        assert_eq!(settings.codec, audio::encode::Codec::Opus);
    }

    #[test]
    fn pcm_follows_the_source() {
        let settings = AudioEncoding::pcm().settings(Some(AudioFormat {
            sample_rate: 44_100,
            layout: audio::Layout::Stereo,
        }));
        assert_eq!(settings.sample_rate, 44_100);
        assert_eq!(settings.layout, audio::Layout::Stereo);
        assert!(
            AudioEncoding {
                bitrate: Some(Bitrate::from_kbps(1)),
                ..AudioEncoding::pcm()
            }
            .validate()
            .is_err()
        );
    }
}
