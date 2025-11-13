use std::time::Duration;

use anyhow::{Context, Result};
use ffmpeg_next::{self as ffmpeg, util::channel_layout::ChannelLayout};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::{PacketSender, audio::OutputStreamHandle, ffmpeg_ext::CodecContextExt};

// pub trait AudioDecoder {
//     fn config(&self) -> AudioConfig;
//     fn push_frame(&mut self, frame: hang::Frame) -> Result<()>;
//     fn poll_samples(&mut self, buf: &mut [f32]) -> Result<usize>;
// }

pub fn new_decoder(
    config: &hang::catalog::AudioConfig,
    stream_handle: OutputStreamHandle,
    shutdown: CancellationToken,
) -> Result<PacketSender> {
    let (packet_tx, packet_rx) = mpsc::channel(1024);
    let ctx = match config.codec {
        hang::catalog::AudioCodec::Opus => {
            let codec_id = ffmpeg::codec::Id::OPUS;
            let codec = ffmpeg::decoder::find(codec_id).unwrap();
            let mut ctx = ffmpeg::codec::Context::new_with_codec(codec)
                .decoder()
                .audio()?;
            if let Some(extradata) = &config.description {
                ctx.set_extradata(&extradata)?;
            }
            ctx.set_channel_layout(if config.channel_count == 1 {
                ChannelLayout::MONO
            } else {
                ChannelLayout::STEREO
            });
            unsafe {
                let ctx_mut = ctx.as_mut_ptr();
                (*ctx_mut).sample_rate = config.sample_rate as i32;
            }
            ctx
        }
        _ => anyhow::bail!(
            "Unsupported codec {} (only aac and opus are supported)",
            config.codec
        ),
    };

    std::thread::spawn(move || {
        if let Err(err) = decode_loop(ctx, packet_rx, stream_handle, shutdown) {
            error!("Decoder failed: {err:?}");
        }
        let _ = ctx;
    });
    Ok(packet_tx)
}

fn decode_loop(
    mut codec: ffmpeg::decoder::Audio,
    mut packet_rx: mpsc::Receiver<hang::Frame>,
    output_stream: OutputStreamHandle,
    shutdown: CancellationToken,
) -> Result<()> {
    let mut last_timestamp = None;

    let target_sample_rate = output_stream
        .lock()
        .expect("poisoned")
        .sample_rate()
        .context("output stream misses sample rate")?
        .get();
    let target_channel_layout = ChannelLayout::STEREO;
    let target_sample_format = ffmpeg_next::util::format::sample::Sample::F32(
        ffmpeg_next::util::format::sample::Type::Packed,
    );

    let mut resampler = ffmpeg::software::resampling::Context::get(
        codec.format(),
        codec.channel_layout(),
        codec.rate(),
        target_sample_format,
        target_channel_layout,
        target_sample_rate,
    )
    .unwrap();
    while let Some(encoded_frame) = packet_rx.blocking_recv() {
        if shutdown.is_cancelled() {
            break;
        }
        trace!("recv frame {}", encoded_frame.payload.len());
        let packet = ffmpeg::Packet::borrow(&encoded_frame.payload);
        match codec.send_packet(&packet) {
            Ok(()) => {}
            Err(err) => {
                warn!(
                    "decode packet failed (len {}): {err:?}",
                    packet.data().unwrap().len(),
                );
                continue;
            }
        }
        // Create an empty frame to hold the decoded audio data.
        let mut decoded_frame = ffmpeg_next::util::frame::Audio::empty();

        while codec.receive_frame(&mut decoded_frame).is_ok() {
            // Create an empty frame to hold the resampled audio data.
            let mut resampled_frame = ffmpeg_next::util::frame::Audio::empty();
            resampler.run(&decoded_frame, &mut resampled_frame).unwrap();

            let frame = resampled_frame;

            debug!("got audio frame with {} samples", frame.samples());

            let delay = match last_timestamp {
                None => Duration::default(),
                Some(last_timestamp) => encoded_frame.timestamp.saturating_sub(last_timestamp),
            };
            if delay > Duration::ZERO {
                std::thread::sleep(delay);
            }
            last_timestamp = Some(encoded_frame.timestamp);
            let mut handle = output_stream.lock().unwrap();

            // If this happens excessively in Release mode, you may want to consider
            // increasing [`StreamWriterConfig::channel_config.latency_seconds`].
            if handle.underflow_occurred() {
                warn!("Underflow occured in stream writer node!");
            }

            // If this happens excessively in Release mode, you may want to consider
            // increasing [`StreamWriterConfig::channel_config.capacity_seconds`]. For
            // example, if you are streaming data from a network, you may want to
            // increase the capacity to several seconds.
            if handle.overflow_occurred() {
                warn!("Overflow occured in stream writer node!");
            }

            // Wait until the node's processor is ready to receive data.
            if handle.is_ready() {
                let expected_bytes =
                    frame.samples() * frame.channels() as usize * core::mem::size_of::<f32>();
                let cpal_sample_data: &[f32] =
                    bytemuck::cast_slice(&frame.data(0)[..expected_bytes]);
                handle.push_interleaved(cpal_sample_data);
                debug!("pushed samples {}", cpal_sample_data.len());
            } else {
                warn!("output handle is inactive")
            }
        }
    }
    Ok(())
}
