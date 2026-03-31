//! Pure-Rust audio file import via symphonia.
//!
//! Decodes audio files (WAV, MP3, FLAC, etc.) using symphonia's codec
//! infrastructure, resamples to 48 kHz stereo using rubato, and feeds
//! samples through a ring buffer for the [`AudioSource`] trait.
//!
//! This eliminates the runtime dependency on ffmpeg for common audio
//! formats while keeping the same ring-buffer interface used by the
//! ffmpeg backend.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use audioadapter_buffers::direct::InterleavedSlice;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use rubato::{
    Async, FixedAsync, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};
use tracing::{debug, info, warn};

use crate::{format::AudioFormat, traits::AudioSource};

/// Size of the ring buffer in f32 samples. 48 kHz * 2 channels * 2 seconds.
const RING_BUFFER_SAMPLES: usize = 48_000 * 2 * 2;

/// Target sample rate for the audio pipeline.
const TARGET_SAMPLE_RATE: u32 = 48_000;

/// Target channel count.
const TARGET_CHANNELS: u32 = 2;

/// Audio source that reads from a file via symphonia (pure Rust).
///
/// A background thread decodes the file, resamples to 48 kHz stereo,
/// and pushes f32 samples into a lock-free ring buffer. The
/// [`AudioSource::pop_samples`] implementation drains from the consumer
/// side.
pub struct AudioFileSource {
    format: AudioFormat,
    consumer: ringbuf::HeapCons<f32>,
    eof: Arc<AtomicBool>,
}

impl std::fmt::Debug for AudioFileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFileSource")
            .field("format", &self.format)
            .field("eof", &self.eof.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl AudioFileSource {
    /// Creates a new audio file source backed by symphonia.
    ///
    /// Starts decoding immediately on a background thread. Returns an
    /// error if the file cannot be opened or probed.
    ///
    /// When `loop_playback` is true, the decoder seeks back to the start
    /// of the stream upon reaching EOF and continues indefinitely.
    pub fn new(path: impl AsRef<Path>, loop_playback: bool) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        anyhow::ensure!(path.exists(), "audio file not found: {}", path.display());

        let format = AudioFormat::stereo_48k();
        let rb = HeapRb::<f32>::new(RING_BUFFER_SAMPLES);
        let (producer, consumer) = rb.split();
        let eof = Arc::new(AtomicBool::new(false));

        spawn_decode_thread(path.clone(), loop_playback, producer, eof.clone());

        info!(
            path = %path.display(),
            loop_playback,
            "audio file source started via symphonia"
        );

        Ok(Self {
            format,
            consumer,
            eof,
        })
    }
}

impl AudioSource for AudioFileSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> anyhow::Result<Option<usize>> {
        let available = self.consumer.occupied_len();
        if available == 0 {
            if self.eof.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("audio file source ended (EOF)"));
            }
            return Ok(None);
        }

        let channels = self.format.channel_count as usize;
        let max_frames = buf.len() / channels;
        let available_frames = available / channels;
        let frames_to_read = max_frames.min(available_frames);
        let samples_to_read = frames_to_read * channels;

        let n = self.consumer.pop_slice(&mut buf[..samples_to_read]);
        debug_assert_eq!(n, samples_to_read);

        Ok(Some(frames_to_read))
    }
}

/// Spawns a background thread that decodes audio with symphonia, resamples
/// to 48 kHz stereo, and pushes samples into the ring buffer.
fn spawn_decode_thread(
    path: PathBuf,
    loop_playback: bool,
    producer: ringbuf::HeapProd<f32>,
    eof: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("audio-file-decode".into())
        .spawn(move || {
            if let Err(e) = decode_loop(&path, loop_playback, producer) {
                warn!(path = %path.display(), "symphonia decode error: {e:#}");
            }
            eof.store(true, Ordering::Relaxed);
            debug!(path = %path.display(), "audio file decoder finished");
        })
        .expect("failed to spawn audio file decode thread");
}

/// Opens the file with symphonia, decodes all packets, and pushes
/// resampled stereo f32 samples into the ring buffer.
fn decode_loop(
    path: &Path,
    loop_playback: bool,
    mut producer: ringbuf::HeapProd<f32>,
) -> anyhow::Result<()> {
    loop {
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format_reader = probed.format;

        // Find the first audio track.
        let track = format_reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow::anyhow!("no audio track found in {}", path.display()))?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        let source_rate = codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);
        let source_channels = codec_params
            .channels
            .map(|c| c.count() as u32)
            .unwrap_or(TARGET_CHANNELS);

        debug!(
            source_rate,
            source_channels,
            path = %path.display(),
            "decoding audio file"
        );

        let mut decoder =
            symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

        // Set up resampler if source rate differs from target.
        let mut resampler = if source_rate != TARGET_SAMPLE_RATE {
            Some(create_resampler(source_rate, TARGET_CHANNELS)?)
        } else {
            None
        };

        // Decode packets until EOF or error.
        loop {
            let packet = match format_reader.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break; // Normal EOF
                }
                Err(e) => return Err(e.into()),
            };

            if packet.track_id() != track_id {
                continue;
            }

            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(msg)) => {
                    warn!("decode error (skipping packet): {msg}");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let spec = *decoded.spec();
            let num_frames = decoded.frames();
            if num_frames == 0 {
                continue;
            }

            // Convert decoded audio to interleaved f32.
            let actual_channels = spec.channels.count();
            let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
            sample_buf.copy_interleaved_ref(decoded);
            let interleaved = sample_buf.samples();

            // Channel conversion: mix down or duplicate to stereo.
            let stereo_samples =
                convert_channels(interleaved, actual_channels, TARGET_CHANNELS as usize);

            // Resample if needed.
            let output = if let Some(ref mut rs) = resampler {
                resample(rs, &stereo_samples, TARGET_CHANNELS as usize)?
            } else {
                stereo_samples
            };

            // Push into ring buffer, blocking briefly if full.
            push_samples(&mut producer, &output);
        }

        if !loop_playback {
            break;
        }

        debug!(path = %path.display(), "looping audio file");
    }

    Ok(())
}

/// Creates a rubato async resampler from `source_rate` to 48 kHz.
fn create_resampler(source_rate: u32, channels: u32) -> anyhow::Result<Async<f32>> {
    let ratio = TARGET_SAMPLE_RATE as f64 / source_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let chunk_size = 1024;
    Ok(Async::new_sinc(
        ratio,
        1.1,
        &params,
        chunk_size,
        channels as usize,
        FixedAsync::Input,
    )?)
}

/// Resamples interleaved f32 samples using rubato.
fn resample(
    resampler: &mut Async<f32>,
    input: &[f32],
    channels: usize,
) -> anyhow::Result<Vec<f32>> {
    use rubato::Resampler as _;

    let frames = input.len() / channels;
    if frames == 0 {
        return Ok(Vec::new());
    }

    let input_adapter = InterleavedSlice::new(input, channels, frames)?;
    let result = resampler.process(&input_adapter, 0, None)?;
    Ok(result.take_data())
}

/// Converts interleaved audio between channel counts.
///
/// Mono to stereo: duplicates the single channel.
/// Stereo to mono: averages left and right.
/// Multi-channel to stereo: takes the first two channels.
/// Same channel count: returns the input unchanged (zero-copy).
fn convert_channels(samples: &[f32], from_ch: usize, to_ch: usize) -> Vec<f32> {
    if from_ch == to_ch {
        return samples.to_vec();
    }

    let frames = samples.len() / from_ch;

    if from_ch == 1 && to_ch == 2 {
        // Mono to stereo: duplicate each sample.
        let mut out = Vec::with_capacity(frames * 2);
        for &s in samples.iter().take(frames) {
            out.push(s);
            out.push(s);
        }
        return out;
    }

    if from_ch == 2 && to_ch == 1 {
        // Stereo to mono: average L+R.
        let mut out = Vec::with_capacity(frames);
        for i in 0..frames {
            out.push((samples[i * 2] + samples[i * 2 + 1]) * 0.5);
        }
        return out;
    }

    if from_ch > to_ch {
        // Multi-channel to stereo: take first `to_ch` channels.
        let mut out = Vec::with_capacity(frames * to_ch);
        for i in 0..frames {
            for ch in 0..to_ch {
                out.push(samples[i * from_ch + ch]);
            }
        }
        return out;
    }

    // Fewer channels to more: duplicate last channel to fill.
    let mut out = Vec::with_capacity(frames * to_ch);
    for i in 0..frames {
        for ch in 0..to_ch {
            let src_ch = ch.min(from_ch - 1);
            out.push(samples[i * from_ch + src_ch]);
        }
    }
    out
}

/// Pushes samples into the ring buffer, spinning briefly when full.
fn push_samples(producer: &mut ringbuf::HeapProd<f32>, samples: &[f32]) {
    let mut offset = 0;
    while offset < samples.len() {
        let pushed = producer.push_slice(&samples[offset..]);
        offset += pushed;
        if pushed == 0 && offset < samples.len() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
