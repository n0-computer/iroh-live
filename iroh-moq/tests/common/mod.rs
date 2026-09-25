//! Shared harness for the node tests.

#![allow(dead_code, reason = "each test file uses a subset of the harness")]
#![allow(
    clippy::mod_module_files,
    reason = "a `tests/common.rs` would become a test binary of its own"
)]

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use iroh::{
    Endpoint, EndpointId, address_lookup::MemoryLookup, endpoint::presets, protocol::Router,
};
use iroh_moq::{Grant, Moq, MoqConfig};
use moq_net::{Pattern, Patterns, Timestamp, broadcast, bytes::Bytes, track};
use n0_future::task::AbortOnDropHandle;

/// Generous, because the suite shares a machine with whatever else is running.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(20);

/// How long a test track keeps its groups, on both ends.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(5);

/// Binds an endpoint against a shared in-memory address lookup.
pub(crate) async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("failed to bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

/// A node that accepts MoQ, with the router that makes it do so.
pub(crate) struct Node {
    pub(crate) endpoint: Endpoint,
    pub(crate) moq: Moq,
    pub(crate) router: Router,
}

impl Node {
    /// Spawns a node whose peers may publish only under `live/<their id>/`.
    pub(crate) async fn spawn() -> Self {
        Self::with_config(MoqConfig::default()).await
    }

    /// Returns `live/<this node's id>/<name>`.
    pub(crate) fn path(&self, name: &str) -> String {
        format!("live/{}/{name}", self.id())
    }

    /// Spawns a node with `config`, filling in the grant `spawn` gives.
    pub(crate) async fn with_config(mut config: MoqConfig) -> Self {
        config.grant.get_or_insert_with(|| Arc::new(own_paths));
        let endpoint = endpoint().await;
        let moq = Moq::new(endpoint.clone(), config);
        let mut router = Router::builder(endpoint.clone());
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, moq.clone());
        }
        Self {
            endpoint,
            moq,
            router: router.spawn(),
        }
    }

    pub(crate) fn id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }

    pub(crate) async fn shutdown(self) {
        self.moq.shutdown().await;
        self.router.shutdown().await.expect("router task panicked");
        self.endpoint.close().await;
    }
}

/// Returns a grant to subscribe anywhere and publish under `live/<peer>/` only.
pub(crate) fn own_paths(peer: EndpointId) -> Grant {
    let own: Pattern = format!("live/{peer}/**").parse().expect("pattern");
    Grant {
        subscribe: Patterns::from(Pattern::all()),
        publish: Patterns::from(own),
    }
}

/// A broadcast whose one track writes a counter every few milliseconds.
pub(crate) struct TestBroadcast {
    pub(crate) producer: broadcast::Producer,
    _writer: AbortOnDropHandle<()>,
}

impl TestBroadcast {
    pub(crate) fn start() -> Self {
        Self::starting_at(0)
    }

    /// Starts a broadcast whose counter starts at `first`, to tell two apart.
    pub(crate) fn starting_at(first: u64) -> Self {
        let producer = broadcast::Info::new().produce();
        let mut track = producer
            .create_track("video", track::Info::default().with_max_age(MAX_AGE))
            .expect("create track");
        let writer = tokio::spawn(async move {
            for n in first.. {
                if track
                    .write_frame(Timestamp::now(), Bytes::from(n.to_be_bytes().to_vec()))
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

/// Reads frames from the test track until one arrives, and returns its counter.
pub(crate) async fn read_counter(broadcast: &broadcast::Consumer) -> u64 {
    tokio::time::timeout(TIMEOUT, async {
        let mut subscriber = broadcast
            .track("video")
            .expect("track")
            .subscribe(track::Subscription::default().with_max_age(MAX_AGE))
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
            .subscribe(track::Subscription::default().with_max_age(MAX_AGE))
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

/// Awaits `future`, failing with `what` after [`TIMEOUT`].
pub(crate) async fn step<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}
