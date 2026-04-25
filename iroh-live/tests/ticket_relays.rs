//! Integration tests for [`LiveTicket`] with embedded relay offers
//! and for [`Live::subscribe_ticket`].
//!
//! The `RelayOffer` field on a ticket lets a publisher advertise an
//! alternate source for the broadcast. Subscribers that hand the
//! ticket to [`Live::subscribe_ticket`] get a [`Subscription`]
//! whose source set contains the direct endpoint plus every
//! advertised relay; the default [`PreferOrdered`] policy picks the
//! first healthy source.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, EndpointAddr, address_lookup::MemoryLookup};
use iroh_live::{Live, RelayOffer, SourceId, ticket::LiveTicket};
use moq_media::{
    codec::VideoCodec,
    format::VideoPreset,
    publish::{LocalBroadcast, VideoInput},
    test_util::TestVideoSource,
};
use n0_tracing_test::traced_test;

const TIMEOUT: Duration = Duration::from_secs(15);
const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;

async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

fn broadcast_with_video() -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(15.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("set video");
    broadcast
}

/// A ticket without relays serialises and deserialises through the
/// binary form. Postcard is positional, so dropping the relay
/// length on the writer side would misalign the reader; this test
/// guards the wire format for the empty case.
#[test]
fn ticket_without_relays_round_trip() {
    let endpoint = iroh::SecretKey::generate().public();
    let addr = EndpointAddr::from(endpoint);
    let ticket = LiveTicket::new(addr, "stream");
    let bytes = ticket.to_bytes();
    let parsed = LiveTicket::from_bytes(&bytes).expect("decode");
    assert_eq!(parsed.broadcast_name, "stream");
    assert_eq!(parsed.endpoint, ticket.endpoint);
    assert!(parsed.relays.is_empty());
}

/// Serialises a ticket with relays and parses it back; the relay
/// offers round-trip exactly.
#[test]
fn ticket_with_relays_round_trip() {
    let endpoint = iroh::SecretKey::generate().public();
    let addr = EndpointAddr::from(endpoint);
    let relay_a = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/r1".into(),
        api_key: Some("token-a".into()),
    };
    let relay_b = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/r2".into(),
        api_key: None,
    };
    let ticket = LiveTicket::new(addr, "stream").with_relays([relay_a.clone(), relay_b.clone()]);

    let bytes = ticket.to_bytes();
    let parsed = LiveTicket::from_bytes(&bytes).expect("decode");
    assert_eq!(parsed.broadcast_name, "stream");
    assert_eq!(parsed.endpoint, ticket.endpoint);
    assert_eq!(parsed.relays.len(), 2);
    assert_eq!(parsed.relays[0], relay_a);
    assert_eq!(parsed.relays[1], relay_b);
}

/// A ticket without relays still produces a single-direct-source
/// subscription. The URL form (used in QR codes) does not yet carry
/// relay offers; only the binary form does. By design: relay
/// material is sensitive (JWTs in particular) and should not be
/// embedded in scannable URIs by default.
#[test]
fn ticket_url_form_does_not_carry_relays() {
    let endpoint = iroh::SecretKey::generate().public();
    let addr = EndpointAddr::from(endpoint);
    let ticket = LiveTicket::new(addr, "stream").with_relays([RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/r".into(),
        api_key: Some("secret".into()),
    }]);

    let url = ticket.to_string();
    let parsed: LiveTicket = url.parse().expect("parse url");
    assert!(
        parsed.relays.is_empty(),
        "URL form must not carry relay offers"
    );
    assert!(
        !url.contains("secret"),
        "URL form must not leak JWT material"
    );
}

/// `subscribe_ticket` with a ticket carrying only the direct
/// endpoint subscribes successfully and the active source matches
/// that endpoint.
#[tokio::test]
#[traced_test]
async fn subscribe_ticket_with_direct_only() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = broadcast_with_video();
    publisher
        .publish("stream", &broadcast)
        .await
        .expect("publish");

    let ticket = LiveTicket::new(publisher.endpoint().addr(), "stream");

    let subscriber = Live::builder(endpoint().await).with_router().spawn();
    let sub = subscriber.subscribe_ticket(&ticket);
    let active = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");

    assert_eq!(
        active.id,
        SourceId::direct(publisher.endpoint().id()),
        "active should be the ticket's direct endpoint"
    );

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// `subscribe_ticket` with both a direct endpoint and an
/// unreachable relay offer keeps the direct path active. The
/// subscription pre-resolves the source set from the ticket and
/// orders direct first.
#[tokio::test]
#[traced_test]
async fn subscribe_ticket_prefers_direct_over_unreachable_relay() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = broadcast_with_video();
    publisher
        .publish("stream", &broadcast)
        .await
        .expect("publish");

    let unreachable_relay = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/".into(),
        api_key: None,
    };
    let ticket =
        LiveTicket::new(publisher.endpoint().addr(), "stream").with_relays([unreachable_relay]);

    let subscriber = Live::builder(endpoint().await).with_router().spawn();
    let sub = subscriber.subscribe_ticket(&ticket);
    let active = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");

    assert!(
        active.id.is_direct(),
        "direct should win over an unreachable relay"
    );

    publisher.shutdown().await;
    subscriber.shutdown().await;
}
