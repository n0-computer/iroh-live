//! The few types `moq-ext-player` and `moq-ext-publisher` share.
//!
//! Both crates report failures as one [`Error`] and slot states as
//! [`SlotState`], so a UI handles a broadcast and a player the same way. The
//! [`stats`] helpers count rates the same way on both sides.

mod error;
pub mod stats;
mod status;

pub use self::{
    error::Error,
    status::{SlotState, send_if_changed},
};
pub use moq_ext_sources::Closed;
