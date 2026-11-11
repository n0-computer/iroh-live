//! Publishing: a broadcast that exists apart from any transport.
//!
//! A [`LocalBroadcast`] holds a catalog, a media clock, one video slot and one
//! audio slot. It fills a `broadcast::Producer`: one a transport created for
//! publishing ([`LocalBroadcast::new_in`]), or one of its own
//! ([`LocalBroadcast::new`]). A player reads it in-process with
//! `RemoteBroadcast::from_consumer(broadcast.consume())`.
//!
//! The video slot opens its source once and encodes it into every rendition of
//! a ladder. Each rendition encodes only while someone watches it.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use moq_ext_media_core::Error;
use moq_ext_sources::{AudioSource, EncodedVideoSource, VideoSource};
use moq_net::Consume;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info, info_span};

use self::status::{Medium, StatusCell};
pub use self::{
    encoding::{AudioEncoding, VideoEncoding, VideoRendition},
    status::{PublishStatus, RenditionState},
};
use crate::stats::{PublishRecorder, PublishStats};

mod audio;
mod encoding;
pub(crate) mod status;
mod video;

/// A running slot task. Dropping it cancels and aborts the task.
#[derive(Debug)]
pub(crate) struct SlotTask {
    stop: CancellationToken,
    /// Set before `stop` when a successor takes the task's tracks over.
    handing_over: Arc<AtomicBool>,
    task: Option<AbortOnDropHandle<()>>,
}

impl SlotTask {
    /// Spawns `run` with a stop token that is a child of `closed`.
    ///
    /// `handing_over` is the flag [`hand_over`](Self::hand_over) sets, which
    /// the task reads once stopped.
    fn spawn<F, Fut>(
        span: Span,
        closed: &CancellationToken,
        handing_over: Arc<AtomicBool>,
        run: F,
    ) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let stop = closed.child_token();
        let task = n0_future::task::spawn(run(stop.clone()).instrument(span));
        Self {
            stop,
            handing_over,
            task: Some(AbortOnDropHandle::new(task)),
        }
    }

    /// Stops the task for a successor, and waits at most `patience` for it to finish.
    ///
    /// A video task leaves its rendition tracks open for the successor to take
    /// over, so subscribers keep their tracks. Other tasks finish theirs, as
    /// [`finish`](Self::finish) does.
    async fn hand_over(self, patience: Duration) {
        self.handing_over.store(true, Ordering::Release);
        self.finish(patience).await;
    }

    /// Stops the task and waits at most `patience` for it to finish.
    ///
    /// A task that finishes in time ends its tracks cleanly, so subscribers see
    /// an end instead of a reset. A slower task is aborted.
    pub(crate) async fn finish(mut self, patience: Duration) {
        self.stop.cancel();
        let Some(task) = self.task.take() else {
            return;
        };
        if tokio::time::timeout(patience, task).await.is_err() {
            debug!(
                ?patience,
                "a publish task did not finish in time and was aborted"
            );
        }
    }
}

impl Drop for SlotTask {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// How long a replaced or closed publish task gets to finish its tracks.
const FINISH_PATIENCE: Duration = Duration::from_secs(2);

/// Everything a slot task needs.
struct Job {
    producer: moq_net::broadcast::Producer,
    catalog: moq_mux::catalog::Producer,
    clock: moq_mux::Clock,
    /// Where the video task records the source timestamp of each frame it publishes.
    timestamps: TimestampLog,
    /// Held while the task owns its track names.
    tracks: Arc<tokio::sync::Mutex<()>>,
    /// The tracks a replaced video task left open, for this one to take over.
    handovers: video::Handovers,
    /// Set when a successor takes this task's tracks over.
    handing_over: Arc<AtomicBool>,
    stats: PublishRecorder,
    reporter: status::Reporter,
    /// The task this one replaces. It stops before the track names are taken.
    predecessor: Option<SlotTask>,
}

impl Job {
    /// Stops the predecessor, then locks the track names.
    ///
    /// Returns `None` if `stop` fires first. Tracks the predecessor left for
    /// this task then stay for the next successor if there is one, and finish
    /// otherwise.
    async fn take_over(
        &mut self,
        stop: &CancellationToken,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let taken = async {
            if let Some(predecessor) = self.predecessor.take() {
                predecessor.hand_over(FINISH_PATIENCE).await;
            }
            self.tracks.clone().lock_owned().await
        };
        tokio::select! {
            guard = taken => Some(guard),
            () = stop.cancelled() => {
                if !self.handing_over.load(Ordering::Acquire) {
                    video::finish_handovers(&self.handovers);
                }
                None
            }
        }
    }
}

/// How far past the broadcast clock a mapped timestamp may land before it counts as a break.
///
/// A source stamps a frame when it captures it, so its frames land at or
/// before the clock's current time. One that lands later than this came from
/// another timeline.
const MAX_AHEAD: Duration = Duration::from_secs(1);

/// How many published video frames a broadcast keeps the source timestamp of.
///
/// About 30 s at 30 fps, far longer than a round trip to a subscriber takes.
const TIMESTAMP_HISTORY: usize = 1024;

/// The source timestamp of each recently published video frame, by its broadcast timestamp.
///
/// A broadcast timestamp maps to one source timestamp, because the broadcast
/// clock never goes back. Every rendition publishes a picture at the same
/// broadcast timestamp, so the renditions of a ladder share one entry.
#[derive(Debug, Clone, Default)]
struct TimestampLog(Arc<Mutex<BTreeMap<moq_net::Timestamp, moq_net::Timestamp>>>);

impl TimestampLog {
    /// Records that the frame stamped `source` was published at `published`.
    ///
    /// Forgets the oldest entry past [`TIMESTAMP_HISTORY`].
    fn record(&self, published: moq_net::Timestamp, source: moq_net::Timestamp) {
        let mut log = self.0.lock().expect("poisoned");
        log.insert(published, source);
        if log.len() > TIMESTAMP_HISTORY {
            log.pop_first();
        }
    }

    /// Returns the source timestamp of the frame published at `published`.
    fn source(&self, published: moq_net::Timestamp) -> Option<moq_net::Timestamp> {
        self.0.lock().expect("poisoned").get(&published).copied()
    }
}

/// Maps a source's timestamps onto the broadcast clock.
///
/// The first frame lands at the clock's current time, and later frames keep
/// the source's own cadence. When the source's timeline breaks, the next frame
/// lands at the current time again, so the broadcast never goes back. A
/// timeline breaks when a frame would land at or before the previous one, or
/// more than [`MAX_AHEAD`] past the clock. That happens when an application
/// pushes another device into the same source, such as a camera whose
/// timestamps start at zero after a test pattern on the process clock.
///
/// Clones share the anchor, so every rendition of a ladder carries the same
/// timestamp for the same picture. Each clone keeps its own last timestamp.
#[derive(Debug, Clone)]
struct Timeline {
    clock: moq_mux::Clock,
    anchor: Arc<Mutex<Anchor>>,
    /// The last timestamp this clone mapped.
    last: Option<moq_net::Timestamp>,
    /// The breaks this clone has seen, to tell it about a new one once.
    breaks: u64,
    /// Where each mapping is recorded, for a timeline that records them.
    log: Option<TimestampLog>,
}

/// Where a [`Timeline`] is anchored.
#[derive(Debug, Default)]
struct Anchor {
    /// Broadcast micros minus source micros, unset before the first frame.
    offset: Option<i128>,
    /// How many times the source's timeline broke.
    breaks: u64,
}

impl Timeline {
    /// Creates a timeline on `clock` that anchors at its first frame.
    fn new(clock: moq_mux::Clock) -> Self {
        Self {
            clock,
            anchor: Default::default(),
            last: None,
            breaks: 0,
            log: None,
        }
    }

    /// Records every mapping in `log`.
    fn recording(mut self, log: TimestampLog) -> Self {
        self.log = Some(log);
        self
    }

    /// Reports whether the source's timeline broke since this clone last asked.
    ///
    /// An encoder starts a new group there, so the new source starts with a
    /// keyframe instead of a delta on a picture of another source.
    fn broke(&mut self) -> bool {
        let breaks = self.anchor.lock().expect("poisoned").breaks;
        std::mem::replace(&mut self.breaks, breaks) != breaks
    }

    /// Continues after `last`, the last timestamp a predecessor published on the same track.
    fn after(mut self, last: Option<moq_net::Timestamp>) -> Self {
        self.last = last;
        self
    }

    /// Returns the last timestamp this clone mapped.
    fn last(&self) -> Option<moq_net::Timestamp> {
        self.last
    }

    /// Returns `timestamp` on the broadcast clock, anchoring again if the source's timeline broke.
    fn map(&mut self, timestamp: moq_net::Timestamp) -> moq_net::Timestamp {
        let now = self.clock.now().as_micros() as i128;
        let source = timestamp.as_micros() as i128;
        let last = self.last.map(|last| last.as_micros() as i128);
        let fits = |offset: i128| {
            let mapped = source + offset;
            last.is_none_or(|last| mapped > last) && mapped <= now + MAX_AHEAD.as_micros() as i128
        };
        let offset = {
            let mut anchor = self.anchor.lock().expect("poisoned");
            match anchor.offset {
                Some(offset) if fits(offset) => offset,
                previous => {
                    let at = last.map_or(now, |last| now.max(last + 1));
                    if let Some(previous) = previous {
                        info!(
                            source_us = source,
                            jump_us = at - (source + previous),
                            "the source's timeline broke, anchoring it at the broadcast clock again"
                        );
                        anchor.breaks += 1;
                    }
                    anchor.offset = Some(at - source);
                    at - source
                }
            }
        };
        let micros = u64::try_from(source + offset).unwrap_or(0);
        let mapped = moq_net::Timestamp::from_micros(micros).unwrap_or(timestamp);
        self.last = Some(mapped);
        if let Some(log) = &self.log {
            log.record(mapped, timestamp);
        }
        mapped
    }
}

/// The task a slot is running, with its generation.
#[derive(Debug)]
struct Slot {
    generation: u64,
    task: SlotTask,
}

/// The state every clone of a [`LocalBroadcast`] shares.
#[derive(derive_more::Debug)]
#[debug("Shared {{ status: {status:?} }}")]
struct Shared {
    producer: moq_net::broadcast::Producer,
    catalog: Mutex<moq_mux::catalog::Producer>,
    clock: moq_mux::Clock,
    /// The source timestamps of the published video, for [`LocalBroadcast::video_source_timestamp`].
    video_timestamps: TimestampLog,
    video: Mutex<Option<Slot>>,
    audio: Mutex<Option<Slot>>,
    /// Held by a video task while it owns track names.
    ///
    /// A replacement waits on it until its predecessor's tracks are gone.
    video_tracks: Arc<tokio::sync::Mutex<()>>,
    /// The same for audio, whose track name does not change on replacement.
    audio_tracks: Arc<tokio::sync::Mutex<()>>,
    /// The tracks a replaced video task left open for its successor.
    video_handovers: video::Handovers,
    generations: AtomicU64,
    status: StatusCell,
    stats: PublishRecorder,
    /// Cancelled by `close`. Every publish task watches it.
    closed: CancellationToken,
    /// Cancelled once `close` has finished every track and the broadcast.
    finished: CancellationToken,
    /// The task `close` runs, held so it is not detached.
    closer: Mutex<Option<AbortOnDropHandle<()>>>,
    /// Cleared slots finishing their tracks, held so they are not detached.
    retiring: Mutex<n0_future::task::JoinSet<()>>,
    span: Span,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The last handle is gone without `close`. End the broadcast so
        // subscribers see it end instead of waiting on it.
        self.closed.cancel();
        if let Err(err) = self.catalog.lock().expect("poisoned").finish() {
            debug!(error = %err, "catalog did not finish cleanly");
        }
        self.producer.close();
    }
}

/// A broadcast this process produces.
///
/// It holds a catalog, a media clock, one video slot and one audio slot. A
/// transport that publishes it hands it the broadcast to fill: build it with
/// [`new_in`](Self::new_in) or `TryFrom<broadcast::Producer>`, as `iroh-live`'s
/// `publish` does. A standalone one from [`new`](Self::new) reads in-process
/// through [`consume`](Self::consume).
///
/// Cloning is cheap. [`close`](Self::close) ends the broadcast for every clone.
/// The setters spawn an encoding task, so call them from within a Tokio
/// runtime.
///
/// Dropping the last clone ends the broadcast without waiting. It aborts the
/// encoding tasks and closes the catalog and the broadcast, so subscribers see
/// it end.
#[derive(Debug, Clone)]
pub struct LocalBroadcast {
    shared: Arc<Shared>,
}

impl Default for LocalBroadcast {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<moq_net::broadcast::Producer> for LocalBroadcast {
    type Error = Error;

    /// Fills `producer`, as [`LocalBroadcast::new_in`] does.
    fn try_from(producer: moq_net::broadcast::Producer) -> Result<Self, Error> {
        Self::new_in(producer)
    }
}

impl LocalBroadcast {
    /// Creates an empty broadcast of its own, published nowhere.
    ///
    /// For a local preview, a test, or a transport that takes any
    /// `broadcast::Consumer`. To publish, build it in the transport's broadcast
    /// with [`new_in`](Self::new_in) instead, so peers read it without a copy.
    #[must_use]
    pub fn new() -> Self {
        Self::new_in(moq_net::broadcast::Info::new().produce())
            .expect("a new broadcast has no catalog track and is open")
    }

    /// Creates an empty broadcast that fills `producer`.
    ///
    /// Pass the broadcast a transport created for publishing, such as an
    /// `iroh_live_node::Publication`'s: its readers read what this writes, with
    /// nothing copied in between. Closing this closes `producer`, which
    /// withdraws the publication.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Broadcast`] if `producer` already holds a catalog
    /// track, or is closed.
    pub fn new_in(mut producer: moq_net::broadcast::Producer) -> Result<Self, Error> {
        // The catalog advertises the clock's wall mapping, so it is built with
        // the clock that stamps the media.
        let clock = moq_mux::Clock::new();
        let config = moq_mux::catalog::Config::default()
            .with_catalog(hang::catalog::Catalog::default())
            .with_clock(clock.clone());
        let catalog =
            moq_mux::catalog::Producer::new(&mut producer, config).map_err(Error::broadcast)?;
        let span = info_span!("broadcast");
        Ok(Self {
            shared: Arc::new(Shared {
                producer,
                catalog: Mutex::new(catalog),
                clock,
                video_timestamps: TimestampLog::default(),
                video: Mutex::new(None),
                audio: Mutex::new(None),
                video_tracks: Default::default(),
                audio_tracks: Default::default(),
                video_handovers: Default::default(),
                generations: AtomicU64::new(0),
                status: StatusCell::default(),
                stats: PublishRecorder::default(),
                closed: CancellationToken::new(),
                finished: CancellationToken::new(),
                closer: Mutex::new(None),
                retiring: Mutex::new(n0_future::task::JoinSet::new()),
                span,
            }),
        })
    }

    /// Encodes `source` into every rendition of `encoding`, replacing the current video.
    ///
    /// Returns once the encoding task has started. Its progress shows in
    /// [`status`](Self::status).
    ///
    /// A rendition with the name of one the replaced video published goes on
    /// in the same track, so its viewers see the new source from its first
    /// keyframe without subscribing again. If the new encoder's stream needs
    /// another decoder, for example a new H.264 level after a switch to a
    /// larger source, the rendition moves to a newly named track such as
    /// `video/1`, and a player in automatic mode follows the catalog to it.
    /// Renditions the new ladder lacks end.
    ///
    /// # Errors
    ///
    /// Fails on invalid configuration: an empty ladder, duplicate rendition
    /// names, a rate above the source's, or a codec no compiled-in encoder
    /// supports. Returns [`Error::Closed`] after [`close`](Self::close).
    pub fn set_video(&self, source: VideoSource, encoding: VideoEncoding) -> Result<(), Error> {
        self.check_open()?;
        encoding.validate(source.format().rate)?;
        let names: Vec<String> = encoding
            .renditions
            .iter()
            .map(|rendition| rendition.name.clone())
            .collect();
        info!(parent: &self.shared.span, source = source.kind(), renditions = ?names, "video set");
        self.replace(Medium::Video, names, move |job, stop| {
            video::run_raw(job, source, encoding, stop)
        });
        Ok(())
    }

    /// Publishes a pre-encoded stream as the one video rendition, `video`.
    ///
    /// The stream's own parameter sets describe the rendition. A source that
    /// ends before its first access unit shows as failed in
    /// [`status`](Self::status).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Closed`] after [`close`](Self::close).
    pub fn set_encoded_video(&self, source: EncodedVideoSource) -> Result<(), Error> {
        self.check_open()?;
        info!(parent: &self.shared.span, "pre-encoded video set");
        self.replace(
            Medium::Video,
            vec![video::ENCODED_RENDITION.to_string()],
            move |job, stop| video::run_encoded(job, source, stop),
        );
        Ok(())
    }

    /// Encodes `source` as the broadcast's audio, replacing the current audio.
    ///
    /// A microphone opens when the publication starts. If it fails then or
    /// later, the audio slot shows the failure in [`status`](Self::status) and
    /// stays failed until the audio is set again.
    ///
    /// # Errors
    ///
    /// Fails for an invalid encoding, or with [`Error::Closed`] after
    /// [`close`](Self::close).
    pub fn set_audio(&self, source: AudioSource, encoding: AudioEncoding) -> Result<(), Error> {
        self.check_open()?;
        encoding.validate()?;
        let name = encoding.track_name();
        info!(parent: &self.shared.span, source = source.kind_name(), track = %name, "audio set");
        self.replace(Medium::Audio, vec![name], move |job, stop| {
            audio::run(job, source, encoding, stop)
        });
        Ok(())
    }

    /// Stops publishing video.
    pub fn clear_video(&self) {
        self.clear(Medium::Video);
    }

    /// Stops publishing audio.
    pub fn clear_audio(&self) {
        self.clear(Medium::Audio);
    }

    /// Returns a receiver of the state of both slots and of every rendition.
    ///
    /// A borrow of the receiver blocks the broadcast's writes, so keep it short
    /// and do not hold it across a call into the broadcast.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<PublishStatus> {
        self.shared.status.subscribe()
    }

    /// Returns the source timestamp of the video frame this broadcast published at `published`.
    ///
    /// The broadcast moves every source timestamp onto its own clock, so a
    /// subscriber's decoded frame carries another timestamp than the frame the
    /// source produced. A subscriber that sends the decoded frame's timestamp
    /// back lets the publisher find its own frame with this, without marking
    /// the pixels. Every rendition shares the timestamp of a picture.
    ///
    /// Returns `None` for a timestamp this broadcast did not publish, for one
    /// older than the last 1024 frames, and for a pre-encoded source, whose
    /// timestamps are published as they are.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn example(broadcast: &moq_ext_publisher::LocalBroadcast, echoed: moq_net::Timestamp) {
    /// // `echoed` is the timestamp of a frame a subscriber decoded.
    /// if let Some(source) = broadcast.video_source_timestamp(echoed) {
    ///     println!("the subscriber saw the frame the source stamped {source:?}");
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn video_source_timestamp(
        &self,
        published: moq_net::Timestamp,
    ) -> Option<moq_net::Timestamp> {
        self.shared.video_timestamps.source(published)
    }

    /// Returns what the broadcast is sending, per rendition.
    #[must_use]
    pub fn stats(&self) -> PublishStats {
        self.shared.stats.snapshot()
    }

    /// Returns a reader of the broadcast, for playing it in-process.
    ///
    /// The same as the [`Consume`] impl, without importing the trait:
    /// `RemoteBroadcast::from_consumer(broadcast.consume())` plays it.
    #[must_use]
    pub fn consume(&self) -> moq_net::broadcast::Consumer {
        self.shared.producer.consume()
    }

    /// Closes the broadcast for every clone.
    ///
    /// Stops the encoders and lets each finish its track. Then it ends the
    /// catalog and the broadcast, which withdraws the broadcast everywhere it
    /// was published. Returns at once, and [`closed`](Self::closed) waits for
    /// the end. Calling it again does nothing.
    pub fn close(&self) {
        if self.shared.closed.is_cancelled() {
            return;
        }
        self.shared.closed.cancel();
        info!(parent: &self.shared.span, "closing");
        let video = self.shared.video.lock().expect("poisoned").take();
        let audio = self.shared.audio.lock().expect("poisoned").take();
        let shared = Arc::downgrade(&self.shared);
        let finished = self.shared.finished.clone();
        let task = n0_future::task::spawn(async move {
            if let Some(video) = video {
                video.task.finish(FINISH_PATIENCE).await;
            }
            if let Some(audio) = audio {
                audio.task.finish(FINISH_PATIENCE).await;
            }
            if let Some(shared) = shared.upgrade() {
                if let Err(err) = shared.catalog.lock().expect("poisoned").finish() {
                    debug!(error = %err, "catalog did not finish cleanly");
                }
                shared.producer.close();
            }
            finished.cancel();
        });
        *self.shared.closer.lock().expect("poisoned") = Some(AbortOnDropHandle::new(task));
    }

    /// Waits until the broadcast is closed and every track has finished.
    ///
    /// Cancellation safe.
    pub async fn closed(&self) {
        self.shared.finished.cancelled().await;
    }

    /// Fails with [`Error::Closed`] once the broadcast is closed.
    fn check_open(&self) -> Result<(), Error> {
        if self.shared.closed.is_cancelled() {
            Err(n0_error::e!(Error::Closed))
        } else {
            Ok(())
        }
    }

    /// Replaces a slot's task in one critical section.
    ///
    /// The new task finishes the old one before it takes the track names. The
    /// status moves to the new generation under the same lock, so a late report
    /// from the old task cannot land on the new slot.
    fn replace<F, Fut>(&self, medium: Medium, names: Vec<String>, run: F)
    where
        F: FnOnce(Job, CancellationToken) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let shared = &self.shared;
        let (lock, tracks, handovers, span) = match medium {
            Medium::Video => (
                &shared.video,
                &shared.video_tracks,
                shared.video_handovers.clone(),
                info_span!(parent: &shared.span, "video"),
            ),
            // Audio finishes its track on replacement, so nothing is handed over.
            Medium::Audio => (
                &shared.audio,
                &shared.audio_tracks,
                Default::default(),
                info_span!(parent: &shared.span, "audio"),
            ),
        };
        let mut slot = lock.lock().expect("poisoned");
        let generation = shared.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let handing_over = Arc::new(AtomicBool::new(false));
        let job = Job {
            producer: shared.producer.clone(),
            catalog: shared.catalog.lock().expect("poisoned").clone(),
            clock: shared.clock.clone(),
            timestamps: shared.video_timestamps.clone(),
            tracks: tracks.clone(),
            handovers,
            handing_over: handing_over.clone(),
            stats: shared.stats.clone(),
            reporter: shared.status.begin(medium, generation, &names),
            predecessor: slot.take().map(|previous| previous.task),
        };
        let task = SlotTask::spawn(span, &shared.closed, handing_over, |stop| run(job, stop));
        *slot = Some(Slot { generation, task });
    }

    /// Stops the task of a slot, and lets it finish its tracks in the background.
    fn clear(&self, medium: Medium) {
        let lock = match medium {
            Medium::Video => &self.shared.video,
            Medium::Audio => &self.shared.audio,
        };
        let previous = lock.lock().expect("poisoned").take();
        if let Some(previous) = previous {
            self.shared.status.clear(medium, previous.generation);
            let mut retiring = self.shared.retiring.lock().expect("poisoned");
            while retiring.try_join_next().is_some() {}
            retiring.spawn(previous.task.finish(FINISH_PATIENCE));
            match medium {
                Medium::Video => self.shared.stats.clear_video(),
                Medium::Audio => self.shared.stats.clear_audio(),
            }
            debug!(parent: &self.shared.span, ?medium, "slot cleared");
        }
    }
}

impl Consume<moq_net::broadcast::Consumer> for LocalBroadcast {
    fn consume(&self) -> moq_net::broadcast::Consumer {
        Self::consume(self)
    }
}

#[cfg(test)]
mod tests;
