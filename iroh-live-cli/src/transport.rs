//! Shared transport setup: binding an endpoint and advertising what it serves.
//!
//! Publishing is node-wide now. A broadcast created through
//! [`Live::publish`](iroh_live::Live::publish) is announced on every session
//! this node has, so pushing to a relay is nothing more than connecting to it.

use std::time::Duration;

use iroh::{Endpoint, SecretKey, endpoint::presets};
use iroh_live::{
    Live, LiveBuilder, Subscription,
    ticket::LiveTicket,
    util::{LanPresence, with_mdns},
};
use n0_error::Result;
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

/// How long a headless subscription waits before it says out loud that nothing
/// has arrived yet.
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
    setup_live_with_key(iroh_live::util::secret_key_from_env()?, serve).await
}

/// Binds an endpoint under `secret_key` and starts the MoQ transport on it.
///
/// The identity is what a ticket names, so a caller holding a stored key
/// (`irl run` with a `secret_key_name`) hands back the same tickets on every
/// run. Otherwise as [`setup_live`].
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
pub async fn setup_live_with_key(secret_key: SecretKey, serve: bool) -> Result<Live> {
    Ok(bind(secret_key, serve).await?.spawn())
}

/// Binds an endpoint that also runs gossip, and starts the MoQ transport on it.
///
/// Rooms discover each other over gossip, so `irl room` needs it where nothing
/// else here does. Always serves: a participant nobody can dial has nothing to
/// contribute.
///
/// # Errors
///
/// Fails if the endpoint cannot bind.
#[cfg(feature = "render")]
pub async fn setup_live_with_gossip() -> Result<Live> {
    let secret_key = iroh_live::util::secret_key_from_env()?;
    Ok(bind(secret_key, true).await?.with_gossip().spawn())
}

/// Binds the endpoint every `setup_live` variant starts from.
///
/// A ticket names an endpoint id and no addresses, so the endpoint carries
/// every way of turning an id back into an address that we have. `presets::N0`
/// brings pkarr publishing and pkarr and DNS resolution, which want internet at
/// both ends; mDNS brings the local network, which wants none. `irl` takes both
/// unconditionally rather than behind a flag, because the one thing a person
/// scanning a QR code cannot be asked is which of the two their network is.
async fn bind(secret_key: SecretKey, serve: bool) -> Result<LiveBuilder> {
    let builder = Endpoint::builder(presets::N0).secret_key(secret_key);
    let endpoint = with_mdns(builder, LanPresence::serving(serve))
        .await
        .bind()
        .await?;
    info!(endpoint_id = %endpoint.id(), "endpoint bound");

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

/// Subscribes to `ticket`, saying so on the way in and out.
///
/// The catalog is what a subscription waits for, and a publisher that has not
/// started yet never sends one, so a subscription that is taking a long time
/// says which broadcast it is still waiting for.
///
/// # Errors
///
/// Fails if the peer cannot be reached, or if it closes the broadcast without
/// ever publishing a catalog.
pub async fn subscribe(live: &Live, ticket: &LiveTicket) -> Result<Subscription> {
    println!("connecting to {ticket} ...");
    let mut subscribing =
        std::pin::pin!(live.subscribe(ticket.endpoint.clone(), &ticket.broadcast_name));

    let sub = match tokio::time::timeout(QUIET_SUBSCRIBE, subscribing.as_mut()).await {
        Ok(result) => result?,
        Err(_) => {
            warn!(
                remote = %ticket.endpoint.id.fmt_short(),
                broadcast = %ticket.broadcast_name,
                seconds = QUIET_SUBSCRIBE.as_secs(),
                "still waiting for the broadcast"
            );
            println!(
                "still waiting for '{}' on {}: is the publisher running? \
                 press Ctrl+C to give up",
                ticket.broadcast_name,
                ticket.endpoint.id.fmt_short()
            );
            subscribing.await?
        }
    };
    info!(
        remote = %ticket.endpoint.id.fmt_short(),
        broadcast = %ticket.broadcast_name,
        "session established"
    );
    Ok(sub)
}

/// Advertises the broadcast: prints its ticket, connects to a relay if one was
/// named, and returns the ticket either way.
///
/// # Errors
///
/// Fails if the relay cannot be reached.
pub async fn advertise(live: &Live, args: &TransportArgs) -> Result<String> {
    let ticket = ticket(live, &args.name);
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
        // The session carries the node origin, so every broadcast this node
        // publishes is announced to the relay as soon as the session is up.
        live.transport().connect(relay).await?;
        info!(relay = %relay.fmt_short(), "pushing to relay");
        println!("pushing to relay {relay}");
    }
    Ok(ticket)
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

/// The ticket for a broadcast this node publishes.
pub fn ticket(live: &Live, name: &str) -> String {
    LiveTicket::new(live.endpoint().id(), name).to_string()
}
