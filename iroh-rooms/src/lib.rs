//! Multi-party rooms over iroh gossip and MoQ.
//!
//! A room is a gossip topic whose members announce themselves and the names of
//! the broadcasts they publish into it. [`Rooms`] owns the gossip instance;
//! [`Rooms::join`] returns a [`Room`], whose membership is a watched
//! [`RoomState`], whose broadcasts are subscribed on demand with
//! [`Room::subscribe`], and whose chat is its own, with a [`ChatReceiver`] per
//! reader. Nothing here knows what the broadcasts carry.
//!
//! A member is in the room while its gossip lease holds, renewed every thirty
//! seconds and dropped about two minutes after the last renewal, or at once
//! when it leaves. Ending a broadcast changes what a member publishes, not
//! whether it is a member.
//!
//! Room broadcasts are private to the members: [`Room::publish`] places a
//! broadcast at `rooms/<topic>/<member>/<name>` with the room's membership as
//! its audience, so it is offered to members and to nobody else who connects,
//! and a member that leaves or expires is cut off from what it was reading.
//! [`Room::subscribe`] and the chat read each member over the session with that
//! member, so no other peer can stand in for it. Membership itself is open to
//! whoever holds the ticket: anyone who knows the topic id can join the gossip
//! topic and announce itself, so the ticket is the boundary.
//!
//! The node's [`MoqConfig::grant`](iroh_moq::MoqConfig::grant) has to let each
//! peer publish under [`publish_scope`], or room broadcasts never reach it.
//! `iroh_live::moq_config` does with the `rooms` feature.
//!
//! Chat receivers carry what other members send from the moment of joining
//! on, each message once, also when two arrive out of order or a member's
//! session drops and comes back. What a member said before is told apart by
//! its sender's clock, but only in the replay a new subscription starts with,
//! so a member whose clock runs behind is still heard. A message sent while
//! the session was down for more than a few seconds can be lost.
//!
//! ```no_run
//! # async fn example(moq: iroh_moq::Moq, broadcast: moq_net::broadcast::Producer)
//! # -> Result<(), Box<dyn std::error::Error>> {
//! use iroh_rooms::{RoomConfig, RoomTicket, Rooms};
//!
//! let rooms = Rooms::new(&moq);
//! // Mount `rooms.protocol_handler()` under `iroh_rooms::ALPN` on the router.
//! let room = rooms
//!     .join(
//!         &RoomTicket::generate(),
//!         RoomConfig::default().with_display_name("ada"),
//!     )
//!     .await?;
//! room.publish("cam", &broadcast)?;
//!
//! let (mut state, mut chat) = (room.state(), room.chat());
//! # use n0_watcher::Watcher;
//! let members = state.get();
//! for (peer, member) in &members.peers {
//!     for name in &member.broadcasts {
//!         let subscription = room.subscribe(*peer, name).await?;
//!         # drop(subscription);
//!     }
//! }
//! let message = chat.recv().await?;
//! println!("{}: {}", message.from.fmt_short(), message.text);
//! # Ok(())
//! # }
//! ```
//!
//! # Tiles that come back
//!
//! A member can end a broadcast and publish it again under the same name
//! faster than its announcement changes, and a member that briefly stops
//! counting this node as a member cuts off what this node reads. Neither
//! changes [`RoomState`], so a grid that reconciles only on state updates keeps
//! a player on a broadcast that ended. Drop the tiles whose broadcast closed,
//! on a timer as well as on state updates, and open the ones the state still
//! lists again. With `iroh-live`'s `RemoteBroadcast` that is
//! `is_closed()`, which turns true about three seconds after the publisher
//! went: the broadcast follows its path, and looks for another route first.
//! `irl room` does this in `drop_closed`.
//!
//! # Cancellation safety
//!
//! [`Rooms::join`], [`Room::subscribe`] and [`ChatReceiver::recv`] are
//! cancellation safe: dropping `join` leaves the topic and publishes nothing,
//! dropping `subscribe` abandons the wait while a dial it started continues,
//! and dropping `recv` loses no message. [`Room::send_chat`] is not: a dropped
//! call may or may not have sent its message. [`Room::leave`] is not either,
//! and is idempotent: call it again to finish.

mod chat;
mod room;
mod ticket;

pub use self::{
    chat::{ChatError, ChatMessage, ChatReceiver},
    room::{ALPN, Error, Room, RoomConfig, RoomPeer, RoomState, Rooms, publish_scope},
    ticket::RoomTicket,
};
