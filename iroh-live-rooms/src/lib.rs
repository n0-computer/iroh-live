//! Multi-party rooms over iroh gossip and MoQ.
//!
//! A room is a gossip topic. Its members announce themselves and the names of
//! the broadcasts they publish into the room. [`Rooms`] owns the gossip
//! instance, and [`Rooms::join`] returns a [`Room`]. Its membership is a
//! watched [`RoomState`], and [`Room::subscribe`] reads a member's broadcast
//! on demand. Nothing here knows what the broadcasts carry.
//!
//! A member stays in the room while its gossip lease holds. It renews the
//! lease every thirty seconds, and drops out about two minutes after the last
//! renewal, or at once when it leaves.
//!
//! Room broadcasts are private to the members. [`Room::publish`] places a
//! broadcast at `rooms/<topic>/<member>/<name>` with the membership as its
//! audience, and a member that leaves loses the paths.
//! [`Room::subscribe`] reads each member over the session with that member, so
//! no other peer can stand in for it.
//!
//! A room ticket is a bearer secret. Anyone who holds it can join the room,
//! read every member's broadcasts and publish into the room. Share it only
//! with the people you want in the room, over a channel you trust.
//!
//! The node's [policy](iroh_live_node::NodeConfig::with_policy) has to let
//! each peer publish under [`publish_scope`], or [`Room::subscribe`] fails.
//! `iroh_live::layout::LiveLayout::policy(&[Arc::new(publish_scope)])` does
//! that.
//!
//! ```no_run
//! # async fn example(node: iroh_live_node::Node)
//! # -> Result<(), Box<dyn std::error::Error>> {
//! use iroh_live_rooms::{RoomTicket, Rooms};
//!
//! let rooms = Rooms::new(&node);
//! // Mount the service with `rooms.mount(router_builder)`.
//! let room = rooms
//!     .join(&RoomTicket::generate(), Some("ada".into()))
//!     .await?;
//! // Fill it with tracks, or build a `LocalBroadcast` in it instead.
//! let _cam: moq_net::broadcast::Producer = room.publish("cam")?;
//!
//! let mut state = room.state();
//! # use n0_watcher::Watcher;
//! let room_state = state.get();
//! for (peer, member) in &room_state.members {
//!     for name in &member.broadcasts {
//!         let subscription = room.subscribe(*peer, name).await?;
//!         # drop(subscription);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! A member can end a broadcast and publish it again, or briefly drop this
//! node from its membership, without [`RoomState`] changing. So watch each
//! subscription for its end, and subscribe again while the state still lists
//! the broadcast.
//!
//! # Cancellation safety
//!
//! [`Rooms::join`] and [`Room::subscribe`] are cancellation safe: a dropped
//! `join` leaves the topic, and a dropped `subscribe` leaves its dial running.
//! [`Room::leave`] is not. It is idempotent: call it again to finish.

mod actor;
mod room;
mod rooms;
mod ticket;

pub use self::{
    room::{Room, RoomPeer, RoomState, parse_room_path, publish_scope},
    rooms::{ALPN, Error, Rooms},
    ticket::RoomTicket,
};
