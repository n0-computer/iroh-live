pub use self::{
    firewheel_nodes::{AecCaptureNode, AecRenderNode},
    processor::AecProcessor,
};

mod processor {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tracing::{debug, info};
    use webrtc_audio_processing::{
        Config,
        config::{EchoCanceller, HighPassFilter},
    };

    const SAMPLE_RATE_HZ: u32 = 48_000;

    #[derive(Clone, Debug)]
    pub struct AecProcessor(Arc<Inner>);

    #[derive(derive_more::Debug)]
    struct Inner {
        #[debug("Processor")]
        processor: webrtc_audio_processing::Processor,
        enabled: AtomicBool,
    }

    impl Default for AecProcessor {
        fn default() -> Self {
            Self::new(true).expect("failed to initialize AecProcessor")
        }
    }

    impl AecProcessor {
        pub fn new(enabled: bool) -> anyhow::Result<Self> {
            let processor = webrtc_audio_processing::Processor::new(SAMPLE_RATE_HZ)?;
            processor.set_config(Self::make_config(None));
            info!("init audio processor");
            Ok(Self(Arc::new(Inner {
                processor,
                enabled: AtomicBool::new(enabled),
            })))
        }

        fn make_config(stream_delay_ms: Option<u16>) -> Config {
            Config {
                echo_canceller: Some(EchoCanceller::Full { stream_delay_ms }),
                high_pass_filter: Some(HighPassFilter::default()),
                ..Config::default()
            }
        }

        pub fn is_enabled(&self) -> bool {
            self.0.enabled.load(Ordering::Relaxed)
        }

        #[allow(unused)]
        pub fn set_enabled(&self, enabled: bool) {
            self.0.enabled.store(enabled, Ordering::Relaxed);
        }

        /// Process a capture frame (1 or 2 channels).
        pub fn process_capture_frame(
            &self,
            channels: &mut [&mut [f32]],
        ) -> Result<(), webrtc_audio_processing::Error> {
            if self.is_enabled() {
                self.0.processor.process_capture_frame(channels)
            } else {
                Ok(())
            }
        }

        /// Process a render frame (1 or 2 channels).
        pub fn process_render_frame(
            &self,
            channels: &mut [&mut [f32]],
        ) -> Result<(), webrtc_audio_processing::Error> {
            if self.is_enabled() {
                self.0.processor.process_render_frame(channels)
            } else {
                Ok(())
            }
        }

        pub fn set_stream_delay(&self, delay_ms: u16) {
            debug!("updating stream delay to {delay_ms}ms");
            self.0.processor.set_config(Self::make_config(Some(delay_ms)));
        }
    }
}

mod firewheel_nodes {
    use std::collections::VecDeque;

    use firewheel::{
        StreamInfo,
        channel_config::{ChannelConfig, ChannelCount},
        diff::{Diff, Patch},
        event::ProcEvents,
        node::{
            AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, ProcBuffers,
            ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
        },
    };

    use super::AecProcessor;

    /// Number of samples per 10ms frame at 48kHz
    const SAMPLES_PER_FRAME: usize = 480;

    // ---- Render Node ----

    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub struct AecRenderNode {
        pub enabled: bool,
        pub stereo: bool,
    }

    impl Default for AecRenderNode {
        fn default() -> Self {
            Self { enabled: true, stereo: true }
        }
    }

    impl AecRenderNode {
        #[allow(unused)]
        pub fn mono() -> Self {
            Self { enabled: true, stereo: false }
        }

        #[allow(unused)]
        pub fn stereo() -> Self {
            Self::default()
        }

        fn channel_count(&self) -> ChannelCount {
            if self.stereo { ChannelCount::STEREO } else { ChannelCount::MONO }
        }
    }

    impl AudioNode for AecRenderNode {
        type Configuration = AecProcessor;

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("webrtc_render")
                .channel_config(ChannelConfig {
                    num_inputs: self.channel_count(),
                    num_outputs: self.channel_count(),
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> impl AudioNodeProcessor {
            AecNodeProcessor::new(config.clone(), self.enabled, self.stereo, false)
        }
    }

    // ---- Capture Node ----

    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub struct AecCaptureNode {
        pub enabled: bool,
        pub stereo: bool,
    }

    impl Default for AecCaptureNode {
        fn default() -> Self {
            Self { enabled: true, stereo: true }
        }
    }

    impl AecCaptureNode {
        #[allow(unused)]
        pub fn mono() -> Self {
            Self { enabled: true, stereo: false }
        }

        #[allow(unused)]
        pub fn stereo() -> Self {
            Self::default()
        }

        fn channel_count(&self) -> ChannelCount {
            if self.stereo { ChannelCount::STEREO } else { ChannelCount::MONO }
        }
    }

    impl AudioNode for AecCaptureNode {
        type Configuration = AecProcessor;

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("webrtc_capture")
                .channel_config(ChannelConfig {
                    num_inputs: self.channel_count(),
                    num_outputs: self.channel_count(),
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> impl AudioNodeProcessor {
            AecNodeProcessor::new(config.clone(), self.enabled, self.stereo, true)
        }
    }

    // ---- Shared Processor ----

    struct AecNodeProcessor {
        processor: AecProcessor,
        enabled: bool,
        is_capture: bool,
        num_channels: usize,
        input: Vec<VecDeque<f32>>,
        output: Vec<VecDeque<f32>>,
        tmp: Vec<Vec<f32>>,
    }

    impl AecNodeProcessor {
        fn new(processor: AecProcessor, enabled: bool, stereo: bool, is_capture: bool) -> Self {
            let n = if stereo { 2 } else { 1 };
            Self {
                processor,
                enabled,
                is_capture,
                num_channels: n,
                input: vec![VecDeque::with_capacity(SAMPLES_PER_FRAME * 4); n],
                output: vec![VecDeque::with_capacity(SAMPLES_PER_FRAME * 4); n],
                tmp: vec![vec![0.0; SAMPLES_PER_FRAME]; n],
            }
        }

        fn clear(&mut self) {
            for ring in &mut self.input { ring.clear(); }
            for ring in &mut self.output { ring.clear(); }
        }
    }

    impl AudioNodeProcessor for AecNodeProcessor {
        fn process(
            &mut self,
            info: &ProcInfo,
            buffers: ProcBuffers,
            events: &mut ProcEvents,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            // Handle patches
            if self.is_capture {
                for patch in events.drain_patches::<AecCaptureNode>() {
                    if let AecCaptureNodePatch::Enabled(v) = patch {
                        self.enabled = v;
                        if !v { self.clear(); }
                    }
                }
            } else {
                for patch in events.drain_patches::<AecRenderNode>() {
                    if let AecRenderNodePatch::Enabled(v) = patch {
                        self.enabled = v;
                        if !v { self.clear(); }
                    }
                }
            }

            let n = info.frames as usize;

            // Bypass if disabled
            if !self.enabled {
                for ch in 0..self.num_channels {
                    buffers.outputs[ch][..n].copy_from_slice(&buffers.inputs[ch][..n]);
                }
                return ProcessStatus::OutputsModified;
            }

            // Push input samples
            for ch in 0..self.num_channels {
                self.input[ch].extend(&buffers.inputs[ch][..n]);
            }

            // Process complete 10ms frames
            while self.input[0].len() >= SAMPLES_PER_FRAME {
                // Drain into tmp buffers
                for ch in 0..self.num_channels {
                    for i in 0..SAMPLES_PER_FRAME {
                        self.tmp[ch][i] = self.input[ch].pop_front().unwrap();
                    }
                }

                // Build slice of mutable slices for the processor
                let mut slices: Vec<&mut [f32]> =
                    self.tmp.iter_mut().map(|v| v.as_mut_slice()).collect();
                let _ = if self.is_capture {
                    self.processor.process_capture_frame(&mut slices)
                } else {
                    self.processor.process_render_frame(&mut slices)
                };

                // Push to output buffers
                for ch in 0..self.num_channels {
                    self.output[ch].extend(&self.tmp[ch]);
                }
            }

            // Pop to output (silence if not enough samples)
            for ch in 0..self.num_channels {
                for i in 0..n {
                    buffers.outputs[ch][i] = self.output[ch].pop_front().unwrap_or(0.0);
                }
            }

            ProcessStatus::OutputsModified
        }

        fn new_stream(&mut self, _stream_info: &StreamInfo, _ctx: &mut ProcStreamCtx) {
            self.clear();
        }
    }
}
