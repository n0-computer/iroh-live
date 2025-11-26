use std::{collections::HashMap, time::Duration};

use hang::{
    Catalog, CatalogConsumer, TrackConsumer,
    catalog::{AudioConfig, VideoConfig},
};
use moq_lite::{BroadcastConsumer, Track};
use n0_error::{Result, StackResultExt, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Span, debug, error, info, info_span, warn};

use crate::{
    av::{
        AudioDecoder, AudioSink, DecodedFrame, PlaybackConfig, Quality, VideoDecoder, VideoSource,
    },
    publish::SharedVideoSource,
};

pub struct SubscribeBroadcast {
    broadcast: BroadcastConsumer,
    catalog_consumer: CatalogConsumer,
    catalog: Catalog,
    shutdown: CancellationToken,
}

impl SubscribeBroadcast {
    pub async fn new(broadcast: BroadcastConsumer) -> Result<Self> {
        let catalog_track = broadcast.subscribe_track(&Catalog::default_track());
        let catalog_consumer = CatalogConsumer::new(catalog_track);
        let mut this = Self {
            broadcast,
            catalog: Catalog::default(),
            catalog_consumer,
            shutdown: CancellationToken::new(),
        };
        this.update_catalog().await?;
        Ok(this)
    }

    pub async fn update_catalog(&mut self) -> Result<()> {
        self.catalog = self
            .catalog_consumer
            .next()
            .await
            .std_context("Failed to fetch catalog")?
            .context("Empty catalog")?;
        Ok(())
    }

    pub fn watch<D: VideoDecoder>(&self) -> Result<WatchTrack> {
        self.watch_with::<D>(&Default::default(), Quality::Highest)
    }

    pub fn watch_with<D: VideoDecoder>(
        &self,
        playback_config: &PlaybackConfig,
        quality: Quality,
    ) -> Result<WatchTrack> {
        let info = self.catalog.video.as_ref().context("no video published")?;
        let track_name =
            select_video_rendition(&info.renditions, quality).context("no video renditions")?;
        self.watch_rendition::<D>(playback_config, &track_name)
    }

    pub fn watch_rendition<D: VideoDecoder>(
        &self,
        playback_config: &PlaybackConfig,
        name: &str,
    ) -> Result<WatchTrack> {
        let video = self.catalog.video.as_ref().context("no video published")?;
        let config = video.renditions.get(name).context("rendition not found")?;
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&Track {
            name: name.to_string(),
            priority: video.priority,
        }));
        let span = info_span!("videodec", %name);
        WatchTrack::spawn::<D>(
            name.to_string(),
            consumer,
            &config,
            playback_config,
            self.shutdown.child_token(),
            span,
        )
    }
    pub fn listen<D: AudioDecoder>(&self, output: impl AudioSink) -> Result<AudioTrack> {
        self.listen_with::<D>(Quality::Highest, output)
    }

    pub fn listen_with<D: AudioDecoder>(
        &self,
        quality: Quality,
        output: impl AudioSink,
    ) -> Result<AudioTrack> {
        debug!(catalog=?self.catalog,"catalog");
        let info = self.catalog.audio.as_ref().context("no audio published")?;
        let track_name =
            select_audio_rendition(&info.renditions, quality).context("no audio renditions")?;
        self.listen_rendition::<D>(&track_name, output)
    }

    pub fn listen_rendition<D: AudioDecoder>(
        &self,
        name: &str,
        output: impl AudioSink,
    ) -> Result<AudioTrack> {
        let audio = self.catalog.audio.as_ref().context("no video published")?;
        let config = audio.renditions.get(name).context("rendition not found")?;
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&Track {
            name: name.to_string(),
            priority: audio.priority,
        }));
        let span = info_span!("audiodec", %name);
        AudioTrack::spawn::<D>(
            name.to_string(),
            consumer,
            config.clone(),
            output,
            self.shutdown.child_token(),
            span,
        )
    }

    pub fn video_renditions(&self) -> impl Iterator<Item = &str> {
        let mut renditions: Vec<_> = self
            .catalog
            .video
            .as_ref()
            .into_iter()
            .map(|v| v.renditions.iter())
            .flatten()
            .map(|(name, config)| (name.as_str(), config.coded_width))
            .collect();
        renditions.sort_by(|a, b| a.1.cmp(&b.1));
        renditions.into_iter().map(|(name, _w)| name)
    }

    pub fn audio_renditions(&self) -> impl Iterator<Item = &str> {
        self.catalog
            .audio
            .as_ref()
            .into_iter()
            .map(|v| v.renditions.iter())
            .flatten()
            .map(|(name, _config)| name.as_str())
    }
}

pub(crate) fn select_rendition<T, P: ToString>(
    renditions: &HashMap<String, T>,
    order: &[P],
) -> Option<String> {
    order
        .iter()
        .map(ToString::to_string)
        .find(|k| renditions.contains_key(k.as_str()))
        .or_else(|| renditions.keys().next().cloned())
}

pub(crate) fn select_video_rendition<'a, T>(
    renditions: &'a HashMap<String, T>,
    q: Quality,
) -> Option<String> {
    use crate::av::VideoPreset::*;
    let order = match q {
        Quality::Highest => [P1080, P720, P360, P180],
        Quality::High => [P720, P360, P180, P1080],
        Quality::Mid => [P360, P180, P720, P1080],
        Quality::Low => [P180, P360, P720, P1080],
    };

    select_rendition(renditions, &order)
}

pub(crate) fn select_audio_rendition<'a, T>(
    renditions: &'a HashMap<String, T>,
    q: Quality,
) -> Option<String> {
    use crate::av::AudioPreset::*;
    let order = match q {
        Quality::Highest | Quality::High => [Hq, Lq],
        Quality::Mid | Quality::Low => [Lq, Hq],
    };
    select_rendition(renditions, &order)
}

pub struct AudioTrack {
    pub(crate) name: String,
    pub(crate) _shutdown_token_guard: DropGuard,
    pub(crate) _task_handle: AbortOnDropHandle<()>,
    pub(crate) _thread_handle: std::thread::JoinHandle<()>,
}

impl AudioTrack {
    pub(crate) fn spawn<D: AudioDecoder>(
        name: String,
        consumer: TrackConsumer,
        config: AudioConfig,
        output: impl AudioSink,
        shutdown: CancellationToken,
        span: Span,
    ) -> Result<Self> {
        let _guard = span.enter();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let output_format = output.format()?;
        info!(?config, "audio thread start");
        let decoder = D::new(&config, output_format)?;
        let thread = std::thread::spawn({
            let shutdown = shutdown.clone();
            let span = span.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) = Self::run_loop(decoder, packet_rx, output, &shutdown) {
                    error!("audio decoder failed: {err:#}");
                }
                info!("audio decoder thread stop");
            }
        });
        let task = tokio::spawn(forward_frames(consumer, packet_tx));
        Ok(Self {
            name,
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
            _shutdown_token_guard: shutdown.drop_guard(),
        })
    }

    pub fn rendition(&self) -> &str {
        &self.name
    }

    pub(crate) fn run_loop(
        mut decoder: impl AudioDecoder,
        mut packet_rx: mpsc::Receiver<hang::Frame>,
        mut sink: impl AudioSink,
        shutdown: &CancellationToken,
    ) -> Result<()> {
        let mut last_timestamp = None;
        while let Some(packet) = packet_rx.blocking_recv() {
            debug!(len = packet.payload.len(), ts=?packet.timestamp, "recv packet");
            let timestamp = packet.timestamp;
            if shutdown.is_cancelled() {
                debug!("stop audio thread: cancelled");
                break;
            }
            decoder.push_packet(packet)?;
            if let Some(samples) = decoder.pop_samples()? {
                debug!("decoded {}", samples.len());
                let delay = match last_timestamp {
                    None => Duration::default(),
                    Some(last_timestamp) => timestamp.saturating_sub(last_timestamp),
                };
                if delay > Duration::ZERO {
                    debug!("sleep {delay:?}");
                    std::thread::sleep(delay);
                }
                sink.push_samples(samples)?;
            }
            last_timestamp = Some(timestamp);
        }
        Ok(())
    }
}

pub struct WatchTrack {
    pub(crate) rendition: String,
    pub(crate) video_frames: mpsc::Receiver<DecodedFrame>,
    pub(crate) viewport: n0_watcher::Watchable<(u32, u32)>,
    pub(crate) _shutdown_token_guard: DropGuard,
    pub(crate) _task_handle: AbortOnDropHandle<()>,
    pub(crate) _thread_handle: std::thread::JoinHandle<()>,
}

impl WatchTrack {
    pub(crate) fn from_shared_source(
        name: String,
        shutdown: CancellationToken,
        mut source: SharedVideoSource,
    ) -> Self {
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let dummy_handle = tokio::task::spawn(async {});
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<DecodedFrame>(2);
        let thread = std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                let mut last_ts = std::time::Instant::now();
                loop {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let (w, h) = (frame.format.dimensions[0], frame.format.dimensions[1]);
                            let buf = frame.raw;
                            // if format.pixel_format == PixelFormat::Bgra {
                            //     for px in buf.chunks_exact_mut(4) {
                            //         px.swap(0, 2);
                            //     }
                            // }
                            if let Some(img) = image::ImageBuffer::from_raw(w, h, buf) {
                                let frame_img = image::Frame::new(img);
                                let ts = last_ts.elapsed();
                                last_ts = std::time::Instant::now();
                                let _ = frame_tx.blocking_send(DecodedFrame {
                                    frame: frame_img,
                                    timestamp: ts,
                                });
                            }
                        }
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                        Err(_) => break,
                    }
                }
            }
        });
        WatchTrack {
            rendition: name,
            video_frames: frame_rx,
            viewport,
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: AbortOnDropHandle::new(dummy_handle),
            _thread_handle: thread,
        }
    }

    pub fn set_viewport(&self, w: u32, h: u32) {
        self.viewport.set((w, h)).ok();
    }

    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    pub fn current_frame(&mut self) -> Option<DecodedFrame> {
        let mut out = None;
        while let Ok(item) = self.video_frames.try_recv() {
            out = Some(item);
        }
        out
    }

    pub(crate) fn spawn<D: VideoDecoder>(
        name: String,
        consumer: TrackConsumer,
        config: &VideoConfig,
        playback_config: &PlaybackConfig,
        shutdown: CancellationToken,
        span: Span,
    ) -> Result<Self> {
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let _guard = span.enter();
        // TODO: support native.
        debug!(?config, "video decoder start");
        let decoder = D::new(config, playback_config)?;
        let thread = std::thread::spawn({
            let shutdown = shutdown.clone();
            let span = span.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) =
                    Self::run_loop(&shutdown, packet_rx, frame_tx, viewport_watcher, decoder)
                {
                    error!("video decoder failed: {err:#}");
                }
                shutdown.cancel();
            }
        });
        let task = tokio::task::spawn(forward_frames(consumer, packet_tx));
        Ok(WatchTrack {
            rendition: name,
            video_frames: frame_rx,
            viewport,
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
        })
    }

    pub(crate) fn run_loop(
        shutdown: &CancellationToken,
        mut packet_rx: mpsc::Receiver<hang::Frame>,
        frame_tx: mpsc::Sender<DecodedFrame>,
        mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
        mut decoder: impl VideoDecoder,
    ) -> Result<(), anyhow::Error> {
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            let Some(packet) = packet_rx.blocking_recv() else {
                break;
            };
            if viewport_watcher.update() {
                let (w, h) = viewport_watcher.peek();
                decoder.set_viewport(*w, *h);
            }
            decoder
                .push_packet(packet)
                .context("failed to push packet")?;
            while let Some(frame) = decoder.pop_frame().context("failed to pop frame")? {
                if frame_tx.blocking_send(frame).is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn forward_frames(mut track: hang::TrackConsumer, sender: mpsc::Sender<hang::Frame>) {
    loop {
        let frame = track.read().await;
        match frame {
            Ok(Some(frame)) => {
                if sender.send(frame).await.is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                warn!("failed to read frame: {err:?}");
                break;
            }
        }
    }
}
