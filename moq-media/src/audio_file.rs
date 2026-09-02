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

use n0_error::{Result, stack_error};
use n0_future::{boxed::BoxStream, stream::StreamExt};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, DecoderOptions},
    formats::{FormatOptions, FormatReader, Track},
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
    /// Dropping this is what stops the decode thread: its next
    /// `blocking_send` fails and the loop returns.
    frames: mpsc::Receiver<moq_audio::Frame>,
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
        std::thread::Builder::new()
            .name("audio-file".into())
            .spawn(move || {
                if let Err(err) = decode_loop(&path, looping, &tx) {
                    warn!(path = %path.display(), error = %err, "audio file decode stopped");
                }
            })
            .map_err(|source| {
                n0_error::e!(AudioFileError::Io {
                    path: display.clone(),
                    source,
                })
            })?;

        Ok(Self { input, frames })
    }

    /// The PCM layout the file decodes to, for the encoder's `Input`.
    pub fn input(&self) -> moq_audio::encode::Input {
        self.input.clone()
    }

    /// Consumes the file and returns its frames as a stream.
    ///
    /// The decode thread stops when the returned stream is dropped, because the
    /// receiver goes with it and the thread's next send fails.
    pub fn into_stream(self) -> BoxStream<moq_audio::Frame> {
        Box::pin(
            n0_future::stream::unfold(self.frames, |mut frames| async move {
                let frame = frames.recv().await?;
                Some((frame, frames))
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

impl Probe {
    /// Reads the layout off a track, falling back to CD-adjacent defaults for a
    /// container that declares neither.
    fn of(track: &Track) -> Self {
        Self {
            sample_rate: track.codec_params.sample_rate.unwrap_or(48_000),
            channels: track
                .codec_params
                .channels
                .map(|channels| channels.count() as u32)
                .unwrap_or(2),
        }
    }
}

/// Opens `path` and returns its container reader alongside the first track that
/// carries audio.
///
/// Shared by the probe and by each decode pass, which both need exactly this
/// and nothing else: looping reopens the file rather than seeking, so the pass
/// starts from the same place the probe did.
fn open_track(path: &Path) -> Result<(Box<dyn FormatReader>, Track), AudioFileError> {
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
        .cloned()
        .ok_or_else(|| n0_error::e!(AudioFileError::NoTrack { path: display }))?;
    Ok((probed.format, track))
}

fn probe(path: &Path) -> Result<Probe, AudioFileError> {
    let (_, track) = open_track(path)?;
    Ok(Probe::of(&track))
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
        let frames = decode_once(path, tx, &started, &mut published)?;
        if !looping {
            debug!(path = %path.display(), "audio file ended");
            return Ok(());
        }
        // A pass that decoded nothing would loop again immediately, and every
        // pass after it too: the pacing sleep is driven by decoded audio, so
        // there is nothing to slow the retry down. A file truncated to less
        // than one packet does exactly that.
        if frames == 0 {
            warn!(path = %path.display(), "audio file decoded to nothing, not looping");
            return Ok(());
        }
        debug!(path = %path.display(), frames, "audio file looping");
    }
}

/// Runs one pass over the file, returning how many frames it published.
fn decode_once(
    path: &Path,
    tx: &mpsc::Sender<moq_audio::Frame>,
    started: &std::time::Instant,
    published: &mut Duration,
) -> Result<usize, AudioFileError> {
    let display = path.display().to_string();
    let decode_err = |source| {
        n0_error::e!(AudioFileError::Decode {
            path: display.clone(),
            source,
        })
    };

    let (mut format, track) = open_track(path)?;
    let track_id = track.id;
    let Probe {
        sample_rate,
        channels,
    } = Probe::of(&track);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(decode_err)?;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut sent = 0;

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
        // `Frame::new` classifies the samples as active, which is right for a
        // file: the encoder decides what is silence, not the source.
        let frame = moq_audio::Frame::new(
            data,
            moq_net::Timestamp::from_micros(published.as_micros() as u64)
                .expect("published duration out of Timestamp range"),
        );
        if tx.blocking_send(frame).is_err() {
            // The publisher went away.
            return Ok(sent);
        }
        sent += 1;

        let frames = interleaved.len() / channels.max(1) as usize;
        *published += Duration::from_secs_f64(frames as f64 / sample_rate as f64);
        // Stay roughly in step with wall clock; a small lead is fine and is
        // what the queue absorbs.
        if let Some(ahead) = published.checked_sub(started.elapsed()) {
            std::thread::sleep(ahead);
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A valid PCM WAV header describing zero samples of audio.
    fn empty_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&96_000u32.to_le_bytes()); // bytes per second
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav
    }

    /// A pass that decodes nothing must not be retried, or the pacing sleep has
    /// nothing to slow it down and the thread spins on the file forever.
    #[test]
    fn a_file_with_no_samples_stops_instead_of_looping() {
        let path = std::env::temp_dir().join(format!("moq-media-empty-{}.wav", std::process::id()));
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(&empty_wav()))
            .expect("write the test file");

        // On its own thread with a deadline, because the failure this guards
        // against is a loop that never returns rather than one that returns the
        // wrong thing.
        let (done, finished) = std::sync::mpsc::channel();
        let looping = path.clone();
        std::thread::spawn(move || {
            let (tx, rx) = mpsc::channel(QUEUE_DEPTH);
            let result = decode_loop(&looping, true, &tx);
            let _ = done.send((result.is_ok(), rx.is_empty()));
        });

        let outcome = finished.recv_timeout(Duration::from_secs(5));
        std::fs::remove_file(&path).ok();
        assert_eq!(
            outcome.ok(),
            Some((true, true)),
            "an empty file should end the loop without publishing anything",
        );
    }
}
