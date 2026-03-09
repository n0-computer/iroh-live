use std::time::Duration;

use hang::catalog::AudioConfig;
use image::RgbaImage;
use strum::{Display, EnumString, VariantNames};

#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channel_count: u32,
}

impl AudioFormat {
    pub fn mono_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 1,
        }
    }

    pub fn stereo_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 2,
        }
    }

    pub fn from_hang_config(config: &AudioConfig) -> Self {
        Self {
            channel_count: config.channel_count,
            sample_rate: config.sample_rate,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub enum PixelFormat {
    #[default]
    Rgba,
    Bgra,
}

#[derive(Clone, Debug)]
pub struct VideoFormat {
    pub pixel_format: PixelFormat,
    pub dimensions: [u32; 2],
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub format: VideoFormat,
    pub raw: bytes::Bytes,
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub is_keyframe: bool,
    pub frame: hang::container::Frame,
}

#[derive(derive_more::Debug)]
pub struct DecodedVideoFrame {
    #[debug(skip)]
    pub frame: image::Frame,
    pub timestamp: Duration,
}

impl DecodedVideoFrame {
    pub fn img(&self) -> &RgbaImage {
        self.frame.buffer()
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Ord, PartialOrd)]
pub enum VideoPreset {
    #[strum(serialize = "180p")]
    P180,
    #[strum(serialize = "360p")]
    P360,
    #[strum(serialize = "720p")]
    P720,
    #[strum(serialize = "1080p")]
    P1080,
}

impl VideoPreset {
    pub fn all() -> [Self; 4] {
        [Self::P180, Self::P360, Self::P720, Self::P1080]
    }

    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::P180 => (320, 180),
            Self::P360 => (640, 360),
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
        }
    }

    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    pub fn fps(&self) -> u32 {
        30
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum AudioPreset {
    Hq,
    Lq,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Default)]
#[strum(serialize_all = "lowercase")]
pub enum Quality {
    Highest,
    #[default]
    High,
    Mid,
    Low,
}

#[derive(Clone, Debug, Default)]
pub struct DecodeConfig {
    pub pixel_format: PixelFormat,
}

#[derive(Clone, Debug, Default)]
pub struct PlaybackConfig {
    pub decode_config: DecodeConfig,
    pub quality: Quality,
}
