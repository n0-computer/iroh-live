use anyhow::{Result, bail};
use hang::{
    Timestamp,
    catalog::{AudioCodec, AudioConfig},
};
use unsafe_libopus::{
    self as opus, OPUS_APPLICATION_VOIP, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST,
    OPUS_SET_INBAND_FEC_REQUEST, OPUS_SET_PACKET_LOSS_PERC_REQUEST, OpusEncoder as RawOpusEncoder,
    varargs,
};

use crate::av::{AudioEncoder, AudioEncoderInner, AudioFormat, AudioPreset};

const SAMPLE_RATE: u32 = 48_000;
const BITRATE_HQ: u64 = 128_000;
/// Opus frame size: 20ms at 48kHz = 960 samples per channel.
const FRAME_SIZE: usize = 960;
/// Maximum Opus packet size.
const MAX_PACKET: usize = 4000;

#[derive(derive_more::Debug)]
pub(crate) struct OpusEncoder {
    #[debug(skip)]
    encoder: *mut RawOpusEncoder,
    sample_rate: u32,
    channel_count: u32,
    bitrate: u64,
    extradata: Vec<u8>,
    /// Internal buffer accumulating interleaved f32 samples until we have a full frame.
    sample_buf: Vec<f32>,
    /// Number of complete frames encoded so far (for timestamp calculation).
    frame_count: u64,
    /// Encoded packets ready for collection.
    packet_buf: Vec<hang::Frame>,
}

// Safety: The OpusEncoder pointer is only used from a single thread.
// The trait system requires Send for AudioEncoderInner.
unsafe impl Send for OpusEncoder {}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        unsafe {
            opus::opus_encoder_destroy(self.encoder);
        }
    }
}

impl OpusEncoder {
    fn new(sample_rate: u32, channel_count: u32, bitrate: u64) -> Result<Self> {
        let mut error: i32 = 0;
        let encoder = unsafe {
            opus::opus_encoder_create(
                sample_rate as i32,
                channel_count as i32,
                OPUS_APPLICATION_VOIP,
                &mut error,
            )
        };
        if error != OPUS_OK || encoder.is_null() {
            bail!("opus_encoder_create failed with error {error}");
        }

        // Configure encoder
        unsafe {
            let ret = opus::opus_encoder_ctl_impl(
                encoder,
                OPUS_SET_BITRATE_REQUEST,
                varargs!(bitrate as i32),
            );
            if ret != OPUS_OK {
                bail!("OPUS_SET_BITRATE failed: {ret}");
            }
            let ret =
                opus::opus_encoder_ctl_impl(encoder, OPUS_SET_INBAND_FEC_REQUEST, varargs!(1i32));
            if ret != OPUS_OK {
                bail!("OPUS_SET_INBAND_FEC failed: {ret}");
            }
            let ret = opus::opus_encoder_ctl_impl(
                encoder,
                OPUS_SET_PACKET_LOSS_PERC_REQUEST,
                varargs!(10i32),
            );
            if ret != OPUS_OK {
                bail!("OPUS_SET_PACKET_LOSS_PERC failed: {ret}");
            }
            let ret = opus::opus_encoder_ctl_impl(encoder, OPUS_SET_DTX_REQUEST, varargs!(1i32));
            if ret != OPUS_OK {
                bail!("OPUS_SET_DTX failed: {ret}");
            }
        }

        // Build OpusHead extradata (19 bytes, RFC 7845 §5.1)
        let extradata = build_opus_head(channel_count, sample_rate);

        Ok(Self {
            encoder,
            sample_rate,
            channel_count,
            bitrate,
            extradata,
            sample_buf: Vec::new(),
            frame_count: 0,
            packet_buf: Vec::new(),
        })
    }

    /// Encode accumulated samples in `sample_buf` in FRAME_SIZE chunks.
    fn encode_pending(&mut self) -> Result<()> {
        let samples_per_frame = FRAME_SIZE * self.channel_count as usize;
        while self.sample_buf.len() >= samples_per_frame {
            let mut out = vec![0u8; MAX_PACKET];
            let ret = unsafe {
                opus::opus_encode_float(
                    self.encoder,
                    self.sample_buf.as_ptr(),
                    FRAME_SIZE as i32,
                    out.as_mut_ptr(),
                    MAX_PACKET as i32,
                )
            };
            if ret < 0 {
                bail!("opus_encode_float failed: {ret}");
            }
            let encoded_len = ret as usize;
            out.truncate(encoded_len);

            let timestamp_us =
                (self.frame_count * FRAME_SIZE as u64 * 1_000_000) / self.sample_rate as u64;
            self.packet_buf.push(hang::Frame {
                payload: out.into(),
                timestamp: Timestamp::from_micros(timestamp_us)?,
                keyframe: true,
            });
            self.frame_count += 1;

            // Remove consumed samples
            self.sample_buf.drain(..samples_per_frame);
        }
        Ok(())
    }
}

impl AudioEncoder for OpusEncoder {
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self> {
        let bitrate = match preset {
            AudioPreset::Hq => BITRATE_HQ,
            AudioPreset::Lq => 32_000,
        };
        Self::new(SAMPLE_RATE, format.channel_count, bitrate)
    }
}

impl AudioEncoderInner for OpusEncoder {
    fn name(&self) -> &str {
        "opus"
    }

    fn config(&self) -> AudioConfig {
        AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            bitrate: Some(self.bitrate),
            description: Some(self.extradata.clone().into()),
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.sample_buf.extend_from_slice(samples);
        self.encode_pending()
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        Ok(if self.packet_buf.is_empty() {
            None
        } else {
            Some(self.packet_buf.remove(0))
        })
    }
}

/// Build a 19-byte OpusHead structure per RFC 7845 §5.1.
fn build_opus_head(channel_count: u32, sample_rate: u32) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead"); // Magic signature
    head.push(1); // Version
    head.push(channel_count as u8); // Channel count
    head.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    head.extend_from_slice(&sample_rate.to_le_bytes()); // Input sample rate
    head.extend_from_slice(&0i16.to_le_bytes()); // Output gain
    head.push(0); // Channel mapping family (0 = mono/stereo)
    head
}
