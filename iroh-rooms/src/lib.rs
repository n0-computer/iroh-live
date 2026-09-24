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
//! topic and announce itself, so the ticket is the boundary. While a member on
//! the previous release is in the room the boundary is weaker: that release
//! publishes its broadcasts at `rooms/<topic>/<name>` to anyone who connects,
//! which also tells them the topic.
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
//! # Wire compatibility
//!
//! This release writes an announcement the previous one can read, answers the
//! previous release's paths, and writes chat in both formats; it reads both
//! announcement layouts and both chat formats. The previous formats go in the
//! next release.

mod chat;
mod room;
mod ticket;

pub use self::{
    chat::{ChatError, ChatMessage, ChatReceiver},
    room::{ALPN, Error, Room, RoomConfig, RoomPeer, RoomState, Rooms},
    ticket::RoomTicket,
};
