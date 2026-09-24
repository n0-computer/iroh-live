//! Room chat: the tracks it travels on, and the receiver an application reads.
//!
//! Every member publishes a small chat broadcast of its own at
//! `rooms/<topic>/<member>/.chat`, to the room's members only, so chat works with
//! the camera off and needs no media. It carries two tracks. `chat.v2` holds one
//! postcard [`ChatFrame`] per group, with the sender's wall-clock time.
//! `chat` holds the bare UTF-8 text, which is the track the release before this
//! one reads, off whatever broadcast it subscribes to; the room lists its chat
//! broadcast among its broadcasts for that release, so an older member finds it.
//! Writing both is the one release of dual wire format; `chat` goes after it.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use iroh::EndpointId;
use moq_net::{Timestamp, broadcast, frame, group, track};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast as channel;
use tracing::{debug, trace, warn};

/// The name of the chat broadcast inside a member's room namespace.
///
/// A leading dot, which no broadcast name a room accepts can start with, so it
/// never collides with a published broadcast.
pub(crate) const CHAT_BROADCAST: &str = ".chat";

/// The track the release before this one reads chat from: bare UTF-8 text.
pub(crate) const LEGACY_CHAT_TRACK: &str = "chat";

/// The track this release reads chat from: one postcard [`ChatFrame`] per group.
pub(crate) const CHAT_TRACK: &str = "chat.v2";

/// Publisher tie-break priority for chat, below audio and video.
const CHAT_PRIORITY: u8 = 10;

/// How long a chat message stays readable, on both ends of the track.
///
/// The publisher keeps a message this long, and a subscriber waits this long
/// behind the newest one before giving up on it. moq-net's default budget is
/// zero, "the newest group or nothing", which would drop a message that a second
/// one overtook before it was read.
const CHAT_MAX_AGE: Duration = Duration::from_secs(5);

/// How many messages a receiver may fall behind before it is told it lagged.
pub(crate) const CHAT_BUFFER: usize = 64;

/// A chat message from a room member.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatMessage {
    /// The member that sent it.
    pub from: EndpointId,
    /// The text.
    pub text: String,
    /// When the sender sent it, by the sender's clock; the arrival time for a
    /// member on the older chat format, which carries none.
    pub sent_at: SystemTime,
}

/// Why [`ChatReceiver::recv`] returned no message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[non_exhaustive]
pub enum ChatError {
    /// This receiver fell this many messages behind, which were skipped. The
    /// next call returns the oldest message still buffered.
    #[display("skipped {_0} chat messages")]
    Lagged(u64),
    /// The room was left.
    #[display("the room was left")]
    Closed,
}

impl std::error::Error for ChatError {}

/// Receives the room's chat messages, from every member but this one.
///
/// Each receiver has its own buffer of 64 messages; one that falls behind loses
/// its oldest messages and learns how many, and never slows the room or another
/// receiver.
#[derive(Debug)]
pub struct ChatReceiver {
    rx: channel::Receiver<ChatMessage>,
}

impl ChatReceiver {
    pub(crate) fn new(rx: channel::Receiver<ChatMessage>) -> Self {
        Self { rx }
    }

    /// Waits for the next message.
    ///
    /// Cancellation safe: a message is either returned or stays buffered.
    ///
    /// # Errors
    ///
    /// Returns [`ChatError::Lagged`] when this receiver fell behind, and
    /// [`ChatError::Closed`] once the room was left.
    pub async fn recv(&mut self) -> Result<ChatMessage, ChatError> {
        self.rx.recv().await.map_err(|err| match err {
            channel::error::RecvError::Lagged(skipped) => ChatError::Lagged(skipped),
            channel::error::RecvError::Closed => ChatError::Closed,
        })
    }
}

/// One message on the `chat.v2` track.
///
/// Postcard is positional, so a field added later goes at the end, where an
/// older reader ignores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatFrame {
    pub(crate) text: String,
    /// Milliseconds since the Unix epoch, by the sender's clock.
    pub(crate) sent_at_ms: u64,
    /// Drawn at random per chat broadcast, so a reader can tell a member that
    /// restarted, whose group sequence starts over, from a repeat.
    pub(crate) writer: u64,
}

impl ChatFrame {
    fn sent_at(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.sent_at_ms)
    }
}

/// Returns the settings of a chat track.
fn chat_track_info() -> track::Info {
    track::Info::default()
        .with_priority(CHAT_PRIORITY)
        .with_max_age(CHAT_MAX_AGE)
}

/// The chat broadcast a member publishes: both tracks, written together.
#[derive(derive_more::Debug)]
pub(crate) struct ChatWriter {
    #[debug(skip)]
    broadcast: broadcast::Producer,
    #[debug(skip)]
    current: track::Producer,
    #[debug(skip)]
    legacy: track::Producer,
    writer: u64,
}

impl ChatWriter {
    /// Creates the chat broadcast and its two tracks.
    pub(crate) fn new() -> Result<Self, moq_net::Error> {
        let broadcast = broadcast::Info::new().produce();
        let current = broadcast.create_track(CHAT_TRACK, chat_track_info())?;
        let legacy = broadcast.create_track(LEGACY_CHAT_TRACK, chat_track_info())?;
        Ok(Self {
            broadcast,
            current,
            legacy,
            writer: rand::random(),
        })
    }

    /// Returns a reader's view of the chat broadcast, for publishing it.
    pub(crate) fn consume(&self) -> broadcast::Consumer {
        self.broadcast.consume()
    }

    /// Writes `text` to both tracks, one group each.
    ///
    /// Empty text is not written: a reader cannot tell it from a group it
    /// failed to read.
    pub(crate) fn send(&mut self, text: &str) -> Result<(), moq_net::Error> {
        if text.is_empty() {
            return Ok(());
        }
        let sent_at_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();
        let frame = ChatFrame {
            text: text.to_owned(),
            sent_at_ms,
            writer: self.writer,
        };
        let bytes = postcard::to_stdvec(&frame).expect("a chat frame serializes");
        self.current.write_frame(Timestamp::now(), bytes)?;
        self.legacy.write_frame(Timestamp::now(), text.to_owned())?;
        Ok(())
    }

    /// Ends both tracks and the broadcast, so readers see a clean close.
    pub(crate) fn finish(&mut self) {
        self.current.finish().ok();
        self.legacy.finish().ok();
        self.broadcast.finish();
    }
}

/// Reads the frames of one chat track in the order they arrive.
///
/// Arrival order, not sequence order: every message is a group of its own, so
/// two sent close together travel on two streams and can land out of order, and
/// a reader in sequence order skips the one that lands second.
struct FrameReader {
    track: track::Subscriber,
    group: Option<group::Consumer>,
}

impl FrameReader {
    /// Subscribes to track `name` of `broadcast`.
    async fn subscribe(
        broadcast: &broadcast::Consumer,
        name: &str,
    ) -> Result<Self, moq_net::Error> {
        let subscription = track::Subscription::default().with_max_age(CHAT_MAX_AGE);
        let track = broadcast.track(name)?.subscribe(subscription).await?;
        Ok(Self { track, group: None })
    }

    /// Returns the next frame and its group's sequence, or `None` once the
    /// track ends.
    async fn next(&mut self) -> Result<Option<(u64, frame::Frame)>, moq_net::Error> {
        loop {
            if let Some(group) = self.group.as_mut() {
                if let Some(frame) = group.read_frame().await? {
                    return Ok(Some((group.sequence, frame)));
                }
                self.group = None;
            }
            match self.track.recv_group().await? {
                Some(group) => self.group = Some(group),
                None => return Ok(None),
            }
        }
    }
}

/// How long after a source's first subscription is established groups still
/// count as the replay moq hands a new subscription, rather than live chat.
///
/// The replay follows the subscription at once, so a second covers it on a
/// slow link. A live message from a sender whose clock runs behind is lost
/// only if it arrives inside this window.
const REPLAY_WINDOW: Duration = Duration::from_secs(1);

/// How many delivered sequences one reading remembers to drop duplicates.
const SEEN: usize = 256;

/// How far one chat source of a member has been read, kept across its reader
/// restarting.
///
/// moq hands a new subscription the newest group whatever its age, so a reader
/// that restarts (the member's session dropped and came back, say) would
/// deliver the last message a second time. The cursor remembers how far it
/// delivered, and whose chat broadcast that counted.
#[derive(Debug)]
pub(crate) struct ChatCursor {
    state: Mutex<CursorState>,
    /// Messages in the replay of the first subscription sent before this, by
    /// the sender's clock, are history from before this member joined the
    /// room, and are not delivered.
    since: SystemTime,
}

#[derive(Debug, Default)]
struct CursorState {
    /// The writer of the groups `next` counts, for the current format.
    writer: Option<u64>,
    /// One past the highest group sequence delivered.
    next: u64,
    /// A hash of the payload of group `next - 1`, for the older format, whose
    /// restart can otherwise look like the replay of that group.
    last: Option<u64>,
    /// Whether a reading ran over this cursor before.
    read: bool,
}

impl ChatCursor {
    /// Returns a cursor whose first reading skips a replay sent before
    /// `since`.
    pub(crate) fn new(since: SystemTime) -> Self {
        Self {
            state: Mutex::new(CursorState::default()),
            since,
        }
    }

    /// Starts one subscription's reading over this cursor, once the
    /// subscription is established.
    fn reading(&self, legacy: bool) -> Reading<'_> {
        let mut state = self.state.lock().expect("poisoned");
        let first = !state.read;
        state.read = true;
        let now = tokio::time::Instant::now();
        Reading {
            cursor: self,
            floor: state.next,
            seen: BTreeSet::new(),
            replay_until: first.then_some(now + REPLAY_WINDOW),
            pending: (legacy && state.next > 0).then(|| Pending {
                until: now + REPLAY_WINDOW,
                held: Vec::new(),
            }),
        }
    }
}

/// One chat message as read off its group.
struct Line {
    sequence: u64,
    /// The writer id, for the current format.
    writer: Option<u64>,
    /// A hash of the payload.
    hash: u64,
    message: ChatMessage,
}

/// One subscription's reading of a chat source.
///
/// Takes the cursor as a floor once, when the subscription starts, and then
/// delivers every group at or above it once, in whatever order they arrive.
struct Reading<'a> {
    cursor: &'a ChatCursor,
    /// Groups below this were delivered by an earlier reading.
    floor: u64,
    /// Sequences delivered by this reading, the most recent [`SEEN`] of them.
    seen: BTreeSet<u64>,
    /// On a cursor's first reading, until when groups are the replay moq
    /// hands a new subscription.
    replay_until: Option<tokio::time::Instant>,
    /// For the older format, until this reading knows whether the member
    /// restarted since the last one.
    pending: Option<Pending>,
}

/// A reading of the older format that has not yet told a restarted member
/// from one that carries on.
///
/// That format has no writer id, so only the replay can tell: moq always
/// replays the newest group, which for a member that carried on sits at or
/// past the last group delivered, with that group's payload if it is that
/// group. A group below it is held until the replay says which it is.
struct Pending {
    /// When the replay is over. Nothing at or past the last group delivered
    /// by then means the member restarted.
    until: tokio::time::Instant,
    held: Vec<Line>,
}

impl Reading<'_> {
    /// Returns when the reading must decide on what it holds, if it holds
    /// anything undecided.
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.pending.as_ref().map(|pending| pending.until)
    }

    /// Takes one line off the track, and returns what to deliver now.
    fn take(&mut self, line: Line) -> Vec<ChatMessage> {
        let Some(pending) = self.pending.as_mut() else {
            return self.admit(line).into_iter().collect();
        };
        let last = self.floor - 1;
        if line.sequence < last {
            pending.held.push(line);
            return Vec::new();
        }
        let held = std::mem::take(&mut pending.held);
        self.pending = None;
        let known = self.cursor.state.lock().expect("poisoned").last;
        if line.sequence == last && known != Some(line.hash) {
            self.restart();
            return held
                .into_iter()
                .chain([line])
                .filter_map(|line| self.admit(line))
                .collect();
        }
        // The member carried on; what was held was delivered before.
        self.admit(line).into_iter().collect()
    }

    /// Decides once the replay is over without a group at or past the last
    /// one delivered: the member restarted, and what was held is news.
    ///
    /// An empty replay says the same, since a member that carried on always
    /// replays its newest group. A replay slower than [`REPLAY_WINDOW`] then
    /// costs one repeated message.
    fn expire(&mut self) -> Vec<ChatMessage> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        self.restart();
        pending
            .held
            .into_iter()
            .filter_map(|line| self.admit(line))
            .collect()
    }

    /// Starts the cursor over, for a member that restarted.
    fn restart(&mut self) {
        debug!("chat source restarted, reading it from its start");
        let mut state = self.cursor.state.lock().expect("poisoned");
        state.next = 0;
        state.last = None;
        self.floor = 0;
        self.seen.clear();
    }

    /// Returns `line`'s message if it is news, and marks it delivered.
    ///
    /// A current-format line from another writer than the one counted means
    /// the member restarted, and the cursor starts over.
    fn admit(&mut self, line: Line) -> Option<ChatMessage> {
        let mut state = self.cursor.state.lock().expect("poisoned");
        if let Some(writer) = line.writer {
            if state.writer.is_some_and(|known| known != writer) {
                debug!("chat writer changed, reading it from its start");
                state.next = 0;
                self.floor = 0;
                self.seen.clear();
            }
            state.writer = Some(writer);
        }
        if line.sequence < self.floor || !self.seen.insert(line.sequence) {
            trace!(sequence = line.sequence, "chat message already delivered");
            return None;
        }
        if self.seen.len() > SEEN
            && let Some(oldest) = self.seen.pop_first()
        {
            // Past this many, anything this old is a duplicate or so late
            // that moq would have dropped it anyway.
            self.floor = self.floor.max(oldest + 1);
        }
        if line.sequence + 1 >= state.next {
            state.next = line.sequence + 1;
            state.last = Some(line.hash);
        }
        drop(state);
        // The older format carries no send time to judge.
        if line.writer.is_some() && self.is_history(line.message.sent_at) {
            return None;
        }
        Some(line.message)
    }

    /// Reports whether a message sent at `sent_at` is history from before
    /// this member joined, which the first reading of a cursor skips in the
    /// replay it starts with.
    ///
    /// Only the replay is judged by the sender's clock, never live chat: a
    /// sender whose clock runs behind would otherwise lose everything it
    /// says for as long as its clock is behind.
    fn is_history(&self, sent_at: SystemTime) -> bool {
        self.replay_until
            .is_some_and(|until| tokio::time::Instant::now() <= until)
            && sent_at < self.cursor.since
    }
}

/// How long a reader keeps reading after its broadcast closed, for what is
/// still in flight.
const CLOSE_DRAIN: Duration = Duration::from_millis(500);

/// Sleeps until `at`, or forever without one.
async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Returns a hash of a chat payload, to tell two messages at one sequence
/// apart.
fn payload_hash(payload: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    payload.hash(&mut hasher);
    hasher.finish()
}

/// Decodes the group `sequence` of member `from`'s chat, in either format.
///
/// `None` for an empty message, and for one that does not decode, which is
/// logged.
fn decode(from: EndpointId, legacy: bool, sequence: u64, payload: &[u8]) -> Option<Line> {
    let hash = payload_hash(payload);
    if legacy {
        return match std::str::from_utf8(payload) {
            Ok(text) if !text.is_empty() => Some(Line {
                sequence,
                writer: None,
                hash,
                message: ChatMessage {
                    from,
                    text: text.to_owned(),
                    sent_at: SystemTime::now(),
                },
            }),
            Ok(_) => None,
            Err(err) => {
                warn!(from = %from.fmt_short(), %err, "chat message is not UTF-8");
                None
            }
        };
    }
    match postcard::from_bytes::<ChatFrame>(payload) {
        Ok(chat) if !chat.text.is_empty() => Some(Line {
            sequence,
            writer: Some(chat.writer),
            hash,
            message: ChatMessage {
                from,
                sent_at: chat.sent_at(),
                text: chat.text,
            },
        }),
        Ok(_) => None,
        Err(err) => {
            warn!(from = %from.fmt_short(), %err, "chat frame does not decode");
            None
        }
    }
}

/// Forwards the chat on `broadcast`, which member `from` publishes, into `tx`.
///
/// Reads `chat.v2` when `legacy` is false, and the bare-text `chat` track of a
/// member on the older format otherwise. Delivers only what `cursor` has not
/// seen, and on its first reading skips the replay of what was said before
/// this member joined. Returns when the track ends or fails, which is also how
/// a broadcast without chat ends it, quietly.
pub(crate) async fn forward(
    from: EndpointId,
    broadcast: broadcast::Consumer,
    legacy: bool,
    cursor: Arc<ChatCursor>,
    tx: channel::Sender<ChatMessage>,
) {
    let name = if legacy {
        LEGACY_CHAT_TRACK
    } else {
        CHAT_TRACK
    };
    let mut reader = match FrameReader::subscribe(&broadcast, name).await {
        Ok(reader) => reader,
        Err(err) => {
            debug!(from = %from.fmt_short(), %err, "no chat track");
            return;
        }
    };
    let mut reading = cursor.reading(legacy);
    // A broadcast that ends does not always end its tracks: when the session
    // it came over dies, moq-net tears the path down and leaves its tracks
    // open, so the reader also watches the broadcast, and once it closes
    // reads what is still in flight for a moment, then stops.
    let mut closed = std::pin::pin!(broadcast.closed());
    let mut draining: Option<tokio::time::Instant> = None;
    loop {
        let next = tokio::select! {
            next = reader.next() => next,
            () = sleep_until(reading.deadline()) => {
                for message in reading.expire() {
                    tx.send(message).ok();
                }
                continue;
            }
            _ = &mut closed, if draining.is_none() => {
                draining = Some(tokio::time::Instant::now() + CLOSE_DRAIN);
                continue;
            }
            () = sleep_until(draining) => {
                debug!(from = %from.fmt_short(), "chat broadcast ended");
                for message in reading.expire() {
                    tx.send(message).ok();
                }
                return;
            }
        };
        let (sequence, frame) = match next {
            Ok(Some(next)) => next,
            Ok(None) => return,
            Err(err) => {
                debug!(from = %from.fmt_short(), %err, "chat track ended");
                return;
            }
        };
        let Some(line) = decode(from, legacy, sequence, &frame.payload) else {
            continue;
        };
        for message in reading.take(line) {
            // No receiver is not an error: nobody is reading chat right now.
            tx.send(message).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use n0_future::task::AbortOnDropHandle;

    use super::*;

    fn member() -> EndpointId {
        iroh::SecretKey::from_bytes(&[9; 32]).public()
    }

    /// Both tracks carry every message, in order: the current format for
    /// current members, the bare text for members on the older one.
    #[tokio::test]
    async fn a_message_reaches_both_formats() {
        let mut writer = ChatWriter::new().expect("writer");
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let (legacy_tx, legacy_rx) = channel::channel(CHAT_BUFFER);
        let current = tokio::spawn(forward(member(), writer.consume(), false, cursor(), tx));
        let legacy = tokio::spawn(forward(
            member(),
            writer.consume(),
            true,
            cursor(),
            legacy_tx,
        ));
        let (mut rx, mut legacy_rx) = (ChatReceiver::new(rx), ChatReceiver::new(legacy_rx));
        // Give both readers their subscription before anything is written.
        tokio::time::sleep(Duration::from_millis(100)).await;

        writer.send("hello").expect("send");
        writer.send("").expect("an empty message is skipped");
        writer.send("world").expect("send");

        for receiver in [&mut rx, &mut legacy_rx] {
            assert_eq!(receiver.recv().await.expect("hello").text, "hello");
            let second = receiver.recv().await.expect("world");
            assert_eq!(second.text, "world");
            assert_eq!(second.from, member());
        }
        writer.finish();
        current.await.expect("reader");
        legacy.await.expect("reader");
        assert_eq!(rx.recv().await, Err(ChatError::Closed));
    }

    fn cursor() -> Arc<ChatCursor> {
        Arc::new(ChatCursor::new(SystemTime::UNIX_EPOCH))
    }

    /// Waits for the next message, failing the test after a while.
    async fn next(rx: &mut ChatReceiver) -> ChatMessage {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a message")
            .expect("a message")
    }

    /// Reports whether a message arrives within a short while.
    async fn quiet(rx: &mut ChatReceiver) -> bool {
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err()
    }

    /// A reader that restarts over the same cursor does not deliver the last
    /// message again, although moq hands the new subscription that group.
    #[tokio::test]
    async fn a_restarted_reader_does_not_repeat_a_message() {
        let mut writer = ChatWriter::new().expect("writer");
        let cursor = cursor();
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);
        let first = tokio::spawn(forward(
            member(),
            writer.consume(),
            false,
            cursor.clone(),
            tx.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        writer.send("hello").expect("send");
        assert_eq!(next(&mut rx).await.text, "hello");
        first.abort();

        let _second = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            writer.consume(),
            false,
            cursor,
            tx,
        )));
        assert!(quiet(&mut rx).await, "the last message was delivered again");
        writer.send("world").expect("send");
        assert_eq!(next(&mut rx).await.text, "world");
    }

    /// A member that restarts writes a new chat broadcast whose sequence starts
    /// over; its messages are new, not repeats.
    #[tokio::test]
    async fn a_restarted_member_is_read_from_its_first_message() {
        let cursor = cursor();
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);
        for text in ["before", "after"] {
            let mut writer = ChatWriter::new().expect("writer");
            let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
                member(),
                writer.consume(),
                false,
                cursor.clone(),
                tx.clone(),
            )));
            tokio::time::sleep(Duration::from_millis(100)).await;
            writer.send(text).expect("send");
            assert_eq!(next(&mut rx).await.text, text);
        }
    }

    /// What a member said before this member joined is history, not news.
    #[tokio::test]
    async fn a_message_from_before_joining_is_not_delivered() {
        let mut writer = ChatWriter::new().expect("writer");
        writer.send("old").expect("send");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let cursor = Arc::new(ChatCursor::new(SystemTime::now()));
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);
        let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            writer.consume(),
            false,
            cursor,
            tx,
        )));
        assert!(
            quiet(&mut rx).await,
            "a message from before joining arrived"
        );
        writer.send("new").expect("send");
        assert_eq!(next(&mut rx).await.text, "new");
    }

    /// A chat broadcast whose groups the test places by hand.
    struct RawChat {
        broadcast: broadcast::Producer,
        track: track::Producer,
    }

    impl RawChat {
        fn new(name: &str) -> Self {
            let broadcast = broadcast::Info::new().produce();
            let track = broadcast
                .create_track(name, chat_track_info())
                .expect("track");
            Self { broadcast, track }
        }

        /// Writes `payload` as group `sequence`.
        fn write(&self, sequence: u64, payload: Vec<u8>) {
            let mut group = self
                .track
                .create_group(group::Info { sequence })
                .expect("group");
            group.write_frame(Timestamp::now(), payload).expect("frame");
            group.finish().expect("finish");
        }

        /// Writes a current-format message sent at `sent_at`.
        fn say(&self, sequence: u64, text: &str, sent_at: SystemTime) {
            let frame = ChatFrame {
                text: text.to_owned(),
                sent_at_ms: sent_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("after the epoch")
                    .as_millis() as u64,
                writer: 7,
            };
            self.write(sequence, postcard::to_stdvec(&frame).expect("encode"));
        }
    }

    /// Two messages that arrive out of order are both delivered: the one that
    /// lands second is late, not a repeat.
    #[tokio::test]
    async fn messages_out_of_order_are_both_delivered() {
        let chat = RawChat::new(CHAT_TRACK);
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);
        let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            chat.broadcast.consume(),
            false,
            cursor(),
            tx,
        )));
        tokio::time::sleep(Duration::from_millis(100)).await;
        chat.say(1, "second", SystemTime::now());
        assert_eq!(next(&mut rx).await.text, "second");
        chat.say(0, "first", SystemTime::now());
        assert_eq!(next(&mut rx).await.text, "first");
    }

    /// A member whose clock runs a minute behind is heard: only the replay a
    /// new subscription starts with is judged by the sender's clock.
    #[tokio::test]
    async fn a_sender_whose_clock_runs_behind_is_heard() {
        let chat = RawChat::new(CHAT_TRACK);
        let cursor = Arc::new(ChatCursor::new(SystemTime::now()));
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);
        let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            chat.broadcast.consume(),
            false,
            cursor,
            tx,
        )));
        tokio::time::sleep(REPLAY_WINDOW + Duration::from_millis(200)).await;
        let behind = SystemTime::now() - Duration::from_secs(60);
        chat.say(0, "live, by a slow clock", behind);
        assert_eq!(next(&mut rx).await.text, "live, by a slow clock");
    }

    /// A member on the older format that restarts begins its sequence again,
    /// and what it says after the restart is delivered.
    #[tokio::test]
    async fn an_older_member_that_restarts_is_heard() {
        let cursor = cursor();
        let (tx, rx) = channel::channel(CHAT_BUFFER);
        let mut rx = ChatReceiver::new(rx);

        let before = RawChat::new(LEGACY_CHAT_TRACK);
        let reader = tokio::spawn(forward(
            member(),
            before.broadcast.consume(),
            true,
            cursor.clone(),
            tx.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        for (sequence, text) in ["a", "b", "c"].iter().enumerate() {
            before.write(sequence as u64, text.as_bytes().to_vec());
            assert_eq!(next(&mut rx).await.text, *text);
        }
        reader.abort();

        // The restarted member said two lines before this reader came back,
        // so the first group it gets sits at the sequence last delivered.
        let after = RawChat::new(LEGACY_CHAT_TRACK);
        after.write(0, b"back".to_vec());
        after.write(1, b"again".to_vec());
        after.write(2, b"and more".to_vec());
        let reader = tokio::spawn(forward(
            member(),
            after.broadcast.consume(),
            true,
            cursor.clone(),
            tx.clone(),
        ));
        let mut heard = BTreeSet::new();
        for _ in 0..3 {
            heard.insert(next(&mut rx).await.text);
        }
        assert_eq!(
            heard,
            BTreeSet::from(["back".into(), "again".into(), "and more".into()])
        );
        reader.abort();

        // A reader that merely restarts over the same member hears nothing
        // twice, whichever of the replayed groups it gets first, and hears
        // what comes next.
        let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            after.broadcast.consume(),
            true,
            cursor,
            tx,
        )));
        tokio::time::sleep(REPLAY_WINDOW + Duration::from_millis(200)).await;
        assert!(quiet(&mut rx).await, "a message was delivered twice");
        after.write(3, b"news".to_vec());
        assert_eq!(next(&mut rx).await.text, "news");
    }

    /// A receiver that falls behind is told how far, and the reader goes on
    /// rather than waiting for it.
    #[tokio::test]
    async fn a_slow_receiver_lags_rather_than_blocking() {
        let mut writer = ChatWriter::new().expect("writer");
        let (tx, rx) = channel::channel(2);
        let mut rx = ChatReceiver::new(rx);
        let _reader = AbortOnDropHandle::new(tokio::spawn(forward(
            member(),
            writer.consume(),
            false,
            cursor(),
            tx,
        )));
        tokio::time::sleep(Duration::from_millis(100)).await;
        for n in 0..5 {
            writer.send(&n.to_string()).expect("send");
            // One group per message, read in order before the next is sent,
            // so the reader forwards all five into the full buffer.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(rx.recv().await, Err(ChatError::Lagged(3)));
        assert_eq!(rx.recv().await.expect("the oldest kept").text, "3");
        assert_eq!(next(&mut rx).await.text, "4");
    }

    /// A frame keeps its sender's time.
    #[test]
    fn a_frame_carries_its_send_time() {
        let frame = ChatFrame {
            text: "hi".into(),
            sent_at_ms: 1_700_000_000_000,
            writer: 7,
        };
        let bytes = postcard::to_stdvec(&frame).expect("encode");
        let decoded: ChatFrame = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, frame);
        assert_eq!(
            decoded.sent_at(),
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_000)
        );
    }
}
