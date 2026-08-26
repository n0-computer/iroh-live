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
use moq_lite::track::{
    Consumer as TrackConsumer, Producer as TrackProducer, Subscriber, Subscription,
};

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
            .write_frame(
                moq_lite::Timestamp::from_millis(0).expect("zero is in range"),
                Bytes::from(text),
            )
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
    track: TrackConsumer,
    priority: u8,
    /// Opened on the first [`recv`](Self::recv). 0.2 made subscribing async, and
    /// this type is handed out from a sync constructor, so it happens lazily.
    #[debug(skip)]
    subscriber: Option<Subscriber>,
}

impl ChatSubscriber {
    /// Creates a new chat subscriber from a track consumer.
    pub fn new(track: TrackConsumer, priority: u8) -> Self {
        Self {
            track,
            priority,
            subscriber: None,
        }
    }

    /// Waits for the next chat message.
    ///
    /// Returns `None` when the track ends (peer left or broadcast closed).
    pub async fn recv(&mut self) -> Option<ChatMessage> {
        if self.subscriber.is_none() {
            match self
                .track
                .subscribe(Subscription::default().with_priority(self.priority))
                .await
            {
                Ok(subscriber) => self.subscriber = Some(subscriber),
                Err(e) => {
                    warn!("chat subscribe failed: {e:#}");
                    return None;
                }
            }
        }
        let subscriber = self.subscriber.as_mut().expect("subscribed above");
        loop {
            let group = match subscriber.next_group().await {
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
        let consumer = producer.consume();

        let mut publisher = ChatPublisher::new(producer);
        let mut subscriber = ChatSubscriber::new(consumer, track_info.priority);

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
        let consumer = producer.consume();

        let mut publisher = ChatPublisher::new(producer);
        let mut subscriber = ChatSubscriber::new(consumer, track_info.priority);

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
        let consumer = producer.consume();

        let mut subscriber = ChatSubscriber::new(consumer, track_info.priority);
        producer.finish().unwrap();

        let result = subscriber.recv().await;
        assert!(result.is_none());
    }
}
