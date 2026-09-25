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
//! audience, and a member that leaves loses what it was reading.
//! [`Room::subscribe`] reads each member over the session with that member, so
//! no other peer can stand in for it. Membership itself is open to anyone with
//! the ticket.
//!
//! The node's [`MoqConfig::grant`](iroh_moq::MoqConfig::grant) has to let each
//! peer publish under [`publish_scope`], or room broadcasts never arrive.
//! `iroh_live::moq_config` does that with the `rooms` feature.
//!
//! ```no_run
//! # async fn example(moq: iroh_moq::Moq, broadcast: moq_net::broadcast::Producer)
//! # -> Result<(), Box<dyn std::error::Error>> {
//! use iroh_live_rooms::{RoomTicket, Rooms};
//!
//! let rooms = Rooms::new(&moq);
//! // Mount `rooms.protocol_handler()` under `iroh_live_rooms::ALPN` on the router.
//! let room = rooms
//!     .join(&RoomTicket::generate(), Some("ada".into()))
//!     .await?;
//! room.publish("cam", &broadcast)?;
//!
//! let mut state = room.state();
//! # use n0_watcher::Watcher;
//! let members = state.get();
//! for (peer, member) in &members.peers {
//!     for name in &member.broadcasts {
//!         let subscription = room.subscribe(*peer, name).await?;
//!         # drop(subscription);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Tiles that come back
//!
//! A member can end a broadcast and publish it again faster than its
//! announcement changes, and a member that briefly drops this node from its
//! membership cuts off what this node reads. Neither changes [`RoomState`]. A
//! grid should therefore drop tiles whose broadcast closed, on a timer as well
//! as on state updates, and reopen the ones the state still lists. With
//! `iroh-live`, check `RemoteBroadcast::is_closed()`, as `irl room` does in
//! `drop_closed`.
//!
//! # Cancellation safety
//!
//! [`Rooms::join`] and [`Room::subscribe`] are cancellation safe: a dropped
//! `join` leaves the topic, and a dropped `subscribe` leaves its dial running.
//! [`Room::leave`] is not. It is idempotent: call it again to finish.

mod room;
mod ticket;

pub use self::{
    room::{ALPN, Error, Room, RoomPeer, RoomState, Rooms, publish_scope},
    ticket::RoomTicket,
};
