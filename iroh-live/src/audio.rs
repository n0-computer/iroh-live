use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use firewheel::{
    CpalConfig, CpalInputConfig, CpalOutputConfig, FirewheelConfig, FirewheelContext,
    channel_config::{ChannelCount, NonZeroChannelCount},
    graph::PortIdx,
    nodes::stream::{
        ResamplingChannelConfig,
        reader::{StreamReaderConfig, StreamReaderNode, StreamReaderState},
        writer::{StreamWriterConfig, StreamWriterNode, StreamWriterState},
    },
};
use hang::catalog::AudioConfig;
use tokio::sync::{mpsc, mpsc::error::TryRecvError, oneshot};
use tracing::{error, info, warn};

mod decoder;
mod encoder;

pub use decoder::*;
pub use encoder::*;

pub type OutputStreamHandle = Arc<Mutex<StreamWriterState>>;
pub type InputStreamHandle = Arc<Mutex<StreamReaderState>>;

pub struct OutputControl {
    handle: OutputStreamHandle,
    paused: Arc<AtomicBool>,
}

impl OutputControl {
    pub fn new(handle: OutputStreamHandle) -> Self {
        Self {
            handle,
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    #[allow(unused)]
    pub fn is_active(&self) -> bool {
        self.handle.lock().expect("poisoned").is_active()
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
        self.handle.lock().expect("poisoned").pause_stream();
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
        self.handle.lock().expect("poisoned").resume();
    }
}

// Generic A/V trait integrations
use crate::av as lav;

/// A simple AudioSource that reads from the default microphone via Firewheel.
pub struct MicrophoneSource {
    handle: InputStreamHandle,
    format: lav::AudioFormat,
}

impl MicrophoneSource {
    pub(crate) fn new(handle: InputStreamHandle, sample_rate: u32, channel_count: u32) -> Self {
        Self {
            handle,
            format: lav::AudioFormat {
                sample_rate,
                channel_count,
            },
        }
    }
}

impl lav::AudioSource for MicrophoneSource {
    fn format(&self) -> lav::AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> anyhow::Result<Option<usize>> {
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
                tracing::warn!("audio input underflow: {num_frames_read} frames missing");
                Ok(Some(buf.len()))
            }
            Some(ReadStatus::OverflowCorrected { num_frames_discarded }) => {
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

pub enum AudioCommand {
    OutputStream {
        config: AudioConfig,
        reply: oneshot::Sender<OutputStreamHandle>,
    },
    InputStream {
        sample_rate: u32,
        channel_count: u32,
        reply: oneshot::Sender<InputStreamHandle>,
    },
}

#[derive(Debug, Clone)]
pub struct AudioBackend {
    tx: mpsc::Sender<AudioCommand>,
}

struct AudioDriver {
    cx: FirewheelContext,
    rx: mpsc::Receiver<AudioCommand>,
}

impl AudioDriver {
    fn new(rx: mpsc::Receiver<AudioCommand>) -> Self {
        let config = FirewheelConfig {
            num_graph_inputs: ChannelCount::new(1).unwrap(),
            ..Default::default()
        };
        let mut cx = FirewheelContext::new(config);
        info!("inputs: {:?}", cx.available_input_devices());
        info!("outputs: {:?}", cx.available_output_devices());
        let config = CpalConfig {
            output: CpalOutputConfig {
                #[cfg(target_os = "linux")]
                device_name: Some("pipewire".to_string()),
                #[cfg(not(target_os = "linux"))]
                device_name: None,
                ..Default::default()
            },
            input: Some(CpalInputConfig {
                #[cfg(target_os = "linux")]
                device_name: Some("pipewire".to_string()),
                #[cfg(not(target_os = "linux"))]
                device_name: None,
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

        Self { cx, rx }
    }

    fn run(&mut self) {
        const INTERVAL: Duration = Duration::from_millis(10);
        loop {
            let tick = Instant::now();
            if self.recv().is_err() {
                info!("closing audio driver: command channel closed");
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

            std::thread::sleep(INTERVAL.saturating_sub(tick.elapsed()));
        }
    }

    fn recv(&mut self) -> Result<(), ()> {
        loop {
            match self.rx.try_recv() {
                Err(TryRecvError::Disconnected) => {
                    info!("stopping audio thread: backend handle dropped");
                    break Err(());
                }
                Err(TryRecvError::Empty) => {
                    break Ok(());
                }
                Ok(command) => self.handle(command),
            }
        }
    }

    fn handle(&mut self, command: AudioCommand) {
        match command {
            AudioCommand::OutputStream { config, reply } => match self.output_stream(&config) {
                Err(err) => {
                    warn!("failed to create audio output stream: {err:#}")
                }
                Ok(stream) => {
                    reply.send(stream).ok();
                }
            },
            AudioCommand::InputStream {
                sample_rate,
                channel_count,
                reply,
            } => match self.input_stream(sample_rate, channel_count) {
                Err(err) => warn!("failed to create audio input stream: {err:#}"),
                Ok(stream) => {
                    reply.send(stream).ok();
                }
            },
        }
    }

    fn output_stream(&mut self, config: &hang::catalog::AudioConfig) -> Result<OutputStreamHandle> {
        // setup stream
        let stream_writer_id = self.cx.add_node(
            StreamWriterNode,
            Some(StreamWriterConfig {
                channels: NonZeroChannelCount::new(config.channel_count)
                    .context("channel count may not be zero")?,
                ..Default::default()
            }),
        );
        let graph_out_node_id = self.cx.graph_out_node_id();
        let graph_out_info = self
            .cx
            .node_info(graph_out_node_id)
            .context("missing audio output node")?;
        let layout: &[(PortIdx, PortIdx)] = match (
            config.channel_count,
            graph_out_info.info.channel_config.num_inputs.get(),
        ) {
            (_, 0) => anyhow::bail!("audio output has no channels"),
            (0, _) => anyhow::bail!("audio stream has no channels"),
            (1, 2) => &[(0, 0), (0, 1)],
            (2, 2) => &[(0, 0), (1, 1)],
            (_, 1) => &[(0, 0)],
            _ => &[(0, 0), (1, 1)],
        };
        self.cx
            .connect(stream_writer_id, graph_out_node_id, layout, false)
            .unwrap();
        let output_stream_sample_rate = self.cx.stream_info().unwrap().sample_rate;
        let event = self
            .cx
            .node_state_mut::<StreamWriterState>(stream_writer_id)
            .unwrap()
            .start_stream(
                config.sample_rate.try_into().unwrap(),
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
        Ok(Arc::new(handle))
    }

    fn input_stream(&mut self, sample_rate: u32, channel_count: u32) -> Result<InputStreamHandle> {
        // Setup stream reader node
        let stream_reader_id = self.cx.add_node(
            StreamReaderNode,
            Some(StreamReaderConfig {
                channels: NonZeroChannelCount::new(channel_count)
                    .context("channel count may not be zero")?,
                ..Default::default()
            }),
        );
        let graph_in_node_id = self.cx.graph_in_node_id();
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
                Default::default(),
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

impl AudioBackend {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(32);
        let _handle = std::thread::spawn(move || AudioDriver::new(rx).run());
        Self { tx }
    }

    /// Convenience: produce a default microphone audio source (48kHz stereo).
    /// Uses a blocking call to initialize the input stream synchronously.
    pub async fn default_microphone(&self) -> anyhow::Result<MicrophoneSource> {
        const SAMPLE_RATE: u32 = 48_000;
        const CHANNELS: u32 = 2;
        let handle = self.input_stream(SAMPLE_RATE, CHANNELS).await?;
        Ok(MicrophoneSource::new(handle, SAMPLE_RATE, CHANNELS))
    }

    pub async fn output_stream(&self, config: AudioConfig) -> Result<OutputStreamHandle> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(AudioCommand::OutputStream { config, reply })
            .await?;
        let handle = reply_rx.await?;
        Ok(handle)
    }

    #[allow(unused)]
    pub fn blocking_output_stream(&self, config: AudioConfig) -> Result<OutputStreamHandle> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .blocking_send(AudioCommand::OutputStream { config, reply })?;
        let handle = reply_rx.blocking_recv()?;
        Ok(handle)
    }

    #[allow(unused)]
    pub async fn input_stream(&self, sample_rate: u32, channels: u32) -> Result<InputStreamHandle> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(AudioCommand::InputStream {
                sample_rate,
                channel_count: channels,
                reply,
            })
            .await?;
        let handle = reply_rx.await?;
        Ok(handle)
    }

    pub fn blocking_input_stream(
        &self,
        sample_rate: u32,
        channels: u32,
    ) -> Result<InputStreamHandle> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx.blocking_send(AudioCommand::InputStream {
            sample_rate,
            channel_count: channels,
            reply,
        })?;
        let handle = reply_rx.blocking_recv()?;
        Ok(handle)
    }
}
