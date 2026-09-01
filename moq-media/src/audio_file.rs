//! Publishing an audio file as if it were a microphone.
//!
//! `moq-audio` pulls symphonia only to decode raw AAC-LC frames off the wire,
//! so it has no container reader. This one demuxes and decodes a local file and
//! presents the result as a stream of [`moq_audio::Frame`]s that
//! [`AudioSource::Frames`](crate::publish::AudioSource::Frames) accepts.
//!
//! No resampling happens here. The encoder is told the file's own rate through
//! [`AudioFile::input`] and converts to the codec's rate itself, which is one
//! resampler instead of two.

use std::{path::Path, time::Duration};

use n0_error::{Result, StdResultExt, stack_error};
use n0_future::{boxed::BoxStream, stream::StreamExt};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, DecoderOptions},
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// How many decoded packets to keep queued ahead of the publisher.
///
/// Bounded so a paused publisher cannot pull an entire file into memory; four
/// packets is well under a second for any codec we read.
const QUEUE_DEPTH: usize = 4;

/// Errors raised while reading an audio file.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum AudioFileError {
    /// The file could not be opened or read.
    #[error("failed to read {path}")]
    Io {
        /// The path that failed.
        path: String,
        /// The underlying error.
        #[error(source, std_err)]
        source: std::io::Error,
    },
    /// The container or codec is not one symphonia can read.
    #[error("failed to decode {path}")]
    Decode {
        /// The path that failed.
        path: String,
        /// The underlying error.
        #[error(source, std_err)]
        source: symphonia::core::errors::Error,
    },
    /// The container held no audio track.
    #[error("no audio track in {path}")]
    NoTrack {
        /// The path that failed.
        path: String,
    },
}

/// A decoded audio file, ready to publish.
#[derive(Debug)]
pub struct AudioFile {
    input: moq_audio::encode::Input,
    frames: mpsc::Receiver<moq_audio::Frame>,
    /// Joined on drop so the decode thread stops with the source.
    _decoder: std::thread::JoinHandle<()>,
}

impl AudioFile {
    /// Opens `path` and starts decoding it on a dedicated thread.
    ///
    /// `looping` restarts at the beginning on end of file, which is what a demo
    /// or a hold-music source wants; otherwise the stream ends there.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read, holds no audio track, or uses a codec
    /// symphonia cannot decode.
    pub fn open(path: impl AsRef<Path>, looping: bool) -> Result<Self, AudioFileError> {
        let path = path.as_ref().to_path_buf();
        let display = path.display().to_string();
        let probe = probe(&path)?;
        let input = moq_audio::encode::Input {
            format: moq_audio::Format::F32,
            sample_rate: probe.sample_rate,
            channels: probe.channels,
        };

        let (tx, frames) = mpsc::channel(QUEUE_DEPTH);
        let decoder = std::thread::Builder::new()
            .name("audio-file".into())
            .spawn(move || {
                if let Err(err) = decode_loop(&path, looping, &tx) {
                    warn!(path = %path.display(), error = %err, "audio file decode stopped");
                }
            })
            .std_context("failed to spawn the audio file decode thread")
            .map_err(|err| {
                n0_error::e!(AudioFileError::Io {
                    path: display.clone(),
                    source: std::io::Error::other(err.to_string()),
                })
            })?;

        Ok(Self {
            input,
            frames,
            _decoder: decoder,
        })
    }

    /// The PCM layout the file decodes to, for the encoder's `Input`.
    pub fn input(&self) -> moq_audio::encode::Input {
        self.input.clone()
    }

    /// Consumes the file and returns its frames as a stream.
    pub fn into_stream(self) -> BoxStream<moq_audio::Frame> {
        let Self {
            frames, _decoder, ..
        } = self;
        // The join handle rides along in the stream so the decode thread lives
        // exactly as long as the reader does.
        Box::pin(
            n0_future::stream::unfold((frames, _decoder), |(mut frames, decoder)| async move {
                let frame = frames.recv().await?;
                Some((frame, (frames, decoder)))
            })
            .fuse(),
        )
    }
}

/// What probing a file tells us before any of it is decoded.
struct Probe {
    sample_rate: u32,
    channels: u32,
}

fn probe(path: &Path) -> Result<Probe, AudioFileError> {
    let display = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|source| {
        n0_error::e!(AudioFileError::Io {
            path: display.clone(),
            source,
        })
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|source| {
            n0_error::e!(AudioFileError::Decode {
                path: display.clone(),
                source,
            })
        })?;

    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| {
            n0_error::e!(AudioFileError::NoTrack {
                path: display.clone(),
            })
        })?;

    Ok(Probe {
        sample_rate: track.codec_params.sample_rate.unwrap_or(48_000),
        channels: track
            .codec_params
            .channels
            .map(|channels| channels.count() as u32)
            .unwrap_or(2),
    })
}

/// Decodes `path` into `tx`, restarting at the beginning when `looping`.
///
/// Paced against the sample count rather than run flat out, because the
/// publisher stamps PTS from sample counts: a decoder that raced ahead would
/// publish a minute of audio in a second and then starve.
fn decode_loop(
    path: &Path,
    looping: bool,
    tx: &mpsc::Sender<moq_audio::Frame>,
) -> Result<(), AudioFileError> {
    let started = std::time::Instant::now();
    let mut published = Duration::ZERO;

    loop {
        decode_once(path, tx, &started, &mut published)?;
        if !looping {
            debug!(path = %path.display(), "audio file ended");
            return Ok(());
        }
        debug!(path = %path.display(), "audio file looping");
    }
}

fn decode_once(
    path: &Path,
    tx: &mpsc::Sender<moq_audio::Frame>,
    started: &std::time::Instant,
    published: &mut Duration,
) -> Result<(), AudioFileError> {
    let display = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|source| {
        n0_error::e!(AudioFileError::Io {
            path: display.clone(),
            source,
        })
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let decode_err = |source| {
        n0_error::e!(AudioFileError::Decode {
            path: display.clone(),
            source,
        })
    };

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(decode_err)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| {
            n0_error::e!(AudioFileError::NoTrack {
                path: display.clone(),
            })
        })?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(48_000);
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(decode_err)?;
    let mut buffer: Option<SampleBuffer<f32>> = None;

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A corrupt packet is not fatal: skip it and keep the file playing.
            Err(symphonia::core::errors::Error::DecodeError(err)) => {
                debug!(error = %err, "skipping a corrupt audio packet");
                continue;
            }
            Err(err) => return Err(decode_err(err)),
        };

        let spec = *decoded.spec();
        let samples =
            buffer.get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        samples.copy_interleaved_ref(decoded);
        let interleaved = samples.samples();
        if interleaved.is_empty() {
            continue;
        }

        let data = bytes::Bytes::from(
            interleaved
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        let frame = moq_audio::Frame {
            timestamp: moq_net::Timestamp::from_micros(published.as_micros() as u64)
                .expect("published duration out of Timestamp range"),
            data,
        };
        if tx.blocking_send(frame).is_err() {
            // The publisher went away.
            return Ok(());
        }

        let frames = interleaved.len() / channels.max(1);
        *published += Duration::from_secs_f64(frames as f64 / sample_rate as f64);
        // Stay roughly in step with wall clock; a small lead is fine and is
        // what the queue absorbs.
        if let Some(ahead) = published.checked_sub(started.elapsed()) {
            std::thread::sleep(ahead);
        }
    }
    Ok(())
}
