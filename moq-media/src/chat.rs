//! Text chat over MoQ tracks.
//!
//! Each chat message is a single group on a dedicated track within a
//! broadcast's catalog. [`ChatPublisher`] writes messages and
//! [`ChatSubscriber`] reads them. The sender's identity comes from the
//! broadcast context (peer ID, user metadata), not from the message payload.
//!
//! Wire format: each group contains one frame of UTF-8 text. No additional
//! framing or serialization.

use std::time::Instant;

use bytes::Bytes;
use moq_lite::track::{Producer as TrackProducer, Subscriber};

use crate::catalog::ChatTrack;
use tracing::{debug, warn};

/// Default track name for chat messages in the hang catalog.
pub const CHAT_TRACK_NAME: &str = "chat";

/// Default priority for the chat track. Lower than audio (2) and video (1)
/// but higher than catalog (100).
pub const CHAT_PRIORITY: u8 = 10;

/// Returns the catalog descriptor for the chat message track.
pub fn chat_track() -> ChatTrack {
    ChatTrack {
        name: CHAT_TRACK_NAME.to_string(),
        priority: CHAT_PRIORITY,
    }
}

/// A received chat message with local reception metadata.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// The message text.
    pub text: String,
    /// When this message was received locally.
    pub received_at: Instant,
}

/// Publishes chat messages to a MoQ track.
///
/// Each call to [`send`](ChatPublisher::send) writes a new group with
/// the message text as a single frame. Messages are delivered to all
/// subscribers in order.
#[derive(derive_more::Debug)]
pub struct ChatPublisher {
    #[debug(skip)]
    track: TrackProducer,
}

impl ChatPublisher {
    /// Creates a new chat publisher from a track producer.
    pub fn new(track: TrackProducer) -> Self {
        Self { track }
    }

    /// Sends a text message on the chat track.
    ///
    /// Returns an error if the track has been closed.
    pub fn send(&mut self, text: impl Into<String>) -> anyhow::Result<()> {
        let text = text.into();
        if text.is_empty() {
            return Ok(());
        }
        // Chat has no media timeline; every message is its own group.
        self.track
            .write_frame(moq_lite::Timestamp::ZERO, Bytes::from(text))
            .map_err(|e| anyhow::anyhow!("chat send failed: {e}"))?;
        Ok(())
    }
}

/// Subscribes to chat messages on a MoQ track.
///
/// Yields [`ChatMessage`]s as they arrive. The subscriber reads groups
/// sequentially; old groups that expired before subscription are skipped.
#[derive(derive_more::Debug)]
pub struct ChatSubscriber {
    #[debug(skip)]
    subscriber: Subscriber,
}

impl ChatSubscriber {
    /// Creates a new chat subscriber from an established track subscription.
    pub fn new(subscriber: Subscriber) -> Self {
        Self { subscriber }
    }

    /// Waits for the next chat message.
    ///
    /// Returns `None` when the track ends (peer left or broadcast closed).
    pub async fn recv(&mut self) -> Option<ChatMessage> {
        loop {
            let group = match self.subscriber.next_group().await {
                Ok(Some(g)) => g,
                Ok(None) => {
                    debug!("chat track ended");
                    return None;
                }
                Err(e) => {
                    warn!("chat track error: {e:#}");
                    return None;
                }
            };

            let mut consumer = group;
            match consumer.read_frame().await {
                Ok(Some(data)) => match String::from_utf8(data.payload.to_vec()) {
                    Ok(text) if !text.is_empty() => {
                        return Some(ChatMessage {
                            text,
                            received_at: Instant::now(),
                        });
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        warn!("chat message not valid UTF-8: {e}");
                        continue;
                    }
                },
                Ok(None) => continue,
                Err(e) => {
                    warn!("chat frame read error: {e:#}");
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let track_info = chat_track();
        let mut broadcast = moq_lite::broadcast::Info::default().produce();
        let producer = broadcast
            .create_track(
                track_info.name.clone(),
                moq_lite::track::Info::default().with_priority(track_info.priority),
            )
            .unwrap();
        let subscription = producer.subscribe(None);

        let mut publisher = ChatPublisher::new(producer);
        let mut subscriber = ChatSubscriber::new(subscription);

        publisher.send("hello").unwrap();
        publisher.send("world").unwrap();

        let msg1 = subscriber.recv().await.unwrap();
        assert_eq!(msg1.text, "hello");

        let msg2 = subscriber.recv().await.unwrap();
        assert_eq!(msg2.text, "world");
    }

    #[tokio::test]
    async fn empty_messages_skipped() {
        let track_info = chat_track();
        let mut broadcast = moq_lite::broadcast::Info::default().produce();
        let producer = broadcast
            .create_track(
                track_info.name.clone(),
                moq_lite::track::Info::default().with_priority(track_info.priority),
            )
            .unwrap();
        let subscription = producer.subscribe(None);

        let mut publisher = ChatPublisher::new(producer);
        let mut subscriber = ChatSubscriber::new(subscription);

        publisher.send("").unwrap();
        publisher.send("after empty").unwrap();

        let msg = subscriber.recv().await.unwrap();
        assert_eq!(msg.text, "after empty");
    }

    #[tokio::test]
    async fn closed_track_returns_none() {
        let track_info = chat_track();
        let mut broadcast = moq_lite::broadcast::Info::default().produce();
        let mut producer = broadcast
            .create_track(
                track_info.name.clone(),
                moq_lite::track::Info::default().with_priority(track_info.priority),
            )
            .unwrap();
        let subscription = producer.subscribe(None);

        let mut subscriber = ChatSubscriber::new(subscription);
        producer.finish().unwrap();

        let result = subscriber.recv().await;
        assert!(result.is_none());
    }
}
