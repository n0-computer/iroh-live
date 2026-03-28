/// Codec configuration types for encoder/decoder initialization.
///
/// These mirror the catalog types from `hang` but are transport-agnostic:
/// no serde, no container format, no jitter fields. The conversion between
/// these types and the hang catalog types lives in `moq-media`.
use bytes::Bytes;

/// Video decoder/encoder configuration describing an encoded video stream.
///
/// Modeled after the WebCodecs `VideoDecoderConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoConfig {
    /// Codec identifier with codec-specific parameters.
    pub codec: VideoCodec,
    /// Out-of-band decoder initialization data (e.g. avcC record for H.264).
    ///
    /// If `None`, initialization data is inline in the bitstream.
    pub description: Option<Bytes>,
    /// Encoded frame width in pixels (hint for buffer allocation).
    pub coded_width: Option<u32>,
    /// Encoded frame height in pixels (hint for buffer allocation).
    pub coded_height: Option<u32>,
    /// Display aspect ratio width (for non-square pixels).
    pub display_ratio_width: Option<u32>,
    /// Display aspect ratio height (for non-square pixels).
    pub display_ratio_height: Option<u32>,
    /// Maximum bitrate in bits per second.
    pub bitrate: Option<u64>,
    /// Frame rate in frames per second.
    pub framerate: Option<f64>,
    /// Whether the decoder should optimize for low latency.
    pub optimize_for_latency: Option<bool>,
}

/// Audio decoder/encoder configuration describing an encoded audio stream.
///
/// Modeled after the WebCodecs `AudioDecoderConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioConfig {
    /// Codec identifier.
    pub codec: AudioCodec,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of audio channels.
    pub channel_count: u32,
    /// Bitrate in bits per second.
    pub bitrate: Option<u64>,
    /// Out-of-band decoder initialization data.
    pub description: Option<Bytes>,
}

/// Supported video codec types with codec-specific parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoCodec {
    /// H.264/AVC.
    H264(H264),
    /// AV1.
    AV1(AV1),
    /// Unsupported or unknown codec (preserved as mimetype string).
    Other(String),
}

/// Supported audio codec types.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioCodec {
    /// Opus.
    Opus,
    /// PCM (raw f32 samples, no compression).
    Pcm,
    /// Unsupported or unknown codec (preserved as mimetype string).
    Other(String),
}

/// H.264/AVC codec parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H264 {
    /// Whether SPS/PPS are inline in the bitstream (avc3) or in the description (avc1).
    pub inline: bool,
    /// Profile indicator (e.g. `0x42` = Baseline, `0x4D` = Main, `0x64` = High).
    pub profile: u8,
    /// Profile compatibility and constraint flags.
    pub constraints: u8,
    /// Level indicator (e.g. `0x1E` = Level 3.0, `0x1F` = Level 3.1).
    pub level: u8,
}

/// AV1 codec parameters.
///
/// Reference: <https://aomediacodec.github.io/av1-isobmff/#codecsparam>
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AV1 {
    /// Profile (0–2).
    pub profile: u8,
    /// Level (determines resolution and bitrate constraints).
    pub level: u8,
    /// Tier (`'M'` = Main, `'H'` = High).
    pub tier: char,
    /// Bit depth (8, 10, or 12).
    pub bitdepth: u8,
    /// Whether the stream is monochrome.
    pub mono_chrome: bool,
    /// Horizontal chroma subsampling.
    pub chroma_subsampling_x: bool,
    /// Vertical chroma subsampling.
    pub chroma_subsampling_y: bool,
    /// Chroma sample position.
    pub chroma_sample_position: u8,
    /// Color primaries specification.
    pub color_primaries: u8,
    /// Transfer characteristics (gamma curve).
    pub transfer_characteristics: u8,
    /// Matrix coefficients for color conversion.
    pub matrix_coefficients: u8,
    /// Full range (`true`) or limited/studio range (`false`).
    pub full_range: bool,
}

impl Default for AV1 {
    fn default() -> Self {
        Self {
            profile: 0,
            level: 0,
            tier: 'M',
            bitdepth: 8,
            mono_chrome: false,
            chroma_subsampling_x: true,
            chroma_subsampling_y: true,
            chroma_sample_position: 0,
            color_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
            full_range: false,
        }
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264(_) => write!(f, "H.264"),
            Self::AV1(_) => write!(f, "AV1"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opus => write!(f, "opus"),
            Self::Pcm => write!(f, "pcm"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

// ── hang interop (feature-gated) ─────────────────────────────────────

#[cfg(feature = "hang")]
mod hang_interop {
    use super::*;

    impl From<hang::catalog::VideoConfig> for VideoConfig {
        fn from(h: hang::catalog::VideoConfig) -> Self {
            Self {
                codec: h.codec.into(),
                description: h.description,
                coded_width: h.coded_width,
                coded_height: h.coded_height,
                display_ratio_width: h.display_ratio_width,
                display_ratio_height: h.display_ratio_height,
                bitrate: h.bitrate,
                framerate: h.framerate,
                optimize_for_latency: h.optimize_for_latency,
            }
        }
    }

    impl From<VideoConfig> for hang::catalog::VideoConfig {
        fn from(c: VideoConfig) -> Self {
            Self {
                codec: c.codec.into(),
                description: c.description,
                coded_width: c.coded_width,
                coded_height: c.coded_height,
                display_ratio_width: c.display_ratio_width,
                display_ratio_height: c.display_ratio_height,
                bitrate: c.bitrate,
                framerate: c.framerate,
                optimize_for_latency: c.optimize_for_latency,
                container: Default::default(),
                jitter: None,
            }
        }
    }

    impl From<hang::catalog::AudioConfig> for AudioConfig {
        fn from(h: hang::catalog::AudioConfig) -> Self {
            Self {
                codec: h.codec.into(),
                sample_rate: h.sample_rate,
                channel_count: h.channel_count,
                bitrate: h.bitrate,
                description: h.description,
            }
        }
    }

    impl From<AudioConfig> for hang::catalog::AudioConfig {
        fn from(c: AudioConfig) -> Self {
            Self {
                codec: c.codec.into(),
                sample_rate: c.sample_rate,
                channel_count: c.channel_count,
                bitrate: c.bitrate,
                description: c.description,
                container: Default::default(),
                jitter: None,
            }
        }
    }

    impl From<hang::catalog::VideoCodec> for VideoCodec {
        fn from(h: hang::catalog::VideoCodec) -> Self {
            match h {
                hang::catalog::VideoCodec::H264(v) => Self::H264(v.into()),
                hang::catalog::VideoCodec::AV1(v) => Self::AV1(v.into()),
                other => Self::Other(other.to_string()),
            }
        }
    }

    impl From<VideoCodec> for hang::catalog::VideoCodec {
        fn from(c: VideoCodec) -> Self {
            match c {
                VideoCodec::H264(v) => Self::H264(v.into()),
                VideoCodec::AV1(v) => Self::AV1(v.into()),
                VideoCodec::Other(s) => Self::Unknown(s),
            }
        }
    }

    impl From<hang::catalog::AudioCodec> for AudioCodec {
        fn from(h: hang::catalog::AudioCodec) -> Self {
            match h {
                hang::catalog::AudioCodec::Opus => Self::Opus,
                // Recognize "pcm" as our PCM codec when it comes back via the catalog.
                hang::catalog::AudioCodec::Unknown(ref s) if s == "pcm" => Self::Pcm,
                other => Self::Other(other.to_string()),
            }
        }
    }

    impl From<AudioCodec> for hang::catalog::AudioCodec {
        fn from(c: AudioCodec) -> Self {
            match c {
                AudioCodec::Opus => Self::Opus,
                // Hang does not have a native PCM variant, so we use Unknown("pcm").
                AudioCodec::Pcm => Self::Unknown("pcm".to_string()),
                AudioCodec::Other(s) => Self::Unknown(s),
            }
        }
    }

    impl From<hang::catalog::H264> for H264 {
        fn from(h: hang::catalog::H264) -> Self {
            Self {
                inline: h.inline,
                profile: h.profile,
                constraints: h.constraints,
                level: h.level,
            }
        }
    }

    impl From<H264> for hang::catalog::H264 {
        fn from(c: H264) -> Self {
            Self {
                inline: c.inline,
                profile: c.profile,
                constraints: c.constraints,
                level: c.level,
            }
        }
    }

    impl From<hang::catalog::AV1> for AV1 {
        fn from(h: hang::catalog::AV1) -> Self {
            Self {
                profile: h.profile,
                level: h.level,
                tier: h.tier,
                bitdepth: h.bitdepth,
                mono_chrome: h.mono_chrome,
                chroma_subsampling_x: h.chroma_subsampling_x,
                chroma_subsampling_y: h.chroma_subsampling_y,
                chroma_sample_position: h.chroma_sample_position,
                color_primaries: h.color_primaries,
                transfer_characteristics: h.transfer_characteristics,
                matrix_coefficients: h.matrix_coefficients,
                full_range: h.full_range,
            }
        }
    }

    impl From<AV1> for hang::catalog::AV1 {
        fn from(c: AV1) -> Self {
            Self {
                profile: c.profile,
                level: c.level,
                tier: c.tier,
                bitdepth: c.bitdepth,
                mono_chrome: c.mono_chrome,
                chroma_subsampling_x: c.chroma_subsampling_x,
                chroma_subsampling_y: c.chroma_subsampling_y,
                chroma_sample_position: c.chroma_sample_position,
                color_primaries: c.color_primaries,
                transfer_characteristics: c.transfer_characteristics,
                matrix_coefficients: c.matrix_coefficients,
                full_range: c.full_range,
            }
        }
    }

    // EncodedFrame ↔ hang::container::Frame conversions

    impl From<hang::container::OrderedFrame> for crate::format::MediaPacket {
        fn from(f: hang::container::OrderedFrame) -> Self {
            let is_keyframe = f.is_keyframe();
            Self {
                timestamp: f.timestamp.into(),
                payload: f.payload,
                is_keyframe,
            }
        }
    }

    impl crate::format::EncodedFrame {
        /// Converts to a hang [`Frame`](hang::container::Frame) for MoQ transport.
        pub fn to_hang_frame(&self) -> hang::container::Frame {
            hang::container::Frame {
                timestamp: hang::container::Timestamp::from_micros(
                    self.timestamp.as_micros() as u64
                )
                .expect("timestamp overflow"),
                payload: self.payload.clone().into(),
            }
        }
    }
}
