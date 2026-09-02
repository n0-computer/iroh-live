//! Subscribing to a broadcast: catalog, decode, playout.
//!
//! [`RemoteBroadcast`] watches a broadcast's catalog and hands out a
//! [`VideoTrack`] and an [`AudioTrack`]. Decoding itself is
//! `moq_video::decode::Consumer` and `moq_audio::decode::Consumer`; what this
//! adds is the three things upstream has no counterpart for.
//!
//! The first is rendition selection. `moq_mux::select` is fixed at
//! construction, so a subscriber that wants to follow its downlink has to
//! choose for itself. [`VideoTrack::enable_adaptation`] feeds transport signals
//! through [`crate::adaptive`] and switches renditions when they say so, opening
//! the replacement alongside the incumbent and swapping on its first frame so
//! the picture never goes blank.
//!
//! The second is the playout clock ([`crate::sync`]), which keeps audio and
//! video aligned across two independent decode paths.
//!
//! The third is the catalog extension: chat and publisher identity ride
//! alongside the media sections, and this is where a subscriber reads them.

use std::sync::{Arc, Mutex};

use hang::catalog::{AudioConfig, VideoConfig};
use moq_mux::catalog::Stream as _;
use n0_error::{Result, e, stack_error};
use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::{Watchable, Watcher as _};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, warn};

use crate::{
    catalog::{Catalog, IrohLiveExt, TrackRef, User},
    frame_channel::FrameReceiver,
    net::NetworkSignals,
    playout::{PlaybackPolicy, SyncMode},
    stats::SubscribeStats,
    sync::Sync,
};

mod adapt;
#[cfg(feature = "playback")]
mod audio;
mod video;

/// Errors raised while subscribing.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum SubscribeError {
    /// The transport rejected a track or broadcast operation.
    #[error(transparent)]
    Net(#[error(source, std_err)] moq_net::Error),
    /// The catalog could not be read.
    #[error(transparent)]
    Mux(#[error(source, std_err)] moq_mux::Error),
    /// A video decoder failed.
    #[error(transparent)]
    Video(#[error(source, std_err)] moq_video::Error),
    /// An audio decoder or the playback device failed.
    #[error(transparent)]
    Audio(#[error(source, std_err)] moq_audio::Error),
    /// The broadcast closed before it published a catalog.
    #[error("broadcast closed before publishing a catalog")]
    NoCatalog,
    /// The broadcast has no track of the requested medium.
    #[error("broadcast has no {medium} track")]
    NoTrack {
        /// Either `video` or `audio`.
        medium: &'static str,
    },
    /// The named rendition is not in the catalog.
    #[error("no rendition named {name}")]
    NoRendition {
        /// The name that was asked for.
        name: String,
    },
}

/// A snapshot of a broadcast's catalog.
///
/// Cheap to clone; hand it around rather than re-reading the catalog.
#[derive(Debug, Clone, Default)]
pub struct CatalogSnapshot(Arc<Catalog>);

/// Compares by identity, not content.
///
/// Two snapshots carrying the same catalog are not equal, and even
/// `CatalogSnapshot::default()` differs from another `default()`, because each
/// allocates. That is deliberate: `Watchable` needs `Eq` to tell an update from
/// a repeat, hang's catalog carries floats and so is only `PartialEq`, and every
/// update allocates a fresh snapshot, so identity never swallows one.
impl PartialEq for CatalogSnapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CatalogSnapshot {}

impl CatalogSnapshot {
    /// Returns the video renditions, in catalog order.
    pub fn video(&self) -> &std::collections::BTreeMap<String, VideoConfig> {
        &self.0.video.renditions
    }

    /// Returns the audio renditions, in catalog order.
    pub fn audio(&self) -> &std::collections::BTreeMap<String, AudioConfig> {
        &self.0.audio.renditions
    }

    /// Returns the publisher's advertised identity, if it set one.
    pub fn user(&self) -> Option<&User> {
        self.0.ext.user.as_ref()
    }

    /// Returns the chat track the publisher advertised, if any.
    pub fn chat(&self) -> Option<&TrackRef> {
        self.0.ext.chat.as_ref()?.message.as_ref()
    }

    /// Returns the underlying catalog.
    pub fn inner(&self) -> &Catalog {
        &self.0
    }

    /// Picks the highest-quality video rendition, by pixel count then bitrate.
    ///
    /// This is the starting point for a subscription; adaptation moves off it
    /// as soon as the transport says the downlink cannot carry it.
    pub fn best_video(&self) -> Option<&str> {
        let best = crate::adaptive::rank_renditions(self.video())
            .into_iter()
            .next()?
            .name;
        self.video()
            .get_key_value(&best)
            .map(|(key, _)| key.as_str())
    }

    /// Picks the first audio rendition, which is the only one publishers make.
    pub fn first_audio(&self) -> Option<&str> {
        self.audio().keys().next().map(String::as_str)
    }
}

/// A broadcast this node subscribes to.
///
/// Created from a `moq_net` broadcast consumer, which for iroh-live comes from
/// [`iroh_moq::MoqSession::subscribe`](https://docs.rs/iroh-moq).
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    inner: Arc<Inner>,
}

#[derive(derive_more::Debug)]
struct Inner {
    name: String,
    #[debug(skip)]
    broadcast: moq_net::broadcast::Consumer,
    catalog: Watchable<CatalogSnapshot>,
    policy: Mutex<PlaybackPolicy>,
    stats: SubscribeStats,
    sync: Sync,
    shutdown: CancellationToken,
    _catalog_task: AbortOnDropHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Everything this broadcast started watches one of these two: the decode
        // tasks and the adaptation loop watch the token, and a `wait_async` in
        // flight watches the clock. Without this, a caller that simply drops a
        // subscription leaves them running, and the stats recorder in
        // `iroh-live` holds a live `Connection` while it does.
        self.shutdown.cancel();
        self.sync.close();
    }
}

impl RemoteBroadcast {
    /// Subscribes to `broadcast` and waits for its first catalog.
    ///
    /// # Errors
    ///
    /// Fails if the catalog track is missing, unreadable, or the broadcast
    /// closes before publishing one.
    pub async fn new(
        name: impl Into<String>,
        broadcast: moq_net::broadcast::Consumer,
    ) -> Result<Self, SubscribeError> {
        Self::with_playback_policy(name, broadcast, PlaybackPolicy::default()).await
    }

    /// Subscribes to `broadcast` with an explicit playout policy.
    ///
    /// # Errors
    ///
    /// See [`new`](Self::new).
    pub async fn with_playback_policy(
        name: impl Into<String>,
        broadcast: moq_net::broadcast::Consumer,
        policy: PlaybackPolicy,
    ) -> Result<Self, SubscribeError> {
        let name = name.into();
        let mut consumer =
            moq_mux::catalog::Consumer::<IrohLiveExt>::new(&broadcast, Default::default()).await?;

        // The first catalog is what tells us the broadcast has any tracks at
        // all, so wait for it here rather than handing back a handle whose
        // every accessor would answer "not yet".
        let first = consumer
            .next()
            .await?
            .ok_or_else(|| e!(SubscribeError::NoCatalog))?;
        if tracing::enabled!(tracing::Level::TRACE)
            && let Ok(json) = serde_json::to_string(&first)
        {
            tracing::trace!(broadcast = %name, catalog = %json, "first catalog");
        }
        let catalog = Watchable::new(CatalogSnapshot(Arc::new(first)));

        let task = {
            let catalog = catalog.clone();
            let name = name.clone();
            spawn(
                async move {
                    loop {
                        match consumer.next().await {
                            Ok(Some(next)) => {
                                // At trace, because a catalog is the first thing
                                // to look at when a publisher and a subscriber
                                // disagree about what is on the wire.
                                if tracing::enabled!(tracing::Level::TRACE)
                                    && let Ok(json) = serde_json::to_string(&next)
                                {
                                    tracing::trace!(catalog = %json, "catalog updated");
                                }
                                catalog.set(CatalogSnapshot(Arc::new(next))).ok();
                            }
                            Ok(None) => {
                                debug!("catalog track ended");
                                break;
                            }
                            Err(err) => {
                                warn!(error = %err, "catalog track failed");
                                break;
                            }
                        }
                    }
                }
                .instrument(error_span!("catalog", broadcast = %name)),
            )
        };

        Ok(Self {
            inner: Arc::new(Inner {
                name,
                broadcast,
                catalog,
                policy: Mutex::new(policy),
                stats: SubscribeStats::default(),
                sync: Sync::new(),
                shutdown: CancellationToken::new(),
                _catalog_task: AbortOnDropHandle::new(task),
            }),
        })
    }

    /// Returns the broadcast's name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Returns the underlying consumer, for tracks this crate does not model.
    pub fn consumer(&self) -> &moq_net::broadcast::Consumer {
        &self.inner.broadcast
    }

    /// Returns the latest catalog.
    pub fn catalog(&self) -> CatalogSnapshot {
        self.inner.catalog.get()
    }

    /// Returns a watcher that yields every catalog update.
    pub fn catalog_watcher(&self) -> n0_watcher::Direct<CatalogSnapshot> {
        self.inner.catalog.watch()
    }

    /// Reports whether the broadcast carries video.
    pub fn has_video(&self) -> bool {
        !self.catalog().video().is_empty()
    }

    /// Reports whether the broadcast carries audio.
    pub fn has_audio(&self) -> bool {
        !self.catalog().audio().is_empty()
    }

    /// Subscribes to the chat track the catalog advertises.
    ///
    /// Returns the raw track: chat is not a media concern, so the caller owns
    /// the message codec.
    pub fn chat(&self) -> Option<moq_net::track::Consumer> {
        let track = self.catalog().chat()?.clone();
        self.inner.broadcast.track(&track.name).ok()
    }

    /// Returns the publisher's advertised identity, if it set one.
    pub fn user(&self) -> Option<User> {
        self.catalog().user().cloned()
    }

    /// Returns the subscribe-side counters, for a UI or a log.
    pub fn stats(&self) -> &SubscribeStats {
        &self.inner.stats
    }

    /// Returns the shared playout clock, so a caller can retune its jitter.
    pub fn sync(&self) -> &Sync {
        &self.inner.sync
    }

    /// Returns the current playout policy.
    pub fn playback_policy(&self) -> PlaybackPolicy {
        self.inner.policy.lock().expect("poisoned").clone()
    }

    /// Replaces the playout policy. Takes effect on the next track opened.
    pub fn set_playback_policy(&self, policy: PlaybackPolicy) {
        *self.inner.policy.lock().expect("poisoned") = policy;
    }

    /// Opens the best video rendition and starts decoding it.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast has no video track, or the decoder cannot open.
    pub async fn video(&self) -> Result<VideoTrack, SubscribeError> {
        let catalog = self.catalog();
        let name = catalog
            .best_video()
            .ok_or_else(|| e!(SubscribeError::NoTrack { medium: "video" }))?
            .to_string();
        self.video_rendition(&name).await
    }

    /// Opens a named video rendition and starts decoding it.
    ///
    /// # Errors
    ///
    /// Fails if the rendition is not in the catalog, or the decoder cannot open.
    pub async fn video_rendition(&self, name: &str) -> Result<VideoTrack, SubscribeError> {
        video::open(self, name).await
    }

    /// Opens the broadcast's audio track and starts playing it.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast has no audio track, or the playback device
    /// cannot open.
    #[cfg(feature = "playback")]
    pub async fn audio(&self) -> Result<AudioTrack, SubscribeError> {
        let catalog = self.catalog();
        let name = catalog
            .first_audio()
            .ok_or_else(|| e!(SubscribeError::NoTrack { medium: "audio" }))?
            .to_string();
        audio::open(self, &name).await
    }

    /// Opens whichever of video and audio the broadcast carries.
    pub async fn media(&self) -> MediaTracks {
        let video = match self.has_video() {
            true => self
                .video()
                .await
                .inspect_err(|err| warn!(error = %err, "video track failed to open"))
                .ok(),
            false => None,
        };
        #[cfg(feature = "playback")]
        let audio = match self.has_audio() {
            true => self
                .audio()
                .await
                .inspect_err(|err| warn!(error = %err, "audio track failed to open"))
                .ok(),
            false => None,
        };
        #[cfg(not(feature = "playback"))]
        let audio = None;
        MediaTracks { video, audio }
    }

    /// Waits until the broadcast closes.
    pub fn closed(&self) -> impl Future<Output = ()> + 'static {
        let broadcast = self.inner.broadcast.clone();
        async move {
            broadcast.closed().await;
        }
    }

    /// Returns the token every decode task on this broadcast watches.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.inner.shutdown.clone()
    }

    /// Stops every decode task on this broadcast.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.sync.close();
    }
}

/// Whichever tracks a broadcast turned out to carry.
#[derive(Debug, Default)]
pub struct MediaTracks {
    /// The video track, if the broadcast has one and it opened.
    pub video: Option<VideoTrack>,
    /// The audio track, if the broadcast has one and it opened.
    pub audio: Option<AudioTrack>,
}

/// A decoding video track.
///
/// Frames land in a latest-wins slot: a renderer that falls behind skips
/// straight to the newest picture instead of draining a backlog.
///
/// Deliberately not `Clone`. Taking a frame removes it from the slot, so two
/// holders would split the stream between them and neither would see a coherent
/// picture. Share the handle, or hand out [`frames`](Self::frames) by reference
/// to something that only reads.
#[derive(Debug)]
pub struct VideoTrack {
    frames: Arc<FrameReceiver<moq_video::Frame>>,
    rendition: Watchable<String>,
    decoder: Watchable<String>,
    control: Arc<VideoControl>,
}

/// The handles a rendition switch needs, shared with the adaptation task.
#[derive(Debug)]
struct VideoControl {
    broadcast: RemoteBroadcast,
    /// Set to request a switch; the decode supervisor picks it up.
    requested: watch::Sender<Option<String>>,
    /// Bumped to ask the supervisor to build the decoder again. A counter
    /// rather than a flag, so two changes in a row are two rebuilds.
    reopen: watch::Sender<u64>,
    _task: AbortOnDropHandle<()>,
    adaptation: Mutex<Option<AbortOnDropHandle<()>>>,
}

impl VideoTrack {
    /// Takes the newest frame, if one arrived since the last call.
    pub fn take(&self) -> Option<moq_video::Frame> {
        self.frames.take()
    }

    /// Waits for the next frame. Returns `None` once the track ends.
    pub async fn recv(&self) -> Option<moq_video::Frame> {
        self.frames.recv().await
    }

    /// Returns the frame slot, for a renderer that polls it directly.
    pub fn frames(&self) -> &FrameReceiver<moq_video::Frame> {
        &self.frames
    }

    /// Returns the rendition currently decoding.
    pub fn rendition(&self) -> String {
        self.rendition.get()
    }

    /// Returns a watcher over the rendition currently decoding.
    pub fn rendition_watcher(&self) -> n0_watcher::Direct<String> {
        self.rendition.watch()
    }

    /// Returns the name of the decoder backend currently running.
    ///
    /// Which backend opened is the first thing worth knowing when playback
    /// looks wrong on a given device, and it can change across a rendition
    /// switch, so it is watchable too.
    pub fn decoder(&self) -> String {
        self.decoder.get()
    }

    /// Returns a watcher over the decoder backend currently running.
    pub fn decoder_watcher(&self) -> n0_watcher::Direct<String> {
        self.decoder.watch()
    }

    /// Switches to a named rendition, whatever the adaptation logic thinks.
    ///
    /// Returns as soon as the switch is requested. The replacement decoder
    /// opens alongside the incumbent and takes over on its first frame, so the
    /// picture does not go blank across it; [`switched_to`](Self::switched_to)
    /// waits for that handover if the caller needs to know it happened.
    pub fn set_rendition(&self, name: impl Into<String>) {
        self.control.requested.send_replace(Some(name.into()));
    }

    /// Builds the decoder again, from the broadcast's current playback policy.
    ///
    /// A decoder reads
    /// [`PlaybackPolicy`](crate::playout::PlaybackPolicy) when it is built and
    /// never looks at it again, so this is what carries a policy change, a
    /// different backend above all, to a track already playing. The replacement
    /// opens alongside the incumbent and takes over on its first frame, the same
    /// handover a rendition switch makes, so the picture does not go blank.
    ///
    /// Returns as soon as the rebuild is requested. A replacement that fails to
    /// open is logged and the incumbent keeps playing, which is what a backend
    /// that is not present on this machine looks like.
    pub fn reopen_decoder(&self) {
        self.control
            .reopen
            .send_modify(|generation| *generation += 1);
    }

    /// Waits until `name` is the rendition actually producing frames.
    ///
    /// Returns immediately if it already is. A switch that never lands, because
    /// the rendition left the catalog or its decoder failed to open, leaves this
    /// pending, so a caller that cannot wait forever wraps it in a timeout.
    pub async fn switched_to(&self, name: &str) {
        let mut watcher = self.rendition.watch();
        loop {
            if self.rendition.get() == name {
                return;
            }
            if watcher.updated().await.is_err() {
                return;
            }
        }
    }

    /// Follows the downlink: switches renditions as `signals` move.
    ///
    /// Replaces any adaptation already running. Drop the returned track, or
    /// call [`disable_adaptation`](Self::disable_adaptation), to stop.
    pub fn enable_adaptation(&self, signals: watch::Receiver<NetworkSignals>) {
        self.enable_adaptation_with(signals, crate::adaptive::AdaptiveConfig::default());
    }

    /// Follows the downlink with explicit thresholds and timers.
    ///
    /// The defaults are tuned for a real link. A test that has to see a switch
    /// inside its own timeout shortens the hold times here rather than waiting
    /// out a four-second upgrade hold.
    pub fn enable_adaptation_with(
        &self,
        signals: watch::Receiver<NetworkSignals>,
        config: crate::adaptive::AdaptiveConfig,
    ) {
        let task = adapt::spawn_adaptation(
            self.control.broadcast.clone(),
            self.rendition.clone(),
            self.control.requested.clone(),
            signals,
            config,
        );
        *self.control.adaptation.lock().expect("poisoned") = Some(task);
    }

    /// Stops following the downlink, holding the current rendition.
    pub fn disable_adaptation(&self) {
        self.control.adaptation.lock().expect("poisoned").take();
    }
}

/// A playing audio track.
#[derive(Debug)]
pub struct AudioTrack {
    /// Keeps the broadcast alive for as long as the track plays. `Drop for
    /// Inner` cancels every decode task, so an audio-only subscription whose
    /// `RemoteBroadcast` went out of scope would otherwise fall silent.
    /// `VideoTrack` holds one through its control block for the same reason.
    _broadcast: RemoteBroadcast,
    rendition: String,
    #[cfg(feature = "playback")]
    control: moq_audio::playback::Control,
    _task: AbortOnDropHandle<()>,
}

impl AudioTrack {
    /// Returns the rendition currently playing.
    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    /// Sets the output volume, where 1.0 is unattenuated.
    #[cfg(feature = "playback")]
    pub fn set_volume(&self, volume: f32) {
        self.control.set_volume(volume);
    }

    /// Returns the output volume.
    #[cfg(feature = "playback")]
    pub fn volume(&self) -> f32 {
        self.control.volume()
    }

    /// Returns the most recent peak level, for a meter.
    #[cfg(feature = "playback")]
    pub fn peak(&self) -> f32 {
        self.control.peak()
    }
}

/// The pieces a decode task shares with the track handle it belongs to.
struct DecodeContext {
    stats: SubscribeStats,
    sync: Sync,
    policy: PlaybackPolicy,
    shutdown: CancellationToken,
}

impl RemoteBroadcast {
    fn decode_context(&self) -> DecodeContext {
        DecodeContext {
            stats: self.inner.stats.clone(),
            sync: self.inner.sync.clone(),
            policy: self.playback_policy(),
            shutdown: self.inner.shutdown.clone(),
        }
    }
}

/// The decode config a policy implies.
fn video_decode_config(policy: &PlaybackPolicy) -> moq_video::decode::Config {
    let mut config = moq_video::decode::Config::new();
    config.kind = policy.decoder.clone();
    config.latency_max = Some(policy.max_latency);
    config.gpu_frames = policy.gpu_frames;
    config
}

/// The decode config a policy implies, on the audio side.
#[cfg(feature = "playback")]
fn audio_decode_config(policy: &PlaybackPolicy) -> moq_audio::decode::Config {
    let mut config = moq_audio::decode::Config::new();
    config.format = moq_audio::Format::F32;
    config.latency_max = Some(policy.max_latency);
    config
}

/// Whether the playout clock gates this track.
fn is_synced(policy: &PlaybackPolicy) -> bool {
    matches!(policy.sync, SyncMode::Synced)
}
