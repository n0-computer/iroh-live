//! Shared transport setup: binding, subscribing, and advertising.

use std::{sync::Arc, time::Duration};

use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    BoundNode, BroadcastTicket, EndpointConfig, Mdns, Node, Session, Subscription,
    layout::LiveLayout,
    media::{RemoteBroadcast, remote_broadcast},
    node::{RelayConfig, RelayLink},
};
use n0_error::Result;
use tracing::{info, warn};

use crate::args::TransportArgs;

/// How long a window waits for a peer before giving up.
///
/// Covers a dial that never completes and a catalog that never arrives. A
/// window cannot be interrupted like a terminal, so `irl call` and `irl room`
/// stop waiting.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a subscription waits before saying nothing arrived yet.
///
/// It keeps waiting afterwards, since starting a subscriber before its
/// publisher is normal.
const QUIET_SUBSCRIBE: Duration = Duration::from_secs(10);

/// A live node, and the router that accepts its sessions if it serves.
///
/// Shared by the windows of a command, and shut down by whichever ends it.
pub type SharedNode = Arc<BoundNode>;

/// Binds an endpoint and starts the MoQ transport on it.
///
/// With `serve` set, a router accepts incoming subscribers. Without it, only
/// outbound connections work.
pub async fn bind_node(serve: bool) -> Result<SharedNode> {
    bind_node_with(EndpointConfig::from_env()?, serve).await
}

/// Binds an endpoint with `config` and starts the MoQ transport on it.
///
/// A ticket carries no addresses, so the endpoint uses mDNS on top of the
/// preset's pkarr and DNS lookup. A node that does not serve only looks up
/// others and does not announce itself.
pub async fn bind_node_with(config: EndpointConfig, serve: bool) -> Result<SharedNode> {
    let mdns = if serve { Mdns::Announce } else { Mdns::Lookup };
    let endpoint = EndpointConfig { mdns, ..config }.bind().await?;
    let node = Node::new(endpoint.clone(), LiveLayout::default().config());
    let router = serve.then(|| node.mount(Router::builder(endpoint)).spawn());
    Ok(Arc::new(BoundNode::new(node, router)))
}

/// Runs `setup` against a bound endpoint, closing it if the setup fails.
///
/// A [`BoundNode`] dropped without [`BoundNode::shutdown`] shuts down in the
/// background, which a command that exits right after does not wait for.
pub async fn with_node<T>(
    transport: SharedNode,
    setup: impl AsyncFnOnce(&SharedNode) -> Result<T>,
) -> Result<(SharedNode, T)> {
    match setup(&transport).await {
        Ok(value) => Ok((transport, value)),
        Err(err) => {
            transport.shutdown().await;
            Err(err)
        }
    }
}

/// A subscription and the media broadcast read through it.
#[derive(Debug, Clone)]
pub struct Subscribed {
    subscription: Subscription,
    broadcast: RemoteBroadcast,
}

impl Subscribed {
    /// Starts reading `subscription`'s broadcast.
    ///
    /// [`crate::playback::catalog`] waits for the catalog.
    pub fn open(subscription: Subscription) -> Self {
        let broadcast = remote_broadcast(&subscription);
        Self {
            subscription,
            broadcast,
        }
    }

    /// Returns the media broadcast.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Returns the resolved path.
    pub fn subscription(&self) -> &Subscription {
        &self.subscription
    }

    /// Returns the session serving the broadcast, if a direct one does.
    pub fn session(&self) -> Option<Session> {
        self.subscription.session()
    }

    /// Closes the session that served the broadcast.
    ///
    /// The session is shared with everything else open to the same peer, so
    /// only a command that reads nothing else from the peer calls this. A room
    /// tile or a call must not: other tiles, the chat or the next call use the
    /// same session.
    pub fn close(&self) {
        if let Some(session) = self.session() {
            session.close(moq_net::Error::Cancel);
        }
    }
}

/// Subscribes to `ticket`, printing progress.
///
/// Prints a notice if the broadcast takes long to appear. Returns once a route
/// is found, before the catalog arrives.
pub async fn subscribe(node: &Node, ticket: &BroadcastTicket) -> Result<Subscribed> {
    println!("connecting to {ticket} ...");
    let mut subscribing = std::pin::pin!(async {
        let subscription = node.subscribe_ticket(ticket).await?;
        n0_error::Ok(Subscribed::open(subscription))
    });

    let sub = match tokio::time::timeout(QUIET_SUBSCRIBE, subscribing.as_mut()).await {
        Ok(result) => result?,
        Err(_) => {
            warn!(
                peer = %ticket.peer().fmt_short(),
                broadcast = %ticket.path(),
                seconds = QUIET_SUBSCRIBE.as_secs(),
                "still waiting for the broadcast"
            );
            println!(
                "still waiting for '{}' on {}: is the publisher running? \
                 press Ctrl+C to give up",
                ticket.path(),
                ticket.peer().fmt_short()
            );
            subscribing.await?
        }
    };
    info!(
        peer = %ticket.peer().fmt_short(),
        broadcast = %ticket.path(),
        path = %sub.subscription().path(),
        "subscribed"
    );
    Ok(sub)
}

/// Advertises this node's broadcast and returns its ticket.
///
/// Prints the ticket unless `--no-serve` is set, and attaches the relay if one
/// was named.
pub fn advertise(node: &Node, args: &TransportArgs) -> Result<String> {
    let ticket = LiveLayout::default()
        .ticket(node.id(), &args.name)?
        .to_string();
    match (args.no_serve, args.relay) {
        (true, Some(_)) => println!("not serving: subscribers reach this broadcast by relay"),
        (true, None) => warn!(
            "--no-serve without --relay: nothing can reach this broadcast, since \
             this node neither accepts subscribers nor pushes to a relay"
        ),
        (false, _) => {
            println!("publishing at {ticket}");
            print_qr(&ticket, args.no_qr);
        }
    }

    if let Some(relay) = args.relay {
        attach_relay(node, relay, &args.name)?;
    }
    Ok(ticket)
}

/// Attaches to `relay`, redialing it if the session drops.
///
/// The relay receives every public broadcast of this node. The link does not
/// consume, or it would mirror the relay's whole namespace into the route table.
fn attach_relay(node: &Node, relay: EndpointId, name: &str) -> Result<RelayLink> {
    let link = node.attach_relay(RelayConfig {
        consume: false,
        ..RelayConfig::iroh(relay)
    })?;
    let path = LiveLayout::default()
        .ticket(node.id(), name)?
        .path()
        .clone();
    info!(relay = %relay.fmt_short(), %path, "pushing to relay");
    println!("pushing to relay {relay}: viewers find the broadcast there at {path}");
    Ok(link)
}

/// Prints a QR code of `ticket`, unless `no_qr` suppresses it.
///
/// A failure is only logged.
pub fn print_qr(ticket: &str, no_qr: bool) {
    if !no_qr && let Err(err) = qr2term::print_qr(ticket) {
        warn!(error = %err, "could not print the QR code");
    }
}
