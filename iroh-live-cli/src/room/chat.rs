//! Room chat: one more broadcast every member publishes into the room.
//!
//! The broadcast carries a `chat` track, a JSON window of the last ten seconds
//! of messages, which `@moq/room` reads too. It is the format moq-room wrote
//! before upstream removed that crate.

use std::{collections::VecDeque, time::Duration};

use eframe::egui;
use iroh::EndpointId;
use iroh_live::rooms::Room;
use moq_json::window;
use moq_net::{broadcast, track};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use tokio::{sync::mpsc, time::Instant};
use tracing::{debug, warn};

/// The name of the track that carries the chat window.
const TRACK: &str = "chat";

/// The chat track's delivery priority, below audio and video.
const PRIORITY: i8 = 10;

/// How long a sent message stays in the window.
const HISTORY: Duration = Duration::from_secs(10);

/// A message entering, leaving or missed from the window.
type Event = window::Event<String>;

/// The name of the chat broadcast.
pub(super) const NAME: &str = "chat";

/// How many typed messages wait for the writer task.
const SEND_QUEUE: usize = 16;

/// How long a reader waits before reading a member again.
const RETRY: Duration = Duration::from_secs(2);

/// How many delivered messages a reader remembers to skip replays.
const REMEMBERED: usize = 64;

/// A message another member sent.
#[derive(Debug)]
pub(super) struct Message {
    pub(super) from: EndpointId,
    pub(super) text: String,
}

/// This member's chat broadcast, and the task that writes and expires it.
pub(super) struct Writer {
    tx: mpsc::Sender<String>,
    _task: AbortOnDropHandle<()>,
}

/// A broadcast that closes when dropped.
struct ClosesOnDrop(broadcast::Producer);

impl Drop for ClosesOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Writer {
    /// Writes the chat into `producer`, the broadcast published into the room.
    pub(super) fn new(mut producer: broadcast::Producer) -> Result<Self> {
        let mut publisher = Publisher::create(&mut producer)?;
        let (tx, mut rx) = mpsc::channel::<String>(SEND_QUEUE);
        let task = tokio::spawn(async move {
            // Closed when the task ends, which withdraws the chat from the
            // room: the room holds the broadcast open otherwise.
            let _producer = ClosesOnDrop(producer);
            loop {
                let text = tokio::select! {
                    text = rx.recv() => text,
                    () = publisher.expiry() => match publisher.prune() {
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
            tx,
            _task: AbortOnDropHandle::new(task),
        })
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
/// Retries after [`RETRY`] when a read ends. A new read replays the member's
/// window, and messages delivered already are skipped.
pub(super) async fn read(
    room: Room,
    from: EndpointId,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) {
    let mut delivered = VecDeque::new();
    loop {
        if let Err(err) = read_once(&room, from, &mut delivered, &tx, &ctx).await {
            debug!(peer = %from.fmt_short(), error = %err, "chat read ended");
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
    let broadcast = subscription.broadcast();
    let mut chat = subscribe(&broadcast).await?;
    loop {
        // A broadcast whose session died does not always end its tracks.
        let event = tokio::select! {
            event = chat.next() => event.std_context("reading the chat")?,
            _ = broadcast.closed() => None,
        };
        let Some(event) = event else {
            return Ok(());
        };
        let Event::Push { index, value } = event.value else {
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

/// The writer of this member's chat window.
///
/// It retires each message once the window's history has passed.
struct Publisher {
    producer: window::Producer<String>,
    /// When each message in the window expires, oldest first.
    expires: VecDeque<Instant>,
}

impl Publisher {
    /// Creates the chat track on `broadcast`.
    fn create(broadcast: &mut broadcast::Producer) -> Result<Self> {
        let info = track::Info::default()
            .with_timescale(moq_net::Timescale::MILLI)
            .with_priority(PRIORITY);
        let track = broadcast
            .create_track(TRACK, info)
            .std_context("creating the chat track")?;
        let config = window::ProducerConfig::default().with_ops(false);
        Ok(Self {
            producer: window::Producer::new(track, config),
            expires: VecDeque::new(),
        })
    }

    /// Appends `text` to the window, retiring expired messages first.
    fn send(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.prune()?;
        let text = text.to_owned();
        self.producer
            .push(moq_net::Timed::from(&text).at(moq_net::Timestamp::now()))
            .std_context("writing the chat window")?;
        self.expires.push_back(Instant::now() + HISTORY);
        Ok(())
    }

    /// Waits until the oldest message expires, or forever with none.
    async fn expiry(&self) {
        match self.expires.front() {
            Some(at) => tokio::time::sleep_until(*at).await,
            None => std::future::pending().await,
        }
    }

    /// Retires the messages whose history has passed.
    fn prune(&mut self) -> Result<()> {
        let now = Instant::now();
        let count = self.expires.iter().take_while(|at| **at <= now).count();
        if count == 0 {
            return Ok(());
        }
        self.producer
            .pop(moq_net::Timed::from(count as u64).at(moq_net::Timestamp::now()))
            .std_context("retiring chat messages")?;
        self.expires.drain(..count);
        Ok(())
    }
}

/// The reader of a member's chat window, from its newest group.
enum Subscriber {
    /// Waiting for the track's newest group, which holds the whole window.
    Positioning(Option<Box<track::Subscriber>>),
    /// Reading the window from that group on.
    Reading(Box<window::Consumer<String>>),
}

impl Subscriber {
    /// Returns the next window event, or `None` once the track ends.
    ///
    /// Cancel safe: a cancelled read keeps its position.
    async fn next(&mut self) -> moq_json::Result<Option<moq_net::Timed<Event>>> {
        if let Self::Positioning(pending) = self {
            let track = pending.as_mut().expect("positioning holds the track");
            // Older groups only replay what the newest holds.
            let largest = track.largest().await;
            let mut track = *pending.take().expect("positioning holds the track");
            if let Some(largest) = largest {
                track = track.with_range(largest.group..);
            }
            *self = Self::Reading(Box::new(window::Consumer::new(track, Default::default())));
        }
        let Self::Reading(consumer) = self else {
            unreachable!("positioned above");
        };
        consumer.next().await
    }
}

/// Subscribes to the chat window of `broadcast`.
async fn subscribe(broadcast: &broadcast::Consumer) -> Result<Subscriber> {
    let track = broadcast
        .track(TRACK)
        .std_context("subscribing to the chat")?;
    let subscriber = track
        .subscribe(None)
        .await
        .std_context("subscribing to the chat")?;
    Ok(Subscriber::Positioning(Some(Box::new(subscriber))))
}

/// Returns the message at `index` once, remembering that it was delivered.
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
        let producer = broadcast::Info::new().produce();
        let writer = Writer::new(producer.clone()).expect("writer");
        let mut chat = subscribe(&producer.consume()).await.expect("subscribe");
        writer.send("hello".into());
        let event = tokio::time::timeout(Duration::from_secs(5), chat.next())
            .await
            .expect("timed out")
            .expect("read")
            .expect("an event");
        assert!(matches!(event.value, Event::Push { value, .. } if value == "hello"));
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
