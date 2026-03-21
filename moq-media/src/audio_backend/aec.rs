//! Acoustic echo cancellation via sonora.
//!
//! Two components:
//!
//! - [`AecProcessor`] wraps `sonora::AudioProcessing` with enable/disable
//!   control. Independent of any audio backend — takes 10ms frames of f32
//!   samples and returns processed frames.
//!
//! - [`AecState`] accumulates samples from the cpal callbacks into 10ms
//!   frames, calls the processor, and drains the output. All processing
//!   is serialized on the input callback thread to avoid Mutex contention
//!   between separate cpal input and output callbacks.

pub(super) use self::{
    processor::{AecProcessor, AecProcessorConfig},
    state::AecState,
};

mod processor {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use sonora::{
        AudioProcessing, Config, StreamConfig,
        config::{EchoCanceller, NoiseSuppression, NoiseSuppressionLevel},
    };
    use tracing::{debug, info};

    #[derive(Debug, Clone)]
    pub(crate) struct AecProcessorConfig {
        /// Stream config for the capture (microphone) side.
        pub capture_config: StreamConfig,
        /// Stream config for the render (playback) side.
        pub render_config: StreamConfig,
    }

    impl Default for AecProcessorConfig {
        fn default() -> Self {
            Self {
                capture_config: StreamConfig::new(48000, 2),
                render_config: StreamConfig::new(48000, 2),
            }
        }
    }

    impl AecProcessorConfig {
        pub(crate) fn new(
            sample_rate_hz: u32,
            capture_channels: u16,
            render_channels: u16,
        ) -> Self {
            Self {
                capture_config: StreamConfig::new(sample_rate_hz, capture_channels),
                render_config: StreamConfig::new(sample_rate_hz, render_channels),
            }
        }

        /// Computes the number of samples per channel for a 10ms frame at the given sample rate.
        pub(crate) fn frame_size(sample_rate_hz: u32) -> usize {
            (sample_rate_hz / 100) as usize
        }
    }

    #[derive(Clone, Debug)]
    pub(crate) struct AecProcessor(Arc<Inner>);

    #[derive(derive_more::Debug)]
    struct Inner {
        #[debug("Processor")]
        processor: Mutex<AudioProcessing>,
        enabled: AtomicBool,
    }

    impl Default for AecProcessor {
        fn default() -> Self {
            Self::new(Default::default(), true).expect("failed to initialize AecProcessor")
        }
    }

    impl AecProcessor {
        pub(crate) fn new(config: AecProcessorConfig, enabled: bool) -> anyhow::Result<Self> {
            let processor_config = Config {
                echo_canceller: Some(EchoCanceller::default()),
                noise_suppression: Some(NoiseSuppression {
                    level: NoiseSuppressionLevel::High,
                    ..Default::default()
                }),
                ..Config::default()
            };

            let processor = AudioProcessing::builder()
                .capture_config(config.capture_config)
                .render_config(config.render_config)
                .config(processor_config)
                .build();

            info!("init audio processor (config={config:?})");
            Ok(Self(Arc::new(Inner {
                processor: Mutex::new(processor),
                enabled: AtomicBool::new(enabled),
            })))
        }

        pub(crate) fn is_enabled(&self) -> bool {
            self.0.enabled.load(Ordering::Relaxed)
        }

        #[allow(unused, reason = "API reserved for future use")]
        pub(crate) fn set_enabled(&self, enabled: bool) {
            self.0.enabled.store(enabled, Ordering::Relaxed);
        }

        /// Processes a capture (microphone) audio frame with explicit stream configs.
        ///
        /// Each element of `src` and `dest` is one channel of 10ms audio samples.
        /// Values should be in `[-1.0, 1.0]`.
        pub(crate) fn process_capture_f32(
            &self,
            src: &[&[f32]],
            dest: &mut [&mut [f32]],
            input_config: &StreamConfig,
            output_config: &StreamConfig,
        ) -> Result<(), sonora::Error> {
            if !self.is_enabled() {
                for (d, s) in dest.iter_mut().zip(src.iter()) {
                    d.copy_from_slice(s);
                }
                return Ok(());
            }
            self.0
                .processor
                .lock()
                .expect("poisoned")
                .process_capture_f32_with_config(src, input_config, output_config, dest)
        }

        /// Processes a render (playback) audio frame with explicit stream configs.
        ///
        /// Each element of `src` and `dest` is one channel of 10ms audio samples.
        /// Values should be in `[-1.0, 1.0]`.
        pub(crate) fn process_render_f32(
            &self,
            src: &[&[f32]],
            dest: &mut [&mut [f32]],
            input_config: &StreamConfig,
            output_config: &StreamConfig,
        ) -> Result<(), sonora::Error> {
            if !self.is_enabled() {
                for (d, s) in dest.iter_mut().zip(src.iter()) {
                    d.copy_from_slice(s);
                }
                return Ok(());
            }
            self.0
                .processor
                .lock()
                .expect("poisoned")
                .process_render_f32_with_config(src, input_config, output_config, dest)
        }

        #[allow(unused, reason = "API reserved for future use")]
        pub(crate) fn set_stream_delay(&self, delay_ms: u32) {
            debug!("updating stream delay to {delay_ms}ms");
            let _ = self
                .0
                .processor
                .lock()
                .expect("poisoned")
                .set_stream_delay_ms(delay_ms as i32);
        }
    }
}

mod state {
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use ringbuf::traits::{Consumer, Observer};
    use sonora::StreamConfig;

    use super::processor::{AecProcessor, AecProcessorConfig};

    /// Pre-allocated capacity for AEC accumulation buffers.
    /// Covers pathological callback sizes (4096 frames) without reallocation.
    const BUF_CAPACITY: usize = 8192;

    /// Callback-based AEC processing state.
    ///
    /// Owned exclusively by the input callback. The output callback writes
    /// its mixed stereo signal into the render reference ring buffer
    /// (lock-free SPSC); the input callback drains it, processes render
    /// frames through sonora, then processes capture frames — all on a
    /// single thread with no Mutex contention.
    pub(crate) struct AecState {
        processor: AecProcessor,
        enabled: Arc<AtomicBool>,
        stream_config: StreamConfig,
        frame_size: usize,

        /// Render reference consumer (from output callback).
        render_ref_cons: ringbuf::HeapCons<f32>,

        /// Render accumulation buffers (per-channel).
        render_buf_l: VecDeque<f32>,
        render_buf_r: VecDeque<f32>,

        /// Capture accumulation buffers (per-channel).
        capture_buf_l: VecDeque<f32>,
        capture_buf_r: VecDeque<f32>,

        /// Capture output ring (processed samples waiting to be read).
        out_buf_l: VecDeque<f32>,
        out_buf_r: VecDeque<f32>,

        /// Pre-allocated temporary buffers for sonora frame processing.
        tmp_src_l: Vec<f32>,
        tmp_src_r: Vec<f32>,
        tmp_dst_l: Vec<f32>,
        tmp_dst_r: Vec<f32>,

        /// Interleaved scratch buffer for reading from render reference ring.
        render_scratch: Vec<f32>,
    }

    impl AecState {
        /// Creates a new AEC callback state.
        ///
        /// - `processor`: shared sonora wrapper.
        /// - `render_ref_cons`: consumer end of the SPSC ring buffer that the
        ///   output callback writes mixed stereo into.
        /// - `enabled`: shared flag for dynamic AEC enable/disable.
        pub(crate) fn new(
            processor: AecProcessor,
            render_ref_cons: ringbuf::HeapCons<f32>,
            enabled: Arc<AtomicBool>,
        ) -> Self {
            let frame_size = AecProcessorConfig::frame_size(super::super::INTERNAL_RATE);
            Self {
                processor,
                enabled,
                stream_config: StreamConfig::new(super::super::INTERNAL_RATE, 2),
                frame_size,
                render_ref_cons,
                render_buf_l: VecDeque::with_capacity(BUF_CAPACITY),
                render_buf_r: VecDeque::with_capacity(BUF_CAPACITY),
                capture_buf_l: VecDeque::with_capacity(BUF_CAPACITY),
                capture_buf_r: VecDeque::with_capacity(BUF_CAPACITY),
                out_buf_l: VecDeque::with_capacity(BUF_CAPACITY),
                out_buf_r: VecDeque::with_capacity(BUF_CAPACITY),
                tmp_src_l: vec![0.0; frame_size],
                tmp_src_r: vec![0.0; frame_size],
                tmp_dst_l: vec![0.0; frame_size],
                tmp_dst_r: vec![0.0; frame_size],
                render_scratch: vec![0.0; BUF_CAPACITY * 2],
            }
        }

        /// Processes stereo interleaved audio in-place.
        ///
        /// 1. Drains the render reference ring buffer and processes render
        ///    frames through sonora.
        /// 2. Pushes the input (capture) samples into accumulation buffers.
        /// 3. Processes complete 10ms capture frames through sonora.
        /// 4. Writes processed capture samples back into the buffer in-place.
        ///
        /// Called from the input callback. No allocation occurs.
        pub(crate) fn process_stereo_interleaved(&mut self, buf: &mut [f32], num_frames: usize) {
            let enabled = self.enabled.load(Ordering::Relaxed);

            // 1. Drain render reference ring buffer into per-channel render buffers.
            let available = self.render_ref_cons.occupied_len();
            if available > 0 {
                let to_read = available.min(self.render_scratch.len());
                let read = self
                    .render_ref_cons
                    .pop_slice(&mut self.render_scratch[..to_read]);
                let render_frames = read / 2;
                for i in 0..render_frames {
                    self.render_buf_l.push_back(self.render_scratch[i * 2]);
                    self.render_buf_r.push_back(self.render_scratch[i * 2 + 1]);
                }
            }

            // 2. Process complete 10ms render frames.
            if enabled {
                while self.render_buf_l.len() >= self.frame_size {
                    for i in 0..self.frame_size {
                        // While-guard ensures >= frame_size samples, but RT audio
                        // callbacks must never panic — output silence on underrun.
                        self.tmp_src_l[i] = self.render_buf_l.pop_front().unwrap_or(0.0);
                        self.tmp_src_r[i] = self.render_buf_r.pop_front().unwrap_or(0.0);
                    }
                    // We discard the render output — sonora only needs it internally
                    // to build its echo model.
                    let src: &[&[f32]] = &[&self.tmp_src_l, &self.tmp_src_r];
                    let dst: &mut [&mut [f32]] = &mut [&mut self.tmp_dst_l, &mut self.tmp_dst_r];
                    let _ = self.processor.process_render_f32(
                        src,
                        dst,
                        &self.stream_config,
                        &self.stream_config,
                    );
                }
            } else {
                // When disabled, still drain the buffers to prevent unbounded growth.
                while self.render_buf_l.len() >= self.frame_size {
                    self.render_buf_l.drain(..self.frame_size);
                    self.render_buf_r.drain(..self.frame_size);
                }
            }

            // 3. Push capture samples into per-channel buffers.
            for i in 0..num_frames {
                self.capture_buf_l.push_back(buf[i * 2]);
                self.capture_buf_r.push_back(buf[i * 2 + 1]);
            }

            // 4. Process complete 10ms capture frames through sonora.
            // out_buf is NOT cleared here — residual processed samples from the
            // previous callback carry over. Only the frames consumed in step 5
            // are drained (via pop_front). This prevents sample loss when the
            // callback buffer size doesn't divide evenly by the 10ms frame size.

            while self.capture_buf_l.len() >= self.frame_size {
                for i in 0..self.frame_size {
                    self.tmp_src_l[i] = self.capture_buf_l.pop_front().unwrap_or(0.0);
                    self.tmp_src_r[i] = self.capture_buf_r.pop_front().unwrap_or(0.0);
                }

                if enabled {
                    let src: &[&[f32]] = &[&self.tmp_src_l, &self.tmp_src_r];
                    let dst: &mut [&mut [f32]] = &mut [&mut self.tmp_dst_l, &mut self.tmp_dst_r];
                    let _ = self.processor.process_capture_f32(
                        src,
                        dst,
                        &self.stream_config,
                        &self.stream_config,
                    );
                    for i in 0..self.frame_size {
                        self.out_buf_l.push_back(self.tmp_dst_l[i]);
                        self.out_buf_r.push_back(self.tmp_dst_r[i]);
                    }
                } else {
                    for i in 0..self.frame_size {
                        self.out_buf_l.push_back(self.tmp_src_l[i]);
                        self.out_buf_r.push_back(self.tmp_src_r[i]);
                    }
                }
            }

            // 5. Write processed output back into the interleaved buffer.
            //    If fewer processed frames are available than input frames
            //    (residual from frame accumulation), output silence for the
            //    remainder.
            for i in 0..num_frames {
                match (self.out_buf_l.pop_front(), self.out_buf_r.pop_front()) {
                    (Some(l), Some(r)) => {
                        buf[i * 2] = l;
                        buf[i * 2 + 1] = r;
                    }
                    _ => {
                        buf[i * 2] = 0.0;
                        buf[i * 2 + 1] = 0.0;
                    }
                }
            }
        }
    }
}
