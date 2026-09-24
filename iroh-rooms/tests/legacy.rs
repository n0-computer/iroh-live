//! A room with a member on the release before publisher-named paths, over real
//! QUIC connections, in both directions.
//!
//! The older member is written out by hand from what that release did: it
//! announces `LegacyPeerState` in the room's gossip map, publishes its
//! broadcasts at `rooms/<topic>/<name>` to anyone who connects, writes chat as
//! bare text on a `chat` track of its broadcasts, and reads chat from the
//! `chat` track of every broadcast a member lists.

mod common;

use std::time::Duration;

use common::{Peer, TIMEOUT, endpoint, wait_for_state};
use iroh::protocol::Router;
use iroh_gossip::Gossip;
use iroh_moq::{Audience, Moq, MoqConfig};
use iroh_rooms::RoomTicket;
use iroh_smol_kv::{ExpiryConfig, WriteScope};
use moq_net::{Timestamp, broadcast, track};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;
use serde::Serialize;

/// The announcement the older release writes.
#[derive(Serialize)]
struct LegacyPeerState {
    broadcasts: Vec<String>,
    display_name: Option<String>,
}

/// A member on the older release.
struct OldMember {
    id: iroh::EndpointId,
    moq: Moq,
    writer: WriteScope,
    topic: iroh_gossip::TopicId,
    _kv: iroh_smol_kv::Client,
    router: Router,
}

impl OldMember {
    /// Joins the room `ticket` names, bootstrapping from `via`.
    async fn join(ticket: &RoomTicket, via: iroh::EndpointId) -> Self {
        let endpoint = endpoint().await;
        let moq = Moq::new(endpoint.clone(), MoqConfig::default());
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let mut router = Router::builder(endpoint.clone());
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, moq.clone());
        }
        let router = router.accept(iroh_rooms::ALPN, gossip.clone()).spawn();
        let topic = tokio::time::timeout(TIMEOUT, gossip.subscribe(ticket.topic_id(), vec![via]))
            .await
            .expect("timed out joining the topic")
            .expect("join the topic");
        let kv = iroh_smol_kv::Client::local(
            topic,
            iroh_smol_kv::Config {
                anti_entropy_interval: Duration::from_secs(60),
                fast_anti_entropy_interval: Duration::from_secs(1),
                expiry: Some(ExpiryConfig {
                    check_interval: Duration::from_secs(10),
                    horizon: Duration::from_secs(120),
                }),
            },
        );
        let writer = kv.write(endpoint.secret_key().clone());
        Self {
            id: endpoint.id(),
            moq,
            writer,
            topic: ticket.topic_id(),
            _kv: kv,
            router,
        }
    }

    /// Announces itself with `broadcasts` under `display_name`.
    async fn announce(&self, broadcasts: &[&str], display_name: &str) {
        let state = LegacyPeerState {
            broadcasts: broadcasts.iter().map(|name| (*name).to_owned()).collect(),
            display_name: Some(display_name.to_owned()),
        };
        self.writer
            .put(
                b"s".as_slice(),
                postcard::to_stdvec(&state).expect("encode"),
            )
            .await
            .expect("announce");
    }

    /// Publishes a broadcast with a `chat` track at `rooms/<topic>/<name>`,
    /// to anyone, as the older release did.
    fn publish(&self, name: &str) -> (broadcast::Producer, track::Producer) {
        let broadcast = broadcast::Info::new().produce();
        let chat = broadcast
            .create_track(
                "chat",
                track::Info::default().with_max_age(Duration::from_secs(5)),
            )
            .expect("chat track");
        self.moq
            .publish_at(
                format!("rooms/{}/{name}", self.topic),
                &broadcast,
                Audience::Everyone,
            )
            .expect("publish");
        (broadcast, chat)
    }

    async fn shutdown(self) {
        self.moq.shutdown().await;
        self.router.shutdown().await.expect("router");
    }
}

/// Sends `text` on `track` until `chat` receives it, and fails after
/// [`TIMEOUT`].
async fn say_until_heard(
    track: &mut track::Producer,
    text: &str,
    chat: &mut iroh_rooms::ChatReceiver,
) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            track
                .write_frame(Timestamp::now(), text.as_bytes().to_vec())
                .expect("write");
            if let Ok(Ok(message)) =
                tokio::time::timeout(Duration::from_millis(500), chat.recv()).await
                && message.text == text
            {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{text:?} never arrived"));
}

/// A current member sees an older one, reads its chat, and still hears it
/// after it restarts its chat, whose sequence then begins again. The older
/// member reads the current one's chat where it always looked.
#[tokio::test]
#[traced_test]
async fn a_room_works_with_a_member_on_the_older_release() {
    let alice = Peer::spawn().await;
    let room = alice.join(&RoomTicket::generate(), "alice").await;
    let old = OldMember::join(&room.ticket(), alice.id()).await;
    let (cam, mut chat_track) = old.publish("cam");
    old.announce(&["cam"], "old").await;

    let seen = wait_for_state(&room, "alice sees the older member", |state| {
        state
            .peers
            .get(&old.id)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;
    assert_eq!(seen.peers[&old.id].display_name.as_deref(), Some("old"));

    // Its chat, off the broadcast it publishes. Two lines, so what it says
    // after a restart starts below what was delivered.
    let mut chat = room.chat();
    say_until_heard(&mut chat_track, "one", &mut chat).await;
    say_until_heard(&mut chat_track, "two", &mut chat).await;

    // It restarts, as a process that crashed and came back within its lease
    // does: its session ends, it publishes anew with its sequence at zero,
    // and it announces itself again, renamed, which brings the reader back.
    // It says one line, once the reader is back, at a sequence below what
    // was delivered before.
    cam.finish();
    for session in old.moq.sessions().get() {
        session.close("restarting");
    }
    let (_cam, mut chat_track) = old.publish("cam");
    old.announce(&["cam"], "old, restarted").await;
    tokio::time::timeout(TIMEOUT, chat_track.used())
        .await
        .expect("the reader never came back")
        .expect("track");
    chat_track
        .write_frame(Timestamp::now(), b"back".to_vec())
        .expect("write");
    let message = tokio::time::timeout(TIMEOUT, chat.recv())
        .await
        .expect("a restarted older member was not heard")
        .expect("a message");
    assert_eq!(message.text, "back");
    assert_eq!(message.from, old.id);

    // The other direction: the older member finds the current one's chat
    // broadcast at the older path, and reads the bare text it expects.
    let session = tokio::time::timeout(TIMEOUT, old.moq.connect(alice.id()))
        .await
        .expect("timed out")
        .expect("connect");
    let subscription = tokio::time::timeout(
        TIMEOUT,
        session.subscribe(format!("rooms/{}/.chat", old.topic)),
    )
    .await
    .expect("timed out resolving the current member's chat")
    .expect("resolve");
    let mut reader = subscription
        .as_moq()
        .track("chat")
        .expect("track")
        .subscribe(track::Subscription::default().with_max_age(Duration::from_secs(5)))
        .await
        .expect("subscribe");
    let heard = tokio::time::timeout(TIMEOUT, async {
        loop {
            room.send_chat("hi from the present").await.expect("send");
            let next = tokio::time::timeout(Duration::from_millis(500), reader.recv_group()).await;
            if let Ok(Ok(Some(mut group))) = next
                && let Ok(Some(frame)) = group.read_frame().await
            {
                return String::from_utf8(frame.payload.to_vec()).expect("text");
            }
        }
    })
    .await
    .expect("the older member never heard the current one");
    assert_eq!(heard, "hi from the present");

    old.shutdown().await;
    alice.shutdown().await;
}
