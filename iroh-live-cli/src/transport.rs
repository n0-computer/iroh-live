//! Shared transport setup: binding, subscribing, and advertising.

use std::{ops::Deref, sync::Arc, time::Duration};

use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    BroadcastTicket, EndpointConfig, Mdns, Node, Session, Subscription,
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
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a shutdown waits for the endpoint to close.
///
/// Closing the endpoint waits until every peer has acknowledged that its
/// connection closed. A live peer does so within a round trip. A peer that
/// vanished never does, and the endpoint then waits out the drain period,
/// which took up to five seconds in tests against a killed peer. Such a peer
/// learns nothing from the wait.
const ENDPOINT_CLOSE: Duration = Duration::from_secs(2);

/// How long a subscription waits before saying nothing arrived yet.
///
/// It keeps waiting afterwards, since starting a subscriber before its
/// publisher is normal.
const QUIET_SUBSCRIBE: Duration = Duration::from_secs(10);

/// A live node and the router on its endpoint.
///
/// Shared by the windows of a command, and shut down by whichever ends it.
pub(crate) type SharedNode = Arc<Bound>;

/// A node and its router, which accepts the node's sessions if it serves.
///
/// A node that does not serve gets a router with nothing mounted, so that
/// [`shutdown`](Self::shutdown) closes the endpoint either way.
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) node: Node,
    pub(crate) router: Router,
}

impl Bound {
    /// Shuts the node down, then the router and the endpoint.
    ///
    /// Waits at most [`ENDPOINT_CLOSE`] for the endpoint to close.
    pub(crate) async fn shutdown(&self) {
        // The router shuts down only the handlers mounted on it.
        crate::shutdown::step("shutting the node down");
        self.node.shutdown().await;
        crate::shutdown::step("closing the endpoint");
        match tokio::time::timeout(ENDPOINT_CLOSE, self.router.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(
                error = format_args!("{err:#}"),
                "the router failed to shut down"
            ),
            Err(_) => info!(
                waited = ?ENDPOINT_CLOSE,
                "stopped waiting for peers to acknowledge the closed connections"
            ),
        }
    }
}

impl Deref for Bound {
    type Target = Node;

    fn deref(&self) -> &Node {
        &self.node
    }
}

/// Binds an endpoint and starts the MoQ transport on it.
///
/// With `serve` set, a router accepts incoming subscribers. Without it, only
/// outbound connections work.
pub(crate) async fn bind_node(serve: bool) -> Result<SharedNode> {
    bind_node_with(EndpointConfig::from_env()?, serve).await
}

/// Binds an endpoint with `config` and starts the MoQ transport on it.
///
/// A ticket carries no addresses, so the endpoint uses mDNS on top of the
/// preset's pkarr and DNS lookup. A node that does not serve only looks up
/// others and does not announce itself.
pub(crate) async fn bind_node_with(config: EndpointConfig, serve: bool) -> Result<SharedNode> {
    let mdns = if serve { Mdns::Announce } else { Mdns::Lookup };
    let endpoint = EndpointConfig { mdns, ..config }.bind().await?;
    let node = Node::new(endpoint.clone(), LiveLayout::default().config());
    let router = Router::builder(endpoint);
    let router = match serve {
        true => node.mount(router),
        false => router,
    };
    let router = router.spawn();
    Ok(Arc::new(Bound { node, router }))
}

/// Runs `setup` against a bound endpoint, closing it if the setup fails.
///
/// A [`Bound`] dropped without [`Bound::shutdown`] leaves its peers to time
/// out.
pub(crate) async fn with_node<T>(
    node: SharedNode,
    setup: impl AsyncFnOnce(&SharedNode) -> Result<T>,
) -> Result<(SharedNode, T)> {
    match setup(&node).await {
        Ok(value) => Ok((node, value)),
        Err(err) => {
            node.shutdown().await;
            Err(err)
        }
    }
}

/// A subscription and the media broadcast read through it.
#[derive(Debug, Clone)]
pub(crate) struct Subscribed {
    subscription: Subscription,
    broadcast: RemoteBroadcast,
}

impl Subscribed {
    /// Starts reading `subscription`'s broadcast.
    ///
    /// [`crate::playback::catalog`] waits for the catalog.
    pub(crate) fn open(subscription: Subscription) -> Self {
        let broadcast = remote_broadcast(&subscription);
        Self {
            subscription,
            broadcast,
        }
    }

    /// Returns the media broadcast.
    pub(crate) fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Returns the subscription the broadcast arrives over.
    pub(crate) fn subscription(&self) -> &Subscription {
        &self.subscription
    }

    /// Returns the session serving the broadcast, if a direct one does.
    pub(crate) fn session(&self) -> Option<Session> {
        self.subscription.session()
    }

    /// Closes the session that served the broadcast.
    ///
    /// The session is shared with everything else open to the same peer, so
    /// only a command that reads nothing else from the peer calls this. A room
    /// tile or a call must not: other tiles, the chat or the next call use the
    /// same session.
    pub(crate) fn close(&self) {
        if let Some(session) = self.session() {
            session.close(moq_net::Error::Cancel);
        }
    }
}

/// Subscribes to `ticket`, printing progress.
///
/// Prints a notice if the broadcast takes long to appear. Returns once a route
/// is found, before the catalog arrives.
pub(crate) async fn subscribe(node: &Node, ticket: &BroadcastTicket) -> Result<Subscribed> {
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
pub(crate) fn advertise(node: &Node, args: &TransportArgs) -> Result<String> {
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
pub(crate) fn print_qr(ticket: &str, no_qr: bool) {
    if !no_qr && let Err(err) = qr2term::print_qr(ticket) {
        warn!(error = %err, "could not print the QR code");
    }
}
