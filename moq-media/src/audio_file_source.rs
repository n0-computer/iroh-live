//! Audio file import via symphonia (pure-Rust decoders + rubato resampler).
//!
//! Decodes WAV, MP3, and FLAC files to 48 kHz stereo f32 PCM. No runtime
//! dependency on ffmpeg.

pub use crate::audio_file_symphonia::AudioFileSource;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::traits::AudioSource;

    /// Verifies that audio file import produces samples from a generated WAV.
    ///
    /// The test generates a short 48 kHz mono WAV with a 440 Hz sine wave,
    /// imports it through whichever backend is active, and confirms that
    /// non-zero samples come out at the expected format.
    #[test]
    fn audio_file_import_produces_samples() {
        let tmp_dir = std::env::temp_dir().join("iroh-live-test-audio-import");
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let wav_path = tmp_dir.join("test_sine.wav");
        write_test_wav(&wav_path);

        let mut source = AudioFileSource::new(&wav_path, false).unwrap();
        assert_eq!(source.format().sample_rate, 48_000);
        assert_eq!(source.format().channel_count, 2);

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
                Err(_) => break,
            }
        }

        assert!(
            total_frames > 0,
            "expected audio samples from import, got none"
        );

        let has_nonzero = buf.iter().any(|&s| s.abs() > 1e-6);
        assert!(
            has_nonzero,
            "all samples are zero — expected sine wave audio"
        );

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    /// Writes a minimal 48 kHz mono 16-bit PCM WAV file with a 440 Hz sine wave.
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
        buf.extend_from_slice(&16u32.to_le_bytes());
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
