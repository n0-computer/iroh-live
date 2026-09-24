//! Room chat: one more broadcast every member publishes into the room.
//!
//! The broadcast is named `chat` and has one track of that name, which carries
//! one message per group. The grid leaves it out, and the window reads every
//! other member's with [`read`].

use std::time::{Duration, SystemTime};

use eframe::egui;
use iroh::EndpointId;
use iroh_live::rooms::Room;
use moq_net::{Timestamp, broadcast, track};
use n0_error::{Result, StdResultExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// The name of the chat broadcast, and of its one track.
pub(super) const NAME: &str = "chat";

/// How long a message stays readable, on both ends of the track.
///
/// moq's default is zero, "the newest group or nothing", which would drop a
/// message that a second one overtook before it was read.
const MAX_AGE: Duration = Duration::from_secs(5);

/// How long a reader waits before reading a member again once its read ended.
const RETRY: Duration = Duration::from_secs(2);

/// How long before joining a message may have been sent and still show, for
/// clocks that disagree a little.
const CLOCK_TOLERANCE: Duration = Duration::from_secs(2);

/// One message on the chat track.
#[derive(Debug, Serialize, Deserialize)]
struct Frame {
    text: String,
    /// Milliseconds since the Unix epoch, by the sender's clock.
    sent_at_ms: u64,
    /// Drawn per chat broadcast, so a reader can tell a member that restarted,
    /// whose group sequence starts over, from a repeat.
    writer: u64,
}

/// A message another member sent.
#[derive(Debug)]
pub(super) struct Message {
    pub(super) from: EndpointId,
    pub(super) text: String,
}

/// This member's chat broadcast.
pub(super) struct Writer {
    broadcast: broadcast::Producer,
    track: track::Producer,
    writer: u64,
}

impl Writer {
    /// Creates the chat broadcast.
    pub(super) fn new() -> Result<Self> {
        let broadcast = broadcast::Info::new().produce();
        let track = broadcast
            .create_track(NAME, track::Info::default().with_max_age(MAX_AGE))
            .std_context("creating the chat track")?;
        Ok(Self {
            broadcast,
            track,
            writer: rand::random(),
        })
    }

    /// Returns the broadcast, for publishing it into the room.
    pub(super) fn broadcast(&self) -> broadcast::Consumer {
        self.broadcast.consume()
    }

    /// Sends `text` to the room.
    pub(super) fn send(&mut self, text: &str) -> Result<()> {
        let frame = Frame {
            text: text.to_owned(),
            sent_at_ms: unix_ms(SystemTime::now()),
            writer: self.writer,
        };
        let bytes = postcard::to_stdvec(&frame).expect("a chat frame serializes");
        self.track
            .write_frame(Timestamp::now(), bytes)
            .std_context("writing a chat message")?;
        Ok(())
    }
}

/// How far a member's chat has been delivered.
#[derive(Debug, Default)]
struct Cursor {
    /// The writer the count is of.
    writer: Option<u64>,
    /// One past the highest group sequence delivered.
    next: u64,
}

/// Reads member `from`'s chat into `tx` until the task is dropped.
///
/// Reads the member again after [`RETRY`] whenever a read ends, and delivers a
/// message once across those reads: a new subscription starts with the newest
/// message, which the last read may have delivered already. The first message
/// the first read gets was sent before this member joined if the sender's
/// clock says so, and is left out.
pub(super) async fn read(
    room: Room,
    from: EndpointId,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) {
    let joined = SystemTime::now() - CLOCK_TOLERANCE;
    let mut cursor = Cursor::default();
    loop {
        if let Err(err) = read_once(&room, from, joined, &mut cursor, &tx, &ctx).await {
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
    joined: SystemTime,
    cursor: &mut Cursor,
    tx: &mpsc::Sender<Message>,
    ctx: &egui::Context,
) -> Result<()> {
    let subscription = room.subscribe(from, NAME).await?;
    let broadcast = subscription.as_moq();
    let mut track = broadcast
        .track(NAME)
        .std_context("no chat track")?
        .subscribe(track::Subscription::default().with_max_age(MAX_AGE))
        .await
        .std_context("subscribing to the chat track")?;
    // Groups below this were delivered by an earlier read.
    let mut floor = cursor.next;
    loop {
        // A broadcast whose session died does not always end its tracks.
        let group = tokio::select! {
            group = track.recv_group() => group.std_context("reading the chat track")?,
            _ = broadcast.closed() => None,
        };
        let Some(mut group) = group else {
            return Ok(());
        };
        let Some(frame) = group
            .read_frame()
            .await
            .std_context("reading a chat message")?
        else {
            continue;
        };
        let frame: Frame = match postcard::from_bytes(&frame.payload) {
            Ok(frame) => frame,
            Err(err) => {
                warn!(remote = %from.fmt_short(), error = %err, "chat frame does not decode");
                continue;
            }
        };
        if cursor.writer != Some(frame.writer) {
            let first = cursor.writer.is_none();
            *cursor = Cursor {
                writer: Some(frame.writer),
                next: 0,
            };
            floor = 0;
            if first && frame.sent_at_ms < unix_ms(joined) {
                cursor.next = group.sequence + 1;
                continue;
            }
        }
        if group.sequence < floor {
            continue;
        }
        cursor.next = cursor.next.max(group.sequence + 1);
        let message = Message {
            from,
            text: frame.text,
        };
        if tx.send(message).await.is_err() {
            return Ok(());
        }
        ctx.request_repaint();
    }
}

/// Returns `time` as milliseconds since the Unix epoch.
fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}
