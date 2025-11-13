use std::{
    task::Poll,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ffmpeg_next::{self as ffmpeg, Rational};
use firewheel::nodes::stream::ReadStatus;
use hang::catalog::AudioConfig;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::{audio::AudioBackend, ffmpeg_ext::CodecContextExt};

const SAMPLE_RATE: u32 = 48_000;
const BITRATE: u64 = 128_000; // 128 kbps

pub trait AudioEncoder {
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn poll_frame(&mut self) -> Result<Poll<Option<hang::Frame>>>;
}

pub struct OpusEncoder {
    encoder: ffmpeg::encoder::Audio,
    frame_count: u64,
    sample_rate: u32,
    channel_count: u32,
    extradata: Vec<u8>,
}

impl OpusEncoder {
    pub fn stereo() -> Result<Self> {
        Self::new(SAMPLE_RATE, 2)
    }

    pub fn mono() -> Result<Self> {
        Self::new(SAMPLE_RATE, 1)
    }

    pub fn new(sample_rate: u32, channel_count: u32) -> Result<Self> {
        tracing::info!(
            "Initializing Opus encoder: {}Hz, {} channels",
            sample_rate,
            channel_count
        );
        ffmpeg::init()?;

        let codec =
            ffmpeg::encoder::find(ffmpeg::codec::Id::OPUS).context("Opus encoder not found")?;
        tracing::debug!("Found Opus codec: {:?}", codec.name());
        let mut ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()?;

        let sample_rate = sample_rate as i32;
        ctx.set_rate(sample_rate);
        ctx.set_bit_rate(BITRATE as usize);
        ctx.set_format(ffmpeg::format::Sample::F32(
            ffmpeg_next::format::sample::Type::Packed,
        ));
        ctx.set_time_base(Rational::new(1, sample_rate));
        ctx.set_channel_layout(if channel_count == 1 {
            ffmpeg::util::channel_layout::ChannelLayout::MONO
        } else {
            ffmpeg::util::channel_layout::ChannelLayout::STEREO
        });

        let encoder = ctx.open()?;

        let extradata = encoder.extradata().unwrap_or(&[]).to_vec();

        tracing::info!("Opus encoder initialized successfully");
        Ok(Self {
            encoder,
            frame_count: 0,
            sample_rate: sample_rate as u32,
            channel_count,
            extradata,
        })
    }
}
impl AudioEncoder for OpusEncoder {
    fn config(&self) -> AudioConfig {
        hang::catalog::AudioConfig {
            codec: hang::catalog::AudioCodec::Opus,
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            bitrate: Some(BITRATE),
            description: Some(self.extradata.clone().into()),
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }

        let samples_per_channel = samples.len() / self.channel_count as usize;
        debug_assert_eq!(samples_per_channel as u32, self.encoder.frame_size());

        let mut audio_frame = ffmpeg::util::frame::Audio::new(
            ffmpeg::util::format::sample::Sample::F32(ffmpeg::util::format::sample::Type::Packed),
            samples_per_channel,
            ffmpeg::util::channel_layout::ChannelLayout::default(self.channel_count as i32),
        );

        // Copy interleaved samples directly since we're using packed format
        let frame_data = audio_frame.data_mut(0);
        let frame_samples: &mut [f32] = bytemuck::cast_slice_mut(frame_data);

        let copy_len = samples.len().min(frame_samples.len());
        frame_samples[..copy_len].copy_from_slice(&samples[..copy_len]);

        audio_frame.set_pts(Some(self.frame_count as i64));
        self.frame_count += samples_per_channel as u64;

        trace!("push samples {}", audio_frame.samples());
        self.encoder.send_frame(&audio_frame)?;
        Ok(())
    }

    fn poll_frame(&mut self) -> Result<Poll<Option<hang::Frame>>> {
        let mut packet = ffmpeg::packet::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let payload = packet.data().unwrap_or(&[]).to_vec();
                let hang_frame = hang::Frame {
                    payload: payload.into(),
                    timestamp: Duration::from_nanos(
                        (self.frame_count * 1_000_000_000) / self.sample_rate as u64,
                    ),
                    keyframe: true, // Audio frames are generally independent
                };
                trace!("poll frame {}", hang_frame.payload.len());
                Ok(Poll::Ready(Some(hang_frame)))
            }
            Err(ffmpeg::Error::Eof) => Ok(Poll::Ready(None)),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                Ok(Poll::Pending)
            }
            Err(e) => Err(e.into()),
        }
    }
}

pub fn capture_and_encode(
    audio_ctx: AudioBackend,
    mut encoder: impl AudioEncoder,
    shutdown: CancellationToken,
    mut on_frame: impl FnMut(hang::Frame),
) -> Result<()> {
    const UPDATE_INTERVAL: Duration = Duration::from_millis(20);

    let config = encoder.config();
    let sample_rate = config.sample_rate;
    let channel_count = config.channel_count;
    let interval = UPDATE_INTERVAL;

    let audio_input = audio_ctx
        .blocking_input_stream(sample_rate, channel_count)
        .context("failed to create audio input stream")?;
    let samples_per_frame = sample_rate / 1000 * interval.as_millis() as u32;
    let mut buf = vec![0.0f32; samples_per_frame as usize * channel_count as usize];

    tracing::info!(
        buf_len = buf.len(),
        channel_count,
        sample_rate,
        ?interval,
        "Starting audio capture"
    );
    'run: loop {
        let mut stream = audio_input.lock().expect("poisoned");
        let start = Instant::now();
        // Check shutdown before each audio frame capture
        if shutdown.is_cancelled() {
            tracing::debug!("Audio capture shutdown requested");
            stream.stop_stream();
            break;
        }

        match stream.read_interleaved(&mut buf) {
            Some(status) => {
                // drop(stream);
                match status {
                    ReadStatus::Ok => {}
                    ReadStatus::InputNotReady => warn!("audio input not ready"),
                    ReadStatus::UnderflowOccurred { num_frames_read } => {
                        warn!("audio input underflow: {num_frames_read} frames missing")
                    }
                    ReadStatus::OverflowCorrected {
                        num_frames_discarded,
                    } => warn!("audio input overflow: {num_frames_discarded} frames discarded"),
                }
                encoder.push_samples(&buf)?;
                while let Poll::Ready(frame) = encoder.poll_frame()? {
                    match frame {
                        Some(frame) => on_frame(frame),
                        None => break 'run,
                    }
                }
            }
            None => warn!("audio input stream is inactive"),
        }
        drop(stream);
        std::thread::sleep(interval.saturating_sub(start.elapsed()));
    }

    // Flush any remaining encoded audio packets on shutdown
    while let Poll::Ready(Some(frame)) = encoder.poll_frame()? {
        on_frame(frame);
    }

    Ok(())
}
