pub(super) use self::{
    firewheel_nodes::{AecCaptureNode, AecRenderNode},
    processor::{AecProcessor, AecProcessorConfig},
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

        /// Compute the number of samples per channel for a 10ms frame at a given sample rate.
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
            self.0.enabled.load(Ordering::SeqCst)
        }

        #[allow(unused, reason = "API reserved for future use")]
        pub(crate) fn set_enabled(&self, enabled: bool) {
            let _prev = self.0.enabled.swap(enabled, Ordering::SeqCst);
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
    use sonora::StreamConfig;

    use super::{AecProcessor, AecProcessorConfig};

    /// Render-side node: feeds output audio into the AEC render stream.
    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub(crate) struct AecRenderNode {
        pub enabled: bool,
    }

    impl Default for AecRenderNode {
        fn default() -> Self {
            Self { enabled: true }
        }
    }

    impl AudioNode for AecRenderNode {
        type Configuration = AecProcessor;

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("aec_render")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::STEREO,
                    num_outputs: ChannelCount::STEREO,
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext<'_>,
        ) -> impl AudioNodeProcessor {
            // Use default 48kHz until new_stream provides the actual rate.
            let frame_size = AecProcessorConfig::frame_size(48000);
            let stream_config = StreamConfig::new(48000, 2);
            RenderProcessor {
                enabled: self.enabled,
                processor: config.clone(),
                frame_size,
                stream_config,
                in_ring_l: VecDeque::with_capacity(frame_size * 4),
                in_ring_r: VecDeque::with_capacity(frame_size * 4),
                out_ring_l: VecDeque::with_capacity(frame_size * 4),
                out_ring_r: VecDeque::with_capacity(frame_size * 4),
                tmp_src_l: vec![0.0; frame_size],
                tmp_src_r: vec![0.0; frame_size],
                tmp_dest_l: vec![0.0; frame_size],
                tmp_dest_r: vec![0.0; frame_size],
            }
        }
    }

    struct RenderProcessor {
        enabled: bool,
        processor: AecProcessor,
        frame_size: usize,
        stream_config: StreamConfig,
        in_ring_l: VecDeque<f32>,
        in_ring_r: VecDeque<f32>,
        out_ring_l: VecDeque<f32>,
        out_ring_r: VecDeque<f32>,
        tmp_src_l: Vec<f32>,
        tmp_src_r: Vec<f32>,
        tmp_dest_l: Vec<f32>,
        tmp_dest_r: Vec<f32>,
    }

    impl RenderProcessor {
        fn reconfigure(&mut self, sample_rate: u32) {
            let frame_size = AecProcessorConfig::frame_size(sample_rate);
            self.frame_size = frame_size;
            self.stream_config = StreamConfig::new(sample_rate, 2);
            self.in_ring_l.clear();
            self.in_ring_r.clear();
            self.out_ring_l.clear();
            self.out_ring_r.clear();
            self.tmp_src_l.resize(frame_size, 0.0);
            self.tmp_src_r.resize(frame_size, 0.0);
            self.tmp_dest_l.resize(frame_size, 0.0);
            self.tmp_dest_r.resize(frame_size, 0.0);
        }
    }

    impl AudioNodeProcessor for RenderProcessor {
        fn process(
            &mut self,
            info: &ProcInfo,
            buffers: ProcBuffers<'_, '_>,
            events: &mut ProcEvents<'_>,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            for patch in events.drain_patches::<AecRenderNode>() {
                match patch {
                    AecRenderNodePatch::Enabled(enabled) => {
                        self.enabled = enabled;
                        if !self.enabled {
                            self.in_ring_l.clear();
                            self.in_ring_r.clear();
                            self.out_ring_l.clear();
                            self.out_ring_r.clear();
                        }
                    }
                }
            }

            let num_frames = info.frames;

            let in_l = &buffers.inputs[0][..num_frames];
            let in_r = &buffers.inputs[1][..num_frames];

            let (out_l, out_rest) = buffers.outputs.split_first_mut().unwrap();
            let out_l = &mut out_l[..num_frames];
            let out_r = &mut out_rest[0][..num_frames];

            if !self.enabled {
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
                return ProcessStatus::OutputsModified;
            }

            // 1. Push input samples into per-channel ring buffers.
            for i in 0..num_frames {
                self.in_ring_l.push_back(in_l[i]);
                self.in_ring_r.push_back(in_r[i]);
            }

            // 2. Process full 10ms frames.
            let frame_size = self.frame_size;
            while self.in_ring_l.len() >= frame_size {
                for i in 0..frame_size {
                    self.tmp_src_l[i] = self.in_ring_l.pop_front().unwrap();
                    self.tmp_src_r[i] = self.in_ring_r.pop_front().unwrap();
                }

                let src: &[&[f32]] = &[&self.tmp_src_l, &self.tmp_src_r];
                let mut dest_l = self.tmp_dest_l.as_mut_slice();
                let mut dest_r = self.tmp_dest_r.as_mut_slice();
                let dest: &mut [&mut [f32]] = &mut [&mut dest_l, &mut dest_r];
                let _ = self.processor.process_render_f32(
                    src,
                    dest,
                    &self.stream_config,
                    &self.stream_config,
                );

                for i in 0..frame_size {
                    self.out_ring_l.push_back(self.tmp_dest_l[i]);
                    self.out_ring_r.push_back(self.tmp_dest_r[i]);
                }
            }

            // 3. Produce outputs.
            for i in 0..num_frames {
                if !self.out_ring_l.is_empty() {
                    out_l[i] = self.out_ring_l.pop_front().unwrap();
                    out_r[i] = self.out_ring_r.pop_front().unwrap();
                } else {
                    out_l[i] = 0.0;
                    out_r[i] = 0.0;
                }
            }

            ProcessStatus::OutputsModified
        }

        fn new_stream(&mut self, stream_info: &StreamInfo, _ctx: &mut ProcStreamCtx<'_>) {
            self.reconfigure(stream_info.sample_rate.get());
        }
    }

    /// Capture-side node: feeds mic audio into the AEC capture stream.
    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub(crate) struct AecCaptureNode {
        pub enabled: bool,
    }

    impl Default for AecCaptureNode {
        fn default() -> Self {
            Self { enabled: true }
        }
    }

    impl AudioNode for AecCaptureNode {
        type Configuration = AecProcessor;

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("aec_capture")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::STEREO,
                    num_outputs: ChannelCount::STEREO,
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext<'_>,
        ) -> impl AudioNodeProcessor {
            let frame_size = AecProcessorConfig::frame_size(48000);
            let stream_config = StreamConfig::new(48000, 2);
            CaptureProcessor {
                enabled: self.enabled,
                processor: config.clone(),
                frame_size,
                stream_config,
                in_ring_l: VecDeque::with_capacity(frame_size * 4),
                in_ring_r: VecDeque::with_capacity(frame_size * 4),
                out_ring_l: VecDeque::with_capacity(frame_size * 4),
                out_ring_r: VecDeque::with_capacity(frame_size * 4),
                tmp_src_l: vec![0.0; frame_size],
                tmp_src_r: vec![0.0; frame_size],
                tmp_dest_l: vec![0.0; frame_size],
                tmp_dest_r: vec![0.0; frame_size],
            }
        }
    }

    struct CaptureProcessor {
        enabled: bool,
        processor: AecProcessor,
        frame_size: usize,
        stream_config: StreamConfig,
        in_ring_l: VecDeque<f32>,
        in_ring_r: VecDeque<f32>,
        out_ring_l: VecDeque<f32>,
        out_ring_r: VecDeque<f32>,
        tmp_src_l: Vec<f32>,
        tmp_src_r: Vec<f32>,
        tmp_dest_l: Vec<f32>,
        tmp_dest_r: Vec<f32>,
    }

    impl CaptureProcessor {
        fn reconfigure(&mut self, sample_rate: u32) {
            let frame_size = AecProcessorConfig::frame_size(sample_rate);
            self.frame_size = frame_size;
            self.stream_config = StreamConfig::new(sample_rate, 2);
            self.in_ring_l.clear();
            self.in_ring_r.clear();
            self.out_ring_l.clear();
            self.out_ring_r.clear();
            self.tmp_src_l.resize(frame_size, 0.0);
            self.tmp_src_r.resize(frame_size, 0.0);
            self.tmp_dest_l.resize(frame_size, 0.0);
            self.tmp_dest_r.resize(frame_size, 0.0);
        }
    }

    impl AudioNodeProcessor for CaptureProcessor {
        fn process(
            &mut self,
            info: &ProcInfo,
            buffers: ProcBuffers<'_, '_>,
            events: &mut ProcEvents<'_>,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            for patch in events.drain_patches::<AecCaptureNode>() {
                match patch {
                    AecCaptureNodePatch::Enabled(enabled) => {
                        self.enabled = enabled;
                        if !self.enabled {
                            self.in_ring_l.clear();
                            self.in_ring_r.clear();
                            self.out_ring_l.clear();
                            self.out_ring_r.clear();
                        }
                    }
                }
            }

            let num_frames = info.frames;

            let in_l = &buffers.inputs[0][..num_frames];
            let in_r = &buffers.inputs[1][..num_frames];

            let (out_l, out_rest) = buffers.outputs.split_first_mut().unwrap();
            let out_l = &mut out_l[..num_frames];
            let out_r = &mut out_rest[0][..num_frames];

            if !self.enabled {
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
                return ProcessStatus::OutputsModified;
            }

            // 1. Push input samples into per-channel ring buffers.
            for i in 0..num_frames {
                self.in_ring_l.push_back(in_l[i]);
                self.in_ring_r.push_back(in_r[i]);
            }

            // 2. Process full 10ms frames.
            let frame_size = self.frame_size;
            while self.in_ring_l.len() >= frame_size {
                for i in 0..frame_size {
                    self.tmp_src_l[i] = self.in_ring_l.pop_front().unwrap();
                    self.tmp_src_r[i] = self.in_ring_r.pop_front().unwrap();
                }

                let src: &[&[f32]] = &[&self.tmp_src_l, &self.tmp_src_r];
                let mut dest_l = self.tmp_dest_l.as_mut_slice();
                let mut dest_r = self.tmp_dest_r.as_mut_slice();
                let dest: &mut [&mut [f32]] = &mut [&mut dest_l, &mut dest_r];
                let _ = self.processor.process_capture_f32(
                    src,
                    dest,
                    &self.stream_config,
                    &self.stream_config,
                );

                for i in 0..frame_size {
                    self.out_ring_l.push_back(self.tmp_dest_l[i]);
                    self.out_ring_r.push_back(self.tmp_dest_r[i]);
                }
            }

            // 3. Produce outputs.
            for i in 0..num_frames {
                if !self.out_ring_l.is_empty() {
                    out_l[i] = self.out_ring_l.pop_front().unwrap();
                    out_r[i] = self.out_ring_r.pop_front().unwrap();
                } else {
                    out_l[i] = 0.0;
                    out_r[i] = 0.0;
                }
            }

            ProcessStatus::OutputsModified
        }

        fn new_stream(&mut self, stream_info: &StreamInfo, _ctx: &mut ProcStreamCtx<'_>) {
            self.reconfigure(stream_info.sample_rate.get());
        }
    }
}
