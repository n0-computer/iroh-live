//! Knobs for tests, behind the `test-util` feature.
//!
//! The player's adaptation thresholds and timers are internal, so they can be
//! retuned in a patch release without any caller noticing. A test that has to
//! see a switch inside its own timeout, such as the network-simulation suite,
//! cannot wait out the production timers, so it shortens them here and hands
//! them to one player with
//! [`PlayerConfig::with_tuning`](crate::PlayerConfig::with_tuning).
//!
//! Nothing in an application should enable this feature: the fields change
//! without notice, and the values are tuned together.
//!
//! ```
//! use std::time::Duration;
//!
//! use iroh_live_media::{PlayerConfig, test_util::Tuning};
//!
//! let mut quick = Tuning::default();
//! quick.downgrade_hold = Duration::from_millis(300);
//! quick.upgrade_hold = Duration::from_millis(500);
//! let config = PlayerConfig::default().with_tuning(quick);
//! # drop(config);
//! ```
//!
//! The decode supervisor's handover is a state machine of its own, driven by
//! the crate's unit tests with made-up open and first-picture completions; its
//! one timer that a slow link can outlast, the switch deadline, is
//! [`Tuning::switch_deadline`].

pub use crate::player::bound::Tuning;
