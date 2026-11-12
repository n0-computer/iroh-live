//! The room service: joins rooms over one gossip instance.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
};
use iroh_gossip::Gossip;
use iroh_live_node::Node;
use iroh_smol_kv::ExpiryConfig;
use n0_error::stack_error;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
use tokio::sync::{OnceCell, mpsc};
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    actor::{Actor, EXPIRY_CHECK_INTERVAL, STATE_HORIZON},
    room::{Inner, Room, RoomState},
    ticket::RoomTicket,
};

/// The ALPN rooms speak, iroh-gossip's.
///
/// [`Rooms::mount`] mounts the service under it.
pub const ALPN: &[u8] = iroh_gossip::ALPN;

/// An error from a room or the room service.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// The room's gossip topic could not be joined.
    #[error(transparent)]
    Gossip(iroh_gossip::api::ApiError),
    /// The node failed a publication, a subscription or a dial.
    #[error(transparent)]
    Node(iroh_live_node::Error),
    /// A broadcast name that is empty or holds a slash.
    #[error("invalid broadcast name {name:?}")]
    InvalidName { name: String },
    /// The room was left.
    #[error("the room was left")]
    Left,
}

/// Rooms on one endpoint.
///
/// Owns the gossip instance all rooms on the endpoint share. Cheap to clone,
/// and [`shutdown`](Self::shutdown) ends it for every clone.
///
/// # Examples
///
/// ```no_run
/// # async fn run(node: iroh_live_node::Node, ticket: iroh_live_rooms::RoomTicket)
/// # -> Result<(), iroh_live_rooms::Error> {
/// use iroh_live_rooms::Rooms;
///
/// let rooms = Rooms::new(&node);
/// // Mount the service with `rooms.mount(router_builder)` to accept members.
/// let room = rooms.join(&ticket, Some("ada".into())).await?;
/// # drop(room);
/// rooms.shutdown().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Rooms {
    node: Node,
    gossip: Gossip,
    /// Set once the gossip has shut down, so the router's shutdown does not repeat it.
    shut_down: Arc<OnceCell<()>>,
    /// Set when [`shutdown`](Self::shutdown) starts.
    ///
    /// The rooms joined here read it, so the gossip map ending under them is
    /// a clean stop and not a failure.
    closing: Arc<AtomicBool>,
    /// Set once the service is mounted on a router.
    mounted: Arc<AtomicBool>,
    /// Set once a join has warned that the service is not mounted.
    warned_unmounted: Arc<AtomicBool>,
}

impl Rooms {
    /// Creates the room service on `node`'s endpoint, with its own gossip.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn new(node: &Node) -> Self {
        let gossip = Gossip::builder().spawn(node.endpoint().clone());
        Self::with_gossip(node, gossip)
    }

    /// Creates the room service over an existing gossip instance.
    ///
    /// The service takes over the gossip: [`shutdown`](Self::shutdown) shuts
    /// it down.
    #[must_use]
    pub fn with_gossip(node: &Node, gossip: Gossip) -> Self {
        Self {
            node: node.clone(),
            gossip,
            shut_down: Arc::default(),
            closing: Arc::default(),
            mounted: Arc::default(),
            warned_unmounted: Arc::default(),
        }
    }

    /// Mounts the service's gossip protocol on `router` under [`ALPN`].
    ///
    /// Without it, other members cannot reach this one's gossip. The router's
    /// shutdown calls [`shutdown`](Self::shutdown).
    #[must_use]
    pub fn mount(&self, router: RouterBuilder) -> RouterBuilder {
        self.mounted.store(true, Ordering::Relaxed);
        router.accept(ALPN, Handler(self.clone()))
    }

    /// Shuts the gossip down for every clone.
    ///
    /// Leaves every gossip topic, so the rooms joined here stop. Call
    /// [`Room::leave`] first to tell the other members at once. Idempotent,
    /// and concurrent calls wait for the same shutdown.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.shut_down
            .get_or_init(|| async {
                if let Err(err) = self.gossip.shutdown().await {
                    warn!(error = %err, "failed to shut down the rooms' gossip");
                }
                debug!("rooms shut down");
            })
            .await;
    }

    /// Joins the room `ticket` names, as `display_name` if given.
    ///
    /// Subscribes to the room's gossip topic and starts announcing this
    /// member.
    ///
    /// # Errors
    ///
    /// Fails if the gossip topic cannot be joined.
    pub async fn join(
        &self,
        ticket: &RoomTicket,
        display_name: Option<String>,
    ) -> Result<Room, Error> {
        if !self.mounted.load(Ordering::Relaxed)
            && !self.warned_unmounted.swap(true, Ordering::Relaxed)
        {
            warn!("rooms are not mounted on a router: peers cannot reach this member's gossip");
        }
        let topic = ticket.topic_id();
        let me = self.node.endpoint().id();
        let gossip_topic = self
            .gossip
            .subscribe(topic, ticket.bootstrap().to_vec())
            .await?;
        let kv = iroh_smol_kv::Client::local(
            gossip_topic,
            iroh_smol_kv::Config {
                anti_entropy_interval: Duration::from_secs(60),
                fast_anti_entropy_interval: Duration::from_secs(1),
                expiry: Some(ExpiryConfig {
                    check_interval: EXPIRY_CHECK_INTERVAL,
                    horizon: STATE_HORIZON,
                }),
            },
        );
        let writer = kv.write(self.node.endpoint().secret_key().clone());

        let (leave, inbox) = mpsc::channel(1);
        // Without `test-util` nothing keeps the sender, so the actor's
        // restart branch never fires.
        let (restart_map, restarts) = mpsc::channel(1);
        #[cfg(not(feature = "test-util"))]
        drop(restart_map);
        let inner = Arc::new(Inner {
            me,
            ticket: ticket.clone(),
            node: self.node.clone(),
            state: Watchable::new(RoomState::default()),
            members: Watchable::new(BTreeSet::new()),
            leave,
            local: Mutex::new(BTreeMap::new()),
            local_changed: Watchable::new(0),
            display_name: Watchable::new(display_name),
            done: Watchable::new(false),
            #[cfg(feature = "test-util")]
            restart_map,
        });
        let actor = Actor {
            inner: inner.clone(),
            kv,
            writer,
            members: BTreeMap::new(),
            resync: None,
            rooms_closing: self.closing.clone(),
        };
        let span = info_span!("room", topic = %topic.fmt_short(), me = %me.fmt_short());
        let task = tokio::spawn(actor.run(inbox, restarts).instrument(span));
        info!(topic = %topic.fmt_short(), "joined room");
        Ok(Room {
            inner,
            _actor: Arc::new(AbortOnDropHandle::new(task)),
        })
    }
}

/// The gossip handler of a [`Rooms`], which shuts the rooms down with the router.
#[derive(Debug, Clone)]
struct Handler(Rooms);

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.0.gossip.accept(connection).await
    }

    async fn shutdown(&self) {
        self.0.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use iroh::{Endpoint, endpoint::presets, protocol::Router};
    use iroh_live_node::NodeConfig;

    use super::*;

    /// Mounting the service, or taking its handler, is what stops the unmounted warning.
    #[tokio::test]
    async fn mounting_marks_the_rooms_mounted() {
        let endpoint = Endpoint::bind(presets::Minimal).await.expect("binds");
        let node = Node::new(endpoint.clone(), NodeConfig::default());
        let rooms = Rooms::new(&node);
        assert!(!rooms.mounted.load(Ordering::Relaxed));

        let router = rooms.mount(Router::builder(endpoint)).spawn();
        assert!(rooms.mounted.load(Ordering::Relaxed));
        router.shutdown().await.expect("the router shuts down");
    }
}
