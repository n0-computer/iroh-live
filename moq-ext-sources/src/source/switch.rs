//! A video source whose input the application swaps while it is published.
//!
//! [`VideoSource::switchable`] returns a source and its [`VideoSwitch`]. The
//! source is an ordinary [`VideoSource`] over a latest-wins slot, so a broadcast
//! encodes it once and a preview reads it, and neither sees a switch. The
//! switch forwards the frames of whichever source is current into that slot,
//! on one task per source, and passes the broadcast's demand back to it.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Instant,
};

use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{Demand, Driver, VideoFormat, VideoSource};
use crate::{ByPtr, Error, SlotState, frames::FrameSlot};

/// The source that feeds a switch, and which install it belongs to.
#[derive(Debug, Default)]
struct Current {
    /// Bumped by every install, so a superseded task leaves the slot alone.
    generation: u64,
    /// The source frames come from.
    ///
    /// Held here, not by the task, so a switch drops it at once and its device
    /// is free for the next open.
    source: Option<VideoSource>,
}

/// What a switch's forwarding tasks share with it.
#[derive(Debug)]
struct Shared {
    slot: FrameSlot,
    current: Mutex<Current>,
    status: watch::Sender<SlotState>,
    /// The demand on the switchable source, which the current source receives.
    demand: Demand,
    /// Cancelled once every handle to the switchable source is gone.
    stop: CancellationToken,
}

impl Shared {
    /// Runs `f` with the current source and status if install `generation` is still current.
    ///
    /// Returns `false` once a later install superseded it.
    fn if_current(
        &self,
        generation: u64,
        f: impl FnOnce(&mut Option<VideoSource>, &watch::Sender<SlotState>),
    ) -> bool {
        let mut current = self.current.lock().expect("poisoned");
        if current.generation != generation {
            return false;
        }
        f(&mut current.source, &self.status);
        true
    }
}

/// Swaps the input of a [`VideoSource::switchable`] source.
///
/// Each call supersedes the one before: [`replace`](Self::replace) and
/// [`clear`](Self::clear) take effect at once, and [`open`](Self::open)
/// cancels an open still running. The previous source is dropped before the
/// next one opens, so the same camera can open again. The switchable source
/// keeps its last frame while nothing feeds it, and ends when every switch is
/// dropped. Cheap to clone.
#[derive(Debug, Clone)]
pub struct VideoSwitch {
    shared: Arc<Shared>,
    task: Arc<Mutex<Option<AbortOnDropHandle<()>>>>,
}

impl VideoSource {
    /// Returns a source whose input the returned [`VideoSwitch`] swaps.
    ///
    /// Publish the source once, and switch cameras, screens or test patterns
    /// behind it without touching the broadcast: its encoders keep running, and
    /// its subscribers keep their tracks. A broadcast anchors each new
    /// source's timestamps at its clock again and starts a group there, so the
    /// new picture starts with a keyframe. `format` describes the frames the
    /// switch carries, as for [`push`](Self::push), and a frame of another size
    /// is scaled to the rendition's.
    ///
    /// The source reports the switch's state in [`VideoSwitch::status`] and
    /// starts [`Off`](SlotState::Off), with no frames until a source is set.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(format: moq_ext_sources::VideoFormat, size: moq_video::Size, rate: moq_video::FrameRate) -> Result<(), moq_ext_sources::Error> {
    /// use moq_ext_sources::VideoSource;
    ///
    /// let (switch, source) = VideoSource::switchable(format);
    /// // Hand `source` to a broadcast once, then switch behind it.
    /// switch.replace(VideoSource::test_pattern(size, rate)?);
    /// switch.open(VideoSource::capture(Default::default()));
    /// # let _ = source;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn switchable(format: VideoFormat) -> (VideoSwitch, Self) {
        let slot = FrameSlot::new();
        let stop = CancellationToken::new();
        let source = Self::new(
            "switch",
            format,
            slot.reader(),
            stop.clone(),
            Driver::Pushed,
        );
        let switch = VideoSwitch {
            shared: Arc::new(Shared {
                slot,
                current: Mutex::default(),
                status: watch::Sender::new(SlotState::Off),
                demand: source.inner.demand.clone(),
                stop,
            }),
            task: Default::default(),
        };
        (switch, source)
    }
}

impl VideoSwitch {
    /// Feeds the switchable source from `source`, from its current frame on.
    ///
    /// The status becomes [`Running`](SlotState::Running), and
    /// [`Ended`](SlotState::Ended) or [`Failed`](SlotState::Failed) when
    /// `source` stops.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn replace(&self, source: VideoSource) {
        info!(source = source.label(), "video switch: replaced");
        self.install(Some(source), SlotState::Running, forward);
    }

    /// Opens a source with `open` in the background and feeds the switchable source from it.
    ///
    /// The current source is dropped first, so `open` can open the same
    /// device again. The status reads [`Starting`](SlotState::Starting) while
    /// `open` runs, then [`Running`](SlotState::Running), or
    /// [`Failed`](SlotState::Failed) with its error. A later call to any
    /// method drops `open` unfinished, which for
    /// [`VideoSource::capture`] releases the device.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn open<F>(&self, open: F)
    where
        F: Future<Output = Result<VideoSource, Error>> + Send + 'static,
    {
        info!("video switch: opening a source");
        self.install(None, SlotState::Starting, |shared, generation| async move {
            let opened = tokio::select! {
                opened = open => opened,
                () = shared.stop.cancelled() => return,
            };
            match opened {
                Ok(source) => {
                    debug!(source = source.label(), "video switch: opened");
                    let installed = shared.if_current(generation, |current, status| {
                        *current = Some(source);
                        status.send_replace(SlotState::Running);
                    });
                    if installed {
                        forward(shared.clone(), generation).await;
                    }
                }
                Err(err) => {
                    warn!(error = format_args!("{err:#}"), "video switch: open failed");
                    shared.if_current(generation, |_, status| {
                        status.send_replace(SlotState::Failed(ByPtr::new(err)));
                    });
                }
            }
        });
    }

    /// Stops feeding the switchable source and drops the current source.
    ///
    /// The status becomes [`Off`](SlotState::Off). The switchable source keeps
    /// its last frame and does not end.
    pub fn clear(&self) {
        info!("video switch: cleared");
        let mut task = self.task.lock().expect("poisoned");
        drop(self.supersede(None, SlotState::Off).1);
        *task = None;
    }

    /// Returns a receiver of the switch's state.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<SlotState> {
        self.shared.status.subscribe()
    }

    /// Starts a new install with `source` and `state`, and returns its generation and the source it dropped.
    fn supersede(
        &self,
        source: Option<VideoSource>,
        state: SlotState,
    ) -> (u64, Option<VideoSource>) {
        let mut current = self.shared.current.lock().expect("poisoned");
        current.generation += 1;
        self.shared.status.send_replace(state);
        let previous = std::mem::replace(&mut current.source, source);
        (current.generation, previous)
    }

    /// Starts a new install with `source` and `state`, and spawns the task `run` returns for it.
    fn install<R, Fut>(&self, source: Option<VideoSource>, state: SlotState, run: R)
    where
        R: FnOnce(Arc<Shared>, u64) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut task = self.task.lock().expect("poisoned");
        let (generation, previous) = self.supersede(source, state);
        // Dropped before the next open runs, so its device is free.
        drop(previous);
        let future = run(self.shared.clone(), generation);
        *task = Some(AbortOnDropHandle::new(n0_future::task::spawn(
            future.instrument(info_span!("video_switch", generation)),
        )));
    }
}

/// Forwards the frames of install `generation`'s source into the switchable source.
///
/// Holds demand on the source while the switchable source has some. Returns
/// once the source ends, a later install supersedes it, or the switchable
/// source is gone.
async fn forward(shared: Arc<Shared>, generation: u64) {
    let mut frames = None;
    shared.if_current(generation, |current, _| {
        frames = current.as_ref().map(VideoSource::frames);
    });
    let Some(mut frames) = frames else {
        return;
    };
    let mut wanted = shared.demand.watch();
    let mut guard = None;
    loop {
        let want = *wanted.borrow_and_update();
        if want != guard.is_some() {
            shared.if_current(generation, |current, _| {
                guard = current.as_ref().filter(|_| want).map(VideoSource::want);
            });
        }
        tokio::select! {
            frame = frames.next() => {
                let Some(frame) = frame else { break };
                let at = frames.sent_at().unwrap_or_else(Instant::now);
                if !shared.if_current(generation, |_, _| shared.slot.send_at(frame, at)) {
                    return;
                }
            }
            changed = wanted.changed() => if changed.is_err() { return },
            () = shared.stop.cancelled() => {
                shared.if_current(generation, |current, _| *current = None);
                return;
            }
        }
    }
    drop(guard);
    shared.if_current(generation, |current, status| {
        let state = match current.take().and_then(|source| source.failure()) {
            Some(failure) => SlotState::Failed(failure.into()),
            None => SlotState::Ended,
        };
        debug!(?state, "video switch: the source stopped");
        status.send_replace(state);
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{FrameSender, video};

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn format() -> VideoFormat {
        VideoFormat {
            size: video::Size::new(2, 2),
            rate: video::FrameRate::new(30, 1).expect("a valid rate"),
        }
    }

    fn push(sender: &FrameSender<video::Frame>, micros: u64) {
        let surface = video::Surface::rgba(&[0; 16], format().size).expect("2x2");
        let timestamp = moq_net::Timestamp::from_micros(micros).expect("in range");
        sender
            .push(video::Frame::new(surface, timestamp))
            .expect("open");
    }

    async fn next_micros(frames: &mut crate::VideoFrames) -> u64 {
        tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("a frame in time")
            .expect("the stream is open")
            .timestamp
            .as_micros() as u64
    }

    async fn becomes(switch: &VideoSwitch, state: SlotState) {
        let mut status = switch.status();
        tokio::time::timeout(TIMEOUT, status.wait_for(|status| *status == state))
            .await
            .unwrap_or_else(|_| panic!("the switch never became {state:?}"))
            .expect("the switch is alive");
    }

    #[tokio::test]
    async fn a_replaced_source_takes_over_the_frames() {
        let (switch, source) = VideoSource::switchable(format());
        let mut frames = source.frames();
        assert_eq!(*switch.status().borrow(), SlotState::Off);

        let (first_sender, first) = VideoSource::push(format());
        switch.replace(first);
        assert_eq!(*switch.status().borrow(), SlotState::Running);
        push(&first_sender, 1);
        assert_eq!(next_micros(&mut frames).await, 1);

        let (second_sender, second) = VideoSource::push(format());
        switch.replace(second);
        assert!(first_sender.is_closed(), "the replaced source was dropped");
        push(&second_sender, 2);
        assert_eq!(next_micros(&mut frames).await, 2);
    }

    #[tokio::test]
    async fn a_forwarded_frame_keeps_its_send_instant() {
        let (switch, source) = VideoSource::switchable(format());
        let mut frames = source.frames();
        let (sender, inner) = VideoSource::push(format());
        let mut inner_frames = inner.frames();
        switch.replace(inner);
        push(&sender, 1);
        next_micros(&mut inner_frames).await;
        next_micros(&mut frames).await;
        assert_eq!(frames.sent_at(), inner_frames.sent_at());
    }

    #[tokio::test]
    async fn the_switchable_source_passes_its_demand_on() {
        let (switch, source) = VideoSource::switchable(format());
        let (sender, inner) = VideoSource::push(format());
        let mut demand = sender.demand();
        switch.replace(inner);
        let wanted = source.want();
        tokio::time::timeout(TIMEOUT, demand.wait_for(|wanted| *wanted))
            .await
            .expect("the demand reached the source")
            .expect("open");
        drop(wanted);
        tokio::time::timeout(TIMEOUT, demand.wait_for(|wanted| !*wanted))
            .await
            .expect("the demand left the source")
            .expect("open");
    }

    #[tokio::test]
    async fn an_open_reports_starting_then_running() {
        let (switch, source) = VideoSource::switchable(format());
        let mut frames = source.frames();
        let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();
        switch.open(async move { Ok(opened_rx.await.expect("sent")) });
        becomes(&switch, SlotState::Starting).await;
        let (sender, inner) = VideoSource::push(format());
        opened_tx.send(inner).expect("the open waits");
        becomes(&switch, SlotState::Running).await;
        push(&sender, 3);
        assert_eq!(next_micros(&mut frames).await, 3);
    }

    #[tokio::test]
    async fn a_failed_open_reports_why() {
        let (switch, _source) = VideoSource::switchable(format());
        switch.open(async { Err(Error::device_msg("no such camera")) });
        let mut status = switch.status();
        let state = tokio::time::timeout(
            TIMEOUT,
            status.wait_for(|state| matches!(state, SlotState::Failed(_))),
        )
        .await
        .expect("the open failed in time")
        .expect("alive")
        .clone();
        let SlotState::Failed(err) = state else {
            unreachable!()
        };
        assert!(matches!(*err, Error::Device { .. }));
    }

    #[tokio::test]
    async fn a_newer_switch_drops_an_open_still_running() {
        let (switch, _source) = VideoSource::switchable(format());
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel::<()>();
        switch.open(async move {
            let _dropped = dropped_tx;
            std::future::pending().await
        });
        becomes(&switch, SlotState::Starting).await;
        let (_sender, inner) = VideoSource::push(format());
        switch.replace(inner);
        assert_eq!(*switch.status().borrow(), SlotState::Running);
        tokio::time::timeout(TIMEOUT, dropped_rx)
            .await
            .expect("the open was dropped")
            .expect_err("the open never finished");
        switch.clear();
        assert_eq!(*switch.status().borrow(), SlotState::Off);
    }

    #[tokio::test]
    async fn a_source_that_ends_leaves_the_stream_open() {
        let (switch, source) = VideoSource::switchable(format());
        let mut frames = source.frames();
        let (sender, inner) = VideoSource::push(format());
        switch.replace(inner);
        push(&sender, 1);
        assert_eq!(next_micros(&mut frames).await, 1);
        drop(sender);
        becomes(&switch, SlotState::Ended).await;
        assert_eq!(
            frames.current().map(|frame| frame.timestamp.as_micros()),
            Some(1),
            "the last frame stays"
        );

        let (sender, inner) = VideoSource::push(format());
        switch.replace(inner);
        push(&sender, 2);
        assert_eq!(next_micros(&mut frames).await, 2);

        drop(switch);
        let ended =
            tokio::time::timeout(TIMEOUT, async { while frames.next().await.is_some() {} }).await;
        assert!(ended.is_ok(), "the stream outlived its switch");
        assert!(sender.is_closed(), "the last source was dropped");
    }
}
