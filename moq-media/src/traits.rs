use anyhow::Result;
use hang::catalog::{AudioConfig, VideoConfig};
use n0_future::boxed::BoxFuture;

use crate::format::{
    AudioEncoderConfig, AudioFormat, AudioPreset, DecodeConfig, DecodedVideoFrame, EncodedFrame,
    MediaPacket, VideoEncoderConfig, VideoFormat, VideoFrame, VideoPreset,
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
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle>;
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

/// Factory for creating audio input/output streams.
///
/// Implementations create [`AudioSource`] and [`AudioSink`] streams with the
/// requested [`AudioFormat`], handling any necessary resampling internally.
pub trait AudioStreamFactory: Send + Sync + 'static {
    /// Creates an audio input stream (microphone capture) with the given format.
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>>;

    /// Creates an audio output stream (speaker playback) with the given format.
    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>>;
}

impl AudioSinkHandle for Box<dyn AudioSink> {
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        (**self).cloned_boxed()
    }
    fn pause(&self) {
        (**self).pause();
    }
    fn resume(&self) {
        (**self).resume();
    }
    fn is_paused(&self) -> bool {
        (**self).is_paused()
    }
    fn toggle_pause(&self) {
        (**self).toggle_pause();
    }
    fn smoothed_peak_normalized(&self) -> Option<f32> {
        (**self).smoothed_peak_normalized()
    }
}

impl AudioSink for Box<dyn AudioSink> {
    fn format(&self) -> Result<AudioFormat> {
        (**self).format()
    }
    fn push_samples(&mut self, buf: &[f32]) -> Result<()> {
        (**self).push_samples(buf)
    }
    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        (**self).handle()
    }
}

pub trait AudioEncoderFactory: AudioEncoder {
    const ID: &str;

    /// Creates an encoder from a full [`AudioEncoderConfig`].
    fn with_config(config: AudioEncoderConfig) -> Result<Self>
    where
        Self: Sized;

    /// Creates an encoder from a preset (convenience wrapper around [`with_config`](Self::with_config)).
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
    where
        Self: Sized,
    {
        Self::with_config(AudioEncoderConfig::from_preset(format, preset))
    }
}

pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;

    /// Sets the target bitrate in bits per second.
    ///
    /// Not all encoders support runtime bitrate changes. The default
    /// implementation is a no-op.
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> {
        Ok(())
    }
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

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        (**self).set_bitrate(bitrate)
    }
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

pub trait VideoSource: Send + 'static {
    fn name(&self) -> &str;
    fn format(&self) -> VideoFormat;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

impl VideoSource for Box<dyn VideoSource> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn format(&self) -> VideoFormat {
        (**self).format()
    }
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        (**self).pop_frame()
    }
    fn start(&mut self) -> Result<()> {
        (**self).start()
    }
    fn stop(&mut self) -> Result<()> {
        (**self).stop()
    }
}

pub trait VideoEncoderFactory: VideoEncoder {
    const ID: &str;

    /// Creates an encoder from a full [`VideoEncoderConfig`].
    fn with_config(config: VideoEncoderConfig) -> Result<Self>
    where
        Self: Sized;

    /// Creates an encoder from a preset (convenience wrapper around [`with_config`](Self::with_config)).
    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized,
    {
        Self::with_config(VideoEncoderConfig::from_preset(preset))
    }
}

pub trait VideoEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;

    /// Sets the target bitrate in bits per second.
    ///
    /// Not all encoders support runtime bitrate changes. The default
    /// implementation is a no-op.
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> {
        Ok(())
    }
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

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        (**self).set_bitrate(bitrate)
    }
}

pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>>;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn set_viewport(&mut self, w: u32, h: u32);
}
