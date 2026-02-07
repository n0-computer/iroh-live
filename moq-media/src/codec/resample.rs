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
    from_rate: u32,
    to_rate: u32,
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
        Ok(Self {
            inner,
            from_rate,
            to_rate,
            channels,
        })
    }

    /// Resample interleaved f32 samples. Returns resampled interleaved data.
    /// If rates match, returns a copy of the input.
    pub(crate) fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        let resampler = match &mut self.inner {
            None => return Ok(input.to_vec()),
            Some(r) => r,
        };

        let frames = input.len() / self.channels;
        if frames == 0 {
            return Ok(Vec::new());
        }

        let input_adapter = InterleavedSlice::new(input, self.channels, frames)?;
        let result = resampler.process(&input_adapter, 0, None)?;
        Ok(result.take_data())
    }

    pub(crate) fn from_rate(&self) -> u32 {
        self.from_rate
    }

    pub(crate) fn to_rate(&self) -> u32 {
        self.to_rate
    }
}
