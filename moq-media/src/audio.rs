use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use firewheel::{
    CpalBackend, CpalConfig, CpalInputConfig, CpalOutputConfig, FirewheelConfig, FirewheelContext,
    backend::AudioBackend as FirewheelAudioBackend,
    channel_config::{ChannelConfig, ChannelCount, NonZeroChannelCount},
    dsp::volume::{DEFAULT_DB_EPSILON, DbMeterNormalizer},
    graph::PortIdx,
    node::NodeID,
    nodes::{
        peak_meter::{PeakMeterNode, PeakMeterSmoother, PeakMeterState},
        stream::{
            ResamplingChannelConfig,
            reader::{StreamReaderConfig, StreamReaderNode, StreamReaderState},
            writer::{StreamWriterConfig, StreamWriterNode, StreamWriterState},
        },
    },
};
use tokio::sync::{mpsc, mpsc::error::TryRecvError, oneshot};
use tracing::{debug, error, info, trace, warn};

use self::aec::{AecCaptureNode, AecProcessor, AecProcessorConfig, AecRenderNode};
use crate::{
    av::{AudioFormat, AudioSink, AudioSinkHandle, AudioSource},
    util::spawn_thread,
};

mod aec;

type StreamWriterHandle = Arc<Mutex<StreamWriterState>>;
type StreamReaderHandle = Arc<Mutex<StreamReaderState>>;

/// Information about an available audio input device.
#[derive(Debug, Clone)]
pub struct AudioInputInfo {
    /// The device name (used to select it).
    pub name: String,
    /// Whether this is the system default input device.
    pub is_default: bool,
}

/// List available audio input devices.
pub fn list_audio_inputs() -> Vec<AudioInputInfo> {
    <CpalBackend as FirewheelAudioBackend>::available_input_devices()
        .into_iter()
        .map(|d| AudioInputInfo {
            name: d.name,
            is_default: d.is_default,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct AudioBackend {
    tx: mpsc::Sender<DriverMessage>,
}

impl Default for AudioBackend {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AudioBackend {
    pub fn new(input_device: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let _handle = spawn_thread("audiodriver", move || {
            AudioDriver::new(rx, input_device).run()
        });
        Self { tx }
    }

    pub async fn default_input(&self) -> Result<InputStream> {
        self.input(AudioFormat::mono_48k()).await
    }

    pub async fn input(&self, format: AudioFormat) -> Result<InputStream> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::InputStream { format, reply })
            .await?;
        let handle = reply_rx.await??;
        Ok(InputStream { handle, format })
    }

    pub async fn default_output(&self) -> Result<OutputStream> {
        self.output(AudioFormat::stereo_48k()).await
    }

    pub async fn output(&self, format: AudioFormat) -> Result<OutputStream> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::OutputStream { format, reply })
            .await?;
        let handle = reply_rx.await??;
        Ok(handle)
    }
}

#[derive(Clone, derive_more::Debug)]
pub struct OutputStream {
    #[debug(skip)]
    handle: StreamWriterHandle,
    paused: Arc<AtomicBool>,
    #[debug(skip)]
    peaks: Arc<Mutex<PeakMeterSmoother<2>>>,
    #[debug(skip)]
    normalizer: DbMeterNormalizer,
}

impl AudioSinkHandle for OutputStream {
    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
    fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
        self.handle.lock().expect("poisoned").pause_stream();
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
        self.handle.lock().expect("poisoned").resume();
    }

    fn toggle_pause(&self) {
        let was_paused = self.paused.fetch_xor(true, Ordering::Relaxed);
        if was_paused {
            self.handle.lock().expect("poisoned").resume();
        } else {
            self.handle.lock().expect("poisoned").pause_stream();
        }
    }

    fn smoothed_peak_normalized(&self) -> Option<f32> {
        Some(
            self.peaks
                .lock()
                .expect("poisoned")
                .smoothed_peaks_normalized_mono(&self.normalizer),
        )
    }
}

impl AudioSink for OutputStream {
    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(self.clone())
    }

    fn format(&self) -> Result<AudioFormat> {
        let info = self.handle.lock().expect("poisoned");
        let sample_rate = info
            .sample_rate()
            .context("output stream misses sample rate")?
            .get();
        let channel_count = info.num_channels().get().get();
        Ok(AudioFormat {
            sample_rate,
            channel_count,
        })
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        let mut handle = self.handle.lock().unwrap();

        // If this happens excessively in Release mode, you may want to consider
        // increasing [`StreamWriterConfig::channel_config.latency_seconds`].
        if handle.underflow_occurred() {
            warn!("Underflow occured in stream writer node!");
        }

        // If this happens excessively in Release mode, you may want to consider
        // increasing [`StreamWriterConfig::channel_config.capacity_seconds`]. For
        // example, if you are streaming data from a network, you may want to
        // increase the capacity to several seconds.
        if handle.overflow_occurred() {
            warn!("Overflow occured in stream writer node!");
        }

        // Wait until the node's processor is ready to receive data.
        if handle.is_ready() {
            // let expected_bytes =
            //     frame.samples() * frame.channels() as usize * core::mem::size_of::<f32>();
            // let cpal_sample_data: &[f32] = bytemuck::cast_slice(&frame.data(0)[..expected_bytes]);
            handle.push_interleaved(samples);
            trace!("pushed samples {}", samples.len());
        } else {
            warn!("output handle is inactive")
        }
        Ok(())
    }
}

impl OutputStream {
    #[allow(unused, reason = "may be used in future")]
    pub fn is_active(&self) -> bool {
        self.handle.lock().expect("poisoned").is_active()
    }
}

/// A simple AudioSource that reads from the default microphone via Firewheel.
#[derive(Clone, derive_more::Debug)]
pub struct InputStream {
    #[debug(skip)]
    handle: StreamReaderHandle,
    format: AudioFormat,
}

impl AudioSource for InputStream {
    fn cloned_boxed(&self) -> Box<dyn AudioSource> {
        Box::new(self.clone())
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>> {
        use firewheel::nodes::stream::ReadStatus;
        let mut handle = self.handle.lock().expect("poisoned");
        match handle.read_interleaved(buf) {
            Some(ReadStatus::Ok) => Ok(Some(buf.len())),
            Some(ReadStatus::InputNotReady) => {
                tracing::warn!("audio input not ready");
                // Maintain pacing; still return a frame-sized buffer
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::UnderflowOccurred { num_frames_read }) => {
                tracing::warn!(
                    "audio input underflow: {} frames missing",
                    buf.len() - num_frames_read
                );
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::OverflowCorrected {
                num_frames_discarded,
            }) => {
                tracing::warn!("audio input overflow: {num_frames_discarded} frames discarded");
                Ok(Some(buf.len()))
            }
            None => {
                tracing::warn!("audio input stream is inactive");
                Ok(None)
            }
        }
    }
}

#[derive(derive_more::Debug)]
enum DriverMessage {
    OutputStream {
        format: AudioFormat,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<OutputStream>>,
    },
    InputStream {
        format: AudioFormat,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<StreamReaderHandle>>,
    },
}

struct AudioDriver {
    cx: FirewheelContext,
    rx: mpsc::Receiver<DriverMessage>,
    aec_processor: AecProcessor,
    aec_render_node: NodeID,
    aec_capture_node: NodeID,
    peak_meters: HashMap<NodeID, Arc<Mutex<PeakMeterSmoother<2>>>>,
}

impl fmt::Debug for AudioDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioDriver")
            .field("aec_render_node", &self.aec_render_node)
            .field("aec_capture_node", &self.aec_capture_node)
            .finish_non_exhaustive()
    }
}

impl AudioDriver {
    fn new(rx: mpsc::Receiver<DriverMessage>, input_device: Option<String>) -> Self {
        let config = FirewheelConfig {
            num_graph_inputs: ChannelCount::new(1).unwrap(),
            ..Default::default()
        };
        let mut cx = FirewheelContext::new(config);
        info!("inputs: {:?}", cx.available_input_devices());
        info!("outputs: {:?}", cx.available_output_devices());

        let input_device_name = input_device.or({
            #[cfg(target_os = "linux")]
            {
                Some("pipewire".to_string())
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        });
        let config = CpalConfig {
            output: CpalOutputConfig {
                #[cfg(target_os = "linux")]
                device_name: Some("pipewire".to_string()),

                ..Default::default()
            },
            input: Some(CpalInputConfig {
                device_name: input_device_name,
                fail_on_no_input: true,
                ..Default::default()
            }),
        };
        cx.start_stream(config).unwrap();
        info!(
            "audio graph in: {:?}",
            cx.node_info(cx.graph_in_node_id()).map(|x| &x.info)
        );
        info!(
            "audio graph out: {:?}",
            cx.node_info(cx.graph_out_node_id()).map(|x| &x.info)
        );

        cx.set_graph_channel_config(ChannelConfig {
            num_inputs: ChannelCount::new(2).unwrap(),
            num_outputs: ChannelCount::new(2).unwrap(),
        });

        let aec_processor = AecProcessor::new(AecProcessorConfig::stereo_in_out(), true)
            .expect("failed to initialize AEC processor");
        let aec_render_node = cx.add_node(AecRenderNode::default(), Some(aec_processor.clone()));
        let aec_capture_node = cx.add_node(AecCaptureNode::default(), Some(aec_processor.clone()));

        let layout = &[(0, 0), (1, 1)];

        cx.connect(cx.graph_in_node_id(), aec_capture_node, layout, true)
            .unwrap();
        cx.connect(aec_render_node, cx.graph_out_node_id(), layout, true)
            .unwrap();

        Self {
            cx,
            rx,
            aec_processor,
            aec_render_node,
            aec_capture_node,
            peak_meters: Default::default(),
        }
    }

    fn run(&mut self) {
        const INTERVAL: Duration = Duration::from_millis(10);
        const PEAK_UPDATE_INTERVAL: Duration = Duration::from_millis(40);
        let mut last_delay: f64 = 0.;
        let mut last_peak_update = Instant::now();

        loop {
            let tick = Instant::now();
            if self.drain_messages().is_err() {
                info!("closing audio driver: message channel closed");
                break;
            }

            if let Err(e) = self.cx.update() {
                error!("audio backend error: {:?}", &e);

                // if let UpdateError::StreamStoppedUnexpectedly(_) = e {
                //     // Notify the stream node handles that the output stream has stopped.
                //     // This will automatically stop any active streams on the nodes.
                //     cx.node_state_mut::<StreamWriterState>(stream_writer_id)
                //         .unwrap()
                //         .stop_stream();
                //     cx.node_state_mut::<StreamReaderState>(stream_reader_id)
                //         .unwrap()
                //         .stop_stream();

                //     // The stream has stopped unexpectedly (i.e the user has
                //     // unplugged their headphones.)
                //     //
                //     // Typically you should start a new stream as soon as
                //     // possible to resume processing (event if it's a dummy
                //     // output device).
                //     //
                //     // In this example we just quit the application.
                //     break;
                // }
            }

            if let Some(info) = self.cx.stream_info() {
                let delay = info.input_to_output_latency_seconds;
                if (last_delay - delay).abs() > (1. / 1000.) {
                    let delay_ms = (delay * 1000.) as u32;
                    info!("update processor delay to {delay_ms}ms");
                    self.aec_processor.set_stream_delay(delay_ms);
                    last_delay = delay;
                }
            }

            // Update peak meters
            let delta = last_peak_update.elapsed();
            if delta > PEAK_UPDATE_INTERVAL {
                for (id, smoother) in self.peak_meters.iter_mut() {
                    smoother.lock().expect("poisoned").update(
                        self.cx
                            .node_state::<PeakMeterState<2>>(*id)
                            .unwrap()
                            .peak_gain_db(DEFAULT_DB_EPSILON),
                        delta.as_secs_f32(),
                    );
                }
                last_peak_update = Instant::now();
            }

            thread::sleep(INTERVAL.saturating_sub(tick.elapsed()));
        }
    }

    fn drain_messages(&mut self) -> Result<(), ()> {
        loop {
            match self.rx.try_recv() {
                Err(TryRecvError::Disconnected) => {
                    info!("stopping audio thread: backend handle dropped");
                    break Err(());
                }
                Err(TryRecvError::Empty) => {
                    break Ok(());
                }
                Ok(message) => self.handle_message(message),
            }
        }
    }

    fn handle_message(&mut self, message: DriverMessage) {
        debug!("handle {message:?}");
        match message {
            DriverMessage::OutputStream { format, reply } => {
                let res = self
                    .output_stream(format)
                    .inspect_err(|err| warn!("failed to create audio output stream: {err:#}"));
                reply.send(res).ok();
            }
            DriverMessage::InputStream { format, reply } => {
                let res = self
                    .input_stream(format)
                    .inspect_err(|err| warn!("failed to create audio input stream: {err:#}"));
                reply.send(res).ok();
            }
        }
    }

    fn output_stream(&mut self, format: AudioFormat) -> Result<OutputStream> {
        let channel_count = format.channel_count;
        let sample_rate = format.sample_rate;
        // setup stream
        let stream_writer_id = self.cx.add_node(
            StreamWriterNode,
            Some(StreamWriterConfig {
                channels: NonZeroChannelCount::new(channel_count)
                    .context("channel count may not be zero")?,
                ..Default::default()
            }),
        );
        let graph_out = self.aec_render_node;
        // let graph_out_info = self
        //     .cx
        //     .node_info(graph_out)
        //     .context("missing audio output node")?;

        let peak_meter_node = PeakMeterNode::<2> { enabled: true };
        let peak_meter_id = self.cx.add_node(peak_meter_node, None);
        let peak_meter_smoother =
            Arc::new(Mutex::new(PeakMeterSmoother::<2>::new(Default::default())));
        self.peak_meters
            .insert(peak_meter_id, peak_meter_smoother.clone());
        self.cx
            .connect(peak_meter_id, graph_out, &[(0, 0), (1, 1)], true)
            .unwrap();

        let layout: &[(PortIdx, PortIdx)] = match channel_count {
            0 => anyhow::bail!("audio stream has no channels"),
            1 => &[(0, 0), (0, 1)],
            _ => &[(0, 0), (1, 1)],
        };
        self.cx
            .connect(stream_writer_id, peak_meter_id, layout, false)
            .unwrap();
        let output_stream_sample_rate = self.cx.stream_info().unwrap().sample_rate;
        let event = self
            .cx
            .node_state_mut::<StreamWriterState>(stream_writer_id)
            .unwrap()
            .start_stream(
                sample_rate.try_into().unwrap(),
                output_stream_sample_rate,
                ResamplingChannelConfig {
                    capacity_seconds: 3.,
                    ..Default::default()
                },
            )
            .unwrap();
        info!("started output stream");
        self.cx.queue_event_for(stream_writer_id, event.into());
        // Wrap the handles in an `Arc<Mutex<T>>>` so that we can send them to other threads.
        let handle = self
            .cx
            .node_state::<StreamWriterState>(stream_writer_id)
            .unwrap()
            .handle();
        Ok(OutputStream {
            handle: Arc::new(handle),
            paused: Arc::new(AtomicBool::new(false)),
            peaks: peak_meter_smoother,
            normalizer: DbMeterNormalizer::new(-60., 0., -20.),
        })
    }

    fn input_stream(&mut self, format: AudioFormat) -> Result<StreamReaderHandle> {
        let sample_rate = format.sample_rate;
        let channel_count = format.channel_count;
        // Setup stream reader node
        let stream_reader_id = self.cx.add_node(
            StreamReaderNode,
            Some(StreamReaderConfig {
                channels: NonZeroChannelCount::new(channel_count)
                    .context("channel count may not be zero")?,
            }),
        );
        let graph_in_node_id = self.aec_capture_node;
        let graph_in_info = self
            .cx
            .node_info(graph_in_node_id)
            .context("missing audio input node")?;

        let layout: &[(PortIdx, PortIdx)] = match (
            graph_in_info.info.channel_config.num_outputs.get(),
            channel_count,
        ) {
            (0, _) => anyhow::bail!("audio input has no channels"),
            (1, 2) => &[(0, 0), (0, 1)],
            (2, 2) => &[(0, 0), (1, 1)],
            (_, 1) => &[(0, 0)],
            _ => &[(0, 0), (1, 1)],
        };
        self.cx
            .connect(graph_in_node_id, stream_reader_id, layout, false)
            .unwrap();

        let input_stream_sample_rate = self.cx.stream_info().unwrap().sample_rate;
        let event = self
            .cx
            .node_state_mut::<StreamReaderState>(stream_reader_id)
            .unwrap()
            .start_stream(
                sample_rate.try_into().unwrap(),
                input_stream_sample_rate,
                ResamplingChannelConfig {
                    capacity_seconds: 3.0,
                    ..Default::default()
                },
            )
            .unwrap();
        self.cx.queue_event_for(stream_reader_id, event.into());

        let handle = self
            .cx
            .node_state::<StreamReaderState>(stream_reader_id)
            .unwrap()
            .handle();
        Ok(Arc::new(handle))
    }
}
