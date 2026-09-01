//! Text chat over a MoQ track.
//!
//! A broadcast carries chat on one well-known track, [`CHAT_TRACK_NAME`]. Each
//! message is a single group holding one frame of UTF-8 text, so there is no
//! framing or serialization beyond the string itself. The sender's identity
//! comes from the broadcast that carries the track, not from the payload.
//!
//! [`ChatPublisher`] writes messages and [`ChatSubscriber`] reads them. Both
//! have a constructor that finds the track on a broadcast by name, which is how
//! a [`Room`](crate::Room) wires chat up without knowing anything about the
//! media on the same broadcast.

use std::time::Instant;

use moq_net::{Timestamp, broadcast, track};
use tracing::warn;

/// Name of the track that carries chat messages.
pub const CHAT_TRACK_NAME: &str = "chat";

/// Publisher tie-break priority for the chat track.
///
/// Lower than audio and video, which are the tracks a viewer notices first when
/// the link is congested.
pub const CHAT_PRIORITY: u8 = 10;

/// Returns the track settings for the chat track.
///
/// Groups are ordered, because chat only makes sense read oldest first, whereas
/// the moq-net default favours the newest group.
pub fn chat_track() -> track::Info {
    track::Info::default()
        .with_priority(CHAT_PRIORITY)
        .with_ordered(true)
}

/// A received chat message, with the time it arrived.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// The message text.
    pub text: String,
    /// When this message was received locally.
    pub received_at: Instant,
}

/// Writer half of a chat track.
///
/// Every [`send`](ChatPublisher::send) writes one group holding the message
/// text as a single frame, so subscribers see messages in the order they were
/// sent.
#[derive(derive_more::Debug)]
pub struct ChatPublisher {
    #[debug(skip)]
    track: track::Producer,
}

impl ChatPublisher {
    /// Creates the chat track on `broadcast` and returns a publisher for it.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast already has a track named [`CHAT_TRACK_NAME`].
    pub fn create(broadcast: &mut broadcast::Producer) -> Result<Self, moq_net::Error> {
        Ok(Self::new(
            broadcast.create_track(CHAT_TRACK_NAME, chat_track())?,
        ))
    }

    /// Creates a publisher over an existing track producer.
    pub fn new(track: track::Producer) -> Self {
        Self { track }
    }

    /// Sends a text message on the chat track.
    ///
    /// Empty messages are dropped rather than written, because a subscriber
    /// cannot tell them apart from a group it failed to read.
    ///
    /// # Errors
    ///
    /// Fails if the track has been closed.
    pub fn send(&mut self, text: impl Into<String>) -> Result<(), moq_net::Error> {
        let text = text.into();
        if text.is_empty() {
            return Ok(());
        }
        self.track.write_frame(Timestamp::now(), text)
    }

    /// Ends the chat track, so subscribers see it close cleanly after
    /// draining messages already sent.
    ///
    /// Dropping a [`ChatPublisher`] without calling this first is an abrupt
    /// teardown: the underlying track discards its cache, and a subscriber
    /// that has not yet read the last message loses it. Call this before the
    /// publisher goes out of scope whenever the last message matters.
    ///
    /// # Errors
    ///
    /// Fails if the track has already been closed.
    pub fn finish(&mut self) -> Result<(), moq_net::Error> {
        self.track.finish()
    }
}

/// Reader half of a chat track.
///
/// Yields [`ChatMessage`]s in group order. Groups that expired before the
/// subscription started are skipped.
#[derive(derive_more::Debug)]
pub struct ChatSubscriber {
    #[debug(skip)]
    track: track::Subscriber,
}

impl ChatSubscriber {
    /// Subscribes to the chat track of `broadcast`.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast has no track named [`CHAT_TRACK_NAME`], which is
    /// the normal outcome for a peer that publishes media without chat.
    pub async fn subscribe(broadcast: &broadcast::Consumer) -> Result<Self, moq_net::Error> {
        let track = broadcast.track(CHAT_TRACK_NAME)?.subscribe(None).await?;
        Ok(Self::new(track))
    }

    /// Creates a subscriber over an existing track subscriber.
    pub fn new(track: track::Subscriber) -> Self {
        Self { track }
    }

    /// Waits for the next chat message.
    ///
    /// Returns `None` once the track ends, which happens when the peer leaves
    /// or closes its broadcast.
    pub async fn recv(&mut self) -> Option<ChatMessage> {
        loop {
            let frame = match self.track.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => return None,
                Err(err) => {
                    warn!("chat track read failed: {err:#}");
                    return None;
                }
            };
            match String::from_utf8(frame.payload.to_vec()) {
                Ok(text) if !text.is_empty() => {
                    return Some(ChatMessage {
                        text,
                        received_at: Instant::now(),
                    });
                }
                Ok(_) => continue,
                Err(err) => {
                    warn!("chat message is not valid UTF-8: {err}");
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn chat_pair() -> (broadcast::Producer, ChatPublisher, ChatSubscriber) {
        let mut producer = broadcast::Info::new().produce();
        let publisher = ChatPublisher::create(&mut producer).expect("create chat track");
        let subscriber = ChatSubscriber::subscribe(&producer.consume())
            .await
            .expect("subscribe to chat track");
        (producer, publisher, subscriber)
    }

    #[tokio::test]
    async fn roundtrip() {
        let (_producer, mut publisher, mut subscriber) = chat_pair().await;

        publisher.send("hello").unwrap();
        publisher.send("world").unwrap();

        assert_eq!(subscriber.recv().await.unwrap().text, "hello");
        assert_eq!(subscriber.recv().await.unwrap().text, "world");
    }

    #[tokio::test]
    async fn empty_messages_skipped() {
        let (_producer, mut publisher, mut subscriber) = chat_pair().await;

        publisher.send("").unwrap();
        publisher.send("after empty").unwrap();

        assert_eq!(subscriber.recv().await.unwrap().text, "after empty");
    }

    #[tokio::test]
    async fn closed_track_returns_none() {
        let (_producer, mut publisher, mut subscriber) = chat_pair().await;

        publisher.send("last").unwrap();
        // `finish` before drop: an unfinished track discards its cache the
        // moment its last producer clone drops (see `track::Drop for Alive`
        // upstream), which would lose "last" before the subscriber gets to
        // read it.
        publisher.finish().unwrap();

        assert_eq!(subscriber.recv().await.unwrap().text, "last");
        assert!(subscriber.recv().await.is_none());
    }
}
