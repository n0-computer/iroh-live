//! Calls between nodes, over real QUIC connections.
//!
//! The sides are bare broadcast producers, so no codec or device is involved.

use std::{
    collections::BTreeSet,
    sync::OnceLock,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointId, address_lookup::MemoryLookup, endpoint::presets, protocol::Router,
};
use iroh_live::{
    Audience, Node, NodeConfig, Reach,
    calls::{Call, CallEvent, Calls, Error, HungUp},
    layout::LiveLayout,
};
use moq_net::broadcast;
use n0_future::StreamExt;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher as _;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Binds an endpoint that resolves the other test endpoints through a shared in-memory lookup.
async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

/// A node on the default layout, mounted, with its call service.
struct Peer {
    node: Node,
    router: Router,
    calls: Calls,
    _side: broadcast::Producer,
}

impl Peer {
    /// Starts a mounted node with its call service.
    async fn start() -> Self {
        let (node, router) = Self::raw().await;
        let (calls, side) = Calls::new(&node, &LiveLayout::default()).expect("calls");
        Self {
            node,
            router,
            calls,
            _side: side,
        }
    }

    /// A node on the default layout and its router, with no call service.
    async fn raw() -> (Node, Router) {
        let endpoint = endpoint().await;
        let node = Node::new(endpoint.clone(), LiveLayout::default().config(&[]));
        let router = node.mount(Router::builder(endpoint)).spawn();
        (node, router)
    }

    /// Returns the endpoint id of the node.
    fn id(&self) -> EndpointId {
        self.node.id()
    }

    /// Shuts down the node and its router.
    async fn shutdown(self) {
        self.node.shutdown().await;
        self.router.shutdown().await.expect("router");
    }
}

/// Polls `condition` until it holds, or fails after [`TIMEOUT`].
async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Awaits `future`, or fails after [`TIMEOUT`].
async fn step<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

/// Places a call from `a` to `b`, which answers once it rings.
async fn connect(a: &Peer, b: &Peer) -> (Call, Call) {
    let (ringing, placed) = tokio::join!(
        async {
            eventually("b rings", || b.calls.ringing().get().contains(&a.id())).await;
            step("answer", b.calls.answer(a.id()))
                .await
                .expect("answer")
        },
        step("call", a.calls.call(b.id())),
    );
    (placed.expect("call"), ringing)
}

#[tokio::test]
#[traced_test]
async fn a_call_rings_answers_and_hangs_up() {
    let (a, b) = (Peer::start().await, Peer::start().await);
    let mut events = b.calls.events();

    let call_a = {
        let calls = a.calls.clone();
        let peer = b.id();
        tokio::spawn(async move { calls.call(peer).await })
    };
    let event = step("ring event", events.next()).await.expect("an event");
    assert_eq!(event, CallEvent::Ringing(a.id()));
    assert_eq!(b.calls.ringing().get(), BTreeSet::from([a.id()]));

    let call_b = step("answer", b.calls.answer(a.id()))
        .await
        .expect("answer");
    let call_a = step("call", call_a).await.expect("task").expect("call");
    assert_eq!(call_a.peer(), b.id());
    assert_eq!(call_b.peer(), a.id());
    assert!(
        b.calls.ringing().get().is_empty(),
        "an active call is not ringing"
    );

    drop(call_a);
    assert_eq!(step("hang-up", call_b.ended()).await, HungUp::Withdrawn);

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn ringing_each_other_makes_one_call() {
    let (a, b) = (Peer::start().await, Peer::start().await);

    let (from_a, from_b) = tokio::join!(
        step("a calls", a.calls.call(b.id())),
        step("b calls", b.calls.call(a.id())),
    );
    let (from_a, from_b) = (from_a.expect("a's call"), from_b.expect("b's call"));
    assert_eq!(from_a.peer(), b.id());
    assert_eq!(from_b.peer(), a.id());
    assert!(a.calls.ringing().get().is_empty());
    assert!(b.calls.ringing().get().is_empty());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_second_call_to_the_same_peer_fails() {
    let (a, b) = (Peer::start().await, Peer::start().await);
    let _calls = connect(&a, &b).await;

    let err = a.calls.call(b.id()).await.expect_err("already in a call");
    assert!(matches!(err, Error::AlreadyInCall { .. }), "{err:#}");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn answer_requires_a_ring() {
    let (a, b) = (Peer::start().await, Peer::start().await);

    let err = a.calls.answer(b.id()).await.expect_err("nobody rings");
    assert!(matches!(err, Error::NotRinging { .. }), "{err:#}");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn calls_need_the_grant() {
    let endpoint = endpoint().await;
    let node = Node::new(endpoint.clone(), NodeConfig::default());

    let result = Calls::new::<broadcast::Producer>(&node, &LiveLayout::default());
    let err = result
        .err()
        .expect("a subscribe-only node cannot publish its side");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    node.shutdown().await;
    endpoint.close().await;
}

#[tokio::test]
#[traced_test]
async fn a_new_events_stream_replays_ringing() {
    let (a, b) = (Peer::start().await, Peer::start().await);

    a.calls.ring(b.id());
    eventually("b rings", || b.calls.ringing().get().contains(&a.id())).await;
    let mut events = b.calls.events();
    let first = step("replay", events.next()).await.expect("an event");
    assert_eq!(first, CallEvent::Ringing(a.id()));

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_hung_up_peer_rings_again_only_after_withdrawing() {
    let (a, b) = (Peer::start().await, Peer::start().await);
    let (call_a, call_b) = connect(&a, &b).await;

    call_b.hang_up();
    assert!(
        !b.calls.ringing().get().contains(&a.id()),
        "a stays out while its old offer stands"
    );
    assert_eq!(step("hang-up", call_a.ended()).await, HungUp::Withdrawn);
    drop(call_a);
    eventually("the offer was withdrawn", || {
        !b.calls.ringing().get().contains(&a.id())
    })
    .await;

    // The announcement listing can miss a withdrawal that a new offer follows
    // at once, so give the withdrawal time to arrive.
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.calls.ring(b.id());
    eventually("a rings again", || {
        b.calls.ringing().get().contains(&a.id())
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn losing_the_session_ends_the_call_as_lost() {
    let (a, b) = (Peer::start().await, Peer::start().await);
    let (call_a, _call_b) = connect(&a, &b).await;

    for session in a.node.sessions().get() {
        session.close("test");
    }
    assert_eq!(step("lost", call_a.ended()).await, HungUp::Lost);

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_callee_that_shuts_down_is_reported_as_lost() {
    let (a, b) = (Peer::start().await, Peer::start().await);
    let mut events = a.calls.events();
    let (call_a, call_b) = connect(&a, &b).await;

    // b keeps its call, so its side goes only with its node.
    let peer = b.id();
    b.shutdown().await;
    assert_eq!(step("lost", call_a.ended()).await, HungUp::Lost);
    let hung_up = step("hang-up event", async {
        loop {
            match events.next().await.expect("an event") {
                CallEvent::HungUp(from, reason) => break (from, reason),
                _ => continue,
            }
        }
    })
    .await;
    assert_eq!(hung_up, (peer, HungUp::Lost));

    drop(call_b);
    a.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_cancelled_call_withdraws_the_ring() {
    let (a, b) = (Peer::start().await, Peer::start().await);

    let waited = tokio::time::timeout(Duration::from_millis(500), a.calls.call(b.id())).await;
    assert!(waited.is_err(), "b never answers");
    eventually("the ring was withdrawn", || {
        !b.calls.ringing().get().contains(&a.id())
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

/// A peer that places a call the way `irl call` does: with raw node calls.
#[tokio::test]
#[traced_test]
async fn calls_interoperate_with_a_raw_path_caller() {
    let layout = LiveLayout::default();
    let b = Peer::start().await;
    let (raw, router) = Peer::raw().await;
    let ringing = n0_watcher::Watchable::new(BTreeSet::from([b.id()]));
    // Ringing holds a session with the peer, so the raw caller dials first.
    step("dial", raw.connect(b.node.endpoint().addr()))
        .await
        .expect("dial b");
    let side = layout
        .publish_with(&raw, "call", Audience::Peers(ringing), |producer| {
            Ok::<_, std::convert::Infallible>(producer.clone())
        })
        .expect("publish the raw side");

    eventually("b rings", || b.calls.ringing().get().contains(&raw.id())).await;
    let call = step("answer", b.calls.answer(raw.id()))
        .await
        .expect("answer");
    assert_eq!(call.peer(), raw.id());

    // The raw side reads b's side once b rang it back.
    let path = layout.path(b.id(), "call").expect("a path");
    step("raw subscribes", raw.subscribe(path, Reach::Direct(b.id())))
        .await
        .expect("raw subscribe");

    drop(side);
    raw.shutdown().await;
    router.shutdown().await.expect("router");
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn calls_interoperate_with_a_raw_path_callee() {
    let layout = LiveLayout::default();
    let a = Peer::start().await;
    let (raw, router) = Peer::raw().await;

    let calls = a.calls.clone();
    let peer = raw.id();
    let placed = tokio::spawn(async move { calls.call(peer).await });

    // The raw side sees a's side appear, publishes its own, and reads a's.
    let a_side = layout.path(a.id(), "call").expect("a path");
    let mut announced = raw.origin().announced();
    step("ring", async {
        while let Some(event) = announced.next().await {
            if let moq_net::announce::Event::Present(announce) = event
                && announce.prefix == a_side
            {
                return;
            }
        }
    })
    .await;
    let (_publication, _side) = layout
        .publish_with(&raw, "call", Audience::peers([a.id()]), |producer| {
            Ok::<_, std::convert::Infallible>(producer.clone())
        })
        .expect("publish the raw side");
    step(
        "raw subscribes",
        raw.subscribe(a_side, Reach::Direct(a.id())),
    )
    .await
    .expect("raw subscribe");

    let call = step("call", placed).await.expect("task").expect("call");
    assert_eq!(call.peer(), raw.id());

    raw.shutdown().await;
    router.shutdown().await.expect("router");
    a.shutdown().await;
}
