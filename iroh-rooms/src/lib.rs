//! Multi-party rooms over iroh gossip and MoQ.
//!
//! A room is a gossip topic plus the MoQ subscriptions that follow from it.
//! Peers publish the *names* of their broadcasts into a replicated key-value
//! map on the topic, and [`Room`] turns every name it sees into a MoQ
//! subscription against that peer, handing the resulting
//! [`broadcast::Consumer`](moq_net::broadcast::Consumer) back as a
//! [`RoomEvent`]. Nothing here knows what the broadcasts carry.
//!
//! [`Room::new`] takes the three pieces it needs rather than an application
//! type: an [`Endpoint`](iroh::Endpoint) for this peer's identity, a
//! [`Moq`](iroh_moq::Moq) for the transport, and a
//! [`Gossip`](iroh_gossip::Gossip) for discovery.
//!
//! ```no_run
//! # async fn example(
//! #     endpoint: &iroh::Endpoint,
//! #     moq: &iroh_moq::Moq,
//! #     gossip: &iroh_gossip::Gossip,
//! # ) -> Result<(), iroh_rooms::Error> {
//! use iroh_rooms::{Room, RoomEvent, RoomTicket};
//!
//! let mut room = Room::new(endpoint, moq, gossip, RoomTicket::generate()).await?;
//! let mut broadcast = room.publish("cam").await?;
//!
//! while let Ok(event) = room.recv().await {
//!     if let RoomEvent::BroadcastSubscribed {
//!         remote, broadcast, ..
//!     } = event
//!     {
//!         println!("{remote} is publishing, and we can now read its tracks");
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The one thing the room reads for itself is chat, which lives on a
//! well-known track name. See the [`chat`] module.

pub mod chat;
mod room;
mod ticket;

pub use self::{
    room::{Error, Room, RoomEvent, RoomEvents, RoomHandle},
    ticket::RoomTicket,
};
