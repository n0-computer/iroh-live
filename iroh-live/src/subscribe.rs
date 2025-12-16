use std::{collections::HashMap, time::Duration};

use hang::{
    Timestamp, TrackConsumer,
    catalog::{AudioConfig, Catalog, CatalogConsumer, VideoConfig},
};
use moq_lite::{BroadcastConsumer, Track};
use n0_error::{Result, StackResultExt, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::{
    sync::mpsc::{self, error::TryRecvError},
    time::Instant,
};
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Span, debug, error, info, info_span, trace, warn};

use crate::{
    av::{
        AudioDecoder, AudioSink, AudioSinkHandle, DecodeConfig, DecodedFrame, Decoders,
        PlaybackConfig, Quality, VideoDecoder, VideoSource,
    },
    ffmpeg::util::Rescaler,
    live::AvRemoteTrack,
    util::spawn_thread,
};

#[derive(derive_more::Debug)]
pub struct SubscribeBroadcast {
    broadcast_name: String,
    #[debug("BroadcastConsumer")]
    broadcast: BroadcastConsumer,
    #[debug("CatalogConsumer")]
    catalog_consumer: CatalogConsumer,
    catalog: Catalog,
    shutdown: CancellationToken,
}

impl SubscribeBroadcast {
    pub async fn new(broadcast_name: String, broadcast: BroadcastConsumer) -> Result<Self> {
        let catalog_track = broadcast.subscribe_track(&Catalog::default_track());
        let catalog_consumer = CatalogConsumer::new(catalog_track);
        let mut this = Self {
            broadcast_name,
            broadcast,
            catalog: Catalog::default(),
            catalog_consumer,
            shutdown: CancellationToken::new(),
        };
        this.update_catalog().await?;
        Ok(this)
    }

    pub fn broadcast_name(&self) -> &str {
        &self.broadcast_name
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
        playback_config: &DecodeConfig,
        quality: Quality,
    ) -> Result<WatchTrack> {
        let info = self.catalog.video.as_ref().context("no video published")?;
        let track_name =
            select_video_rendition(&info.renditions, quality).context("no video renditions")?;
        self.watch_rendition::<D>(playback_config, &track_name)
    }

    pub fn watch_rendition<D: VideoDecoder>(
        &self,
        playback_config: &DecodeConfig,
        name: &str,
    ) -> Result<WatchTrack> {
        let video = self.catalog.video.as_ref().context("no video published")?;
        let config = video.renditions.get(name).context("rendition not found")?;
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&Track {
            name: name.to_string(),
            priority: video.priority,
        }));
        let span = info_span!("videodec", %name);
        WatchTrack::from_consumer::<D>(
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

    pub fn closed(&self) -> impl Future<Output = ()> + 'static {
        self.broadcast.closed()
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    pub fn watch_and_listen<D: Decoders>(
        self,
        audio_out: impl AudioSink,
        config: PlaybackConfig,
    ) -> Result<AvRemoteTrack> {
        AvRemoteTrack::new::<D>(self, audio_out, config)
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
    name: String,
    handle: Box<dyn AudioSinkHandle>,
    _shutdown_token_guard: DropGuard,
    _task_handle: AbortOnDropHandle<()>,
    _thread_handle: std::thread::JoinHandle<()>,
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
        let handle = output.handle();
        let thread_name = format!("audio-dec-{}", name);
        let thread = spawn_thread(thread_name, {
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
            handle,
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
        const INTERVAL: Duration = Duration::from_millis(10);
        let mut remote_start = None;
        let loop_start = Instant::now();

        'outer: for i in 0.. {
            let tick = Instant::now();

            if shutdown.is_cancelled() {
                debug!("stop audio thread: cancelled");
                break;
            }

            loop {
                match packet_rx.try_recv() {
                    Ok(packet) => {
                        let remote_start = *remote_start.get_or_insert_with(|| packet.timestamp);

                        let loop_elapsed = tick.duration_since(loop_start);
                        let remote_elapsed: Duration = packet
                            .timestamp
                            .checked_sub(remote_start)
                            .unwrap_or(Timestamp::ZERO)
                            .into();
                        let diff_ms =
                            (loop_elapsed.as_secs_f32() - remote_elapsed.as_secs_f32()) * 1000.;

                        // TODO: Skip outdated packets?
                        trace!(len = packet.payload.num_bytes(), ts=?packet.timestamp, ?loop_elapsed, ?remote_elapsed, ?diff_ms, "recv packet");
                        if !sink.is_paused() {
                            decoder.push_packet(packet)?;
                            if let Some(samples) = decoder.pop_samples()? {
                                sink.push_samples(samples)?;
                            }
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        debug!("stop audio thread: packet_rx disconnected");
                        break 'outer;
                    }
                    Err(TryRecvError::Empty) => {
                        trace!("no packet to recv");
                        break;
                    }
                }
            }

            let expected_time = i * INTERVAL;
            let real_time = Instant::now().duration_since(loop_start);
            let sleep = expected_time.saturating_sub(real_time);
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        }
        Ok(())
    }

    pub fn handle(&self) -> &dyn AudioSinkHandle {
        self.handle.as_ref()
    }
}

pub struct WatchTrack {
    video_frames: WatchTrackFrames,
    handle: WatchTrackHandle,
}

pub struct WatchTrackHandle {
    viewport: Watchable<(u32, u32)>,
    guard: WatchTrackGuard,
}

impl WatchTrackHandle {
    pub fn set_viewport(&self, w: u32, h: u32) {
        self.viewport.set((w, h)).ok();
    }

    pub fn rendition(&self) -> &str {
        &self.guard.rendition
    }
}

pub struct WatchTrackFrames {
    rx: mpsc::Receiver<DecodedFrame>,
}

impl WatchTrackFrames {
    pub fn current_frame(&mut self) -> Option<DecodedFrame> {
        let mut out = None;
        while let Ok(item) = self.rx.try_recv() {
            out = Some(item);
        }
        out
    }

    pub async fn next_frame(&mut self) -> Option<DecodedFrame> {
        if let Some(frame) = self.current_frame() {
            Some(frame)
        } else {
            self.rx.recv().await
        }
    }
}

struct WatchTrackGuard {
    rendition: String,
    _shutdown_token_guard: DropGuard,
    _task_handle: Option<AbortOnDropHandle<()>>,
    _thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl WatchTrack {
    pub fn empty(rendition: impl ToString) -> Self {
        let (tx, rx) = mpsc::channel(1);
        let task = tokio::task::spawn(async move {
            std::future::pending::<()>().await;
            let _ = tx;
        });
        let guard = WatchTrackGuard {
            rendition: rendition.to_string(),
            _shutdown_token_guard: CancellationToken::new().drop_guard(),
            _task_handle: Some(AbortOnDropHandle::new(task)),
            _thread_handle: None,
        };
        Self {
            video_frames: WatchTrackFrames { rx },
            handle: WatchTrackHandle {
                viewport: Default::default(),
                guard,
            },
        }
    }

    pub(crate) fn from_video_source(
        rendition: String,
        shutdown: CancellationToken,
        mut source: impl VideoSource,
        decode_config: DecodeConfig,
    ) -> Self {
        let viewport = Watchable::new((1u32, 1u32));
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<DecodedFrame>(2);
        let thread_name = format!("video-preview-{}", rendition);
        let thread = spawn_thread(thread_name, {
            let mut viewport = viewport.watch();
            let shutdown = shutdown.clone();
            move || {
                let fps = 30;
                let mut rescaler = Rescaler::new(decode_config.pixel_format.to_ffmpeg(), None)
                    .expect("failed to create rescaler");
                let frame_duration = Duration::from_secs_f32(1. / fps as f32);
                if let Err(err) = source.start() {
                    warn!("Video source failed to start: {err:?}");
                    return;
                }
                let start = Instant::now();
                for i in 0.. {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    if viewport.update() {
                        let (w, h) = viewport.peek();
                        rescaler.set_target_dimensions(*w, *h);
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let frame = rescaler
                                .process(&frame.to_ffmpeg())
                                .expect("rescaler failed");
                            let frame =
                                DecodedFrame::from_ffmpeg(frame, frame_duration, start.elapsed());
                            let _ = frame_tx.blocking_send(frame);
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                    let expected_time = i * frame_duration;
                    let actual_time = start.elapsed();
                    if expected_time < actual_time {
                        std::thread::sleep(actual_time - expected_time);
                    }
                }
                if let Err(err) = source.stop() {
                    warn!("Video source failed to stop: {err:?}");
                    return;
                }
            }
        });
        let guard = WatchTrackGuard {
            rendition,
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: None,
            _thread_handle: Some(thread),
        };
        WatchTrack {
            video_frames: WatchTrackFrames { rx: frame_rx },
            handle: WatchTrackHandle { viewport, guard },
        }
    }

    pub(crate) fn from_consumer<D: VideoDecoder>(
        rendition: String,
        consumer: TrackConsumer,
        config: &VideoConfig,
        playback_config: &DecodeConfig,
        shutdown: CancellationToken,
        span: Span,
    ) -> Result<Self> {
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let viewport = Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let _guard = span.enter();
        debug!(?config, "video decoder start");
        let decoder = D::new(config, playback_config)?;
        let thread_name = format!("video-dec-{}", rendition);
        let thread = spawn_thread(thread_name, {
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
        let guard = WatchTrackGuard {
            rendition,
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: Some(AbortOnDropHandle::new(task)),
            _thread_handle: Some(thread),
        };
        Ok(WatchTrack {
            video_frames: WatchTrackFrames { rx: frame_rx },
            handle: WatchTrackHandle { viewport, guard },
        })
    }

    pub fn split(self) -> (WatchTrackFrames, WatchTrackHandle) {
        (self.video_frames, self.handle)
    }

    pub fn set_viewport(&self, w: u32, h: u32) {
        self.handle.set_viewport(w, h);
    }

    pub fn rendition(&self) -> &str {
        self.handle.rendition()
    }

    pub fn current_frame(&mut self) -> Option<DecodedFrame> {
        self.video_frames.current_frame()
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
            let t = Instant::now();
            decoder
                .push_packet(packet)
                .context("failed to push packet")?;
            trace!(t=?t.elapsed(), "videodec: push_packet");
            while let Some(frame) = decoder.pop_frame().context("failed to pop frame")? {
                trace!(t=?t.elapsed(), "videodec: pop frame");
                if frame_tx.blocking_send(frame).is_err() {
                    break;
                }
                trace!(t=?t.elapsed(), "videodec: tx");
            }
        }
        Ok(())
    }
}

async fn forward_frames(mut track: hang::TrackConsumer, sender: mpsc::Sender<hang::Frame>) {
    loop {
        let frame = track.read_frame().await;
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
