use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use hang::catalog::{AudioConfig, VideoConfig};
use hang::{Catalog, CatalogConsumer, CatalogProducer, TrackConsumer};
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, ConnectionStats},
    protocol::ProtocolHandler,
};
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer, Track};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use n0_watcher::Watcher;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, error, error_span, info, info_span, instrument, warn};
use web_transport_iroh::Request;

use crate::av::{
    AudioDecoder, AudioEncoder, AudioSink, AudioSource, Backend, DecodedFrame, PixelFormat,
    Quality, VideoDecoder, VideoEncoder, VideoSource,
};
use crate::ffmpeg::audio::FfmpegAudioDecoder;
use crate::ffmpeg::video::FfmpegVideoDecoder;

pub mod audio;
pub mod av;
pub mod ffmpeg;
mod ffmpeg_ext;
// pub mod native;
mod ticket;
pub mod video;

pub use ticket::LiveTicket;

pub const ALPN: &[u8] = b"iroh-live/1";

#[derive(Debug, Clone)]
pub struct Live {
    endpoint: Endpoint,
    tx: mpsc::Sender<ActorMessage>,
    shutdown_token: CancellationToken,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Live {
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let actor = Actor::default();
        let shutdown_token = actor.shutdown_token.clone();
        let actor_task = n0_future::task::spawn(async move {
            actor.run(rx).instrument(error_span!("live-actor")).await
        });
        Self {
            shutdown_token,
            endpoint,
            tx,
            _actor_handle: Arc::new(AbortOnDropHandle::new(actor_task)),
        }
    }

    pub fn protocol_handler(&self) -> LiveProtocolHandler {
        LiveProtocolHandler {
            tx: self.tx.clone(),
        }
    }

    pub async fn publish(&self, broadcast: &PublishBroadcast) -> Result<LiveTicket> {
        let ticket = LiveTicket {
            endpoint_id: self.endpoint.id(),
            broadcast_name: broadcast.name.clone(),
        };
        self.tx
            .send(ActorMessage::PublishBroadcast(
                broadcast.name.clone(),
                broadcast.broadcast.clone(),
            ))
            .await
            .std_context("live actor died")?;
        Ok(ticket)
    }

    #[instrument(skip_all, fields(remote=tracing::field::Empty))]
    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<LiveSession> {
        let addr = addr.into();
        tracing::Span::current().record("remote", tracing::field::display(addr.id.fmt_short()));
        let connection = self.endpoint.connect(addr, ALPN).await?;
        let url: url::Url = format!("iroh://{}", connection.remote_id())
            .parse()
            .unwrap();
        let session = web_transport_iroh::Session::raw(connection, url);
        let session = LiveSession::connect(session).await?;
        Ok(session)
    }

    pub fn shutdown(&self) {
        self.shutdown_token.cancel();
    }
}

#[derive(Debug, Clone)]
pub struct LiveProtocolHandler {
    tx: mpsc::Sender<ActorMessage>,
}

impl ProtocolHandler for LiveProtocolHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let request = Request::accept(connection)
            .await
            .context("Failed to accept request")?;
        info!(url=%request.url(), "accepted");
        let session = request.ok().await.std_context("Failed to accept session")?;
        let session = LiveSession::accept(session).await?;
        self.tx
            .send(ActorMessage::HandleSession(session))
            .await
            .map_err(|_| anyerr!("live actor died"))?;
        Ok(())
    }
}

pub struct LiveSession {
    remote: EndpointId,
    wt_session: web_transport_iroh::Session,
    moq_session: moq_lite::Session<web_transport_iroh::Session>,
    publish: OriginProducer,
    subscribe: OriginConsumer,
}

impl LiveSession {
    pub async fn connect(wt_session: web_transport_iroh::Session) -> Result<Self> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let remote = wt_session.remote_id();
        let moq_session =
            moq_lite::Session::connect(wt_session.clone(), publish.consumer, subscribe.producer)
                .await
                .std_context("failed to accept session")?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            remote,
            moq_session,
            wt_session,
        })
    }
    pub async fn accept(wt_session: web_transport_iroh::Session) -> Result<Self> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let remote = wt_session.remote_id();
        let moq_session =
            moq_lite::Session::accept(wt_session.clone(), publish.consumer, subscribe.producer)
                .await
                .std_context("failed to accept session")?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            remote,
            moq_session,
            wt_session,
        })
    }

    pub fn stats(&self) -> ConnectionStats {
        self.wt_session.stats()
    }

    pub async fn consume(&mut self, name: &str) -> Result<ConsumeBroadcast> {
        let broadcast = self.wait_for_broadcast(name).await?;
        ConsumeBroadcast::new(broadcast).await
    }

    pub fn publish(&self, broadcast: &PublishBroadcast) {
        let consumer = broadcast.broadcast.consume();
        self.publish
            .publish_broadcast(broadcast.name.clone(), consumer);
    }

    async fn wait_for_broadcast(&mut self, name: &str) -> Result<BroadcastConsumer> {
        loop {
            let (path, consumer) = self
                .subscribe
                .announced()
                .await
                .std_context("session closed before broadcast was announced")?;
            debug!("peer announced broadcast: {path}");
            if path.as_str() == name {
                return consumer.std_context("peer closed the broadcast");
            }
        }
    }
}

enum ActorMessage {
    HandleSession(LiveSession),
    PublishBroadcast(BroadcastName, BroadcastProducer),
}

struct SessionState {
    publish: OriginProducer,
}

type BroadcastName = String;

pub type PacketSender = mpsc::Sender<hang::Frame>;

#[derive(Default)]
struct Actor {
    shutdown_token: CancellationToken,
    broadcasts: HashMap<BroadcastName, BroadcastProducer>,
    sessions: HashMap<EndpointId, SessionState>,
    session_tasks: JoinSet<(EndpointId, Result<(), moq_lite::Error>)>,
}

impl Actor {
    pub async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
        loop {
            tokio::select! {
                msg = inbox.recv() => {
                    match msg {
                        None => break,
                        Some(msg) => self.handle_message(msg)
                    }
                }
                Some(res) = self.session_tasks.join_next(), if !self.session_tasks.is_empty() => {
                    let (endpoint_id, res) = res.expect("session task panicked");
                    info!(remote=%endpoint_id.fmt_short(), "session closed: {res:?}");
                    self.sessions.remove(&endpoint_id);
                }
            }
        }
    }

    fn handle_message(&mut self, msg: ActorMessage) {
        match msg {
            ActorMessage::HandleSession(msg) => self.handle_incoming_session(msg),
            ActorMessage::PublishBroadcast(name, producer) => {
                self.handle_publish_broadcast(name, producer)
            }
        }
    }

    fn handle_incoming_session(&mut self, session: LiveSession) {
        tracing::info!("handle new incoming session");
        let LiveSession {
            remote,
            moq_session,
            publish,
            subscribe: _,
            ..
        } = session;
        for (name, producer) in self.broadcasts.iter() {
            publish.publish_broadcast(name.to_string(), producer.consume());
        }
        self.sessions.insert(remote, SessionState { publish });

        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    moq_session.close(moq_lite::Error::Cancel);
                    Ok(())
                }
                result = moq_session.closed() => result,
            };
            (remote, res)
        });
    }

    fn handle_publish_broadcast(&mut self, name: BroadcastName, producer: BroadcastProducer) {
        for session in self.sessions.values_mut() {
            session
                .publish
                .publish_broadcast(name.clone(), producer.consume());
        }
        self.broadcasts.insert(name, producer);
    }
}

pub struct PublishBroadcast {
    name: BroadcastName,
    broadcast: BroadcastProducer,
    catalog: CatalogProducer,
    shutdown: CancellationToken,
    video_state: Option<EncoderState>,
    audio_state: Option<EncoderState>,
}

struct EncoderState {
    shutdown: CancellationToken,
    _threads: Vec<EncoderThread>,
}

impl Drop for EncoderState {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl PublishBroadcast {
    pub fn new(name: &str) -> Self {
        let name = name.to_string();
        let mut broadcast = BroadcastProducer::default();
        let catalog = Catalog::default().produce();
        broadcast.insert_track(catalog.consumer.track);
        let catalog = catalog.producer;

        Self {
            name,
            broadcast,
            catalog,
            shutdown: CancellationToken::new(),
            video_state: None,
            audio_state: None,
        }
    }

    pub fn set_video<I, E>(
        &mut self,
        source: impl VideoSource + Send + 'static,
        renditions: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::VideoPreset)>,
        E: VideoEncoder + Send + 'static,
    {
        self.video_state = None;
        let priority = 1u8;
        let (tx, rx) = tokio::sync::watch::channel(None);
        let source_format = source.format();
        let shutdown = self.shutdown.child_token();
        // capture loop
        std::thread::spawn({
            let mut source = source;
            let shutdown = shutdown.clone();
            move || {
                loop {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let _ = tx.send(Some(frame));
                        }
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            }
        });
        let (threads, renditions): (Vec<_>, HashMap<_, _>) = renditions
            .into_iter()
            .map(|(encoder, preset)| {
                let name = format!("video-{}", preset);
                let span = info_span!("videoenc", %name);
                let rendition = (name.clone(), encoder.config());
                let track = self.broadcast.create_track(Track {
                    name: name.clone(),
                    priority,
                });
                let producer = hang::TrackProducer::new(track);
                let thread = EncoderThread::spawn_video(
                    rx.clone(),
                    encoder,
                    producer,
                    source_format.clone(),
                    shutdown.child_token(),
                    span,
                );
                (thread, rendition)
            })
            .unzip();
        let video = hang::catalog::Video {
            renditions,
            priority,
            display: None,
            rotation: None,
            flip: None,
            detection: None,
        };
        self.catalog.set_video(Some(video));
        self.catalog.publish();
        self.video_state = Some(EncoderState {
            shutdown,
            _threads: threads,
        });
        Ok(())
    }

    pub fn set_audio<I, E>(
        &mut self,
        source: impl AudioSource + Clone + Send + 'static,
        renditions: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::AudioPreset)>,
        E: AudioEncoder + Send + 'static,
    {
        let priority = 2u8;

        let shutdown = self.shutdown.child_token();
        let (threads, renditions): (Vec<_>, HashMap<_, _>) = renditions
            .into_iter()
            .map(|(encoder, preset)| {
                let name = format!("{preset}");
                let span = info_span!("audioenc", %name);
                let rendition = (name.clone(), encoder.config());
                let track = self.broadcast.create_track(Track {
                    name: name.clone(),
                    priority,
                });
                let producer = hang::TrackProducer::new(track);
                // Clone source per encoder thread
                let thread = EncoderThread::spawn_audio(
                    source.clone(),
                    encoder,
                    producer,
                    shutdown.clone(),
                    span,
                );
                (thread, rendition)
            })
            .unzip();

        let audio = hang::catalog::Audio {
            renditions,
            priority,
            captions: None,
            speaking: None,
        };
        self.catalog.set_audio(Some(audio));
        self.catalog.publish();
        self.audio_state = Some(EncoderState {
            shutdown,
            _threads: threads,
        });
        Ok(())
    }
}

impl Drop for PublishBroadcast {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.broadcast.close();
    }
}

pub struct ConsumeBroadcast {
    broadcast: BroadcastConsumer,
    catalog_consumer: CatalogConsumer,
    catalog: Catalog,
    shutdown: CancellationToken,
}

impl ConsumeBroadcast {
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

    pub fn watch(&self) -> Result<WatchTrack> {
        self.watch_with(&Default::default(), Quality::Highest, Backend::Native)
    }

    pub fn watch_with(
        &self,
        playback_config: &PlaybackConfig,
        quality: Quality,
        backend: Backend,
    ) -> Result<WatchTrack> {
        let info = self.catalog.video.as_ref().context("no video published")?;
        let track_name =
            select_video_rendition(&info.renditions, quality).context("no video renditions")?;
        self.watch_rendition(playback_config, &track_name, backend)
    }

    pub fn watch_rendition(
        &self,
        playback_config: &PlaybackConfig,
        name: &str,
        _backend: Backend,
    ) -> Result<WatchTrack> {
        let video = self.catalog.video.as_ref().context("no video published")?;
        let config = video.renditions.get(name).context("rendition not found")?;
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&Track {
            name: name.to_string(),
            priority: video.priority,
        }));
        let span = info_span!("videodec", %name);
        WatchTrack::spawn::<FfmpegVideoDecoder>(
            name.to_string(),
            consumer,
            &config,
            playback_config,
            self.shutdown.child_token(),
            span,
        )
    }
    pub fn listen(&self, output: impl AudioSink) -> Result<AudioTrack> {
        self.listen_with(Quality::Highest, output)
    }

    pub fn listen_with(&self, quality: Quality, output: impl AudioSink) -> Result<AudioTrack> {
        debug!(catalog=?self.catalog,"catalog");
        let info = self.catalog.audio.as_ref().context("no audio published")?;
        let track_name =
            select_audio_rendition(&info.renditions, quality).context("no audio renditions")?;
        self.listen_rendition(&track_name, output)
    }

    pub fn listen_rendition(&self, name: &str, output: impl AudioSink) -> Result<AudioTrack> {
        let audio = self.catalog.audio.as_ref().context("no video published")?;
        let config = audio.renditions.get(name).context("rendition not found")?;
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&Track {
            name: name.to_string(),
            priority: audio.priority,
        }));
        let span = info_span!("audiodec", %name);
        AudioTrack::spawn::<FfmpegAudioDecoder>(
            name.to_string(),
            consumer,
            config.clone(),
            output,
            self.shutdown.child_token(),
            span,
        )
    }

    pub fn video_renditions(&self) -> impl Iterator<Item = &str> {
        self.catalog
            .video
            .as_ref()
            .into_iter()
            .map(|v| v.renditions.iter())
            .flatten()
            .map(|(name, _config)| name.as_str())
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

fn select_rendition<T, P: ToString>(
    renditions: &HashMap<String, T>,
    order: &[P],
) -> Option<String> {
    order
        .iter()
        .map(ToString::to_string)
        .find(|k| renditions.contains_key(k.as_str()))
        .or_else(|| renditions.keys().next().cloned())
}

fn select_video_rendition<'a, T>(renditions: &'a HashMap<String, T>, q: Quality) -> Option<String> {
    use av::VideoPreset::*;
    let order = match q {
        Quality::Highest => [P1080, P720, P360, P180],
        Quality::High => [P720, P360, P180, P1080],
        Quality::Mid => [P360, P180, P720, P1080],
        Quality::Low => [P180, P360, P720, P1080],
    };

    select_rendition(renditions, &order)
}

fn select_audio_rendition<'a, T>(renditions: &'a HashMap<String, T>, q: Quality) -> Option<String> {
    use av::AudioPreset::*;
    let order = match q {
        Quality::Highest | Quality::High => [Hq, Lq],
        Quality::Mid | Quality::Low => [Lq, Hq],
    };
    select_rendition(renditions, &order)
}

#[derive(Clone, Default)]
pub struct PlaybackConfig {
    pub pixel_format: PixelFormat,
}

pub struct AudioTrack {
    name: String,
    // pub handle: audio::OutputControl,
    shutdown: CancellationToken,
    _task_handle: AbortOnDropHandle<()>,
    _thread_handle: std::thread::JoinHandle<()>,
}

impl AudioTrack {
    fn spawn<D: AudioDecoder>(
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
            shutdown,
        })
    }

    pub fn rendition(&self) -> &str {
        &self.name
    }

    fn run_loop(
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

impl Drop for AudioTrack {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

pub struct WatchTrack {
    rendition: String,
    video_frames: mpsc::Receiver<DecodedFrame>,
    viewport: n0_watcher::Watchable<(u32, u32)>,
    shutdown: CancellationToken,
    _task_handle: AbortOnDropHandle<()>,
    _thread_handle: std::thread::JoinHandle<()>,
}

impl WatchTrack {
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

    fn spawn<D: VideoDecoder>(
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
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
            shutdown,
        })
    }

    fn run_loop(
        shutdown: &CancellationToken,
        mut packet_rx: mpsc::Receiver<hang::Frame>,
        frame_tx: mpsc::Sender<av::DecodedFrame>,
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

impl Drop for WatchTrack {
    fn drop(&mut self) {
        self.shutdown.cancel();
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

pub struct EncoderThread {
    _thread_handle: std::thread::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl EncoderThread {
    pub fn spawn_video(
        mut frames_rx: tokio::sync::watch::Receiver<Option<crate::av::VideoFrame>>,
        mut encoder: impl VideoEncoder + Send + 'static,
        mut producer: hang::TrackProducer,
        format: crate::av::VideoFormat,
        shutdown: CancellationToken,
        span: Span,
    ) -> Self {
        let handle = std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                tracing::debug!(
                    src_format = ?format,
                    dst_config = ?encoder.config(),
                    "video encoder thread start"
                );
                let framerate = encoder.config().framerate.unwrap_or(30.0);
                let interval = Duration::from_secs_f64(1. / framerate);
                loop {
                    let start = Instant::now();
                    if shutdown.is_cancelled() {
                        debug!("stop video encoder: cancelled");
                        break;
                    }
                    let frame = frames_rx.borrow_and_update().clone();
                    if let Some(frame) = frame {
                        if let Err(err) = encoder.push_frame(&format, frame) {
                            warn!("video encoder failed: {err:#}");
                            break;
                        };
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    std::thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                tracing::debug!("video encoder thread stop");
            }
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }

    pub fn spawn_audio(
        source: impl AudioSource + Send + 'static,
        encoder: impl AudioEncoder + Send + 'static,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
        span: tracing::Span,
    ) -> Self {
        let sd = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let _guard = span.enter();
            tracing::debug!(config=?encoder.config(), "audio encoder thread start");
            let mut source = source;
            let mut encoder = encoder;
            let shutdown = sd;
            // 20ms framing to align with typical Opus config (48kHz → 960 samples/ch)
            const INTERVAL: Duration = Duration::from_millis(20);
            let config = encoder.config();
            let samples_per_frame = (config.sample_rate / 1000) * INTERVAL.as_millis() as u32;
            let mut buf = vec![0.0f32; samples_per_frame as usize * config.channel_count as usize];
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                let start = Instant::now();
                match source.pop_samples(&mut buf) {
                    Ok(Some(n)) => {
                        // Expect a full frame; if shorter, zero-pad via slice len
                        let n = n.min(buf.len());
                        if let Err(err) = encoder.push_samples(&buf[..n]) {
                            error!("audio push_samples failed: {err:#}");
                            break;
                        }
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    Ok(None) => {
                        // keep pacing
                    }
                    Err(err) => {
                        error!("audio source failed: {err:#}");
                        break;
                    }
                }
                let sleep = INTERVAL.saturating_sub(start.elapsed());
                std::thread::sleep(sleep);
            }
            // drain
            while let Ok(Some(pkt)) = encoder.pop_packet() {
                producer.write(pkt);
            }
            producer.inner.close();
            tracing::debug!("audio encoder thread stop");
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }
}

impl Drop for EncoderThread {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// #[derive(Clone, Debug)]
// pub struct VideoRendition {
//     pub name: String,
//     pub codec: crate::av::VideoCodec,
//     pub preset: crate::av::VideoPreset,
// }

// impl VideoRendition {
//     fn from_parts(name: &str, config: &VideoConfig) -> Option<Self> {
//         let codec = match config.codec {
//             hang::catalog::VideoCodec::H264(_) => Some(crate::av::VideoCodec::H264),
//             hang::catalog::VideoCodec::AV1(_) => Some(crate::av::VideoCodec::Av1),
//             _ => None,
//         }?;
//         let preset = crate::av::VideoPreset::from_str(name).ok()?;
//         Some(Self {
//             name: name.to_owned(),
//             codec,
//             preset,
//         })
//     }
// }
