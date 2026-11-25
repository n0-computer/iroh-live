//! Room chat: one more broadcast every member publishes into the room.
//!
//! The broadcast is named [`NAME`] and carries a `chat` track, a JSON window
//! of the last ten seconds of messages, which `@moq/room` reads too.
//!
//! [`Chat`] is the whole thing in one handle: it publishes this member's chat,
//! reads every other member's, and hands their messages over through
//! [`Chat::try_recv`] or [`Chat::recv`]. [`Writer`] and [`read`] are the
//! parts, for an application that follows the membership itself.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use iroh::EndpointId;
use moq_json::window;
use moq_net::{broadcast, track};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio::{sync::mpsc, time::Instant};
use tracing::{debug, warn};

use crate::Room;

/// The name of the track that carries the chat window.
const TRACK: &str = "chat";

/// The chat track's delivery priority, below audio and video.
const PRIORITY: i8 = 10;

/// How long a sent message stays in the window.
const HISTORY: Duration = Duration::from_secs(10);

/// A message entering, leaving or missed from the window.
type Event = window::Event<String>;

/// The name of the chat broadcast.
pub const NAME: &str = "chat";

/// How many typed messages wait for the writer task.
const SEND_QUEUE: usize = 16;

/// How many received messages wait for the application in [`Chat`].
const RECV_QUEUE: usize = 64;

/// How long a reader waits before reading a member again.
const RETRY: Duration = Duration::from_secs(2);

/// How many delivered messages a reader remembers to skip replays.
const REMEMBERED: usize = 64;

/// A message another member sent.
#[derive(Debug, Clone)]
pub struct Message {
    /// The member that sent it.
    pub from: EndpointId,
    /// The message text.
    pub text: String,
}

/// This member's chat in a room, and every other member's.
///
/// Dropping it withdraws this member's chat and stops the readers.
#[derive(Debug)]
pub struct Chat {
    writer: Writer,
    rx: mpsc::Receiver<Message>,
    _follow: AbortOnDropHandle<()>,
}

impl Chat {
    /// Publishes this member's chat into `room` and starts reading the others.
    ///
    /// Keeps one reader per member that publishes [`NAME`], as the
    /// membership changes.
    ///
    /// # Errors
    ///
    /// Fails if the chat broadcast cannot be published.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn new(room: &Room) -> Result<Self> {
        let writer = Writer::new(room.publish(NAME)?)?;
        let (tx, rx) = mpsc::channel(RECV_QUEUE);
        let follow = tokio::spawn(follow(room.clone(), tx));
        Ok(Self {
            writer,
            rx,
            _follow: AbortOnDropHandle::new(follow),
        })
    }

    /// Sends `text` to the room, unless the writer is backed up.
    ///
    /// Other members' copies come back through [`recv`](Self::recv), this
    /// member's own do not.
    pub fn send(&self, text: impl Into<String>) {
        self.writer.send(text.into());
    }

    /// Returns the next received message, if one is waiting.
    pub fn try_recv(&mut self) -> Option<Message> {
        self.rx.try_recv().ok()
    }

    /// Waits for the next received message.
    ///
    /// Returns `None` once the room was left. Cancel safe.
    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }
}

/// Keeps one chat reader per member that publishes chat, until `tx` closes.
async fn follow(room: Room, tx: mpsc::Sender<Message>) {
    let mut state = room.state();
    let mut readers: HashMap<EndpointId, AbortOnDropHandle<()>> = HashMap::new();
    loop {
        let members = state.get().members;
        readers.retain(|id, _| {
            members
                .get(id)
                .is_some_and(|member| member.broadcasts.contains(NAME))
        });
        for (id, member) in &members {
            if member.broadcasts.contains(NAME) && !readers.contains_key(id) {
                let reader = tokio::spawn(read(room.clone(), *id, tx.clone()));
                readers.insert(*id, AbortOnDropHandle::new(reader));
            }
        }
        tokio::select! {
            changed = state.updated() => if changed.is_err() { return },
            () = tx.closed() => return,
        }
    }
}

/// This member's chat broadcast, and the task that writes and expires it.
#[derive(Debug)]
pub struct Writer {
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
    ///
    /// # Errors
    ///
    /// Fails if the chat track cannot be created on `producer`.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn new(mut producer: broadcast::Producer) -> Result<Self> {
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
    pub fn send(&self, text: String) {
        if self.tx.try_send(text).is_err() {
            warn!("the chat writer is backed up, dropping a message");
        }
    }
}

/// Reads member `from`'s chat into `tx` until `tx` closes or the task is dropped.
///
/// Retries after two seconds when a read ends. A new read replays the
/// member's window, and messages delivered already are skipped.
pub async fn read(room: Room, from: EndpointId, tx: mpsc::Sender<Message>) {
    let mut delivered = VecDeque::new();
    loop {
        if let Err(err) = read_once(&room, from, &mut delivered, &tx).await {
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
        let config = window::Config::default().with_ops(false);
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
            .push(&text, track_time())
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
            .pop(count as u64, track_time())
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
    async fn next(&mut self) -> moq_json::Result<Option<moq_json::Timed<Event>>> {
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

/// Returns the current time on the chat track's millisecond scale.
fn track_time() -> moq_net::Timestamp {
    moq_net::Timestamp::from(moq_net::time::Instant::now())
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
