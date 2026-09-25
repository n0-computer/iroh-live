//! One-to-one calls.

use iroh::EndpointId;
use iroh_live_media::RemoteBroadcast;
use iroh_moq::{Session, Subscription};

use crate::{BroadcastTicket, Error, Live};

/// The name each side of a call publishes its own broadcast under.
///
/// Each side publishes `live/<its id>/call` and subscribes to the other's
/// over the session between them.
pub const CALL: &str = "call";

/// A call: the session with the peer, and the peer's side read over it.
#[derive(Debug, Clone)]
pub struct Call {
    session: Session,
    subscription: Subscription,
    remote: RemoteBroadcast,
}

impl Call {
    /// Dials `peer` and subscribes to its side of the call.
    pub async fn dial(live: &Live, peer: EndpointId) -> Result<Self, Error> {
        let session = live.moq().connect(peer).await?;
        Self::accept(live, session).await
    }

    /// Subscribes to the side of the call the peer of `session` publishes.
    ///
    /// Waits until the peer announces it. A peer that is not calling never
    /// does, so bound the wait with a timeout.
    pub async fn accept(live: &Live, session: Session) -> Result<Self, Error> {
        let path = BroadcastTicket::new(session.remote_id(), CALL).path();
        let subscription = session.subscribe(path).await?;
        let remote = live.remote_broadcast(&subscription);
        Ok(Self {
            session,
            subscription,
            remote,
        })
    }

    /// Returns the peer's side of the call.
    pub fn remote(&self) -> &RemoteBroadcast {
        &self.remote
    }

    /// Returns the session with the peer.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Returns the subscription the peer's side arrives over.
    pub fn subscription(&self) -> &Subscription {
        &self.subscription
    }

    /// Hangs up by closing the session.
    pub fn close(&self) {
        self.session.close("hung up");
    }
}
