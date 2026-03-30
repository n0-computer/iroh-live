//! Audio file import via ffmpeg transcode to raw PCM.
//!
//! Spawns ffmpeg as a child process to decode any supported audio format
//! (MP3, WAV, FLAC, OGG, AAC, etc.) into raw f32le PCM at 48 kHz stereo.
//! The output is buffered in a ring buffer that the synchronous
//! [`AudioSource`] trait can drain without blocking.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use tracing::{debug, error, info, warn};

use crate::{format::AudioFormat, traits::AudioSource};

/// Size of the ring buffer in f32 samples. 48 kHz * 2 channels * 2 seconds
/// gives enough headroom for ffmpeg read jitter without wasting memory.
const RING_BUFFER_SAMPLES: usize = 48_000 * 2 * 2;

/// Audio source that reads from a file via ffmpeg transcode.
///
/// Spawns an ffmpeg subprocess that decodes the input file into raw f32le
/// PCM (48 kHz, stereo) piped to stdout. A background thread reads from
/// stdout and fills a lock-free ring buffer. The [`AudioSource::pop_samples`]
/// implementation drains from the consumer side of that ring buffer.
///
/// Supports optional looping via ffmpeg's `-stream_loop -1` flag.
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
    /// Creates a new audio file source.
    ///
    /// Spawns ffmpeg immediately to start decoding. Returns an error if
    /// ffmpeg is not found or the file path is invalid.
    ///
    /// When `loop_playback` is true, ffmpeg loops the input indefinitely
    /// (`-stream_loop -1`). The ffmpeg process runs with `-re` to read
    /// at realtime speed, preventing the ring buffer from filling faster
    /// than the encoder can consume.
    pub fn new(path: impl AsRef<Path>, loop_playback: bool) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        anyhow::ensure!(path.exists(), "audio file not found: {}", path.display());

        let format = AudioFormat::stereo_48k();
        let rb = HeapRb::<f32>::new(RING_BUFFER_SAMPLES);
        let (producer, consumer) = rb.split();
        let eof = Arc::new(AtomicBool::new(false));

        let child = spawn_ffmpeg(&path, loop_playback)?;
        spawn_reader_thread(child, producer, eof.clone(), path.clone());

        info!(
            path = %path.display(),
            loop_playback,
            "audio file source started"
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
                // ffmpeg finished and buffer is drained — signal end.
                return Err(anyhow::anyhow!("audio file source ended"));
            }
            // Buffer temporarily empty but ffmpeg is still running.
            return Ok(None);
        }

        let channels = self.format.channel_count as usize;
        // Read whole frames only (stereo = 2 samples per frame).
        let max_frames = buf.len() / channels;
        let available_frames = available / channels;
        let frames_to_read = max_frames.min(available_frames);
        let samples_to_read = frames_to_read * channels;

        let n = self.consumer.pop_slice(&mut buf[..samples_to_read]);
        debug_assert_eq!(n, samples_to_read);

        Ok(Some(frames_to_read))
    }
}

/// Spawns the ffmpeg subprocess that decodes audio to raw f32le PCM.
fn spawn_ffmpeg(path: &Path, loop_playback: bool) -> anyhow::Result<std::process::Child> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);

    if loop_playback {
        cmd.args(["-stream_loop", "-1"]);
    }

    // -re reads at native framerate so we don't flood the ring buffer.
    cmd.args(["-re", "-i"]);
    cmd.arg(path.as_os_str());

    // Output raw f32le PCM at 48 kHz stereo to stdout.
    cmd.args([
        "-f",
        "f32le",
        "-acodec",
        "pcm_f32le",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-",
    ]);

    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!("failed to spawn ffmpeg for audio import — is ffmpeg installed? {e}")
        })?;

    Ok(child)
}

/// Background thread that reads raw PCM bytes from ffmpeg stdout and
/// pushes f32 samples into the ring buffer producer.
fn spawn_reader_thread(
    mut child: std::process::Child,
    mut producer: ringbuf::HeapProd<f32>,
    eof: Arc<AtomicBool>,
    path: PathBuf,
) {
    std::thread::Builder::new()
        .name("audio-file-read".into())
        .spawn(move || {
            let Some(stdout) = child.stdout.take() else {
                error!("ffmpeg stdout missing");
                eof.store(true, Ordering::Relaxed);
                return;
            };

            if let Err(e) = read_loop(stdout, &mut producer) {
                warn!(path = %path.display(), "audio file read error: {e:#}");
            }

            eof.store(true, Ordering::Relaxed);
            debug!(path = %path.display(), "audio file reader finished");

            // Reap the child process to avoid zombies.
            match child.wait() {
                Ok(status) if !status.success() => {
                    warn!(
                        code = ?status.code(),
                        path = %path.display(),
                        "ffmpeg exited with non-zero status"
                    );
                }
                Err(e) => {
                    warn!(path = %path.display(), "failed to wait on ffmpeg child: {e}");
                }
                Ok(_) => {}
            }
        })
        .expect("failed to spawn audio file reader thread");
}

/// Reads raw f32le bytes from ffmpeg stdout and pushes samples into
/// the ring buffer. Sleeps briefly when the buffer is full.
fn read_loop(
    mut stdout: std::process::ChildStdout,
    producer: &mut ringbuf::HeapProd<f32>,
) -> anyhow::Result<()> {
    use std::io::Read;

    // Read in 20ms chunks to match the encoder tick rate.
    // 48000 Hz * 2 channels * 20ms = 1920 samples = 7680 bytes.
    const CHUNK_SAMPLES: usize = 48_000 * 2 * 20 / 1000;
    let mut byte_buf = vec![0u8; CHUNK_SAMPLES * 4]; // f32 = 4 bytes

    loop {
        let n = stdout.read(&mut byte_buf)?;
        if n == 0 {
            break; // EOF
        }

        // Truncate to whole f32 samples (4-byte aligned).
        let sample_bytes = n - (n % 4);
        let samples: &[f32] = bytemuck_cast_slice(&byte_buf[..sample_bytes]);

        // Push samples into the ring buffer, spinning briefly if full.
        let mut offset = 0;
        while offset < samples.len() {
            let pushed = producer.push_slice(&samples[offset..]);
            offset += pushed;
            if pushed == 0 && offset < samples.len() {
                // Ring buffer full — wait a bit for the encoder to drain.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    Ok(())
}

/// Reinterprets a byte slice as f32 samples (little-endian on LE platforms).
///
/// This is safe because ffmpeg outputs native-endian f32 and we run on
/// little-endian platforms. The slice length must be a multiple of 4.
fn bytemuck_cast_slice(bytes: &[u8]) -> &[f32] {
    assert!(bytes.len().is_multiple_of(4));
    // SAFETY: f32 and [u8; 4] have the same size and alignment requirements
    // are met. Vec<u8> guarantees at least 1-byte alignment, but since the
    // allocation comes from the global allocator with size >= 4, it will be
    // at least 4-byte aligned in practice. We additionally assert alignment.
    let ptr = bytes.as_ptr();
    assert!(
        ptr.align_offset(std::mem::align_of::<f32>()) == 0,
        "byte buffer is not f32-aligned"
    );
    unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), bytes.len() / 4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that ffmpeg-based audio file import produces samples.
    ///
    /// Generates a short WAV file with a sine wave, imports it through
    /// the ffmpeg pipeline, and checks that samples come out. Skips
    /// gracefully if ffmpeg is not installed.
    #[test]
    fn audio_file_import_produces_samples() {
        // Check ffmpeg availability first.
        let ffmpeg_check = std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if ffmpeg_check.is_err() || !ffmpeg_check.unwrap().success() {
            eprintln!("skipping test: ffmpeg not available");
            return;
        }

        // Generate a minimal WAV file with a 440 Hz sine wave (0.5s, mono, 48 kHz).
        let tmp_dir = std::env::temp_dir().join("iroh-live-test-audio-import");
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let wav_path = tmp_dir.join("test_sine.wav");
        write_test_wav(&wav_path);

        let mut source = AudioFileSource::new(&wav_path, false).unwrap();
        assert_eq!(source.format().sample_rate, 48_000);
        assert_eq!(source.format().channel_count, 2);

        // Wait for ffmpeg to produce some data (up to 2 seconds).
        let mut total_frames = 0usize;
        let mut buf = vec![0.0f32; 1920]; // 20ms at 48kHz stereo
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);

        while std::time::Instant::now() < deadline {
            match source.pop_samples(&mut buf) {
                Ok(Some(frames)) => {
                    total_frames += frames;
                    if total_frames > 960 {
                        break;
                    }
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break, // EOF
            }
        }

        assert!(
            total_frames > 0,
            "expected audio samples from ffmpeg import, got none"
        );

        // Verify that at least some samples are non-zero (sine wave content).
        let has_nonzero = buf.iter().any(|&s| s.abs() > 1e-6);
        assert!(
            has_nonzero,
            "all samples are zero — expected sine wave audio"
        );

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    /// Writes a minimal 48 kHz mono WAV file with a 440 Hz sine wave.
    fn write_test_wav(path: &Path) {
        let sample_rate = 48_000u32;
        let duration_samples = sample_rate / 2; // 0.5 seconds
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
        let block_align = channels * (bits_per_sample / 8);
        let data_size = duration_samples * u32::from(block_align);

        let mut buf = Vec::with_capacity(44 + data_size as usize);

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data_size).to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());

        for i in 0..duration_samples {
            let t = i as f64 / sample_rate as f64;
            let sample = (t * 440.0 * std::f64::consts::TAU).sin() * 0.5;
            let pcm = (sample * i16::MAX as f64) as i16;
            buf.extend_from_slice(&pcm.to_le_bytes());
        }

        std::fs::write(path, buf).unwrap();
    }
}
