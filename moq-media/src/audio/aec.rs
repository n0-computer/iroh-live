pub use self::{
    firewheel_nodes::{AecCaptureNode, AecRenderNode},
    processor::{AecProcessor, AecProcessorConfig},
};

mod processor {
    use std::{
        num::NonZeroU32,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use anyhow::Result;
    use tracing::{debug, info};
    use webrtc_audio_processing::{
        Config, EchoCancellation, EchoCancellationSuppressionLevel, InitializationConfig,
    };

    #[derive(Debug, Clone)]
    pub struct AecProcessorConfig {
        pub num_input_channels: NonZeroU32,
        pub num_output_channels: NonZeroU32,
    }

    impl Default for AecProcessorConfig {
        fn default() -> Self {
            Self {
                num_input_channels: 2.try_into().unwrap(),
                num_output_channels: 2.try_into().unwrap(),
            }
        }
    }

    impl AecProcessorConfig {
        pub fn stereo_in_out() -> Self {
            Self::default()
        }
    }

    #[derive(Clone, Debug)]
    pub struct AecProcessor(Arc<Inner>);

    #[derive(derive_more::Debug)]
    struct Inner {
        #[debug("Processor")]
        processor: Mutex<webrtc_audio_processing::Processor>,
        config: Mutex<Config>,
        // capture_delay: AtomicU64,
        // playback_delay: AtomicU64,
        enabled: AtomicBool,
        // capture_channels: AtomicUsize,
        // playback_channels: AtomicUsize,
    }

    impl Default for AecProcessor {
        fn default() -> Self {
            Self::new(Default::default(), true).expect("failed to initialize AecProcessor")
        }
    }

    impl AecProcessor {
        pub fn new(config: AecProcessorConfig, enabled: bool) -> anyhow::Result<Self> {
            let suppression_level = EchoCancellationSuppressionLevel::High;
            // High pass filter is a prerequisite to running echo cancellation.
            let processor_config = Config {
                echo_cancellation: Some(EchoCancellation {
                    suppression_level,
                    stream_delay_ms: None,
                    enable_delay_agnostic: true,
                    enable_extended_filter: true,
                }),
                enable_high_pass_filter: true,
                ..Config::default()
            };

            let mut processor = webrtc_audio_processing::Processor::new(&InitializationConfig {
                num_capture_channels: config.num_input_channels.get() as i32,
                num_render_channels: config.num_output_channels.get() as i32,
                enable_experimental_agc: true,
                enable_intelligibility_enhancer: true, // ..InitializationConfig::default()
            })?;
            processor.set_config(processor_config.clone());

            // processor.set_config(config.clone());
            info!("init audio processor (config={config:?})");
            Ok(Self(Arc::new(Inner {
                processor: Mutex::new(processor),
                config: Mutex::new(processor_config),
                enabled: AtomicBool::new(enabled),
            })))
        }

        pub fn is_enabled(&self) -> bool {
            self.0.enabled.load(Ordering::SeqCst)
        }

        #[allow(unused)]
        pub fn set_enabled(&self, enabled: bool) {
            let _prev = self.0.enabled.swap(enabled, Ordering::SeqCst);
        }

        /// Processes and modifies the audio frame from a capture device by applying
        /// signal processing as specified in the config. `frame` should hold an
        /// interleaved f32 audio frame, with [`NUM_SAMPLES_PER_FRAME`] samples.
        // webrtc-audio-processing expects a 10ms chunk for each process call.
        pub fn process_capture_frame(
            &self,
            frame: &mut [f32],
        ) -> Result<(), webrtc_audio_processing::Error> {
            if !self.is_enabled() {
                return Ok(());
            }
            self.0
                .processor
                .lock()
                .expect("poisoned")
                .process_capture_frame(frame)
        }

        /// Processes and optionally modifies the audio frame from a playback device.
        /// `frame` should hold an interleaved `f32` audio frame, with
        /// [`NUM_SAMPLES_PER_FRAME`] samples.
        pub fn process_render_frame(
            &self,
            frame: &mut [f32],
        ) -> Result<(), webrtc_audio_processing::Error> {
            if !self.is_enabled() {
                return Ok(());
            }
            self.0
                .processor
                .lock()
                .expect("poisoned")
                .process_render_frame(frame)
        }

        pub fn set_stream_delay(&self, delay_ms: u32) {
            debug!("updating stream delay to {delay_ms}ms");
            // let playback = self.0.playback_delay.load(Ordering::Relaxed);
            // let capture = self.0.capture_delay.load(Ordering::Relaxed);
            // let total = playback + capture;
            let mut config = self.0.config.lock().expect("poisoned");
            config.echo_cancellation.as_mut().unwrap().stream_delay_ms = Some(delay_ms as i32);
            self.0
                .processor
                .lock()
                .expect("poisoned")
                .set_config(config.clone());
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
    use webrtc_audio_processing::NUM_SAMPLES_PER_FRAME;

    use super::AecProcessor;

    const CHANNELS: usize = 2;
    const FRAME_SAMPLES: usize = (NUM_SAMPLES_PER_FRAME as usize) * CHANNELS;

    /// Simple render-side node: feeds output audio into WebRTC's render stream.
    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub struct AecRenderNode {
        pub enabled: bool,
    }

    impl Default for AecRenderNode {
        fn default() -> Self {
            Self { enabled: true }
        }
    }

    impl AudioNode for AecRenderNode {
        /// We use the wrapped WebRTC processor as our configuration object.
        ///
        /// Note: `WebrtcAudioProcessor` already internally wraps an `Arc<Inner>`,
        /// so cloning this config shares the underlying processor between nodes.
        type Configuration = AecProcessor;

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("webrtc_render")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::STEREO,
                    num_outputs: ChannelCount::STEREO,
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> impl AudioNodeProcessor {
            // Clone = share the same underlying Arc<Inner>.
            let webrtc = config.clone();

            // Inform the processor how many playback channels we have.
            // (You can handle errors here instead of unwrap() in real code.)
            // webrtc.init_playback(CHANNELS).ok();

            RenderProcessor {
                enabled: self.enabled,
                processor: webrtc,
                in_ring: VecDeque::with_capacity(FRAME_SAMPLES * 4),
                out_ring: VecDeque::with_capacity(FRAME_SAMPLES * 4),
                tmp_chunk: vec![0.0; FRAME_SAMPLES],
            }
        }
    }

    struct RenderProcessor {
        enabled: bool,
        processor: AecProcessor,
        // Interleaved input samples to be fed into WebRTC in 10ms chunks.
        in_ring: VecDeque<f32>,
        // Interleaved processed samples coming back from WebRTC.
        out_ring: VecDeque<f32>,
        // Scratch buffer for one NUM_SAMPLES_PER_FRAME chunk (interleaved).
        tmp_chunk: Vec<f32>,
    }

    impl AudioNodeProcessor for RenderProcessor {
        fn process(
            &mut self,
            info: &ProcInfo,
            buffers: ProcBuffers,
            events: &mut ProcEvents,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            // Handle parameter patches.
            for patch in events.drain_patches::<AecRenderNode>() {
                match patch {
                    AecRenderNodePatch::Enabled(enabled) => {
                        self.enabled = enabled;
                        if !self.enabled {
                            // Clear any buffered state when disabling to avoid stale audio.
                            self.in_ring.clear();
                            self.out_ring.clear();
                        }
                    }
                }
            }

            let num_frames = info.frames as usize;
            // println!("num_frames: {num_frames}");

            // Get input/output slices like in the FilterNode example.
            let in_l = &buffers.inputs[0][..num_frames];
            let in_r = &buffers.inputs[1][..num_frames];

            let (out_l, out_rest) = buffers.outputs.split_first_mut().unwrap();
            let out_l = &mut out_l[..num_frames];
            let out_r = &mut out_rest[0][..num_frames];

            // If disabled, just pass through.
            if !self.enabled {
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
                return ProcessStatus::OutputsModified;
            }

            // 1. Push current block into the interleaved input ring buffer.
            for i in 0..num_frames {
                self.in_ring.push_back(in_l[i]);
                self.in_ring.push_back(in_r[i]);
            }

            // 2. While we have at least one full 10ms frame, process it.
            while self.in_ring.len() >= FRAME_SAMPLES {
                // Fill tmp_chunk with a full frame of interleaved samples.
                for s in &mut self.tmp_chunk[..FRAME_SAMPLES] {
                    *s = self.in_ring.pop_front().unwrap();
                }

                // Feed into processor render stream.
                let _ = self.processor.process_render_frame(&mut self.tmp_chunk);

                // Store processed samples into the output ring.
                for &s in &self.tmp_chunk[..FRAME_SAMPLES] {
                    self.out_ring.push_back(s);
                }
            }

            // 3. Produce outputs for this audio block.
            //
            // We always need `num_frames * CHANNELS` samples. If we don't have
            // enough processed samples yet, we output silence for the missing part.
            for i in 0..num_frames {
                if self.out_ring.len() >= CHANNELS {
                    out_l[i] = self.out_ring.pop_front().unwrap();
                    out_r[i] = self.out_ring.pop_front().unwrap();
                } else {
                    // Not enough processed data yet -> output silence.
                    out_l[i] = 0.0;
                    out_r[i] = 0.0;
                }
            }

            ProcessStatus::OutputsModified
        }

        fn new_stream(&mut self, _stream_info: &StreamInfo, _ctx: &mut ProcStreamCtx) {
            // Reset buffers for new stream.
            self.in_ring.clear();
            self.out_ring.clear();
        }
    }

    /// Capture-side node: feeds mic audio into [`AecProcessor`]'s capture stream.
    #[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
    pub struct AecCaptureNode {
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
                .debug_name("webrtc_capture")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::STEREO,
                    num_outputs: ChannelCount::STEREO,
                })
        }

        fn construct_processor(
            &self,
            config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> impl AudioNodeProcessor {
            CaptureProcessor {
                enabled: self.enabled,
                processor: config.clone(),
                in_ring: VecDeque::with_capacity(FRAME_SAMPLES * 4),
                out_ring: VecDeque::with_capacity(FRAME_SAMPLES * 4),
                tmp_chunk: vec![0.0; FRAME_SAMPLES],
            }
        }
    }

    struct CaptureProcessor {
        enabled: bool,
        processor: AecProcessor,
        // Interleaved input samples to be fed into WebRTC in 10ms chunks.
        in_ring: VecDeque<f32>,
        // Interleaved processed samples coming back from WebRTC.
        out_ring: VecDeque<f32>,
        // Scratch buffer for one NUM_SAMPLES_PER_FRAME chunk (interleaved).
        tmp_chunk: Vec<f32>,
    }

    impl AudioNodeProcessor for CaptureProcessor {
        fn process(
            &mut self,
            info: &ProcInfo,
            buffers: ProcBuffers,
            events: &mut ProcEvents,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            for patch in events.drain_patches::<AecCaptureNode>() {
                match patch {
                    AecCaptureNodePatch::Enabled(enabled) => {
                        self.enabled = enabled;
                        if !self.enabled {
                            self.in_ring.clear();
                            self.out_ring.clear();
                        }
                    }
                }
            }

            let frames = info.frames;
            let num_frames = frames as usize;

            let in_l = &buffers.inputs[0][..num_frames];
            let in_r = &buffers.inputs[1][..num_frames];

            let (out_l, out_rest) = buffers.outputs.split_first_mut().unwrap();
            let out_l = &mut out_l[..num_frames];
            let out_r = &mut out_rest[0][..num_frames];

            if !self.enabled {
                // Bypass if disabled.
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
                return ProcessStatus::OutputsModified;
            }

            // 1. Push current block into the interleaved input ring buffer.
            for i in 0..num_frames {
                self.in_ring.push_back(in_l[i]);
                self.in_ring.push_back(in_r[i]);
            }

            // 2. While we have at least one full 10ms frame, process it.
            while self.in_ring.len() >= FRAME_SAMPLES {
                for s in &mut self.tmp_chunk[..FRAME_SAMPLES] {
                    *s = self.in_ring.pop_front().unwrap();
                }

                let _ = self.processor.process_capture_frame(&mut self.tmp_chunk);

                for &s in &self.tmp_chunk[..FRAME_SAMPLES] {
                    self.out_ring.push_back(s);
                }
            }

            // 3. Produce outputs for this audio block.
            //
            // If we don't have enough processed samples to cover the whole block,
            // we output silence for the missing frames.
            for i in 0..num_frames {
                if self.out_ring.len() >= CHANNELS {
                    out_l[i] = self.out_ring.pop_front().unwrap();
                    out_r[i] = self.out_ring.pop_front().unwrap();
                } else {
                    out_l[i] = 0.0;
                    out_r[i] = 0.0;
                }
            }

            ProcessStatus::OutputsModified
        }

        fn new_stream(&mut self, _stream_info: &StreamInfo, _ctx: &mut ProcStreamCtx) {
            // Reset state for new stream.
            self.in_ring.clear();
            self.out_ring.clear();
        }
    }
}
