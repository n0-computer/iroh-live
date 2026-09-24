//! Room chat: one more broadcast every member publishes into the room.
//!
//! The broadcast is named `chat` and carries `moq-room`'s chat track, a JSON
//! window that holds the last ten seconds of messages, which is what
//! `@moq/room` reads too. The grid leaves it out, and the window reads every
//! other member's with [`read`].

use std::{collections::VecDeque, time::Duration};

use eframe::egui;
use iroh::EndpointId;
use iroh_live::rooms::Room;
use moq_net::broadcast;
use moq_room::chat::{Event, Publisher, Subscriber};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// The name of the chat broadcast.
pub(super) const NAME: &str = "chat";

/// How many typed messages wait for the writer task.
const SEND_QUEUE: usize = 16;

/// How long a reader waits before reading a member again once its read ended.
const RETRY: Duration = Duration::from_secs(2);

/// How many delivered messages a reader remembers, to skip them when a new
/// read replays the window.
const REMEMBERED: usize = 64;

/// A message another member sent.
#[derive(Debug)]
pub(super) struct Message {
    pub(super) from: EndpointId,
    pub(super) text: String,
}

/// This member's chat broadcast, and the task that writes and expires it.
pub(super) struct Writer {
    broadcast: broadcast::Consumer,
    tx: mpsc::Sender<String>,
    _task: AbortOnDropHandle<()>,
}

impl Writer {
    /// Creates the chat broadcast.
    pub(super) fn new() -> Result<Self> {
        let mut producer = broadcast::Info::new().produce();
        let mut publisher =
            Publisher::create(&mut producer).std_context("creating the chat track")?;
        let broadcast = producer.consume();
        let (tx, mut rx) = mpsc::channel::<String>(SEND_QUEUE);
        let task = tokio::spawn(async move {
            // Held here, so the broadcast lives as long as the task.
            let _producer = producer;
            loop {
                let text = tokio::select! {
                    text = rx.recv() => text,
                    expired = publisher.expire() => match expired {
                        Ok(()) => continue,
                        Err(err) => {
                            warn!(error = %err, "the chat window failed");
                            return;
                        }
                    },
                };
                let Some(text) = text else { return };
                if let Err(err) = publisher.send(&text) {
                    warn!(error = %err, "failed to send the chat message");
                }
            }
        });
        Ok(Self {
            broadcast,
            tx,
            _task: AbortOnDropHandle::new(task),
        })
    }

    /// Returns the broadcast, for publishing it into the room.
    pub(super) fn broadcast(&self) -> broadcast::Consumer {
        self.broadcast.clone()
    }

    /// Sends `text` to the room, unless the writer is backed up.
    pub(super) fn send(&self, text: String) {
        if self.tx.try_send(text).is_err() {
            warn!("the chat writer is backed up, dropping a message");
        }
    }
}

/// Reads member `from`'s chat into `tx` until the task is dropped.
///
/// Reads the member again after [`RETRY`] whenever a read ends. A new read
/// starts with the member's whole window, and the messages this reader
/// delivered already are skipped.
pub(super) async fn read(
    room: Room,
    from: EndpointId,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) {
    let mut delivered = VecDeque::new();
    loop {
        if let Err(err) = read_once(&room, from, &mut delivered, &tx, &ctx).await {
            debug!(remote = %from.fmt_short(), error = %err, "chat read ended");
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Reads member `from`'s chat until its broadcast ends.
async fn read_once(
    room: &Room,
    from: EndpointId,
    delivered: &mut VecDeque<(u64, String)>,
    tx: &mpsc::Sender<Message>,
    ctx: &egui::Context,
) -> Result<()> {
    let subscription = room.subscribe(from, NAME).await?;
    let broadcast = subscription.as_moq();
    let mut chat = Subscriber::subscribe(&broadcast)
        .await
        .std_context("subscribing to the chat")?;
    loop {
        // A broadcast whose session died does not always end its tracks.
        let event = tokio::select! {
            event = chat.recv() => event.std_context("reading the chat")?,
            _ = broadcast.closed() => None,
        };
        let Some(event) = event else {
            return Ok(());
        };
        let Event::Push { index, value } = event else {
            continue;
        };
        let Some(text) = remember(delivered, index, value) else {
            continue;
        };
        if tx.send(Message { from, text }).await.is_err() {
            return Ok(());
        }
        ctx.request_repaint();
    }
}

/// Returns the message at `index` unless it was delivered already, and
/// remembers it.
///
/// Keyed by index and text: a member that restarted counts from zero again,
/// with new text.
fn remember(delivered: &mut VecDeque<(u64, String)>, index: u64, text: String) -> Option<String> {
    let seen = (index, text);
    if delivered.contains(&seen) {
        return None;
    }
    let text = seen.1.clone();
    delivered.push_back(seen);
    if delivered.len() > REMEMBERED {
        delivered.pop_front();
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message the writer sends reaches a subscriber of its broadcast.
    #[tokio::test]
    async fn a_message_reaches_a_subscriber() {
        let writer = Writer::new().expect("writer");
        let mut chat = Subscriber::subscribe(&writer.broadcast())
            .await
            .expect("subscribe");
        writer.send("hello".into());
        let event = tokio::time::timeout(Duration::from_secs(5), chat.recv())
            .await
            .expect("timed out")
            .expect("read")
            .expect("an event");
        assert!(matches!(event, Event::Push { value, .. } if value == "hello"));
    }

    /// A replayed window is skipped, and a restarted member is heard.
    #[test]
    fn a_replay_is_skipped_and_a_restart_is_not() {
        let mut delivered = VecDeque::new();
        assert!(remember(&mut delivered, 0, "a".into()).is_some());
        assert!(remember(&mut delivered, 1, "b".into()).is_some());
        assert!(
            remember(&mut delivered, 1, "b".into()).is_none(),
            "a replay"
        );
        assert!(
            remember(&mut delivered, 0, "again".into()).is_some(),
            "a restart"
        );
    }
}
