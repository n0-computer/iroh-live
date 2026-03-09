use anyhow::Result;
use hang::{
    catalog::{AudioConfig, VideoConfig},
    container::OrderedFrame,
};

use crate::format::{
    AudioFormat, AudioPreset, DecodeConfig, DecodedVideoFrame, EncodedFrame, VideoFormat,
    VideoFrame, VideoPreset,
};

pub trait Decoders {
    type Audio: AudioDecoder;
    type Video: VideoDecoder;
}

pub trait AudioSource: Send + 'static {
    fn cloned_boxed(&self) -> Box<dyn AudioSource>;
    fn format(&self) -> AudioFormat;
    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

pub trait AudioSink: AudioSinkHandle {
    fn format(&self) -> Result<AudioFormat>;
    fn push_samples(&mut self, buf: &[f32]) -> Result<()>;
    fn handle(&self) -> Box<dyn AudioSinkHandle>;
}

pub trait AudioSinkHandle: Send + 'static {
    fn pause(&self);
    fn resume(&self);
    fn is_paused(&self) -> bool;
    fn toggle_pause(&self);
    /// Smoothed peak, normalized to 0..1.
    // TODO: document how smoothing and normalization are expected
    fn smoothed_peak_normalized(&self) -> Option<f32> {
        None
    }
}

pub trait AudioEncoderFactory: AudioEncoder {
    const ID: &str;
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
    where
        Self: Sized;
}

pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
}

impl AudioEncoder for Box<dyn AudioEncoder> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn config(&self) -> AudioConfig {
        (**self).config()
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        (**self).push_samples(samples)
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        (**self).pop_packet()
    }
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized;
    fn push_packet(&mut self, packet: OrderedFrame) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

pub trait VideoSource: Send + 'static {
    fn name(&self) -> &str;
    fn format(&self) -> VideoFormat;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

pub trait VideoEncoderFactory: VideoEncoder {
    const ID: &str;

    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized;
}

pub trait VideoEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
}

impl VideoEncoder for Box<dyn VideoEncoder> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn config(&self) -> VideoConfig {
        (**self).config()
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        (**self).push_frame(frame)
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        (**self).pop_packet()
    }
}

pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>>;
    fn push_packet(&mut self, packet: OrderedFrame) -> Result<()>;
    fn set_viewport(&mut self, w: u32, h: u32);
}
