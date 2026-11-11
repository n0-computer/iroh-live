//! The few types `moq-ext-sources`, `moq-ext-player` and `moq-ext-publisher` share.
//!
//! All three crates report failures as one [`Error`]. The player and the
//! publisher report slot states as [`SlotState`], so a UI handles a broadcast
//! and a player the same way. The [`stats`] helpers count rates the same way
//! on both sides.

mod error;
pub mod stats;
mod status;

pub use self::{
    error::{Error, SourceClosed},
    status::{ByPtr, SlotState, send_if_changed},
};
