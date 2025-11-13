use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::audio::OutputStreamHandle;

pub fn new_decoder(
    _config: &hang::catalog::AudioConfig,
    stream_handle: OutputStreamHandle,
    shutdown: CancellationToken,
) -> Result<mpsc::Sender<hang::Frame>> {
    // We ignore config.description for opus and let the decoder derive state from packets.
    let (tx, mut rx) = mpsc::channel::<hang::Frame>(1024);

    // Determine output sample rate and channels from output stream
    let (out_rate, out_channels) = {
        let out = stream_handle.lock().expect("poisoned");
        let sr = out.sample_rate().expect("output sample rate").get();
        let ch = 2u32; // Stream writer expects interleaved stereo by default
        (sr, ch)
    };

    let decoder_channels = match out_channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        n => anyhow::bail!("unsupported output channels: {n}"),
    };

    // Use output stream rate to avoid an extra resampler.
    let mut dec = opus::Decoder::new(out_rate, decoder_channels)?;

    std::thread::spawn(move || {
        let mut last_timestamp = None;
        // 120ms at 48kHz max frame size from opus spec; ensure buffer large enough
        let mut pcm = vec![0f32; (out_rate as usize) * (out_channels as usize) / 2];
        while let Some(encoded) = rx.blocking_recv() {
            if shutdown.is_cancelled() { break; }
            trace!("recv audio pkt {} bytes", encoded.payload.len());

            // Pace output by timestamps to keep A/V sync similar to ffmpeg path
            if let Some(last) = last_timestamp {
                let delay = encoded.timestamp.saturating_sub(last);
                if delay > Duration::ZERO { std::thread::sleep(delay); }
            }
            last_timestamp = Some(encoded.timestamp);

            // Decode into f32 interleaved PCM
            let frame_samples = match dec.decode_float(&encoded.payload, &mut pcm, false) {
                Ok(n) => n, // samples per channel
                Err(err) => {
                    warn!("opus decode error: {err:?}");
                    continue;
                }
            } as usize;

            let n_f32 = frame_samples * (out_channels as usize);
            let pcm = &pcm[..n_f32];

            let mut handle = stream_handle.lock().expect("poisoned");
            if handle.underflow_occurred() { warn!("Underflow in audio output"); }
            if handle.overflow_occurred() { warn!("Overflow in audio output"); }
            if handle.is_ready() { handle.push_interleaved(pcm); }
        }
    });

    Ok(tx)
}
