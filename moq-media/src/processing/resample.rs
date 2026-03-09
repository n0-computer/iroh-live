use anyhow::Result;
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Audio resampler wrapping rubato.
///
/// If input and output rates match, samples pass through without processing.
#[derive(Debug)]
pub(crate) struct Resampler {
    inner: Option<Async<f32>>,
    channels: usize,
}

impl Resampler {
    /// Create a new resampler. If `from_rate == to_rate`, no resampling is performed.
    pub(crate) fn new(from_rate: u32, to_rate: u32, channels: u32) -> Result<Self> {
        let channels = channels as usize;
        let inner = if from_rate == to_rate {
            None
        } else {
            let ratio = to_rate as f64 / from_rate as f64;
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };
            let chunk_size = 1024;
            Some(Async::new_sinc(
                ratio,
                1.1,
                &params,
                chunk_size,
                channels,
                FixedAsync::Input,
            )?)
        };
        Ok(Self { inner, channels })
    }

    /// Resample interleaved f32 samples. Returns resampled interleaved data.
    /// If rates match, returns a copy of the input.
    pub(crate) fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        let Some(resampler) = &mut self.inner else {
            return Ok(input.to_vec());
        };

        let frames = input.len() / self.channels;
        if frames == 0 {
            return Ok(Vec::new());
        }

        let input_adapter = InterleavedSlice::new(input, self.channels, frames)?;
        let result = resampler.process(&input_adapter, 0, None)?;
        Ok(result.take_data())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_resample() {
        let mut resampler = Resampler::new(48000, 48000, 1).unwrap();
        let input: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0).sin()).collect();
        let output = resampler.process(&input).unwrap();
        assert_eq!(output.len(), input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn downsample_48k_to_16k() {
        let mut resampler = Resampler::new(48000, 16000, 1).unwrap();
        // 1024 samples at 48kHz
        let input: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.01).sin()).collect();
        let output = resampler.process(&input).unwrap();
        // Output should be roughly 1/3 the length (16k/48k)
        let expected_len = (1024.0_f64 * 16000.0 / 48000.0).round() as usize;
        let tolerance = expected_len / 5; // rubato may buffer
        assert!(
            output.len().abs_diff(expected_len) < tolerance,
            "output len {} not near expected {expected_len}",
            output.len()
        );
    }

    #[test]
    fn upsample_16k_to_48k() {
        let mut resampler = Resampler::new(16000, 48000, 1).unwrap();
        let input: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.03).sin()).collect();
        let output = resampler.process(&input).unwrap();
        // Output should be roughly 3x the length
        let expected_len = (1024.0_f64 * 48000.0 / 16000.0).round() as usize;
        let tolerance = expected_len / 5;
        assert!(
            output.len().abs_diff(expected_len) < tolerance,
            "output len {} not near expected {expected_len}",
            output.len()
        );
    }

    #[test]
    fn stereo_resample() {
        let mut resampler = Resampler::new(48000, 24000, 2).unwrap();
        // 1024 frames * 2 channels = 2048 samples
        let input: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.01).sin()).collect();
        let output = resampler.process(&input).unwrap();
        // Output should have even number of samples (stereo)
        assert_eq!(output.len() % 2, 0, "stereo output should have even length");
    }

    #[test]
    fn empty_input() {
        let mut resampler = Resampler::new(48000, 16000, 1).unwrap();
        let output = resampler.process(&[]).unwrap();
        assert!(output.is_empty());
    }
}
