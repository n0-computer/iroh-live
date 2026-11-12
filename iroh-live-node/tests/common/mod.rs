//! Shared harness for the node tests.

#![allow(
    dead_code,
    unused_imports,
    reason = "each test file uses a subset of the harness"
)]
#![allow(
    clippy::mod_module_files,
    reason = "a `tests/common.rs` would become a test binary of its own"
)]

use std::time::Duration;

use bytes::Bytes;
use iroh::EndpointId;
pub(crate) use iroh_live_node::test_util::{TIMEOUT, TestNode, endpoint, eventually, step};
use iroh_live_node::{
    Audience, Error, Grant, Node, NodeConfig, Publication, Session,
    policy::{self, PeerRequest, Policy},
};
use moq_net::{Pattern, announce, broadcast, track};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;

/// How long a test track keeps its groups, on both ends.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(5);

/// Returns the subscription the tests read with, which tolerates [`MAX_AGE`] of reordering.
pub(crate) fn subscription() -> track::Subscription {
    track::Subscription::default().with_delay(track::Delay::default().with_max(MAX_AGE))
}

/// A [`TestNode`] whose peers may publish only under `live/<their id>/` by default.
#[derive(Debug, derive_more::Deref)]
pub(crate) struct Peer(TestNode);

impl Peer {
    /// Spawns a node whose peers may publish only under `live/<their id>/`.
    pub(crate) async fn spawn() -> Self {
        Self::with_config(NodeConfig::default()).await
    }

    /// Returns `live/<this node's id>/<name>`.
    pub(crate) fn path(&self, name: &str) -> String {
        format!("live/{}/{name}", self.id())
    }

    /// Spawns a node with `config`, filling in the policy `spawn` gives.
    pub(crate) async fn with_config(config: NodeConfig) -> Self {
        let config = match config.policy {
            Some(_) => config,
            None => config.with_policy(own_paths_policy()),
        };
        Self::bare(config).await
    }

    /// Spawns a node with exactly `config`, the default policy included.
    pub(crate) async fn bare(config: NodeConfig) -> Self {
        Self(TestNode::spawn(config).await)
    }
}

/// Returns a grant to subscribe anywhere and publish under `live/<peer>/` only.
pub(crate) fn own_paths(peer: EndpointId) -> Grant {
    let own: Pattern = format!("live/{peer}/**").parse().expect("pattern");
    Grant::publish_under(own)
}

/// Returns a policy that admits every peer with [`own_paths`].
pub(crate) fn own_paths_policy() -> impl Policy<EndpointId> + Clone {
    policy::from_fn(|request: &PeerRequest| Ok(own_paths(*request.peer()).into()))
}

/// A broadcast whose one track writes a counter every few milliseconds.
pub(crate) struct TestBroadcast {
    pub(crate) producer: broadcast::Producer,
    _writer: AbortOnDropHandle<()>,
}

impl TestBroadcast {
    /// Writes the counter into `producer`, starting at `first`.
    ///
    /// A distinct `first` tells two broadcasts apart.
    pub(crate) fn writing(producer: broadcast::Producer, first: u64) -> Self {
        let mut track = producer
            .create_track("video", track::Info::default().with_max_age(MAX_AGE))
            .expect("create track");
        let writer = tokio::spawn(async move {
            for n in first.. {
                if track
                    .write_frame(None, Bytes::from(n.to_be_bytes().to_vec()))
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Self {
            producer,
            _writer: AbortOnDropHandle::new(writer),
        }
    }
}

/// A publication whose broadcast writes the counter.
///
/// Derefs to the [`Publication`].
#[derive(derive_more::Debug)]
#[debug("{publication:?}")]
pub(crate) struct Published {
    pub(crate) publication: Publication,
    _broadcast: TestBroadcast,
}

impl std::ops::Deref for Published {
    type Target = Publication;

    fn deref(&self) -> &Publication {
        &self.publication
    }
}

/// Publishes a counter broadcast at `path` on `moq`.
pub(crate) fn publish(
    moq: &Node,
    path: impl AsRef<str>,
    audience: Audience,
) -> Result<Published, iroh_live_node::Error> {
    publish_from(moq, path, audience, 0)
}

/// Publishes a counter broadcast at `path` on `moq`, counting from `first`.
pub(crate) fn publish_from(
    moq: &Node,
    path: impl AsRef<str>,
    audience: Audience,
    first: u64,
) -> Result<Published, iroh_live_node::Error> {
    let publication = moq.publish(path.as_ref(), audience)?;
    let broadcast = TestBroadcast::writing(publication.broadcast().clone(), first);
    Ok(Published {
        publication,
        _broadcast: broadcast,
    })
}

/// Reads frames from the test track until one arrives, and returns its counter.
pub(crate) async fn read_counter(broadcast: &broadcast::Consumer) -> u64 {
    tokio::time::timeout(TIMEOUT, async {
        let mut subscriber = broadcast
            .track("video")
            .expect("track")
            .subscribe(subscription())
            .await
            .expect("subscribe to track");
        loop {
            let mut group = subscriber
                .recv_group()
                .await
                .expect("track failed")
                .expect("track ended");
            if let Some(frame) = group.read_frame().await.expect("group failed") {
                let bytes: [u8; 8] = frame.payload[..].try_into().expect("a u64");
                return u64::from_be_bytes(bytes);
            }
        }
    })
    .await
    .expect("timed out reading a frame")
}

/// Subscribes to the test track and reads one group from it.
pub(crate) async fn reading(broadcast: &broadcast::Consumer) -> track::Subscriber {
    tokio::time::timeout(TIMEOUT, async {
        let mut subscriber = broadcast
            .track("video")
            .expect("track")
            .subscribe(subscription())
            .await
            .expect("subscribe to track");
        subscriber
            .recv_group()
            .await
            .expect("track failed")
            .expect("track ended");
        subscriber
    })
    .await
    .expect("timed out reading a group")
}

/// Waits until `subscriber`'s track ends, failing with `what` after [`TIMEOUT`].
pub(crate) async fn ends(what: &str, subscriber: &mut track::Subscriber) {
    let ended = tokio::time::timeout(TIMEOUT, async {
        while let Ok(Some(_)) = subscriber.recv_group().await {}
    })
    .await;
    assert!(ended.is_ok(), "still reading: {what}");
}

/// Asserts that `future` does not complete within `window`.
pub(crate) async fn stays_pending<T: std::fmt::Debug>(
    what: &str,
    window: Duration,
    future: impl std::future::Future<Output = T>,
) {
    if let Ok(done) = tokio::time::timeout(window, future).await {
        panic!("{what}: {done:?}");
    }
}

/// Waits until `updates` announces `path`.
///
/// A cursor coalesces an announcement it has not handed out with its
/// retraction, so wait for this before [`retracted`].
pub(crate) async fn announced(updates: &mut announce::Consumer, path: &str) {
    wait_for(updates, path, true).await
}

/// Waits until `updates` retracts `path`.
pub(crate) async fn retracted(updates: &mut announce::Consumer, path: &str) {
    wait_for(updates, path, false).await
}

async fn wait_for(updates: &mut announce::Consumer, path: &str, active: bool) {
    step(&format!("{path} announced: {active}"), async {
        loop {
            let event = updates.next().await.expect("origin closed");
            let (announce, now_active) = match event {
                announce::Event::Absent(announce) | announce::Event::Offline(announce) => {
                    (announce, false)
                }
                announce::Event::Present(announce)
                | announce::Event::Unavailable(announce)
                | announce::Event::Available(announce) => (announce, true),
            };
            if announce.prefix.as_str() == path && now_active == active {
                return;
            }
        }
    })
    .await
}

/// Waits until `node` holds a session with `peer`, and returns it.
///
/// Waits without a bound. Wrap it in [`step`].
pub(crate) async fn session_with(node: &Node, peer: EndpointId) -> Session {
    let mut sessions = node.sessions();
    loop {
        if let Some(session) = sessions
            .get()
            .into_iter()
            .find(|session| session.peer() == peer)
        {
            return session;
        }
        sessions.updated().await.expect("the node is alive");
    }
}

/// Reports whether `node` lists a session with `peer`.
pub(crate) fn has_session_with(node: &Node, peer: EndpointId) -> bool {
    node.sessions()
        .get()
        .iter()
        .any(|session| session.peer() == peer)
}

/// Waits for a dialed session the remote side refuses to end, or checks that the dial failed.
pub(crate) async fn dial_refused(dial: Result<Session, Error>) {
    match dial {
        Ok(session) => {
            step("the refused session closes", session.closed()).await;
        }
        Err(err) => assert!(matches!(err, Error::Refused { .. }), "{err:#}"),
    }
}
