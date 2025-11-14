use anyhow::Result;
use strum::{Display, EnumString, VariantNames};
use tokio::sync::{mpsc, mpsc::UnboundedReceiver, mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;

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
    fn with_preset(preset: AudioPreset) -> Result<Self>
    where
        Self: Sized;
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
    fn default() -> Self {
        PixelFormat::Rgba
    }
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
    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized;
    fn config(&self) -> hang::catalog::VideoConfig;
    fn push_frame(&mut self, format: &VideoFormat, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

// Shared video decoder context used by native and FFmpeg decoders
pub type FrameReceiver = mpsc::Receiver<crate::video::decoder::DecodedFrame>;
pub type ResizeSender = UnboundedSender<(u32, u32)>;
pub type PacketSender = mpsc::Sender<hang::Frame>;

pub struct DecoderContext {
    pub(crate) target_pixel_format: PixelFormat,
    pub(crate) packet_rx: mpsc::Receiver<hang::Frame>,
    pub(crate) frame_tx: mpsc::Sender<crate::video::decoder::DecodedFrame>,
    pub(crate) resize_rx: UnboundedReceiver<(u32, u32)>,
    pub(crate) shutdown: CancellationToken,
}

impl DecoderContext {
    pub fn new(
        shutdown: CancellationToken,
        target_pixel_format: PixelFormat,
    ) -> (Self, FrameReceiver, ResizeSender, PacketSender) {
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let (resize_tx, resize_rx) = mpsc::unbounded_channel();
        let ctx = DecoderContext {
            target_pixel_format,
            packet_rx,
            frame_tx,
            resize_rx,
            shutdown,
        };
        (ctx, frame_rx, resize_tx, packet_tx)
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum AudioCodec {
    Opus,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum VideoCodec {
    H264,
    Av1,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, PartialEq, Eq)]
#[strum(serialize_all = "lowercase")]
pub enum Backend {
    Native,
    Ffmpeg,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq)]
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
