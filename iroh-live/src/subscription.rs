use iroh_live_media::RemoteBroadcast;
use iroh_moq::MoqSession;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;

use crate::util::LinkSignals;

/// A subscription to one remote broadcast.
///
/// Bundles the MoQ session, the broadcast, and the transport signals its
/// players adapt on. Created by [`Live::subscribe`](crate::Live::subscribe),
/// which attaches the signals to the broadcast so a caller does not have to.
#[derive(Debug)]
pub struct Subscription {
    session: MoqSession,
    broadcast: RemoteBroadcast,
    signals: watch::Receiver<LinkSignals>,
    /// Stops the signal producer with the subscription.
    _signals: AbortOnDropHandle<()>,
}

impl Subscription {
    /// Attaches the session's link signals to `broadcast`, as its players'
    /// network signals.
    ///
    /// [`Live::subscribe`](crate::Live::subscribe) is the usual way in, and it
    /// calls this. Reach for it directly when the two halves came from
    /// somewhere else, as an `iroh-rooms` event hands them back separately.
    pub fn new(session: MoqSession, broadcast: RemoteBroadcast) -> Self {
        let (signals, task) = crate::util::spawn_signal_producer(&session);
        let reader = signals.clone();
        let broadcast = broadcast.with_network(move || reader.borrow().sample());
        Self {
            session,
            broadcast,
            signals,
            _signals: task,
        }
    }

    /// Returns the underlying MoQ session.
    pub fn session(&self) -> &MoqSession {
        &self.session
    }

    /// Returns the broadcast, for playing and recording it.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Returns the transport signals, for diagnostics.
    pub fn signals(&self) -> &watch::Receiver<LinkSignals> {
        &self.signals
    }
}
