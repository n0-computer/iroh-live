use std::{
    collections::HashMap,
    fmt,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use firewheel::{
    FirewheelConfig, FirewheelContext,
    backend::AudioBackend as FirewheelAudioBackend,
    channel_config::{ChannelConfig, ChannelCount, NonZeroChannelCount},
    cpal::{CpalBackend, CpalConfig, CpalInputConfig, CpalOutputConfig, cpal},
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
    format::AudioFormat,
    traits::{AudioSink, AudioSinkHandle, AudioSource},
    util::spawn_thread,
};

mod aec;

type StreamWriterHandle = Arc<Mutex<StreamWriterState>>;
type StreamReaderHandle = Arc<Mutex<StreamReaderState>>;

/// Options for configuring the [`AudioBackend`].
#[derive(Debug, Clone)]
pub struct AudioBackendOpts {
    /// Initial input device. `None` = system default.
    pub input_device: Option<DeviceId>,
    /// Initial output device. `None` = system default.
    pub output_device: Option<DeviceId>,
    /// When a device disconnects unexpectedly, automatically switch to
    /// the system default device. Default: `true`.
    pub fallback_to_default: bool,
}

impl Default for AudioBackendOpts {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            fallback_to_default: true,
        }
    }
}

/// Identifier for an audio device
#[derive(Debug, Clone, derive_more::Display, Eq, PartialEq)]
pub struct DeviceId(cpal::DeviceId);

impl DeviceId {
    fn to_cpal(&self) -> (cpal::HostId, cpal::DeviceId) {
        (self.0.0, self.0.clone())
    }
}

impl FromStr for DeviceId {
    type Err = anyhow::Error;

    fn from_str(id: &str) -> Result<Self> {
        let id = cpal::DeviceId::from_str(id)?;
        Ok(Self(id))
    }
}

/// Information about an available audio device.
#[derive(Debug, Clone, derive_more::Display)]
#[display("{name}{}", is_default.then(|| " (default)").unwrap_or_default())]
pub struct AudioDevice {
    /// The device id
    pub id: DeviceId,
    /// Human-readable device name
    pub name: String,
    /// Whether this is the system default input device.
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct AudioBackend {
    tx: mpsc::Sender<DriverMessage>,
}

impl Default for AudioBackend {
    fn default() -> Self {
        Self::new(AudioBackendOpts::default())
    }
}

impl AudioBackend {
    /// List available audio input devices.
    pub fn list_inputs() -> Vec<AudioDevice> {
        let mut out = Vec::new();
        let enumerator = CpalBackend::enumerator();
        for (i, host_id) in enumerator.available_hosts().into_iter().enumerate() {
            if let Ok(host) = enumerator.get_host(host_id) {
                for (j, device) in host.input_devices().into_iter().enumerate() {
                    let id = DeviceId(device.id);
                    out.push(AudioDevice {
                        name: device.name.unwrap_or_else(|| id.to_string()),
                        id,
                        is_default: i == 0 && j == 0,
                    })
                }
            }
        }
        out
    }

    /// List available audio output devices.
    pub fn list_outputs() -> Vec<AudioDevice> {
        let mut out = Vec::new();
        let enumerator = CpalBackend::enumerator();
        for (i, host_id) in enumerator.available_hosts().into_iter().enumerate() {
            if let Ok(host) = enumerator.get_host(host_id) {
                for (j, device) in host.output_devices().into_iter().enumerate() {
                    let id = DeviceId(device.id);
                    out.push(AudioDevice {
                        name: device.name.unwrap_or_else(|| id.to_string()),
                        id,
                        is_default: i == 0 && j == 0,
                    })
                }
            }
        }
        out
    }

    pub fn new(opts: AudioBackendOpts) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let _handle = spawn_thread("audiodriver", move || AudioDriver::new(rx, opts).run());
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
        Ok(InputStream {
            handle,
            format,
            inactive_count: 0,
        })
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

    /// Switch the input device. `None` = system default.
    pub async fn switch_input(&self, device: Option<DeviceId>) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: Some(device),
                output_device: None,
                reply,
            })
            .await?;
        reply_rx.await?
    }

    /// Switch the output device. `None` = system default.
    pub async fn switch_output(&self, device: Option<DeviceId>) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: None,
                output_device: Some(device),
                reply,
            })
            .await?;
        reply_rx.await?
    }

    /// Switch both input and output devices at once. `None` = system default.
    pub async fn switch_devices(
        &self,
        input: Option<DeviceId>,
        output: Option<DeviceId>,
    ) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: Some(input),
                output_device: Some(output),
                reply,
            })
            .await?;
        reply_rx.await?
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

    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(self.clone())
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
    /// Count of consecutive inactive reads, used to rate-limit warnings.
    #[debug(skip)]
    inactive_count: u32,
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
            Some(ReadStatus::Ok) => {
                if self.inactive_count > 0 {
                    info!(
                        "audio input stream became active after {} inactive reads",
                        self.inactive_count
                    );
                    self.inactive_count = 0;
                }
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::InputNotReady) => {
                tracing::warn!("audio input not ready");
                self.inactive_count = 0;
                // Maintain pacing; still return a frame-sized buffer
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::UnderflowOccurred { num_frames_read }) => {
                tracing::warn!(
                    "audio input underflow: {} frames missing",
                    buf.len() - num_frames_read
                );
                self.inactive_count = 0;
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::OverflowCorrected {
                num_frames_discarded,
            }) => {
                tracing::warn!("audio input overflow: {num_frames_discarded} frames discarded");
                self.inactive_count = 0;
                Ok(Some(buf.len()))
            }
            None => {
                self.inactive_count += 1;
                // Log at warn level on the first occurrence, then every 250
                // reads (~5s at 20ms intervals) to avoid flooding the log.
                if self.inactive_count == 1 {
                    tracing::warn!("audio input stream is inactive");
                } else if self.inactive_count.is_multiple_of(250) {
                    tracing::warn!(
                        "audio input stream still inactive ({} reads)",
                        self.inactive_count,
                    );
                }
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
    SwitchDevice {
        /// `None` = don't change, `Some(None)` = system default, `Some(Some(id))` = specific device.
        input_device: Option<Option<DeviceId>>,
        /// `None` = don't change, `Some(None)` = system default, `Some(Some(id))` = specific device.
        output_device: Option<Option<DeviceId>>,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<()>>,
    },
}

struct TrackedInputStream {
    node_id: NodeID,
    format: AudioFormat,
    handle: StreamReaderHandle,
}

struct TrackedOutputStream {
    node_id: NodeID,
    format: AudioFormat,
    handle: StreamWriterHandle,
}

struct AudioDriver {
    cx: FirewheelContext,
    rx: mpsc::Receiver<DriverMessage>,
    aec_processor: AecProcessor,
    aec_render_node: NodeID,
    aec_capture_node: NodeID,
    peak_meters: HashMap<NodeID, Arc<Mutex<PeakMeterSmoother<2>>>>,
    opts: AudioBackendOpts,
    current_input_device: Option<DeviceId>,
    current_output_device: Option<DeviceId>,
    input_streams: Vec<TrackedInputStream>,
    output_streams: Vec<TrackedOutputStream>,
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
    fn new(rx: mpsc::Receiver<DriverMessage>, opts: AudioBackendOpts) -> Self {
        let config = FirewheelConfig {
            num_graph_inputs: ChannelCount::new(1).unwrap(),
            ..Default::default()
        };
        let mut cx = FirewheelContext::new(config);

        let (input_device, output_device) = Self::resolve_devices(&opts);
        let cpal_config = Self::build_cpal_config(&input_device, &output_device);
        cx.start_stream(cpal_config).unwrap();
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

        let stream_sample_rate = cx.stream_info().unwrap().sample_rate.get();
        let aec_config = AecProcessorConfig::new(stream_sample_rate, 2, 2);
        let aec_processor =
            AecProcessor::new(aec_config, true).expect("failed to initialize AEC processor");
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
            opts,
            current_input_device: input_device,
            current_output_device: output_device,
            input_streams: Vec::new(),
            output_streams: Vec::new(),
        }
    }

    /// Apply platform-specific device fallbacks (Linux pipewire).
    fn resolve_devices(opts: &AudioBackendOpts) -> (Option<DeviceId>, Option<DeviceId>) {
        let input_device = opts.input_device.clone();
        let output_device = opts.output_device.clone();

        #[cfg(target_os = "linux")]
        let input_device = input_device.or_else(|| DeviceId::from_str("alsa:pipewire").ok());
        #[cfg(target_os = "linux")]
        let output_device = output_device.or_else(|| DeviceId::from_str("alsa:pipewire").ok());

        (input_device, output_device)
    }

    /// Build a [`CpalConfig`] from resolved device IDs.
    fn build_cpal_config(
        input_device: &Option<DeviceId>,
        output_device: &Option<DeviceId>,
    ) -> CpalConfig {
        let (output_host, output_id) = match output_device {
            Some(device) => {
                let (host, device) = device.to_cpal();
                (Some(host), Some(device))
            }
            None => (None, None),
        };
        let (input_host, input_id) = match input_device {
            Some(device) => {
                let (host, device) = device.to_cpal();
                (Some(host), Some(device))
            }
            None => (None, None),
        };
        CpalConfig {
            output: CpalOutputConfig {
                host: output_host,
                device_id: output_id,
                ..Default::default()
            },
            input: Some(CpalInputConfig {
                host: input_host,
                device_id: input_id,
                fail_on_no_input: true,
                ..Default::default()
            }),
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
            DriverMessage::SwitchDevice {
                input_device,
                output_device,
                reply,
            } => {
                let input = match input_device {
                    Some(dev) => dev,
                    None => self.opts.input_device.clone(),
                };
                let output = match output_device {
                    Some(dev) => dev,
                    None => self.opts.output_device.clone(),
                };
                let res = self.switch_device_internal(input, output);
                reply.send(res).ok();
            }
        }
    }

    /// Stop the current CPAL stream, start a new one with the given devices, and
    /// re-activate all tracked streams — swapping fresh handles into consumers.
    fn switch_device_internal(
        &mut self,
        input_device: Option<DeviceId>,
        output_device: Option<DeviceId>,
    ) -> Result<()> {
        info!(
            "switching audio devices: input={:?}, output={:?}",
            input_device, output_device
        );

        // 1. Stop the CPAL stream.
        self.cx.stop_stream();

        // 2. Wait for the processor to be returned from the audio thread.
        while !self.cx.can_start_stream() {
            thread::sleep(Duration::from_millis(1));
        }

        // 3. Rebuild CPAL config with new devices (applying platform fallbacks).
        let opts = AudioBackendOpts {
            input_device: input_device.clone(),
            output_device: output_device.clone(),
            ..self.opts.clone()
        };
        let (resolved_input, resolved_output) = Self::resolve_devices(&opts);
        let cpal_config = Self::build_cpal_config(&resolved_input, &resolved_output);

        // 4. Start new CPAL stream.
        self.cx
            .start_stream(cpal_config)
            .context("failed to start audio stream with new device")?;

        let stream_sample_rate = self.cx.stream_info().unwrap().sample_rate;

        // 5. Re-activate output streams and swap handles.
        for tracked in &self.output_streams {
            let event = self
                .cx
                .node_state_mut::<StreamWriterState>(tracked.node_id)
                .unwrap()
                .start_stream(
                    tracked.format.sample_rate.try_into().unwrap(),
                    stream_sample_rate,
                    ResamplingChannelConfig {
                        capacity_seconds: 3.,
                        ..Default::default()
                    },
                )
                .unwrap();
            self.cx.queue_event_for(tracked.node_id, event.into());
            let fresh = self
                .cx
                .node_state::<StreamWriterState>(tracked.node_id)
                .unwrap()
                .handle();
            *tracked.handle.lock().unwrap() = fresh.into_inner().unwrap();
        }

        // 6. Re-activate input streams and swap handles.
        for tracked in &self.input_streams {
            let event = self
                .cx
                .node_state_mut::<StreamReaderState>(tracked.node_id)
                .unwrap()
                .start_stream(
                    tracked.format.sample_rate.try_into().unwrap(),
                    stream_sample_rate,
                    ResamplingChannelConfig {
                        capacity_seconds: 3.,
                        ..Default::default()
                    },
                )
                .unwrap();
            self.cx.queue_event_for(tracked.node_id, event.into());
            let fresh = self
                .cx
                .node_state::<StreamReaderState>(tracked.node_id)
                .unwrap()
                .handle();
            *tracked.handle.lock().unwrap() = fresh.into_inner().unwrap();
        }

        // 7. Update AEC delay for the new device's latency.
        if let Some(info) = self.cx.stream_info() {
            let delay_ms = (info.input_to_output_latency_seconds * 1000.) as u32;
            info!("update processor delay to {delay_ms}ms after device switch");
            self.aec_processor.set_stream_delay(delay_ms);
        }

        self.current_input_device = input_device;
        self.current_output_device = output_device;
        info!("audio device switch completed");
        Ok(())
    }

    fn output_stream(&mut self, format: AudioFormat) -> Result<OutputStream> {
        let stream_writer_id = self.cx.add_node(
            StreamWriterNode,
            Some(StreamWriterConfig {
                channels: NonZeroChannelCount::new(format.channel_count)
                    .context("channel count may not be zero")?,
                ..Default::default()
            }),
        );
        let graph_out = self.aec_render_node;

        let peak_meter_node = PeakMeterNode::<2> { enabled: true };
        let peak_meter_id = self.cx.add_node(peak_meter_node, None);
        let peak_meter_smoother =
            Arc::new(Mutex::new(PeakMeterSmoother::<2>::new(Default::default())));
        self.peak_meters
            .insert(peak_meter_id, peak_meter_smoother.clone());
        self.cx
            .connect(peak_meter_id, graph_out, &[(0, 0), (1, 1)], true)
            .unwrap();

        let layout: &[(PortIdx, PortIdx)] = match format.channel_count {
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
                format.sample_rate.try_into().unwrap(),
                output_stream_sample_rate,
                ResamplingChannelConfig {
                    capacity_seconds: 3.,
                    ..Default::default()
                },
            )
            .unwrap();
        info!("started output stream");
        self.cx.queue_event_for(stream_writer_id, event.into());
        let handle: StreamWriterHandle = Arc::new(
            self.cx
                .node_state::<StreamWriterState>(stream_writer_id)
                .unwrap()
                .handle(),
        );
        self.output_streams.push(TrackedOutputStream {
            node_id: stream_writer_id,
            format,
            handle: handle.clone(),
        });
        Ok(OutputStream {
            handle,
            paused: Arc::new(AtomicBool::new(false)),
            peaks: peak_meter_smoother,
            normalizer: DbMeterNormalizer::new(-60., 0., -20.),
        })
    }

    fn input_stream(&mut self, format: AudioFormat) -> Result<StreamReaderHandle> {
        let stream_reader_id = self.cx.add_node(
            StreamReaderNode,
            Some(StreamReaderConfig {
                channels: NonZeroChannelCount::new(format.channel_count)
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
            format.channel_count,
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
                format.sample_rate.try_into().unwrap(),
                input_stream_sample_rate,
                ResamplingChannelConfig {
                    capacity_seconds: 3.0,
                    ..Default::default()
                },
            )
            .unwrap();
        self.cx.queue_event_for(stream_reader_id, event.into());

        let handle: StreamReaderHandle = Arc::new(
            self.cx
                .node_state::<StreamReaderState>(stream_reader_id)
                .unwrap()
                .handle(),
        );
        self.input_streams.push(TrackedInputStream {
            node_id: stream_reader_id,
            format,
            handle: handle.clone(),
        });
        Ok(handle)
    }
}
