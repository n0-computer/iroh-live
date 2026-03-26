//! Shared transport: create Live, publish via serve/relay/room combinations.

use iroh_live::{Live, media::publish::LocalBroadcast, rooms::RoomTicket, ticket::LiveTicket};
use moq_lite::BroadcastProducer;
use tracing::info;

use crate::args::TransportArgs;

/// Creates a [`Live`] instance. When `serve` is true, spawns with router so
/// incoming subscribers are accepted. When false, only outbound connections work.
pub async fn setup_live(serve: bool) -> anyhow::Result<Live> {
    let secret_key = iroh_live::util::secret_key_from_env()?;
    #[allow(unused_mut, reason = "mut needed when qlog feature is enabled")]
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0);
    #[cfg(feature = "qlog")]
    {
        let prefix = format!("live-{}", secret_key.public().fmt_short());
        builder = builder.transport_config(
            iroh::endpoint::QuicTransportConfig::builder()
                .qlog_from_env(&prefix)
                .build(),
        )
    }
    let endpoint = builder.secret_key(secret_key).bind().await?;
    if serve {
        Ok(Live::builder(endpoint).spawn_with_router())
    } else {
        Ok(Live::builder(endpoint).spawn())
    }
}

/// Publishes a [`LocalBroadcast`] according to transport flags: serve locally,
/// push to relay, and/or publish into a room. Prints ticket when serving.
pub async fn publish_broadcast(
    live: &Live,
    broadcast: &LocalBroadcast,
    args: &TransportArgs,
) -> anyhow::Result<()> {
    let serve = !args.no_serve;
    if serve {
        live.publish(&args.name, broadcast).await?;
        print_ticket(live, &args.name, args.no_qr);
    }

    if let Some(ref relay_addr) = args.relay {
        push_to_relay(live, &args.name, broadcast.producer().consume(), relay_addr).await?;
    }

    if let Some(ref room_ticket) = args.room {
        push_to_room(live, &args.name, broadcast.producer(), room_ticket).await?;
    }

    Ok(())
}

/// Publishes a raw [`BroadcastProducer`] according to transport flags.
pub async fn publish_producer(
    live: &Live,
    producer: BroadcastProducer,
    args: &TransportArgs,
) -> anyhow::Result<()> {
    let serve = !args.no_serve;
    if serve {
        live.publish_producer(&args.name, producer.clone()).await?;
        print_ticket(live, &args.name, args.no_qr);
    }

    if let Some(ref relay_addr) = args.relay {
        push_to_relay(live, &args.name, producer.consume(), relay_addr).await?;
    }

    if let Some(ref room_ticket) = args.room {
        push_to_room(live, &args.name, producer, room_ticket).await?;
    }

    Ok(())
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
) -> anyhow::Result<()> {
    let room = live.join_room(room_ticket.clone()).await?;
    room.publish(name, producer).await?;
    println!("room ticket: {}", room.ticket());
    // Keep room alive by leaking — it will be cleaned up on Live::shutdown().
    std::mem::forget(room);
    Ok(())
}

fn print_ticket(live: &Live, name: &str, no_qr: bool) {
    let ticket = LiveTicket::new(live.endpoint().addr(), name);
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    if !no_qr && let Err(e) = qr2term::print_qr(&ticket_str) {
        tracing::warn!("could not print QR code: {e}");
    }
}
