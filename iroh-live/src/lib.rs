use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use hang::{Catalog, CatalogConsumer, CatalogProducer, TrackConsumer};
use iroh::{Endpoint, EndpointAddr, EndpointId, endpoint::Connection, protocol::ProtocolHandler};
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer, Track};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, error_span, info, instrument, warn};
use web_transport_iroh::Request;

use crate::{
    audio::{AudioBackend, OutputControl},
    av::{AudioEncoder, AudioSource, PixelFormat, VideoEncoder, VideoSource},
    video::DecodedFrame,
};

pub mod audio;
pub mod av;
pub mod ffmpeg;
pub mod native;
mod ffmpeg_ext;
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
    pub remote: EndpointId,
    pub session: moq_lite::Session<web_transport_iroh::Session>,
    pub publish: OriginProducer,
    pub subscribe: OriginConsumer,
}

impl LiveSession {
    pub async fn connect(session: web_transport_iroh::Session) -> Result<Self> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let remote = session.remote_id();
        let session = moq_lite::Session::connect(session, publish.consumer, subscribe.producer)
            .await
            .std_context("failed to accept session")?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            remote,
            session,
        })
    }
    pub async fn accept(session: web_transport_iroh::Session) -> Result<Self> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let remote = session.remote_id();
        let session = moq_lite::Session::accept(session, publish.consumer, subscribe.producer)
            .await
            .std_context("failed to accept session")?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            remote,
            session,
        })
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
            session,
            publish,
            subscribe: _,
        } = session;
        for (name, producer) in self.broadcasts.iter() {
            publish.publish_broadcast(name.to_string(), producer.consume());
        }
        self.sessions.insert(remote, SessionState { publish });

        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    session.close(moq_lite::Error::Cancel);
                    Ok(())
                }
                result = session.closed() => result,
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
        }
    }

    pub fn set_video<I, E>(&mut self, source: impl VideoSource + Send + 'static, renditions: I) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::VideoPreset)>,
        E: VideoEncoder + Send + 'static,
    {
        let priority = 1u8;
        let mut encoders: Vec<(String, E, hang::TrackProducer)> = Vec::new();
        let mut renditions_cfg = HashMap::new();
        for (enc, preset) in renditions.into_iter() {
            let name = preset.as_name().to_owned();
            renditions_cfg.insert(name.clone(), enc.config());
            let track = self.broadcast.create_track(Track { name: name.clone(), priority });
            encoders.push((name, enc, hang::TrackProducer::new(track)));
        }
        let video = hang::catalog::Video { renditions: renditions_cfg, priority, display: None, rotation: None, flip: None, detection: None };
        self.catalog.set_video(Some(video.clone()));
        self.catalog.publish();
        let mut catalog = self.catalog.clone();
        let shutdown = self.shutdown.child_token();
        let _handle = std::thread::spawn(move || {
            // Fanout loop: read once, encode to all renditions
            let mut src = source;
            let format = src.format();
            loop {
                if shutdown.is_cancelled() { break; }
                match src.pop_frame() {
                    Ok(Some(frame)) => {
                        for (_, enc, _) in encoders.iter_mut() {
                            let _ = enc.push_frame(&format, crate::av::VideoFrame { raw: frame.raw.clone() });
                        }
                        for (_, enc, prod) in encoders.iter_mut() {
                            while let Ok(Some(pkt)) = enc.pop_packet() { prod.write(pkt); }
                        }
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    Err(err) => { tracing::error!("video source failed: {err:#}"); break; }
                }
            }
            for (_, _, mut prod) in encoders.into_iter() { prod.inner.close(); }
            catalog.set_video(None); catalog.publish();
        });
        Ok(())
    }

    pub fn set_audio<I, E>(&mut self, source: impl AudioSource + Send + 'static, renditions: I) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::AudioPreset)>,
        E: AudioEncoder + Send + 'static,
    {
        let priority = 2u8;
        let mut encoders: Vec<(String, E, hang::TrackProducer)> = Vec::new();
        let mut renditions_cfg = HashMap::new();
        for (enc, preset) in renditions.into_iter() {
            let name = format!("{:}", preset); // uses Display from strum
            renditions_cfg.insert(name.clone(), enc.config());
            let track = self.broadcast.create_track(Track { name: name.clone(), priority });
            encoders.push((name, enc, hang::TrackProducer::new(track)));
        }
        let audio = hang::catalog::Audio { renditions: renditions_cfg, priority, captions: None, speaking: None };
        self.catalog.set_audio(Some(audio));
        self.catalog.publish();
        let mut catalog = self.catalog.clone();
        let shutdown = self.shutdown.child_token();
        let _handle = std::thread::spawn(move || {
            const INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
            let mut src = source;
            let fmt = src.format();
            let samples_per_frame = (fmt.sample_rate / 1000) * INTERVAL.as_millis() as u32;
            let mut buf = vec![0.0f32; samples_per_frame as usize * fmt.channel_count as usize];
            loop {
                if shutdown.is_cancelled() { break; }
                match src.pop_samples(&mut buf) {
                    Ok(Some(_)) => {
                        for (_, enc, _) in encoders.iter_mut() { let _ = enc.push_samples(&buf); }
                        for (_, enc, prod) in encoders.iter_mut() { while let Ok(Some(pkt)) = enc.pop_packet() { prod.write(pkt); } }
                    }
                    Ok(None) => std::thread::sleep(INTERVAL),
                    Err(err) => { tracing::error!("audio source failed: {err:#}"); break; }
                }
                std::thread::sleep(INTERVAL);
            }
            for (_, _, mut prod) in encoders.into_iter() { prod.inner.close(); }
            catalog.set_audio(None); catalog.publish();
        });
        Ok(())
    }
}

fn audio_loop(
    mut source: impl AudioSource,
    mut encoder: impl AudioEncoder,
    producer: &mut hang::TrackProducer,
    shutdown: CancellationToken,
) {
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
}

fn video_loop(
    mut source: impl VideoSource + Send + 'static,
    mut encoder: impl VideoEncoder + Send + 'static,
    producer: &mut hang::TrackProducer,
    shutdown: CancellationToken,
) {
    let format = source.format();
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        match source.pop_frame() {
            Ok(Some(frame)) => {
                if let Err(err) = encoder.push_frame(&format, frame) {
                    error!("video encoder failed: {err:#}");
                    break;
                }
                while let Ok(Some(pkt)) = encoder.pop_packet() {
                    producer.write(pkt);
                }
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(err) => {
                error!("video source failed: {err:#}");
                break;
            }
        }
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

    pub fn watch(&self, playback_config: &PlaybackConfig) -> Result<WatchTrack> {
        self.watch_with(playback_config, Quality::Highest)
    }

    pub fn watch_with(&self, playback_config: &PlaybackConfig, quality: Quality) -> Result<WatchTrack> {
        let info = self.catalog.video.as_ref().context("no video published")?;
        let (track_name, config) = select_rendition(&info.renditions, quality)
            .unwrap_or_else(|| info.renditions.iter().next().expect("no renditions"));
        let track = Track {
            name: track_name.to_string(),
            priority: info.priority,
        };
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&track));

        // Select decoder based on codec
        let (frame_rx, resize_tx) = match &config.codec {
            hang::catalog::VideoCodec::AV1(_av1) => {
                let (ctx, frame_rx, resize_tx, packet_tx) =
                    crate::native::video::DecoderContext::new(
                        self.shutdown.child_token(),
                        playback_config.pixel_format,
                    );
                let _decoder = crate::native::video::Decoder::new(config, ctx)?;
                let _task = n0_future::task::spawn(forward_frames(consumer, packet_tx.clone()));
                // Adapt native frames to the FFmpeg DecodedFrame type used in WatchTrack
                // Use capacity 1 and drop on overflow to always show the latest frame
                let (compat_tx, compat_rx) = tokio::sync::mpsc::channel(1);
                tokio::spawn(async move {
                    let mut rx = frame_rx;
                    while let Some(f) = rx.recv().await {
                        let adapted = video::DecodedFrame { frame: f.frame, timestamp: f.timestamp };
                        if compat_tx.try_send(adapted).is_err() {
                            // drop if receiver is busy; keep only the latest frame semantics
                        }
                    }
                });
                (compat_rx, resize_tx)
            }
            _ => {
                let (ctx, frame_rx, resize_tx, packet_tx) =
                    crate::video::DecoderContext::new(
                        self.shutdown.child_token(),
                        playback_config.pixel_format,
                    );
                let _decoder = crate::video::Decoder::new(config, ctx)?;
                let _task = n0_future::task::spawn(forward_frames(consumer, packet_tx.clone()));
                (frame_rx, resize_tx)
            }
        };
        
        let watch_track = WatchTrack {
            video_frames: frame_rx,
            resize_tx,
        };
        Ok(watch_track)
    }

    pub async fn listen(&self, audio_ctx: AudioBackend) -> Result<AudioTrack> {
        self.listen_with(audio_ctx, Quality::Highest).await
    }

    pub async fn listen_with(&self, audio_ctx: AudioBackend, quality: Quality) -> Result<AudioTrack> {
        let info = self.catalog.audio.as_ref().context("no audio published")?;
        let (track_name, config) = select_rendition(&info.renditions, quality)
            .unwrap_or_else(|| info.renditions.iter().next().expect("no renditions"));
        let track = Track {
            name: track_name.to_string(),
            priority: info.priority,
        };
        let consumer = TrackConsumer::new(self.broadcast.subscribe_track(&track));
        let shutdown = self.shutdown.child_token();
        let audio_stream = audio_ctx.output_stream(config.clone()).await?;
        let input = audio::new_decoder(&config, audio_stream.clone(), shutdown)
            .context("failed to create audio decoder")?;
        let _task = tokio::spawn(async move { forward_frames(consumer, input).await });
        let audio_track = AudioTrack {
            handle: OutputControl::new(audio_stream),
        };
        Ok(audio_track)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Quality { Highest, High, Mid, Low }

fn select_rendition<'a, T>(r: &'a HashMap<String, T>, q: Quality) -> Option<(&'a String, &'a T)> {
    let order = match q {
        Quality::Highest => ["1080p","720p","360p","180p"],
        Quality::High => ["720p","360p","180p","1080p"],
        Quality::Mid => ["360p","180p","720p","1080p"],
        Quality::Low => ["180p","360p","720p","1080p"],
    };
    for name in order {
        if let Some(cfg) = r.get(name) {
            let key = r.keys().find(|k| k.as_str() == name).unwrap();
            return Some((key, cfg));
        }
    }
    None
}

#[derive(Clone, Default)]
pub struct PlaybackConfig {
    pub pixel_format: PixelFormat,
}

pub struct AudioTrack {
    pub handle: audio::OutputControl,
}

pub struct WatchTrack {
    video_frames: video::FrameReceiver,
    resize_tx: video::ResizeSender,
}

impl WatchTrack {
    pub fn set_viewport(&self, w: u32, h: u32) {
        self.resize_tx.send((w, h)).ok();
    }

    pub fn current_frame(&mut self) -> Option<DecodedFrame> {
        let mut out = None;
        while let Ok(item) = self.video_frames.try_recv() {
            out = Some(item);
        }
        out
    }
}

async fn forward_frames(
    mut track: hang::TrackConsumer,
    sender: mpsc::Sender<hang::Frame>,
) -> Result<(), anyhow::Error> {
    loop {
        let frame = track.read().await;
        match frame {
            Ok(Some(frame)) => {
                if sender.send(frame).await.is_err() {
                    // Receiver dropped, shutdown gracefully
                    break Ok(());
                }
            }
            Ok(None) => break Ok(()),
            Err(err) => {
                warn!("failed to read frame: {err:?}");
                break Err(err.into());
            }
        }
    }
}
