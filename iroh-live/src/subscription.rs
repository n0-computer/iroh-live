use iroh_moq::MoqSession;
use moq_media::{net::NetworkSignals, subscribe::RemoteBroadcast};
use tokio::sync::watch;

/// Wraps a MoQ session, its remote broadcast, and network signal production
/// into a single handle.
///
/// Created by [`Live::subscribe`](crate::Live::subscribe).
/// The constructor auto-wires stats recording and signal production, so
/// callers no longer need to call `spawn_stats_recorder` and
/// `spawn_signal_producer` manually.
///
/// Dropping the `Subscription` does not require special cleanup — the
/// session's own `Drop` handles connection teardown.
#[derive(Debug)]
pub struct Subscription {
    session: MoqSession,
    broadcast: RemoteBroadcast,
    signals: watch::Receiver<NetworkSignals>,
}

impl Subscription {
    /// Creates a new subscription with pre-wired stats and signals.
    ///
    /// Called internally by [`Live::subscribe`](crate::Live::subscribe).
    /// Spawns background tasks for connection stats recording and network
    /// signal production.
    pub(crate) fn new(session: MoqSession, broadcast: RemoteBroadcast) -> Self {
        crate::util::spawn_stats_recorder(
            session.conn(),
            broadcast.stats().net.clone(),
            broadcast.shutdown_token(),
        );
        let signals =
            crate::util::spawn_signal_producer(session.conn(), broadcast.shutdown_token());

        Self {
            session,
            broadcast,
            signals,
        }
    }

    /// Returns the underlying MoQ session.
    pub fn session(&self) -> &MoqSession {
        &self.session
    }

    /// Returns the remote broadcast for subscribing to video/audio tracks.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Returns the network signals receiver for adaptive rendition selection.
    pub fn signals(&self) -> &watch::Receiver<NetworkSignals> {
        &self.signals
    }

    /// Subscribes to video and audio using default decoders.
    ///
    /// Convenience wrapper around [`RemoteBroadcast::media`].
    #[cfg(any_codec)]
    pub async fn media(
        &self,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
        config: moq_media::format::PlaybackConfig,
    ) -> n0_error::Result<moq_media::subscribe::MediaTracks> {
        self.broadcast.media(audio_backend, config).await
    }

    /// Subscribes to video and audio with custom decoder types.
    ///
    /// Convenience wrapper around [`RemoteBroadcast::media_with_decoders`].
    pub async fn media_with_decoders<D: moq_media::traits::Decoders>(
        &self,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
        config: moq_media::format::PlaybackConfig,
    ) -> n0_error::Result<moq_media::subscribe::MediaTracks> {
        self.broadcast
            .media_with_decoders::<D>(audio_backend, config)
            .await
    }

    /// Destructures into individual components.
    pub fn into_parts(self) -> (MoqSession, RemoteBroadcast, watch::Receiver<NetworkSignals>) {
        (self.session, self.broadcast, self.signals)
    }
}
