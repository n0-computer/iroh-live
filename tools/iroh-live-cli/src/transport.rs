//! Shared transport: create Live, publish via serve/relay/room combinations.

use iroh_live::{
    Live,
    media::publish::LocalBroadcast,
    rooms::{Room, RoomTicket},
    ticket::LiveTicket,
};
use moq_lite::BroadcastProducer;
use tracing::info;

use crate::args::TransportArgs;

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
    let serve = !args.no_serve;
    if serve {
        live.publish(&args.name, broadcast).await?;
        print_ticket(live, &args.name, args.no_qr);
    }

    if let Some(ref relay_addr) = args.relay {
        push_to_relay(live, &args.name, broadcast.producer().consume(), relay_addr).await?;
    }

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
    let serve = !args.no_serve;
    if serve {
        live.publish_producer(&args.name, producer.clone()).await?;
        print_ticket(live, &args.name, args.no_qr);
    }

    if let Some(ref relay_addr) = args.relay {
        push_to_relay(live, &args.name, producer.consume(), relay_addr).await?;
    }

    let room = if let Some(ref room_ticket) = args.room {
        Some(push_to_room(live, &args.name, producer, room_ticket).await?)
    } else {
        None
    };

    Ok(room)
}

async fn push_to_relay(
    live: &Live,
    name: &str,
    consumer: moq_lite::BroadcastConsumer,
    relay_addr: &str,
) -> anyhow::Result<()> {
    let id: iroh::EndpointId = relay_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid relay address: {e}"))?;
    let session = live.transport().connect(id).await?;
    session.publish(name, consumer);
    info!(%relay_addr, "published to relay");
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
