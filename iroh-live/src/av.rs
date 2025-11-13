use anyhow::Result;
use strum::{Display, EnumString, EnumVariantNames};

#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channel_count: u32,
}

pub trait AudioSource {
    fn format(&self) -> AudioFormat;
    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

pub trait AudioEncoder {
    fn config(&self) -> hang::catalog::AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

#[derive(Copy, Clone, Debug)]
pub enum PixelFormat {
    Rgba,
    Bgra,
}

impl Default for PixelFormat {
    fn default() -> Self { PixelFormat::Rgba }
}

#[derive(Clone, Debug)]
pub struct VideoFormat {
    pub pixel_format: PixelFormat,
    pub dimensions: [u32; 2],
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub raw: Vec<u8>,
}

pub trait VideoSource {
    fn format(&self) -> VideoFormat;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
}

pub trait VideoEncoder {
    fn config(&self) -> hang::catalog::VideoConfig;
    fn push_frame(&mut self, format: &VideoFormat, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

#[derive(Debug, Clone, Copy, Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "kebab-case")]
pub enum AudioCodec { Opus }

#[derive(Debug, Clone, Copy, Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "kebab-case")]
pub enum VideoCodec { H264, Av1 }

#[derive(Debug, Clone, Copy, Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum Backend { Native, Ffmpeg }

#[derive(Debug, Clone, Copy, Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum VideoPreset { P180, P360, P720, P1080 }

impl VideoPreset {
    pub fn as_name(&self) -> &'static str {
        match self { Self::P180 => "180p", Self::P360 => "360p", Self::P720 => "720p", Self::P1080 => "1080p" }
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum AudioPreset { Hq, Lq }
