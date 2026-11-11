//! Calls between nodes, over real QUIC connections.
//!
//! The sides are bare broadcast producers, so no codec or device is involved.

use std::{collections::BTreeSet, time::Duration};

use iroh::EndpointId;
use iroh_live::{
    Audience, NodeConfig, Reach,
    calls::{Call, CallEvent, Calls, Error, HungUp},
    layout::LiveLayout,
    node::policy::Policy,
};
use iroh_live_node::test_util::{TestNode, eventually, step};
use moq_net::broadcast;
use n0_future::StreamExt;
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher as _};

/// A [`TestNode`] on the default layout, with its call service.
struct Peer {
    test: TestNode,
    calls: Calls,
    _side: broadcast::Producer,
}

impl std::ops::Deref for Peer {
    type Target = TestNode;

    fn deref(&self) -> &TestNode {
        &self.test
    }
}

impl Peer {
    /// Starts a mounted node with its call service.
    async fn start() -> Self {
        Self::start_with(LiveLayout::default().config(&[])).await
    }

    /// Starts a mounted node with `config` and its call service.
    async fn start_with(config: NodeConfig) -> Self {
        let test = TestNode::spawn(config).await;
        let (calls, side) = Calls::new(&test.node, &LiveLayout::default()).expect("calls");
        Self {
            test,
            calls,
            _side: side,
        }
    }
}

/// Starts a mounted node on the default layout, with no call service.
async fn raw() -> TestNode {
    TestNode::spawn(LiveLayout::default().config(&[])).await
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
        session.close(moq_net::Error::Cancel);
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
    let test = raw().await;
    let raw = &test.node;
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
    test.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn calls_interoperate_with_a_raw_path_callee() {
    let layout = LiveLayout::default();
    let a = Peer::start().await;
    let test = raw().await;
    let raw = &test.node;

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

    test.shutdown().await;
    a.shutdown().await;
}

/// Waits until `calls` lists `peer` as ringing if `ringing`, or no longer does if not.
async fn rings(calls: &Calls, peer: EndpointId, ringing: bool) {
    let mut watch = calls.ringing();
    step("the ring turns", async {
        while watch.get().contains(&peer) != ringing {
            watch.updated().await.expect("calls closed");
        }
    })
    .await;
}

/// A reader that falls behind the bounded event queue gets `Lagged`, then the newer events.
#[tokio::test]
#[traced_test]
async fn a_lagging_event_reader_is_told_it_lagged() {
    // More rings than the event queue holds, which is 64.
    const RINGS: usize = 70;
    let layout = LiveLayout::default();
    let b = Peer::start().await;
    let test = raw().await;
    let raw = &test.node;
    step("dial", raw.connect(b.node.endpoint().addr()))
        .await
        .expect("dial b");
    let audience = n0_watcher::Watchable::new(BTreeSet::new());
    let _side = layout
        .publish_with(
            &raw,
            "call",
            Audience::Peers(audience.clone()),
            |producer| Ok::<_, std::convert::Infallible>(producer.clone()),
        )
        .expect("publish the raw side");

    // Subscribed now, and never read until the rings are over.
    let mut events = b.calls.events();
    for _ in 0..RINGS {
        audience.set(BTreeSet::from([b.id()])).ok();
        rings(&b.calls, raw.id(), true).await;
        audience.set(BTreeSet::new()).ok();
        rings(&b.calls, raw.id(), false).await;
    }

    let first = step("the first event", events.next()).await;
    assert_eq!(first, Some(CallEvent::Lagged));
    let next = step("the event after the lag", events.next()).await;
    assert_eq!(next, Some(CallEvent::Ringing(raw.id())));

    test.shutdown().await;
    b.shutdown().await;
}

/// A callee that admits only its contacts refuses a stranger's ring, and drops a removed contact.
#[tokio::test]
#[traced_test]
async fn a_contacts_only_callee_refuses_strangers_and_drops_removed_contacts() {
    let (alice, carol) = (Peer::start().await, Peer::start().await);
    let contacts = Watchable::new(BTreeSet::from([alice.id()]));
    let known = contacts.clone();
    let policy = LiveLayout::default()
        .policy(&[])
        .filter(move |request| known.get().contains(request.peer()));
    let bob = Peer::start_with(NodeConfig::default().with_policy(policy)).await;

    // Bob's policy refuses carol's session, so her ring never reaches him.
    let refused = step("a stranger's call", carol.calls.call(bob.id())).await;
    assert!(refused.is_err(), "a stranger's call fails");
    assert!(!bob.calls.ringing().get().contains(&carol.id()));

    // Alice is a contact, so her call goes through.
    let (call_alice, _call_bob) = connect(&alice, &bob).await;

    // Once she is no longer a contact, a re-decision closes her session.
    contacts.set(BTreeSet::new()).ok();
    bob.node.reauthorize(alice.id());
    assert_eq!(
        step("the call ends", call_alice.ended()).await,
        HungUp::Lost
    );
    assert!(bob.node.grant_for(alice.id()).is_none());

    for peer in [alice, bob, carol] {
        peer.shutdown().await;
    }
}
