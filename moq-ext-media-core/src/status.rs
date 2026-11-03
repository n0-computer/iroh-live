//! The slot state a broadcast and a player both report, and a watch helper.

use std::sync::Arc;

use tokio::sync::watch;

use crate::Error;

/// Changes the value in place, and wakes its receivers only if it changed.
///
/// The player's selector watches the status it writes, so a write that
/// changes nothing must not wake it.
pub fn send_if_changed<T: Clone + PartialEq>(
    sender: &watch::Sender<T>,
    change: impl FnOnce(&mut T),
) {
    sender.send_if_modified(|value| {
        let before = value.clone();
        change(value);
        *value != before
    });
}

/// The state of one media slot, on a broadcast or on a player.
///
/// `PublishStatus` and `PlayerStatus` both use it, so
/// a UI reports both the same way. Two `Failed` states are equal only when they
/// hold the same error. This lets a watcher tell an update from a repeat.
#[derive(Debug, Clone, Default)]
pub enum SlotState {
    /// Nothing is set.
    #[default]
    Off,
    /// Opening, probing encoders, or waiting for a first keyframe.
    Starting,
    /// Media is flowing.
    Running,
    /// Stopped by an error.
    ///
    /// On a broadcast, the hardware-to-software encoder fallback retries, and
    /// moq-audio reopens a microphone when the device returns. On a player,
    /// video and audio both try again after a pause.
    Failed(Arc<Error>),
    /// The source ended.
    Ended,
}

impl PartialEq for SlotState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Off, Self::Off)
            | (Self::Starting, Self::Starting)
            | (Self::Running, Self::Running)
            | (Self::Ended, Self::Ended) => true,
            (Self::Failed(left), Self::Failed(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for SlotState {}
