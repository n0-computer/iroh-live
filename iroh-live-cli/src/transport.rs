//! Shared transport: create Live, publish via serve/relay/room combinations.
//!
//! Publishing into an iroh-live actor registers the broadcast on the actor's
//! publish origin. The actor forwards the announcement to every session that
//! shares the transport, whether the session was established outbound (for
//! example a push to a relay) or inbound (a subscriber dialing us directly).
//! This lets the three transport modes (serve, relay, room) coexist without
//! each of them needing their own explicit publish call on every session.

use iroh_live::{
    Live,
    media::publish::LocalBroadcast,
    rooms::{Room, RoomTicket},
    ticket::LiveTicket,
};
use moq_lite::BroadcastProducer;
use tracing::info;

use crate::args::{RelaySpec, TransportArgs};

/// Creates a [`Live`] instance. When `serve` is true, spawns with router so
/// incoming subscribers are accepted. When false, only outbound connections work.
pub async fn setup_live(serve: bool) -> anyhow::Result<Live> {
    let mut builder = Live::from_env().await?;
    if serve {
        builder = builder.with_router();
    }
    Ok(builder.spawn())
}

/// Publishes a [`LocalBroadcast`] according to transport flags: serve locally,
/// push to relay, and/or publish into a room. Prints ticket when serving.
///
/// Returns the [`Room`] handle when `--room` is used, so the caller can keep
/// it alive for the session's duration instead of leaking memory.
pub async fn publish_broadcast(
    live: &Live,
    broadcast: &LocalBroadcast,
    args: &TransportArgs,
) -> anyhow::Result<Option<Room>> {
    // The actor forwards this broadcast to every session we have now or
    // open later, so we register once here rather than pushing it
    // per-session. `--no-serve` only suppresses the incoming router and
    // the ticket print; the publish itself is still registered so relay
    // or room pushes pick it up.
    live.publish(&args.name, broadcast).await?;

    if !args.no_serve {
        print_ticket(live, &args.name, args.no_qr);
    }

    connect_to_relays(live, &args.relays).await?;

    let room = if let Some(ref room_ticket) = args.room {
        Some(push_to_room(live, &args.name, broadcast.producer(), room_ticket).await?)
    } else {
        None
    };

    Ok(room)
}

/// Publishes a raw [`BroadcastProducer`] according to transport flags.
///
/// Returns the [`Room`] handle when `--room` is used.
pub async fn publish_producer(
    live: &Live,
    producer: BroadcastProducer,
    args: &TransportArgs,
) -> anyhow::Result<Option<Room>> {
    live.publish_broadcast_producer(&args.name, producer.clone())
        .await?;

    if !args.no_serve {
        print_ticket(live, &args.name, args.no_qr);
    }

    connect_to_relays(live, &args.relays).await?;

    let room = if let Some(ref room_ticket) = args.room {
        Some(push_to_room(live, &args.name, producer, room_ticket).await?)
    } else {
        None
    };

    Ok(room)
}

/// Opens a session to every relay in `relays`. The broadcast
/// registered on the actor is forwarded to each new session
/// automatically via [`Live::connect_relay`]'s `HandleSession`
/// integration, so a single `--relay` flag fans the publish out
/// across every relay listed.
async fn connect_to_relays(live: &Live, relays: &[RelaySpec]) -> anyhow::Result<()> {
    for spec in relays {
        let target = spec.to_target();
        let _session = live.connect_relay(&target).await?;
        info!(relay = %target.endpoint(), path = %spec.path, "connected to relay");
    }
    Ok(())
}

async fn push_to_room(
    live: &Live,
    name: &str,
    producer: BroadcastProducer,
    room_ticket: &RoomTicket,
) -> anyhow::Result<Room> {
    let room = live.join_room(room_ticket.clone()).await?;
    room.publish_producer(name, producer).await?;
    println!("room ticket: {}", room.ticket());
    Ok(room)
}

fn print_ticket(live: &Live, name: &str, no_qr: bool) {
    let ticket = LiveTicket::new(live.endpoint().addr(), name);
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    if !no_qr && let Err(e) = qr2term::print_qr(&ticket_str) {
        tracing::warn!("could not print QR code: {e}");
    }
}
