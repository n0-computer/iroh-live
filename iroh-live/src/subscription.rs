use iroh_moq::MoqSession;
use moq_media::{net::NetworkSignals, subscribe::RemoteBroadcast};
use tokio::sync::watch;

/// A subscription to one remote broadcast.
///
/// Bundles the MoQ session, the broadcast, and the transport signals that drive
/// rendition adaptation. Created by [`Live::subscribe`](crate::Live::subscribe),
/// which wires the stats recorder and signal producer so a caller does not have
/// to.
///
/// Dropping it needs no special cleanup: the session tears its own connection
/// down.
#[derive(Debug)]
pub struct Subscription {
    session: MoqSession,
    broadcast: RemoteBroadcast,
    signals: watch::Receiver<NetworkSignals>,
}

impl Subscription {
    /// Wires the stats recorder and signal producer onto a fresh subscription.
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

    /// Returns the broadcast, for opening its video and audio tracks.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Returns the transport signals, for
    /// [`VideoTrack::enable_adaptation`](moq_media::subscribe::VideoTrack::enable_adaptation).
    pub fn signals(&self) -> &watch::Receiver<NetworkSignals> {
        &self.signals
    }

    /// Opens whichever of video and audio the broadcast carries.
    pub async fn media(&self) -> moq_media::subscribe::MediaTracks {
        self.broadcast.media().await
    }

    /// Splits into the session, the broadcast, and the signal receiver.
    pub fn into_parts(self) -> (MoqSession, RemoteBroadcast, watch::Receiver<NetworkSignals>) {
        (self.session, self.broadcast, self.signals)
    }
}
