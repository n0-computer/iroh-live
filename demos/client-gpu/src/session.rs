//! The client's connection to the gpu node.
//!
//! A [`Session`] owns the endpoint, the published video, the control track and
//! the player of the returned video. Settings that cannot change on a live
//! session, the congestion controller, need a new one.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use iroh::{
    EndpointId,
    endpoint::{Builder, QuicTransportConfig},
};
use iroh_live::{
    EndpointOptions, Live, Mdns,
    media::{
        Latency, LocalBroadcast, Player, PlayerConfig, VideoFrames,
        video::{self, Frame},
    },
};
use n0_future::task::{AbortOnDropHandle, spawn};
use noq_proto::congestion::{Bbr3Config, CubicConfig, NewRenoConfig};
use tokio::sync::{oneshot, watch};

use crate::{
    Offer, control,
    filter::Filter,
    pipeline::{Pipeline, VideoSettings},
    stamp::SentLog,
};

/// How long to wait for the gpu node to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// A congestion controller for the client's own uplink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Congestion {
    /// BBR3, which the MoQ preset uses.
    #[default]
    Bbr3,
    /// CUBIC, iroh's default.
    Cubic,
    /// NewReno.
    NewReno,
}

impl Congestion {
    /// Every controller, in menu order.
    pub const ALL: [Self; 3] = [Self::Bbr3, Self::Cubic, Self::NewReno];

    /// Returns the name shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bbr3 => "BBR3",
            Self::Cubic => "CUBIC",
            Self::NewReno => "NewReno",
        }
    }

    fn transport_config(self) -> QuicTransportConfig {
        let builder = QuicTransportConfig::builder();
        match self {
            Self::Bbr3 => builder.congestion_controller_factory(Arc::new(Bbr3Config::default())),
            Self::Cubic => builder.congestion_controller_factory(Arc::new(CubicConfig::default())),
            Self::NewReno => {
                builder.congestion_controller_factory(Arc::new(NewRenoConfig::default()))
            }
        }
        .build()
    }
}

/// Everything the client lets the user change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The published video. Applies live.
    pub video: VideoSettings,
    /// The gpu filter. Applies live.
    pub filter: Filter,
    /// The minimum of the returned video's player latency, in ms. Applies live.
    pub latency_min_ms: u64,
    /// The maximum of the returned video's player latency, in ms. Applies live.
    pub latency_max_ms: u64,
    /// The congestion controller. Needs a new endpoint, so a reconnect.
    pub congestion: Congestion,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            video: VideoSettings::default(),
            filter: Filter::None,
            latency_min_ms: 0,
            latency_max_ms: 150,
            congestion: Congestion::default(),
        }
    }
}

impl Settings {
    fn latency(&self) -> Latency {
        Latency {
            min: Duration::from_millis(self.latency_min_ms),
            max: Duration::from_millis(self.latency_max_ms),
        }
    }
}

/// Calls `wake` whenever the user interface should repaint.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A connected client.
#[derive(derive_more::Debug)]
pub struct Session {
    live: Live,
    broadcast: LocalBroadcast,
    sent: Arc<SentLog>,
    #[debug(skip)]
    wake: Wake,
    filter: watch::Sender<Filter>,
    pipeline: Option<Pipeline>,
    pending: Option<oneshot::Receiver<Result<Pipeline>>>,
    player: Player,
    returned: VideoFrames,
    _offer: Offer,
    _control: control::Sender,
    _wake_returned: AbortOnDropHandle<()>,
}

impl Session {
    /// Connects to `gpu`: publishes the video and plays what comes back.
    ///
    /// # Errors
    ///
    /// Fails if the endpoint cannot bind, the source cannot open, or the gpu
    /// node does not answer in time.
    pub async fn connect(gpu: EndpointId, settings: &Settings, wake: Wake) -> Result<Self> {
        let options = EndpointOptions {
            mdns: Mdns::Announce,
            ..EndpointOptions::from_env()?
        };
        // The preset installs BBR3, so the transport config comes after it.
        let builder = options.apply(Builder::empty()).await;
        let endpoint = builder
            .transport_config(settings.congestion.transport_config())
            .bind()
            .await
            .context("cannot bind the endpoint")?;
        let live = Live::builder(endpoint).with_router().spawn();

        let broadcast = LocalBroadcast::new();
        let sent = Arc::new(SentLog::default());
        let pipeline = {
            let wake = wake.clone();
            Pipeline::start(&broadcast, &settings.video, sent.clone(), move || wake()).await?
        };
        let offer = Offer::new(&live, gpu, &broadcast)?;
        let (filter, filter_rx) = watch::channel(settings.filter);
        let control = control::Sender::spawn(&live, gpu, filter_rx)?;

        let subscription = tokio::time::timeout(ANSWER_TIMEOUT, live.answer(gpu))
            .await
            .context("the gpu node did not answer: is it running?")??;
        let config = PlayerConfig {
            latency: settings.latency(),
            // The latency probe reads the returned pixels on the CPU.
            decoder: video::decode::Kind::Software,
            ..PlayerConfig::default()
        };
        let player = live.remote_broadcast(&subscription).play(config)?;
        let player_frames = player.video();
        let mut frames = player_frames.clone();
        let wake_returned = {
            let wake = wake.clone();
            spawn(async move {
                while frames.next().await.is_some() {
                    wake();
                }
            })
        };
        Ok(Self {
            live,
            broadcast,
            sent,
            wake,
            filter,
            pipeline: Some(pipeline),
            pending: None,
            player,
            returned: player_frames,
            _offer: offer,
            _control: control,
            _wake_returned: AbortOnDropHandle::new(wake_returned),
        })
    }

    /// Returns the endpoint id of this client.
    pub fn id(&self) -> EndpointId {
        self.live.endpoint().id()
    }

    /// Returns the newest local preview frame, if one is new.
    ///
    /// These are the stamped frames as sent.
    pub fn next_local(&mut self) -> Option<Arc<Frame>> {
        self.pipeline.as_mut()?.local.try_next()
    }

    /// Returns the newest frame that came back from the gpu node, if one is new.
    pub fn next_returned(&mut self) -> Option<Arc<Frame>> {
        self.returned.try_next()
    }

    /// Returns the time from the capture of `frame`'s original to now.
    ///
    /// Call it right after the frame is drawn. `None` if the frame has no
    /// readable id or the id is not a recent one.
    pub fn latency_of(&self, frame: &Frame) -> Option<Duration> {
        crate::stamp::read(frame).and_then(|id| self.sent.since(id))
    }

    /// Sends a new filter to the gpu node.
    pub fn set_filter(&self, filter: Filter) {
        self.filter.send_replace(filter);
    }

    /// Changes the returned video's player latency.
    ///
    /// # Errors
    ///
    /// Fails if `min` is above `max`.
    pub fn set_latency(&self, settings: &Settings) -> Result<()> {
        Ok(self.player.set_latency(settings.latency())?)
    }

    /// Replaces the source and encoding, as `settings` say.
    ///
    /// The old source stops first, so a camera is free to open again. The new
    /// pipeline arrives later: call [`poll`](Self::poll).
    pub fn restart_video(&mut self, settings: VideoSettings) {
        self.pipeline = None;
        let (tx, rx) = oneshot::channel();
        let (broadcast, sent, wake) =
            (self.broadcast.clone(), self.sent.clone(), self.wake.clone());
        spawn(async move {
            let started = Pipeline::start(&broadcast, &settings, sent, move || wake()).await;
            tx.send(started).ok();
        });
        self.pending = Some(rx);
    }

    /// Takes in a restarted pipeline, and returns why a restart failed.
    pub fn poll(&mut self) -> Option<String> {
        let rx = self.pending.as_mut()?;
        match rx.try_recv() {
            Ok(Ok(pipeline)) => self.pipeline = Some(pipeline),
            Ok(Err(err)) => {
                self.pending = None;
                return Some(format!("{err:#}"));
            }
            Err(oneshot::error::TryRecvError::Empty) => return None,
            Err(oneshot::error::TryRecvError::Closed) => {}
        }
        self.pending = None;
        None
    }

    /// Returns whether a restarted pipeline is still opening.
    pub fn is_restarting(&self) -> bool {
        self.pending.is_some()
    }

    /// Closes the endpoint.
    pub async fn shutdown(self) {
        self.live.shutdown().await;
    }
}
