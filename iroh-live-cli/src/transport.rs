//! Shared transport setup: binding, subscribing, and advertising.

use std::time::Duration;

use iroh::EndpointId;
use iroh_live::{
    BroadcastTicket, EndpointOptions, Live, LiveBuilder, Mdns, RemoteBroadcast, Session,
    Subscription,
    moq::{RelayConfig, RelayLink},
};
use n0_error::{Result, StdResultExt};
use tracing::{info, warn};

use crate::args::TransportArgs;

/// How long a peer is given before a window stops waiting for it.
///
/// Covers a dial that never completes and a broadcast that never publishes a
/// catalog, which look the same from here: something was announced and nothing
/// arrived. `irl call` and `irl room` both give up after this, because a window
/// that waits forever shows a spinner nobody can cancel.
#[cfg(feature = "render")]
pub const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a headless subscription waits before saying nothing arrived yet.
///
/// `irl watch`, `irl record`, and `irl run` keep waiting afterwards: a
/// subscriber started before its publisher is a normal way to use them, and
/// the terminal can be interrupted. Saying nothing at all is what leaves a user
/// guessing whether the ticket was wrong.
const QUIET_SUBSCRIBE: Duration = Duration::from_secs(10);

/// Binds an endpoint and starts the MoQ transport on it.
///
/// With `serve` set, a router accepts incoming subscribers. Without it only
/// outbound connections work, which is what `--no-serve` wants: the broadcast
/// still reaches a relay, but nobody dials this node directly.
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
pub async fn setup_live(serve: bool) -> Result<Live> {
    setup_live_with(EndpointOptions::from_env()?, serve).await
}

/// Binds an endpoint with `options` and starts the MoQ transport on it.
///
/// The identity is what a ticket names, so a caller holding a stored key
/// (`irl run` with a `secret_key_name`) hands back the same tickets on every
/// run. Otherwise as [`setup_live`].
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
pub async fn setup_live_with(options: EndpointOptions, serve: bool) -> Result<Live> {
    Ok(bind(options, serve).await?.spawn())
}

/// Binds an endpoint that also runs rooms, and starts the MoQ transport on it.
///
/// The room service is created on the node before the router, so the router
/// mounts it. Always serves: a participant nobody can dial has nothing to
/// contribute.
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
#[cfg(feature = "render")]
pub async fn setup_live_with_rooms() -> Result<(Live, iroh_live::rooms::Rooms)> {
    let endpoint = EndpointOptions::from_env()?.bind().await?;
    let moq = iroh_live::Moq::new(endpoint.clone(), iroh_live::moq_config());
    let rooms = iroh_live::rooms::Rooms::new(&moq);
    let live = Live::builder(endpoint)
        .with_moq(moq)
        .with_router()
        .accept(iroh_live::rooms::ALPN, rooms.protocol_handler())
        .spawn();
    Ok((live, rooms))
}

/// Binds the endpoint every `setup_live` variant starts from.
///
/// Returns the builder for the node on it, accepting sessions when `serve`
/// holds.
///
/// A ticket names an endpoint id and no addresses, so the endpoint carries
/// every way of turning an id back into an address that we have: pkarr and DNS
/// from the preset, which want internet at both ends, and mDNS, which wants
/// none. A node that does not serve looks others up over mDNS without
/// announcing itself, since it would advertise an endpoint that refuses every
/// session.
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
pub async fn bind(options: EndpointOptions, serve: bool) -> Result<LiveBuilder> {
    let mdns = if serve { Mdns::Announce } else { Mdns::Lookup };
    let endpoint = EndpointOptions { mdns, ..options }.bind().await?;
    let mut builder = Live::builder(endpoint);
    if serve {
        builder = builder.with_router();
    }
    Ok(builder)
}

/// Runs `setup` against a bound endpoint, closing it if the setup fails.
///
/// An endpoint dropped without [`Live::shutdown`] logs an error and leaves its
/// peers to time the connection out, so a command that gives up between binding
/// and running goes through here rather than returning the error directly.
///
/// # Errors
///
/// Returns whatever `setup` returned, having shut the endpoint down first.
pub async fn with_live<T>(
    live: Live,
    setup: impl AsyncFnOnce(&Live) -> Result<T>,
) -> Result<(Live, T)> {
    match setup(&live).await {
        Ok(value) => Ok((live, value)),
        Err(err) => {
            live.shutdown().await;
            Err(err)
        }
    }
}

/// A subscribed broadcast.
///
/// The path resolved in the route table, and the media broadcast read through
/// it, which carries the serving link's signals for adaptation.
#[derive(Debug, Clone)]
pub struct Subscribed {
    subscription: Subscription,
    broadcast: RemoteBroadcast,
}

impl Subscribed {
    /// Starts reading `subscription`'s broadcast.
    ///
    /// The catalog arrives on its own; [`crate::playback::catalog`] waits for
    /// it.
    pub fn open(live: &Live, subscription: Subscription) -> Self {
        let broadcast = live.remote_broadcast(&subscription);
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
    /// For a viewer that is done with its peer: the session is shared with
    /// anything else this node has open to the same peer. `irl watch`, `record`
    /// and `run` own theirs outright; a room tile drops only its player, since
    /// the member's chat rides the same session. The players stop when they are
    /// dropped.
    pub fn close(&self) {
        if let Some(session) = self.session() {
            session.close("stopped watching");
        }
    }
}

/// Subscribes to `ticket`, saying so on the way in and out.
///
/// A publisher that has not started yet announces nothing, so a subscription
/// that is taking a long time says which broadcast it is still waiting for.
/// Returns once a route is found; the catalog arrives afterwards.
///
/// # Errors
///
/// Fails if the peer cannot be reached.
pub async fn subscribe(live: &Live, ticket: &BroadcastTicket) -> Result<Subscribed> {
    println!("connecting to {ticket} ...");
    let mut subscribing = std::pin::pin!(async {
        let subscription = live
            .moq()
            .subscribe(ticket.path(), iroh_live::Reach::Both(ticket.peer()))
            .await?;
        n0_error::Ok(Subscribed::open(live, subscription))
    });

    let sub = match tokio::time::timeout(QUIET_SUBSCRIBE, subscribing.as_mut()).await {
        Ok(result) => result?,
        Err(_) => {
            warn!(
                remote = %ticket.peer().fmt_short(),
                broadcast = %ticket.name(),
                seconds = QUIET_SUBSCRIBE.as_secs(),
                "still waiting for the broadcast"
            );
            println!(
                "still waiting for '{}' on {}: is the publisher running? \
                 press Ctrl+C to give up",
                ticket.name(),
                ticket.peer().fmt_short()
            );
            subscribing.await?
        }
    };
    info!(
        remote = %ticket.peer().fmt_short(),
        broadcast = %ticket.name(),
        path = %sub.subscription().path(),
        "subscribed"
    );
    Ok(sub)
}

/// Advertises this node's broadcast and returns its ticket.
///
/// Prints the ticket, and attaches to a relay if one was named.
///
/// The relay link stays until the node shuts down.
///
/// # Errors
///
/// Fails if the relay link cannot be set up.
pub fn advertise(live: &Live, args: &TransportArgs) -> Result<String> {
    let ticket = live.ticket(&args.name).to_string();
    match (args.no_serve, args.relay) {
        // Nobody can dial this node, so the ticket names an endpoint that
        // refuses every session and the relay is the only way out.
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

/// Attaches to the relay `relay`, redialing it if the session drops.
///
/// The relay then receives every public broadcast this node publishes. The
/// link only publishes: `irl publish` subscribes to nothing, and a consuming
/// link would mirror the relay's whole namespace into the route table.
fn attach_relay(live: &Live, relay: EndpointId, name: &str) -> Result<RelayLink> {
    let url = format!("iroh://{relay}/")
        .parse()
        .std_context("an endpoint id is a valid URL host")?;
    let link = live.moq().attach_relay(RelayConfig {
        consume: false,
        ..RelayConfig::new(url)
    })?;
    let path = live.ticket(name).path();
    info!(relay = %relay.fmt_short(), %path, "pushing to relay");
    println!("pushing to relay {relay}: viewers find the broadcast there at {path}");
    Ok(link)
}

/// Prints a QR code of `ticket`, unless `no_qr` suppresses it.
///
/// A terminal that cannot draw one is not a reason to stop, so a failure is
/// logged and nothing else.
pub fn print_qr(ticket: &str, no_qr: bool) {
    if !no_qr && let Err(err) = qr2term::print_qr(ticket) {
        warn!(error = %err, "could not print the QR code");
    }
}
