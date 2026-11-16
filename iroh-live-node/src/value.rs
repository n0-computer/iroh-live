//! A reader of a latest-value JSON track that follows its subscription to a new publisher.

use std::{fmt, time::Duration};

use moq_net::broadcast;
use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tracing::debug;

use crate::{Subscription, json};

/// How long a track that failed waits for its broadcast to end before the error counts.
///
/// A track fails as its broadcast ends, a moment before the broadcast reports
/// that it closed.
const END_GRACE: Duration = Duration::from_millis(500);

/// A reader of a latest-value JSON track that follows its [`Subscription`].
///
/// Returned by [`Subscription::value_track`]. When the publisher restarts and
/// a new broadcast takes over the path, the reader opens the track on the new
/// broadcast and reads on, the same as media follows the subscription. The
/// first read after the move returns the new publisher's current value. The
/// reader ends only once the subscription closes.
///
/// The reader holds the subscription, so the subscription stays open while
/// the reader lives.
pub struct ValueReader<T> {
    subscription: Subscription,
    name: String,
    /// The broadcast that serves the path, as the subscription follows it.
    current: watch::Receiver<broadcast::Consumer>,
    /// The broadcast the track was opened on.
    broadcast: broadcast::Consumer,
    /// The track on [`broadcast`](Self::broadcast), or `None` once it ended
    /// or when that broadcast has no such track.
    track: Option<json::snapshot::Consumer<T>>,
}

impl<T> fmt::Debug for ValueReader<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueReader")
            .field("path", self.subscription.path())
            .field("name", &self.name)
            .field("open", &self.track.is_some())
            .finish()
    }
}

impl<T: DeserializeOwned> ValueReader<T> {
    /// Creates a reader of track `name` that follows `subscription`.
    ///
    /// Opens the track on the broadcast that serves the path now.
    pub(crate) fn new(
        subscription: Subscription,
        current: watch::Receiver<broadcast::Consumer>,
        name: &str,
    ) -> Result<Self, moq_net::Error> {
        let mut current = current;
        let broadcast = current.borrow_and_update().clone();
        let track = open(&broadcast, name)?;
        Ok(Self {
            subscription,
            name: name.to_string(),
            current,
            broadcast,
            track: Some(track),
        })
    }

    /// Returns the name of the track.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Waits for the newest value, skipping older ones.
    ///
    /// Returns `None` once the subscription closes. Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails when a value does not decode, or when the publisher refuses the
    /// track, such as a track it does not publish. A track that ends with its
    /// broadcast is no error: the reader waits for the next broadcast.
    pub async fn latest(&mut self) -> json::Result<Option<json::Timed<T>>> {
        self.read(Read::Latest).await
    }

    /// Waits for the next value, in order within the newest group.
    ///
    /// Returns `None` once the subscription closes. Cancellation safe.
    ///
    /// # Errors
    ///
    /// As [`latest`](Self::latest).
    pub async fn next(&mut self) -> json::Result<Option<json::Timed<T>>> {
        self.read(Read::Next).await
    }

    /// Reads a value from the track, and moves to the next broadcast when it ends.
    async fn read(&mut self, how: Read) -> json::Result<Option<json::Timed<T>>> {
        loop {
            let Some(track) = self.track.as_mut() else {
                if !self.next_broadcast().await {
                    return Ok(None);
                }
                continue;
            };
            let read = async {
                match how {
                    Read::Latest => track.latest().await,
                    Read::Next => track.next().await,
                }
            };
            // The reader holds the subscription, and so the sender, so the
            // watch never closes under it.
            let result = tokio::select! {
                result = read => Some(result),
                _ = self.current.changed() => None,
            };
            // The subscription moved to another broadcast before a value came.
            let Some(result) = result else {
                self.reopen();
                continue;
            };
            match result {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {
                    debug!(path = %self.subscription.path(), track = self.name, "value track finished, waiting for the next broadcast");
                    self.track = None;
                }
                Err(json::Error::Net(err)) => {
                    if !self.ended_with_broadcast().await {
                        return Err(json::Error::Net(err));
                    }
                    debug!(path = %self.subscription.path(), track = self.name, error = %err, "value track ended with its broadcast");
                    self.track = None;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Waits for another broadcast to serve the path, and opens the track on it.
    ///
    /// Returns `false` once the subscription closed instead.
    async fn next_broadcast(&mut self) -> bool {
        let moved = tokio::select! {
            changed = self.current.changed() => changed.is_ok(),
            () = self.subscription.closed() => false,
        };
        if moved {
            self.reopen();
        }
        moved
    }

    /// Opens the track on the broadcast that serves the path now.
    fn reopen(&mut self) {
        self.broadcast = self.current.borrow_and_update().clone();
        self.track = match open(&self.broadcast, &self.name) {
            Ok(track) => {
                debug!(path = %self.subscription.path(), track = self.name, "reading the value track of a new broadcast");
                Some(track)
            }
            Err(err) => {
                debug!(path = %self.subscription.path(), track = self.name, error = %err, "the new broadcast has no such value track");
                None
            }
        };
    }

    /// Reports whether the broadcast the track was read from ended, within [`END_GRACE`].
    async fn ended_with_broadcast(&self) -> bool {
        let ended = async {
            tokio::select! {
                () = self.broadcast.closed() => {}
                () = self.subscription.closed() => {}
            }
        };
        tokio::time::timeout(END_GRACE, ended).await.is_ok()
    }
}

/// Which value a read returns.
#[derive(Debug, Clone, Copy)]
enum Read {
    /// The newest, skipping older ones.
    Latest,
    /// The next, in order.
    Next,
}

/// Opens the latest-value track `name` of `broadcast`.
fn open<T: DeserializeOwned>(
    broadcast: &broadcast::Consumer,
    name: &str,
) -> Result<json::snapshot::Consumer<T>, moq_net::Error> {
    let track = broadcast.track(name)?;
    Ok(json::snapshot::Consumer::new(track, Default::default()))
}
