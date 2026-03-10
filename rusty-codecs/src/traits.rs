use anyhow::Result;
use n0_future::boxed::BoxFuture;

use crate::{
    config::{AudioConfig, VideoConfig},
    format::{
        AudioEncoderConfig, AudioFormat, AudioPreset, DecodeConfig, DecodedVideoFrame,
        EncodedFrame, MediaPacket, VideoEncoderConfig, VideoFormat, VideoFrame, VideoPreset,
    },
};

/// Pairs an [`AudioDecoder`] and [`VideoDecoder`] implementation for use by
/// the dynamic decoding pipeline.
pub trait Decoders {
    /// Audio decoder type.
    type Audio: AudioDecoder;
    /// Video decoder type.
    type Video: VideoDecoder;
}

/// Provides PCM audio samples from a capture device or file.
pub trait AudioSource: Send + 'static {
    /// Returns a boxed clone of this source.
    fn cloned_boxed(&self) -> Box<dyn AudioSource>;
    /// Returns the audio format produced by this source.
    fn format(&self) -> AudioFormat;
    /// Fills `buf` with interleaved f32 samples and returns the number of
    /// frames written, or `None` when no data is available yet.
    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

/// Accepts PCM audio samples for playback.
pub trait AudioSink: AudioSinkHandle {
    /// Returns the audio format accepted by this sink.
    fn format(&self) -> Result<AudioFormat>;
    /// Pushes interleaved f32 samples into the playback buffer.
    fn push_samples(&mut self, buf: &[f32]) -> Result<()>;
    /// Returns a clonable handle for pause/resume control from other threads.
    fn handle(&self) -> Box<dyn AudioSinkHandle>;
}

/// Thread-safe handle for controlling an [`AudioSink`] (pause, resume, level metering).
pub trait AudioSinkHandle: Send + 'static {
    /// Returns a boxed clone of this handle.
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle>;
    /// Pauses audio playback.
    fn pause(&self);
    /// Resumes audio playback.
    fn resume(&self);
    /// Returns whether playback is currently paused.
    fn is_paused(&self) -> bool;
    /// Toggles playback between paused and playing.
    fn toggle_pause(&self);
    /// Returns the smoothed peak level, normalized to `0.0..=1.0`.
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

/// Factory trait for constructing audio encoders from configuration or presets.
///
/// Extends [`AudioEncoder`] with static constructor methods and a codec
/// identifier constant.
pub trait AudioEncoderFactory: AudioEncoder {
    /// Unique identifier for this encoder implementation (e.g. `"opus"`).
    const ID: &str;

    /// Creates an encoder from a full [`AudioEncoderConfig`].
    fn with_config(config: AudioEncoderConfig) -> Result<Self>
    where
        Self: Sized;

    /// Returns the catalog [`AudioConfig`] for the given parameters
    /// without constructing an encoder instance.
    fn config_for(config: &AudioEncoderConfig) -> AudioConfig;

    /// Creates an encoder from a preset (convenience wrapper around [`with_config`](Self::with_config)).
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
    where
        Self: Sized,
    {
        Self::with_config(AudioEncoderConfig::from_preset(format, preset))
    }
}

/// Encodes PCM audio samples into compressed packets.
///
/// Uses a push/pop streaming interface: push interleaved f32 samples via
/// [`push_samples`](Self::push_samples), then drain encoded packets with
/// [`pop_packet`](Self::pop_packet).
pub trait AudioEncoder: Send + 'static {
    /// Returns the encoder's display name.
    fn name(&self) -> &str;
    /// Returns the catalog [`AudioConfig`] describing the encoded stream.
    fn config(&self) -> AudioConfig;
    /// Pushes interleaved f32 PCM samples into the encoder's internal buffer.
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    /// Pops the next encoded packet, or `None` if the encoder needs more input.
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

/// Decodes compressed audio packets back into PCM samples.
pub trait AudioDecoder: Send + 'static {
    /// Creates a new decoder for the given catalog config, resampling output
    /// to `target_format` if necessary.
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized;
    /// Pushes an encoded packet into the decoder.
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    /// Pops decoded interleaved f32 samples, or `None` if the decoder needs more input.
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

/// Provides raw video frames from a capture device or synthetic source.
pub trait VideoSource: Send + 'static {
    /// Returns the source's display name.
    fn name(&self) -> &str;
    /// Returns the video format produced by this source.
    fn format(&self) -> VideoFormat;
    /// Pops the next captured frame, or `None` if no frame is ready.
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    /// Starts frame capture.
    fn start(&mut self) -> Result<()>;
    /// Stops frame capture.
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

/// Factory trait for constructing video encoders from configuration or presets.
///
/// Extends [`VideoEncoder`] with static constructor methods and a codec
/// identifier constant.
pub trait VideoEncoderFactory: VideoEncoder {
    /// Unique identifier for this encoder implementation (e.g. `"h264-openh264"`).
    const ID: &str;

    /// Creates an encoder from a full [`VideoEncoderConfig`].
    fn with_config(config: VideoEncoderConfig) -> Result<Self>
    where
        Self: Sized;

    /// Returns the catalog [`VideoConfig`] for the given parameters
    /// without constructing an encoder instance.
    fn config_for(config: &VideoEncoderConfig) -> VideoConfig;

    /// Creates an encoder from a preset (convenience wrapper around [`with_config`](Self::with_config)).
    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized,
    {
        Self::with_config(VideoEncoderConfig::from_preset(preset))
    }
}

/// Encodes raw video frames into compressed packets.
///
/// Uses a push/pop streaming interface: push RGBA frames via
/// [`push_frame`](Self::push_frame), then drain encoded packets with
/// [`pop_packet`](Self::pop_packet).
pub trait VideoEncoder: Send + 'static {
    /// Returns the encoder's display name.
    fn name(&self) -> &str;
    /// Returns the catalog [`VideoConfig`] describing the encoded stream.
    fn config(&self) -> VideoConfig;
    /// Pushes a raw RGBA frame into the encoder.
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    /// Pops the next encoded packet, or `None` if the encoder needs more input.
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

/// Decodes compressed video packets back into displayable frames.
pub trait VideoDecoder: Send + 'static {
    /// Creates a new decoder for the given catalog config and playback settings.
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized;
    /// Returns the decoder's display name.
    fn name(&self) -> &str;
    /// Pops the next decoded frame, or `None` if the decoder needs more input.
    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>>;
    /// Pushes an encoded packet into the decoder.
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    /// Sets the target viewport dimensions for optional downscaling.
    fn set_viewport(&mut self, w: u32, h: u32);
}
