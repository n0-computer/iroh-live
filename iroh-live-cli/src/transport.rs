//! Shared transport setup: binding, subscribing, and advertising.

use std::{ops::Deref, time::Duration};

use iroh::{EndpointId, protocol::Router};
use iroh_live::layout::LiveLayout;
use iroh_live::media::remote_broadcast;
use iroh_live::{
    BroadcastTicket, EndpointOptions, Mdns, Node, Session, Subscription,
    media::RemoteBroadcast,
    node::{RelayConfig, RelayLink},
};
use n0_error::Result;
use tracing::{error, info, warn};

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
/// Derefs to [`Node`]. Cheap to clone. [`shutdown`](Self::shutdown) shuts the
/// node down and closes its endpoint, which [`Node::shutdown`] alone does not.
#[derive(Debug, Clone)]
pub struct Transport {
    live: Node,
    /// Accepts incoming sessions. `None` for a node that only dials.
    router: Option<Router>,
}

impl Transport {
    /// Wraps `live`, mounting it on a new router if `serve` is set.
    pub fn new(live: Node, serve: bool) -> Self {
        let router = serve.then(|| live.mount(Router::builder(live.endpoint().clone())).spawn());
        Self { live, router }
    }

    /// Wraps `live`, which `router` already serves.
    ///
    /// For a node with services of its own on the router, such as rooms.
    pub fn serving(live: Node, router: Router) -> Self {
        Self {
            live,
            router: Some(router),
        }
    }

    /// Shuts the node down, then closes the endpoint.
    ///
    /// Sessions say goodbye before the endpoint closes. Idempotent.
    pub async fn shutdown(&self) {
        self.live.shutdown().await;
        match &self.router {
            // Closes the endpoint once the router's handlers have shut down.
            Some(router) => {
                if let Err(err) = router.shutdown().await {
                    error!(error = %err, "failed to shut down the iroh router");
                    self.live.endpoint().close().await;
                }
            }
            None => self.live.endpoint().close().await,
        }
    }
}

impl Deref for Transport {
    type Target = Node;

    fn deref(&self) -> &Node {
        &self.live
    }
}

/// Binds an endpoint and starts the MoQ transport on it.
///
/// With `serve` set, a router accepts incoming subscribers. Without it, only
/// outbound connections work.
pub async fn setup_live(serve: bool) -> Result<Transport> {
    setup_live_with(EndpointOptions::from_env()?, serve).await
}

/// Binds an endpoint with `options` and starts the MoQ transport on it.
///
/// A ticket carries no addresses, so the endpoint uses mDNS on top of the
/// preset's pkarr and DNS lookup. A node that does not serve only looks up
/// others and does not announce itself.
pub async fn setup_live_with(options: EndpointOptions, serve: bool) -> Result<Transport> {
    let mdns = if serve { Mdns::Announce } else { Mdns::Lookup };
    let endpoint = EndpointOptions { mdns, ..options }.bind().await?;
    Ok(Transport::new(
        Node::new(endpoint, LiveLayout::default().config(&[])),
        serve,
    ))
}

/// Runs `setup` against a bound endpoint, closing it if the setup fails.
///
/// An endpoint dropped without [`Transport::shutdown`] logs an error, and its
/// peers have to time out.
pub async fn with_live<T>(
    transport: Transport,
    setup: impl AsyncFnOnce(&Transport) -> Result<T>,
) -> Result<(Transport, T)> {
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
            session.close("stopped watching");
        }
    }
}

/// Subscribes to `ticket`, printing progress.
///
/// Prints a notice if the broadcast takes long to appear. Returns once a route
/// is found, before the catalog arrives.
pub async fn subscribe(live: &Node, ticket: &BroadcastTicket) -> Result<Subscribed> {
    println!("connecting to {ticket} ...");
    let mut subscribing = std::pin::pin!(async {
        let subscription = live.subscribe_ticket(ticket).await?;
        n0_error::Ok(Subscribed::open(subscription))
    });

    let sub = match tokio::time::timeout(QUIET_SUBSCRIBE, subscribing.as_mut()).await {
        Ok(result) => result?,
        Err(_) => {
            warn!(
                remote = %ticket.peer().fmt_short(),
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
        remote = %ticket.peer().fmt_short(),
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
pub fn advertise(live: &Node, args: &TransportArgs) -> Result<String> {
    let ticket = LiveLayout::default()
        .ticket(live.id(), &args.name)?
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
        attach_relay(live, relay, &args.name)?;
    }
    Ok(ticket)
}

/// Attaches to `relay`, redialing it if the session drops.
///
/// The relay receives every public broadcast of this node. The link does not
/// consume, or it would mirror the relay's whole namespace into the route table.
fn attach_relay(live: &Node, relay: EndpointId, name: &str) -> Result<RelayLink> {
    let link = live.attach_relay(RelayConfig {
        consume: false,
        ..RelayConfig::iroh(relay)
    })?;
    let path = LiveLayout::default()
        .ticket(live.id(), name)?
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
