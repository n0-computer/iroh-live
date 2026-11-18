//! Publishing a [`LocalBroadcast`] through the layout or a room, with the node's allocator.
//!
//! `LiveLayout` and `Room` live in crates that know no media types, so their
//! generic `publish` cannot hand a broadcast the node's allocator. The
//! extension traits here publish a [`LocalBroadcast`] and hand it over.

use iroh_live_node::{Node, layout::LiveLayout};
use moq_ext_publisher::LocalBroadcast;

/// Publishes a [`LocalBroadcast`] through a [`LiveLayout`] that follows the node's rate control.
///
/// The trait is sealed: only [`LiveLayout`] implements it. The generic
/// [`LiveLayout::publish`] stays for a bare producer, and
/// [`Node::publish_with`] with a manual
/// [`set_bandwidth`](LocalBroadcast::set_bandwidth) for another audience.
///
/// # Examples
///
/// ```no_run
/// use iroh_live::{
///     Node,
///     layout::LiveLayout,
///     media::{LiveLayoutExt, LocalBroadcast},
/// };
///
/// # fn run(node: Node) -> Result<(), Box<dyn std::error::Error>> {
/// let broadcast: LocalBroadcast = LiveLayout::default().publish_media(&node, "studio")?;
/// # drop(broadcast);
/// # Ok(())
/// # }
/// ```
pub trait LiveLayoutExt: sealed::Sealed {
    /// Publishes a [`LocalBroadcast`] at `<root>/<node id>/<name>` to granted sessions, and returns it.
    ///
    /// On a node with [`rate_control`](iroh_live_node::NodeConfig::rate_control),
    /// the broadcast's encoders follow [`Node::bandwidth`]. Otherwise this is
    /// [`LiveLayout::publish`].
    ///
    /// # Errors
    ///
    /// Fails like [`LiveLayout::publish`].
    fn publish_media(
        &self,
        node: &Node,
        name: &str,
    ) -> Result<LocalBroadcast, iroh_live_node::Error>;
}

impl LiveLayoutExt for LiveLayout {
    fn publish_media(
        &self,
        node: &Node,
        name: &str,
    ) -> Result<LocalBroadcast, iroh_live_node::Error> {
        let broadcast: LocalBroadcast = self.publish(node, name)?;
        follow_rate(node, &broadcast);
        Ok(broadcast)
    }
}

/// Publishes a [`LocalBroadcast`] into a [`Room`](iroh_live_rooms::Room) that follows the node's rate control.
///
/// The trait is sealed: only [`Room`](iroh_live_rooms::Room) implements it.
/// The generic [`Room::publish`](iroh_live_rooms::Room::publish) stays for a
/// bare producer.
#[cfg(feature = "rooms")]
pub trait RoomExt: sealed::Sealed {
    /// Publishes a [`LocalBroadcast`] into the room as `name`, to its members only, and returns it.
    ///
    /// On a node with [`rate_control`](iroh_live_node::NodeConfig::rate_control),
    /// the broadcast's encoders follow [`Node::bandwidth`]. Otherwise this is
    /// [`Room::publish`](iroh_live_rooms::Room::publish).
    ///
    /// # Errors
    ///
    /// Fails like [`Room::publish`](iroh_live_rooms::Room::publish).
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    fn publish_media(&self, name: &str) -> Result<LocalBroadcast, iroh_live_rooms::Error>;
}

#[cfg(feature = "rooms")]
impl RoomExt for iroh_live_rooms::Room {
    fn publish_media(&self, name: &str) -> Result<LocalBroadcast, iroh_live_rooms::Error> {
        let broadcast: LocalBroadcast = self.publish(name)?;
        follow_rate(self.node(), &broadcast);
        Ok(broadcast)
    }
}

/// Hands `node`'s allocator to `broadcast`, if the node has rate control.
pub(crate) fn follow_rate(node: &Node, broadcast: &LocalBroadcast) {
    if let Some(allocator) = node.bandwidth() {
        broadcast.set_bandwidth(allocator);
    }
}

mod sealed {
    /// Keeps the extension traits of [`publish`](super) to the types implemented here.
    #[allow(unnameable_types, reason = "a sealed trait is unnameable on purpose")]
    pub trait Sealed {}

    impl Sealed for iroh_live_node::layout::LiveLayout {}
    #[cfg(feature = "rooms")]
    impl Sealed for iroh_live_rooms::Room {}
}
