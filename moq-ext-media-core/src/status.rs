//! The slot state a broadcast and a player both report, and a watch helper.

use std::{fmt, ops::Deref, sync::Arc};

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
/// hold the same error, as [`ByPtr`] says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    Failed(ByPtr<Error>),
    /// The source ended.
    Ended,
}

/// An `Arc` that compares by pointer, so a watcher tells an update from a repeat.
///
/// Errors are not `PartialEq`, and a status that holds one must still be
/// comparable. Two `ByPtr` are equal only when they share one allocation.
#[derive(Debug)]
pub struct ByPtr<T>(pub Arc<T>);

impl<T> ByPtr<T> {
    /// Wraps `value` in a new allocation.
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Clone for ByPtr<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> PartialEq for ByPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for ByPtr<T> {}

impl<T> From<Arc<T>> for ByPtr<T> {
    fn from(value: Arc<T>) -> Self {
        Self(value)
    }
}

impl<T> Deref for ByPtr<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: fmt::Display> fmt::Display for ByPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
