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

use std::time::{Duration, SystemTime};

use iroh::EndpointId;
use moq_net::{Timestamp, broadcast, frame, group, track};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast as channel;
use tracing::{debug, warn};

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
/// Each receiver has its own buffer of [`CHAT_BUFFER`] messages; one that falls
/// behind loses its oldest messages and learns how many, and never slows the
/// room or another receiver.
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

/// Reads the frames of one chat track in sequence order.
struct FrameReader {
    track: track::Ordered,
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
        Ok(Self {
            track: track.ordered(),
            group: None,
        })
    }

    /// Returns the next frame, or `None` once the track ends.
    async fn next(&mut self) -> Result<Option<frame::Frame>, moq_net::Error> {
        loop {
            if let Some(group) = self.group.as_mut() {
                if let Some(frame) = group.read_frame().await? {
                    return Ok(Some(frame));
                }
                self.group = None;
            }
            match self.track.next_group().await? {
                Some(group) => self.group = Some(group),
                None => return Ok(None),
            }
        }
    }
}

/// Forwards the chat on `broadcast`, which member `from` publishes, into `tx`.
///
/// Reads `chat.v2` when `legacy` is false, and the bare-text `chat` track of a
/// member on the older format otherwise. Returns when the track ends or fails,
/// which is also how a broadcast without chat ends it, quietly.
pub(crate) async fn forward(
    from: EndpointId,
    broadcast: broadcast::Consumer,
    legacy: bool,
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
    loop {
        let frame = match reader.next().await {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(err) => {
                debug!(from = %from.fmt_short(), %err, "chat track ended");
                return;
            }
        };
        let message = if legacy {
            match String::from_utf8(frame.payload.to_vec()) {
                Ok(text) if !text.is_empty() => ChatMessage {
                    from,
                    text,
                    sent_at: SystemTime::now(),
                },
                Ok(_) => continue,
                Err(err) => {
                    warn!(from = %from.fmt_short(), %err, "chat message is not UTF-8");
                    continue;
                }
            }
        } else {
            match postcard::from_bytes::<ChatFrame>(&frame.payload) {
                Ok(chat) if !chat.text.is_empty() => ChatMessage {
                    from,
                    sent_at: chat.sent_at(),
                    text: chat.text,
                },
                Ok(_) => continue,
                Err(err) => {
                    warn!(from = %from.fmt_short(), %err, "chat frame does not decode");
                    continue;
                }
            }
        };
        // No receiver is not an error: nobody is reading chat right now.
        tx.send(message).ok();
    }
}

#[cfg(test)]
mod tests {
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
        let current = tokio::spawn(forward(member(), writer.consume(), false, tx));
        let legacy = tokio::spawn(forward(member(), writer.consume(), true, legacy_tx));
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

    /// A receiver that falls behind is told how far, and the room goes on.
    #[tokio::test]
    async fn a_slow_receiver_lags_rather_than_blocking() {
        let (tx, rx) = channel::channel(2);
        let mut rx = ChatReceiver::new(rx);
        for n in 0..5 {
            tx.send(ChatMessage {
                from: member(),
                text: n.to_string(),
                sent_at: SystemTime::now(),
            })
            .expect("a receiver exists");
        }
        assert_eq!(rx.recv().await, Err(ChatError::Lagged(3)));
        assert_eq!(rx.recv().await.expect("the oldest kept").text, "3");
    }

    /// A frame keeps its sender's time.
    #[test]
    fn a_frame_carries_its_send_time() {
        let frame = ChatFrame {
            text: "hi".into(),
            sent_at_ms: 1_700_000_000_000,
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
