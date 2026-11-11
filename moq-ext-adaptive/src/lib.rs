//! Rendition choice and decoder handover for adaptive video playback.
//!
//! This crate is pure logic. It does no I/O, spawns nothing, and depends on
//! neither tokio nor any media or transport type. Every method that depends on
//! time takes a [`std::time::Instant`] argument, so the tests drive the state
//! machines with a fake clock and run in microseconds.
//!
//! There are two state machines, which a player wires together:
//!
//! - [`Bound`] decides which rendition of a ladder should play. Each pass it
//!   gets the ranked ladder ([`Rung`]s, best first), the caller's
//!   [`Constraints`] and a [`NetworkSample`], and returns the best rung the
//!   link carries. Time enters only as hysteresis, tuned by [`Adaptation`].
//!   [`choose`] wraps it with the rendition [`Mode`] (automatic, pinned or off),
//!   and [`Backoffs`] tracks renditions whose decoders failed. [`step_down`]
//!   tells whether a switch moves down the ladder.
//! - [`Switcher`] hands the picture from one decoder to the next. It tracks one
//!   incumbent and at most one replacement, and takes over once the replacement
//!   has decoded a picture close to the incumbent's playhead, or has run out of
//!   patience. The decoders themselves are opaque type parameters.
//!
//! The async loops that feed these machines, with their channels and timers,
//! live in the player.

mod bound;
mod network;
mod select;
mod switch;

pub use self::{
    bound::{Adaptation, Bound, Constraints, Rung},
    network::NetworkSample,
    select::{Backoffs, Choice, Mode, PinProblem, best, choose, step_down},
    switch::{Abandoned, CATCH_UP_PATIENCE, CATCH_UP_SLACK, Outcome, Switcher, Target, Verdict},
};
