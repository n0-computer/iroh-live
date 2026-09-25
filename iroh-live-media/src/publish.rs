//! Publishing: a broadcast that exists apart from any transport.
//!
//! A [`LocalBroadcast`] holds a catalog, a media clock, one video slot and one
//! audio slot. A transport publishes it by reading it through
//! [`moq_net::Consume`]. [`RemoteBroadcast::local`](crate::RemoteBroadcast::local)
//! plays it in-process without a transport.
//!
//! The video slot opens its source once and encodes it into every rendition of
//! a ladder. Each rendition encodes only while someone watches it.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use moq_net::Consume;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info};

use self::status::{Medium, StatusCell};
pub use self::{
    encoding::{AudioEncoding, VideoEncoding, VideoRendition},
    status::{PublishStatus, RenditionState, SlotState},
};
use crate::{
    AudioSource, EncodedVideoSource, VideoSource,
    error::Error,
    stats::{PublishRecorder, PublishStats},
};

mod audio;
mod encoding;
pub(crate) mod status;
mod video;

/// A running slot task. Dropping it cancels and aborts the task.
#[derive(Debug)]
pub(crate) struct SlotTask {
    stop: CancellationToken,
    task: Option<AbortOnDropHandle<()>>,
}

impl SlotTask {
    /// Spawns `run` with a stop token that is a child of `closed`.
    fn spawn<F, Fut>(span: tracing::Span, closed: &CancellationToken, run: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let stop = closed.child_token();
        let task = n0_future::task::spawn(run(stop.clone()).instrument(span));
        Self {
            stop,
            task: Some(AbortOnDropHandle::new(task)),
        }
    }

    /// Stops the task and waits at most `patience` for it to finish.
    ///
    /// A task that finishes in time ends its tracks cleanly, so subscribers see
    /// an end instead of a reset. A slower task is aborted.
    pub(crate) async fn finish(mut self, patience: std::time::Duration) {
        self.stop.cancel();
        let Some(task) = self.task.take() else {
            return;
        };
        if tokio::time::timeout(patience, task).await.is_err() {
            debug!("a publish task did not finish in time and was aborted");
        }
    }
}

impl Drop for SlotTask {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// How long a replaced or closed publish task gets to finish its tracks.
const FINISH_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything a slot task needs.
struct Job {
    producer: moq_net::broadcast::Producer,
    catalog: moq_mux::catalog::Producer,
    clock: moq_mux::Clock,
    /// Held while the task owns its track names.
    tracks: Arc<tokio::sync::Mutex<()>>,
    stats: PublishRecorder,
    reporter: status::Reporter,
    /// The task this one replaces. It finishes before the track names are taken.
    predecessor: Option<SlotTask>,
}

impl Job {
    /// Finishes the predecessor, then locks the track names.
    ///
    /// Returns `None` once stopped.
    async fn take_over(
        &mut self,
        stop: &CancellationToken,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        if let Some(predecessor) = self.predecessor.take() {
            tokio::select! {
                () = predecessor.finish(FINISH_PATIENCE) => {}
                () = stop.cancelled() => return None,
            }
        }
        tokio::select! {
            guard = self.tracks.clone().lock_owned() => Some(guard),
            () = stop.cancelled() => None,
        }
    }
}

/// Maps a source's timestamps onto the broadcast clock.
///
/// It is anchored at the first frame, so the source keeps its own cadence. A
/// video ladder shares one, so every rendition carries the same timestamp for
/// the same picture.
#[derive(Debug, Clone, Copy)]
struct Rebase {
    /// Broadcast micros minus source micros.
    delta: i128,
}

impl Rebase {
    fn anchor(clock: moq_mux::Clock, first: moq_net::Timestamp) -> Self {
        Self {
            delta: clock.now().as_micros() as i128 - first.as_micros() as i128,
        }
    }

    fn map(self, timestamp: moq_net::Timestamp) -> moq_net::Timestamp {
        let micros = (timestamp.as_micros() as i128 + self.delta).max(0) as u64;
        moq_net::Timestamp::from_micros(micros).unwrap_or(timestamp)
    }
}

/// The task a slot is running, with its generation.
#[derive(Debug)]
struct Slot {
    generation: u64,
    task: SlotTask,
}

#[derive(derive_more::Debug)]
#[debug("Shared {{ status: {status:?} }}")]
struct Shared {
    producer: moq_net::broadcast::Producer,
    catalog: Mutex<moq_mux::catalog::Producer>,
    clock: moq_mux::Clock,
    video: Mutex<Option<Slot>>,
    audio: Mutex<Option<Slot>>,
    /// Held by a video task while it owns track names. A replacement waits on
    /// it until its predecessor's tracks are gone.
    video_tracks: Arc<tokio::sync::Mutex<()>>,
    /// The same for audio, whose track name does not change on replacement.
    audio_tracks: Arc<tokio::sync::Mutex<()>>,
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
    span: tracing::Span,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The last handle is gone without `close`. End the broadcast so
        // subscribers see it end instead of waiting on it.
        self.closed.cancel();
        if let Err(err) = self.catalog.lock().expect("poisoned").finish() {
            debug!(error = %err, "catalog did not finish cleanly");
        }
        self.producer.finish();
    }
}

/// A broadcast this process produces.
///
/// It holds a catalog, a media clock, one video slot and one audio slot. You
/// publish it by passing it to a transport such as `iroh-live` or
/// `iroh-live-rooms`, which reads it through `moq_net::Consume<broadcast::Consumer>`.
///
/// Cloning is cheap. [`close`](Self::close) ends the broadcast for every clone.
/// The setters spawn an encoding task, so call them from within a Tokio
/// runtime.
#[derive(Debug, Clone)]
pub struct LocalBroadcast {
    shared: Arc<Shared>,
}

impl Default for LocalBroadcast {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalBroadcast {
    /// Creates an empty broadcast, published nowhere yet.
    pub fn new() -> Self {
        let mut producer = moq_net::broadcast::Info::new().produce();
        // The catalog advertises the clock's wall mapping, so it is built with
        // the clock that stamps the media.
        let clock = moq_mux::Clock::new();
        let config = moq_mux::catalog::Config::default()
            .with_catalog(hang::catalog::Catalog::default())
            .with_clock(clock);
        let catalog = moq_mux::catalog::Producer::new(&mut producer, config)
            .expect("a new broadcast has no catalog track yet");
        let span = tracing::info_span!("broadcast");
        Self {
            shared: Arc::new(Shared {
                producer,
                catalog: Mutex::new(catalog),
                clock,
                video: Mutex::new(None),
                audio: Mutex::new(None),
                video_tracks: Default::default(),
                audio_tracks: Default::default(),
                generations: AtomicU64::new(0),
                status: StatusCell::default(),
                stats: PublishRecorder::default(),
                closed: CancellationToken::new(),
                finished: CancellationToken::new(),
                closer: Mutex::new(None),
                retiring: Mutex::new(n0_future::task::JoinSet::new()),
                span,
            }),
        }
    }

    /// Encodes `source` into every rendition of `encoding`, replacing the current video.
    ///
    /// Returns once the encoding task has started. Its progress shows in
    /// [`status`](Self::status). A replaced video gets a moment to finish its
    /// tracks before the new one takes their names.
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

    /// Returns the state of both slots and of every rendition.
    pub fn status(&self) -> n0_watcher::Direct<PublishStatus> {
        self.shared.status.watch()
    }

    /// Returns what the broadcast is sending, per rendition.
    pub fn stats(&self) -> PublishStats {
        self.shared.stats.snapshot()
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
                shared.producer.finish();
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
        let (lock, tracks, span) = match medium {
            Medium::Video => (
                &shared.video,
                &shared.video_tracks,
                tracing::info_span!(parent: &shared.span, "video"),
            ),
            Medium::Audio => (
                &shared.audio,
                &shared.audio_tracks,
                tracing::info_span!(parent: &shared.span, "audio"),
            ),
        };
        let mut slot = lock.lock().expect("poisoned");
        let generation = shared.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Job {
            producer: shared.producer.clone(),
            catalog: shared.catalog.lock().expect("poisoned").clone(),
            clock: shared.clock,
            tracks: tracks.clone(),
            stats: shared.stats.clone(),
            reporter: shared.status.begin(medium, generation, &names),
            predecessor: slot.take().map(|previous| previous.task),
        };
        let task = SlotTask::spawn(span, &shared.closed, |stop| run(job, stop));
        *slot = Some(Slot { generation, task });
    }

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
        self.shared.producer.consume()
    }
}

#[cfg(test)]
mod tests;
