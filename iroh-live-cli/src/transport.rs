//! Shared transport setup: binding an endpoint and advertising what it serves.
//!
//! Publishing is node-wide now. A broadcast created through
//! [`Live::publish`](iroh_live::Live::publish) is announced on every session
//! this node has, so pushing to a relay is nothing more than connecting to it.

use iroh::{Endpoint, SecretKey, endpoint::presets};
use iroh_live::{Live, ticket::LiveTicket};
use n0_error::Result;
use tracing::info;

use crate::args::TransportArgs;

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
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .bind()
        .await?;
    info!(endpoint_id = %endpoint.id(), "endpoint bound");

    let mut builder = Live::builder(endpoint);
    if serve {
        builder = builder.with_router();
    }
    Ok(builder.spawn())
}

/// Advertises the broadcast: prints its ticket and connects to a relay if one
/// was named.
///
/// # Errors
///
/// Fails if the relay cannot be reached.
pub async fn advertise(live: &Live, args: &TransportArgs) -> Result<()> {
    if !args.no_serve {
        print_ticket(live, &args.name, args.no_qr);
    }

    if let Some(relay) = args.relay {
        // The session carries the node origin, so every broadcast this node
        // publishes is announced to the relay as soon as the session is up.
        live.transport().connect(relay).await?;
        info!(relay = %relay.fmt_short(), "pushing to relay");
        println!("pushing to relay {relay}");
    }
    Ok(())
}

/// Prints the ticket a subscriber needs, and a QR code of it unless suppressed.
pub fn print_ticket(live: &Live, name: &str, no_qr: bool) -> String {
    let ticket = ticket(live, name);
    println!("publishing at {ticket}");
    print_qr(&ticket, no_qr);
    ticket
}

/// Prints a QR code of `ticket`, unless `no_qr` suppresses it.
///
/// A terminal that cannot draw one is not a reason to stop, so a failure is
/// logged and nothing else.
pub fn print_qr(ticket: &str, no_qr: bool) {
    if !no_qr && let Err(err) = qr2term::print_qr(ticket) {
        tracing::warn!(error = %err, "could not print the QR code");
    }
}

/// The ticket for a broadcast this node publishes.
pub fn ticket(live: &Live, name: &str) -> String {
    LiveTicket::new(live.endpoint().addr(), name).to_string()
}
