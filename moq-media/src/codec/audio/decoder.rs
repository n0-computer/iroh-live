use anyhow::{Result, bail};
use bytes::Buf;
use hang::catalog::{AudioCodec, AudioConfig};
use unsafe_libopus::{self as opus, OPUS_OK, OpusDecoder as RawOpusDecoder};

use crate::av::{AudioDecoder, AudioFormat};
use crate::codec::resample::Resampler;

/// Maximum Opus frame size: 120ms at 48kHz = 5760 samples per channel.
const MAX_FRAME_SIZE: usize = 5760;

#[derive(derive_more::Debug)]
pub struct OpusAudioDecoder {
    #[debug(skip)]
    decoder: *mut RawOpusDecoder,
    channel_count: u32,
    #[debug(skip)]
    resampler: Resampler,
    /// Decoded + resampled sample buffer.
    samples: Vec<f32>,
}

// Safety: The OpusDecoder pointer is only used from a single thread.
unsafe impl Send for OpusAudioDecoder {}

impl Drop for OpusAudioDecoder {
    fn drop(&mut self) {
        unsafe {
            opus::opus_decoder_destroy(self.decoder);
        }
    }
}

impl AudioDecoder for OpusAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> {
        if !matches!(&config.codec, AudioCodec::Opus) {
            bail!(
                "Unsupported codec {} (only opus is supported)",
                config.codec
            );
        }

        let channel_count = config.channel_count;
        let mut error: i32 = 0;
        let decoder = unsafe {
            opus::opus_decoder_create(config.sample_rate as i32, channel_count as i32, &mut error)
        };
        if error != OPUS_OK || decoder.is_null() {
            bail!("opus_decoder_create failed with error {error}");
        }

        let resampler =
            Resampler::new(config.sample_rate, target_format.sample_rate, channel_count)?;

        Ok(Self {
            decoder,
            channel_count,
            resampler,
            samples: Vec::new(),
        })
    }

    fn push_packet(&mut self, mut packet: hang::Frame) -> Result<()> {
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
        let max_samples = MAX_FRAME_SIZE * self.channel_count as usize;
        let mut pcm = vec![0f32; max_samples];

        let decoded_samples = unsafe {
            opus::opus_decode_float(
                self.decoder,
                payload.as_ptr(),
                payload.len() as i32,
                pcm.as_mut_ptr(),
                MAX_FRAME_SIZE as i32,
                0, // no FEC
            )
        };
        if decoded_samples < 0 {
            bail!("opus_decode_float failed: {decoded_samples}");
        }

        let total_samples = decoded_samples as usize * self.channel_count as usize;
        pcm.truncate(total_samples);

        // Resample if needed (e.g., 48kHz → target rate)
        let resampled = self.resampler.process(&pcm)?;
        self.samples = resampled;

        Ok(())
    }

    fn pop_samples(&mut self) -> Result<Option<&[f32]>> {
        if self.samples.is_empty() {
            Ok(None)
        } else {
            Ok(Some(&self.samples))
        }
    }
}
