//! A latest-value JSON track writer that stamps each update itself.
//!
//! A [`moq_json::snapshot`] track on a timed track needs a timestamp on every
//! update, and moq-net refuses a group that starts at or before the group
//! before it. A scalar value, such as a `u64`, starts a group with every
//! change, so two changes in the same millisecond fail when the caller stamps
//! them from a clock. [`ValueWriter`] stamps each update from the moq clock
//! and moves a stamp that does not pass the previous one a tick past it, so
//! `update(&value)` succeeds however often it is called.
//!
//! The writer changes nothing on the wire. Readers use
//! [`moq_json::snapshot::Consumer`] and see the stamps in
//! [`Timed::at`](moq_json::Timed::at).
//!
//! # Examples
//!
//! ```
//! use moq_ext_json::ValueWriter;
//! use moq_net::{Timescale, track};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let track = broadcast
//!     .create_track(
//!         "latency",
//!         track::Info::default().with_timescale(Timescale::MILLI),
//!     )
//!     .ok_or("the track exists")?;
//! let latency = ValueWriter::<u64>::new(track, Default::default());
//! // Both succeed, even within one millisecond.
//! latency.update(&150)?;
//! latency.update(&160)?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use moq_json::snapshot;
use moq_net::{Timestamp, time::Instant, track};
use serde::{Serialize, de::DeserializeOwned};

#[cfg(test)]
mod tests;

/// Writes a latest-value JSON track, stamping each update with the time it is made.
///
/// Each update gets the moq clock's time, `Timestamp::from(Instant::now())`
/// at the track's timescale, or one tick after the previous stamp if the clock
/// has not moved past it. A tick is a millisecond on a track that counts
/// milliseconds, as the tracks iroh-live creates do. The stamps strictly
/// increase, as the track's group starts must, so a burst of updates within
/// one millisecond runs a few milliseconds ahead of the clock and meets it
/// again once the updates slow down.
///
/// The track must be timed: on a track without a
/// [`timescale`](track::Info::timescale), every update fails with
/// [`moq_net::Error::TimestampMismatch`].
///
/// Clones share the track and the stamps, and each update stamps and writes
/// under one lock, so clones on several threads cannot reorder their stamps.
/// The last clone to drop finishes the track, so readers see it end.
pub struct ValueWriter<T: Serialize> {
    shared: Arc<Mutex<Shared<T>>>,
}

impl<T: Serialize> Clone for ValueWriter<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T: Serialize> fmt::Debug for ValueWriter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueWriter")
            .field("last", &self.lock().last)
            .finish_non_exhaustive()
    }
}

/// The state that the clones of one [`ValueWriter`] share.
struct Shared<T: Serialize> {
    producer: snapshot::Producer<T>,
    /// The track's properties, whose timescale the stamps are kept at.
    info: track::Info,
    /// The newest stamp written, at the track's timescale, which the next one must pass.
    last: Option<Timestamp>,
}

impl<T: Serialize> ValueWriter<T> {
    /// Creates a writer that publishes to `track`.
    ///
    /// The track must be timed, and nothing else should write to it.
    pub fn new(track: track::Producer, config: snapshot::Config) -> Self {
        let info = track.info().clone();
        Self {
            shared: Arc::new(Mutex::new(Shared {
                producer: snapshot::Producer::new(track, config),
                info,
                last: None,
            })),
        }
    }

    /// Publishes `value`, stamped now.
    ///
    /// Returns the encoded size of the frame written, or `None` if `value`
    /// equals the current value and nothing was written.
    ///
    /// # Errors
    ///
    /// Fails with [`moq_net::Error::Closed`] once the track is finished,
    /// with [`moq_net::Error::TimestampMismatch`] on an untimed track, and
    /// with any error of [`snapshot::Producer::update`].
    pub fn update(&self, value: &T) -> moq_json::Result<Option<usize>> {
        let mut shared = self.lock();
        let at = shared.next(Timestamp::from(Instant::now()))?;
        let written = shared.producer.update(value, at)?;
        if written.is_some() {
            shared.advance(at);
        }
        Ok(written)
    }

    /// Publishes `value` at `at`, a time the caller chose.
    ///
    /// The caller owns the order: an `at` that starts a group at or before
    /// the previous group fails with [`moq_net::Error::TimestampMismatch`]. A
    /// later [`update`](Self::update) stamps after `at`.
    ///
    /// # Errors
    ///
    /// As [`update`](Self::update), and fails with
    /// [`moq_net::Error::TimestampMismatch`] for an `at` the track cannot
    /// take.
    pub fn update_at(&self, value: &T, at: Timestamp) -> moq_json::Result<Option<usize>> {
        let mut shared = self.lock();
        let at = shared.info.rescale(at)?;
        let written = shared.producer.update(value, at)?;
        if written.is_some() {
            shared.advance(at);
        }
        Ok(written)
    }

    /// Edits the current value in place and publishes it, stamped now.
    ///
    /// The closure receives the current value: everything published so far,
    /// or `T::default()` if nothing has been. An unchanged value publishes
    /// nothing. See [`snapshot::Producer::mutate`].
    ///
    /// # Errors
    ///
    /// As [`update`](Self::update), and fails if the current value does not
    /// deserialize as `T`.
    pub fn mutate(&self, edit: impl FnOnce(&mut T)) -> moq_json::Result<()>
    where
        T: Default + DeserializeOwned,
    {
        let mut shared = self.lock();
        let at = shared.next(Timestamp::from(Instant::now()))?;
        shared.producer.mutate(edit, at)?;
        // `mutate` does not say whether it wrote, so count the stamp as used.
        // A spare tick only moves the next stamp ahead, which is harmless.
        shared.advance(at);
        Ok(())
    }

    /// Finishes the track, so readers see it end.
    ///
    /// Clones then fail to update with [`moq_net::Error::Closed`]. Dropping
    /// the last clone finishes the track too, so call this only to see the
    /// error or to end the track while clones remain.
    ///
    /// # Errors
    ///
    /// Fails if the track was aborted, or if finishing it fails.
    pub fn finish(self) -> moq_json::Result<()> {
        let producer = self.lock().producer.clone();
        producer.finish().map(drop)
    }

    /// Returns the moq-json producer, for `poll_refresh`, `cut` or `consume`.
    ///
    /// It shares the track with the writer: a write through it bypasses the
    /// stamps, and the writer's last clone finishes the track for it too.
    pub fn producer(&self) -> snapshot::Producer<T> {
        self.lock().producer.clone()
    }

    /// Locks the shared state.
    fn lock(&self) -> MutexGuard<'_, Shared<T>> {
        // The lock is held across the caller's `mutate` closure, so a panic
        // there poisons it. The state stays consistent, since `last` moves only
        // after a write, so recover rather than fail every later update.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: Serialize> Shared<T> {
    /// Returns `now` at the track's timescale, or one tick after the last stamp if `now` is not past it.
    fn next(&self, now: Timestamp) -> moq_json::Result<Timestamp> {
        let now = self.info.rescale(now)?;
        Ok(match self.last {
            Some(last) if now <= last => tick_after(last).unwrap_or(now),
            _ => now,
        })
    }

    /// Records `at` as written, if it is later than the last stamp.
    fn advance(&mut self, at: Timestamp) {
        if self.last.is_none_or(|last| at > last) {
            self.last = Some(at);
        }
    }
}

impl<T: Serialize> Drop for Shared<T> {
    /// Finishes the track if no one finished it, so readers see it end.
    fn drop(&mut self) {
        // Finishing a finished track does nothing, and an aborted one already
        // ended with its cause, so the error carries nothing to act on.
        let _finished = self.producer.clone().finish();
    }
}

/// Returns the timestamp one tick of its own scale after `at`, or `None` at the end of the range.
fn tick_after(at: Timestamp) -> Option<Timestamp> {
    let scale = at.scale()?;
    Timestamp::new(at.value().checked_add(1)?, scale).ok()
}
