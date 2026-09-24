//! Decoding an audio file as a source.
//!
//! `moq-audio` pulls symphonia only to decode raw AAC-LC frames off the wire,
//! so it has no container reader. This one demuxes and decodes a local file on
//! a thread of its own, in real time, into the fan-out every attached
//! broadcast reads.
//!
//! No resampling happens here. The encoder is told the file's own rate and
//! converts to the codec's rate itself, which is one resampler instead of two.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use n0_error::AnyError;
use symphonia::core::{
    codecs::{CodecParameters, audio::AudioDecoderOptions},
    formats::{FormatOptions, FormatReader, Track, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{AudioFormat, sender::PcmFanout};
use crate::error::Error;

/// Opens `path`, and decodes it into `fanout` on a thread of its own until the
/// file ends, or `stop` is cancelled.
///
/// Returns the file's own sample rate and layout, which the encoder is told.
///
/// # Errors
///
/// Fails if the file cannot be read, holds no audio track, or uses a codec
/// symphonia cannot decode. The first packet is decoded before this returns,
/// so a file that opens and then refuses its first packet fails here too.
pub(crate) fn spawn(
    path: PathBuf,
    looping: bool,
    fanout: PcmFanout,
    stop: CancellationToken,
) -> Result<AudioFormat, Error> {
    let probe = probe(&path)?;
    let format = AudioFormat::new(probe.sample_rate, probe.layout);
    std::thread::Builder::new()
        .name("audio-file".into())
        .spawn(move || {
            if let Err(err) = decode_loop(&path, looping, &fanout, &stop) {
                warn!(path = %path.display(), error = %format!("{err:#}"), "audio file decode stopped");
            }
        })?;
    Ok(format)
}

/// An error about `path`, with `source` behind it.
fn file_error(path: &Path, source: impl std::fmt::Display) -> Error {
    n0_error::e!(Error::Device {
        source: AnyError::from_string(format!("{}: {source}", path.display()))
    })
}

/// What probing a file tells us before any of it is decoded.
struct Probe {
    sample_rate: u32,
    layout: moq_audio::Layout,
}

impl Probe {
    /// Reads the layout off a track, falling back to CD-adjacent defaults for a
    /// container that declares neither.
    fn of(track: &Track) -> Self {
        let params = audio_params(track);
        Self {
            sample_rate: params
                .and_then(|params| params.sample_rate)
                .unwrap_or(48_000),
            // A declared count of zero is treated like no count at all, and
            // both fall back to stereo. Zero is the only count `from_channels`
            // refuses, so after the filter it cannot fail.
            layout: params
                .and_then(|params| params.channels.as_ref())
                .map(|channels| channels.count() as u32)
                .filter(|&channels| channels > 0)
                .and_then(|channels| moq_audio::Layout::from_channels(channels).ok())
                .unwrap_or(moq_audio::Layout::Stereo),
        }
    }
}

/// Opens `path` and returns its container reader alongside the first track that
/// carries audio.
///
/// Shared by the probe and by each decode pass, which both need exactly this
/// and nothing else: looping reopens the file rather than seeking, so the pass
/// starts from the same place the probe did.
fn open_track(path: &Path) -> Result<(Box<dyn FormatReader>, Track), Error> {
    let file = std::fs::File::open(path).map_err(|source| file_error(path, source))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|source| file_error(path, source))?;

    let track = format
        .tracks()
        .iter()
        .find(|track| audio_params(track).is_some())
        .cloned()
        .ok_or_else(|| file_error(path, "the file holds no audio track"))?;
    Ok((format, track))
}

/// The audio codec parameters of `track`, or `None` when it carries none or is
/// not an audio track.
///
/// Both are one check now: symphonia types the parameters per media kind, where
/// it used to hand out one struct for every track and a null codec id for the
/// ones it could not read.
fn audio_params(track: &Track) -> Option<&symphonia::core::codecs::audio::AudioCodecParameters> {
    match track.codec_params.as_ref()? {
        CodecParameters::Audio(params) => Some(params),
        _ => None,
    }
}

fn probe(path: &Path) -> Result<Probe, Error> {
    let (_, track) = open_track(path)?;
    Ok(Probe::of(&track))
}

/// Decodes `path` into `fanout`, restarting at the beginning when `looping`,
/// until the file ends or `stop` is cancelled.
///
/// Paced against the sample count rather than run flat out, because the
/// publisher stamps PTS from sample counts: a decoder that raced ahead would
/// publish a minute of audio in a second and then starve.
fn decode_loop(
    path: &Path,
    looping: bool,
    fanout: &PcmFanout,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let started = std::time::Instant::now();
    let mut published = Duration::ZERO;

    loop {
        let frames = decode_once(path, fanout, stop, &started, &mut published)?;
        if !looping || stop.is_cancelled() {
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
    fanout: &PcmFanout,
    stop: &CancellationToken,
    started: &std::time::Instant,
    published: &mut Duration,
) -> Result<usize, Error> {
    let decode_err = |source: symphonia::core::errors::Error| file_error(path, source);

    let (mut format, track) = open_track(path)?;
    let track_id = track.id;
    let Probe {
        sample_rate,
        layout,
    } = Probe::of(&track);
    let channels = layout.channels();

    let params =
        audio_params(&track).ok_or_else(|| file_error(path, "the file holds no audio track"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(decode_err)?;
    // Reused across packets so a file is not one allocation per packet.
    let mut interleaved: Vec<f32> = Vec::new();
    let mut sent = 0;

    // `next_packet` reports the end of the file as `None` and a file it cannot
    // read the rest of as an error. Symphonia 0.5 had only the error, so this
    // loop used to read every failure as the end and `:loop` replayed the
    // readable prefix of a corrupt file forever, silently.
    while let Some(packet) = format.next_packet().map_err(decode_err)? {
        if packet.track_id != track_id {
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

        decoded.copy_to_vec_interleaved(&mut interleaved);
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
        if stop.is_cancelled() {
            // The source went away.
            return Ok(sent);
        }
        // An error only means no broadcast is attached right now.
        let _ = fanout.send(frame);
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
        wav(&[], 1)
    }

    /// A PCM WAV carrying `samples` interleaved across `channels`.
    ///
    /// Sixteen-bit, 48 kHz, which is what the decoder converts from and the
    /// only thing about the file this crate does not choose.
    fn wav(samples: &[i16], channels: u16) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let block_align = channels * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000 * u32::from(block_align)).to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }

    /// Writes `contents` to a uniquely named file in the temp directory.
    fn temp_file(tag: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "iroh-live-media-{tag}-{}-{:?}.wav",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(contents))
            .expect("write the test file");
        path
    }

    /// The decoder converts to interleaved f32 and the publisher stamps from
    /// the sample count, so a file that decodes to the wrong number of samples
    /// or the wrong channel order publishes audio that is the wrong length and
    /// the wrong shape. Neither shows up in a test that only opens an empty
    /// file, which is all this had when symphonia 0.6 rewrote the conversion.
    #[test]
    fn a_file_decodes_to_interleaved_samples_in_order() {
        // Distinguishable per channel and per frame, so interleaving that is
        // transposed or off by one is visible in the values.
        let frames: Vec<i16> = (0..960).flat_map(|n| [n as i16, -(n as i16)]).collect();
        let path = temp_file("stereo", &wav(&frames, 2));

        let probe = probe(&path).expect("a valid stereo WAV opens");
        assert_eq!(probe.sample_rate, 48_000);
        assert_eq!(probe.layout, moq_audio::Layout::Stereo);

        let (fanout, mut rx) = tokio::sync::broadcast::channel(1024);
        let decoded = std::thread::spawn({
            let path = path.clone();
            move || decode_loop(&path, false, &fanout, &CancellationToken::new())
        });

        let mut samples: Vec<f32> = Vec::new();
        while let Ok(frame) = rx.blocking_recv() {
            samples.extend(
                frame
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes),
            );
        }
        decoded
            .join()
            .expect("the decode thread")
            .expect("decoding");
        std::fs::remove_file(&path).ok();

        assert_eq!(
            samples.len(),
            frames.len(),
            "every sample in the file should reach the publisher",
        );
        // Interleaved, so the left channel rises and the right one falls.
        let scale = f32::from(i16::MAX);
        for (index, pair) in samples.as_chunks::<2>().0.iter().enumerate() {
            let expected = index as f32 / scale;
            assert!(
                (pair[0] - expected).abs() < 1e-3 && (pair[1] + expected).abs() < 1e-3,
                "frame {index} decoded as {pair:?}, expected [{expected}, -{expected}]",
            );
        }
    }

    /// A pass that decodes nothing must not be retried, or the pacing sleep has
    /// nothing to slow it down and the thread spins on the file forever.
    #[test]
    fn a_file_with_no_samples_stops_instead_of_looping() {
        let path =
            std::env::temp_dir().join(format!("iroh-live-media-empty-{}.wav", std::process::id()));
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(&empty_wav()))
            .expect("write the test file");

        // On its own thread with a deadline, because the failure this guards
        // against is a loop that never returns rather than one that returns the
        // wrong thing.
        let (done, finished) = std::sync::mpsc::sync_channel(1);
        let looping = path.clone();
        std::thread::spawn(move || {
            let (fanout, rx) = tokio::sync::broadcast::channel(16);
            let result = decode_loop(&looping, true, &fanout, &CancellationToken::new());
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
