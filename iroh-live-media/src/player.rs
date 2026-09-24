//! Playback: one player over a remote broadcast.
//!
//! A [`Player`] owns everything mutable about one playback: its decoders, its
//! playout clock, its rendition choice and its statistics. Two views of one
//! broadcast are two players, and cannot interfere.
//!
//! Underneath, three tasks cooperate. The selector turns the rendition mode,
//! the catalog and the network into the rendition that should play. The video
//! supervisor keeps one decoder playing and at most one replacement warming
//! up, and hands over once the replacement has caught up
//! ([`switch`](self::switch)). The audio task decodes into the output and
//! reports how much it has buffered, which is what the video waits on.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher as _};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::{
    AudioOutput, NetworkSample, RemoteBroadcast, SlotState,
    error::{Error, SwitchError},
    frames::{FrameSlot, VideoFrames},
    stats::{Cell, FrameTiming, PlaybackStats, Timeline},
    video,
};

mod audio;
pub(crate) mod bound;
mod clock;
mod select;
pub(crate) mod switch;
mod video_task;

pub(crate) use self::clock::PlayoutClock;

/// How to choose the video rendition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenditionMode {
    /// Follow the link, never above `max_height` if set (a grid tile).
    ///
    /// Without network signals on the broadcast, holds the best rendition that
    /// fits.
    Auto {
        /// The tallest rendition to choose, in pixels.
        max_height: Option<u32>,
    },
    /// Hold one rendition.
    ///
    /// If it cannot be played, plays as [`Auto`](Self::Auto) would, says why
    /// in [`PlayerStatus::switch_error`], and returns to it when it becomes
    /// available.
    Pinned(String),
    /// Unsubscribe video; audio keeps playing (a tile scrolled off screen).
    Off,
}

impl Default for RenditionMode {
    /// Automatic selection with no height limit.
    fn default() -> Self {
        Self::auto()
    }
}

impl RenditionMode {
    /// Returns automatic selection with no height limit.
    pub const fn auto() -> Self {
        Self::Auto { max_height: None }
    }

    /// Returns automatic selection that never goes above `max_height`.
    pub const fn auto_below(max_height: u32) -> Self {
        Self::Auto {
            max_height: Some(max_height),
        }
    }

    /// Returns a mode holding the rendition `name`.
    pub fn pinned(name: impl Into<String>) -> Self {
        Self::Pinned(name.into())
    }
}

/// How far behind live to run.
///
/// The playout clock holds each picture for `min` past its arrival, plus
/// whatever audio is queued at the speaker, and media later than `max` is
/// skipped rather than played; `max` also bounds what the transport keeps for
/// the player (moq `max_age`). `min == max` is a fixed latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Latency {
    /// The jitter allowance every picture is held for.
    pub min: Duration,
    /// The oldest media still worth playing.
    pub max: Duration,
}

impl Latency {
    /// Returns a fixed latency: held for `latency`, skipped past it.
    pub const fn fixed(latency: Duration) -> Self {
        Self {
            min: latency,
            max: latency,
        }
    }

    /// Returns a latency the playout may run anywhere between `min` and `max`.
    pub const fn range(min: Duration, max: Duration) -> Self {
        Self { min, max }
    }

    /// No buffer and no pacing: a frame presents as soon as it decodes.
    ///
    /// Media older than 150 ms is still skipped, since a player with no
    /// buffer is one that wants the live edge.
    pub const IMMEDIATE: Self = Self {
        min: Duration::ZERO,
        max: Duration::from_millis(150),
    };

    /// Whether frames are held for the clock at all.
    pub(crate) fn paced(&self) -> bool {
        !self.min.is_zero()
    }
}

impl Default for Latency {
    /// Held for 100 ms, and skipped past 150 ms.
    fn default() -> Self {
        Self::range(Duration::from_millis(100), Duration::from_millis(150))
    }
}

/// How a player plays.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PlayerConfig {
    /// How to choose the video rendition.
    pub rendition: RenditionMode,
    /// How far behind live to run.
    pub latency: Latency,
    /// Where audio plays. `None` does not subscribe to audio at all.
    pub audio: Option<AudioOutput>,
    /// Which decoder backend to open; upstream's `Kind`, `Auto` by default.
    pub decoder: video::decode::Kind,
    /// The adaptation thresholds and timers, the production ones unless a
    /// test set others through `with_tuning`.
    pub(crate) tuning: bound::Tuning,
}

impl PlayerConfig {
    /// Returns the config with a rendition mode.
    #[must_use]
    pub fn with_rendition(mut self, mode: RenditionMode) -> Self {
        self.rendition = mode;
        self
    }

    /// Returns the config with a latency.
    #[must_use]
    pub fn with_latency(mut self, latency: Latency) -> Self {
        self.latency = latency;
        self
    }

    /// Returns the config with audio playing through `output`.
    #[must_use]
    pub fn with_audio(mut self, output: &AudioOutput) -> Self {
        self.audio = Some(output.clone());
        self
    }

    /// Returns the config with a decoder backend.
    #[must_use]
    pub fn with_decoder(mut self, decoder: video::decode::Kind) -> Self {
        self.decoder = decoder;
        self
    }

    /// Returns the config with the adaptation thresholds and timers `tuning`.
    ///
    /// For tests that cannot wait out the production timers; see
    /// [`test_util`](crate::test_util). Behind the `test-util` feature, which
    /// no application should enable.
    #[cfg(feature = "test-util")]
    #[must_use]
    pub fn with_tuning(mut self, tuning: crate::test_util::Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    fn validate(&self) -> Result<(), Error> {
        validate_latency(&self.latency)
    }
}

fn validate_latency(latency: &Latency) -> Result<(), Error> {
    match latency.min > latency.max {
        true => Err(Error::invalid(format!(
            "a latency's minimum ({:?}) is above its maximum ({:?})",
            latency.min, latency.max
        ))),
        false => Ok(()),
    }
}

/// The state of a player.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PlayerStatus {
    /// The video slot.
    pub video: SlotState,
    /// The audio slot.
    pub audio: SlotState,
    /// The rendition mode asked for.
    pub mode: RenditionMode,
    /// The rendition on screen.
    pub rendition: Option<String>,
    /// A rendition whose decoder is warming up to take over.
    pub switching_to: Option<String>,
    /// Why the last switch or pin could not be honoured.
    pub switch_error: Option<Arc<Error>>,
    /// The decoder backend running, such as `vaapi`.
    pub decoder: Option<String>,
    /// The rendition whose failure `video` reports, when it is `Failed`, so a
    /// wait for another rendition is not handed an error that is not its own.
    pub(crate) failed_rendition: Option<String>,
}

impl PartialEq for PlayerStatus {
    fn eq(&self, other: &Self) -> bool {
        self.video == other.video
            && self.audio == other.audio
            && self.mode == other.mode
            && self.rendition == other.rendition
            && self.switching_to == other.switching_to
            && self.decoder == other.decoder
            && self.failed_rendition == other.failed_rendition
            && match (&self.switch_error, &other.switch_error) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for PlayerStatus {}

/// The player's status, written by its tasks.
#[derive(Debug, Clone)]
pub(crate) struct StatusCell {
    value: Arc<Mutex<PlayerStatus>>,
    watch: Watchable<PlayerStatus>,
}

impl StatusCell {
    fn new(status: PlayerStatus) -> Self {
        Self {
            watch: Watchable::new(status.clone()),
            value: Arc::new(Mutex::new(status)),
        }
    }

    /// Changes the status in place and publishes the result.
    pub(crate) fn update(&self, f: impl FnOnce(&mut PlayerStatus)) {
        let mut value = self.value.lock().expect("poisoned");
        f(&mut value);
        self.watch.set(value.clone()).ok();
    }

    /// Returns the current status.
    pub(crate) fn get(&self) -> PlayerStatus {
        self.value.lock().expect("poisoned").clone()
    }
}

/// How a switch to a rendition ended, for [`Player::wait_for_rendition`].
#[derive(Debug, Clone)]
pub(crate) enum SwitchEvent {
    /// The rendition took over.
    Landed(String),
    /// A switch to the rendition was given up.
    Abandoned(String, Abandon),
}

/// Why a switch was given up.
#[derive(Debug, Clone)]
pub(crate) enum Abandon {
    Superseded,
    Withdrawn,
    Failed(Arc<Error>),
}

/// The writers behind [`PlaybackStats`].
#[derive(Debug, Clone, Default)]
pub(crate) struct PlaybackRecorder {
    pub(crate) video: Cell<Option<crate::stats::VideoPlaybackStats>>,
    pub(crate) audio: Cell<Option<crate::stats::AudioPlaybackStats>>,
    pub(crate) network: Cell<Option<NetworkSample>>,
    /// Written by the video task as it presents pictures.
    pub(crate) video_timeline: Timeline,
    /// Written by the audio task as it writes to the output.
    pub(crate) audio_timeline: Timeline,
}

/// What the player's controls ask for, shared with its tasks.
#[derive(Debug)]
pub(crate) struct Controls {
    pub(crate) mode: watch::Sender<RenditionMode>,
    pub(crate) latency: watch::Sender<Latency>,
    pub(crate) decoder: watch::Sender<video::decode::Kind>,
    pub(crate) volume: watch::Sender<f32>,
}

/// One playback: video into [`VideoFrames`], audio into an
/// [`AudioOutput`].
///
/// Dropping it stops its decoders. Not `Clone`: the player is the owner of
/// its playback, and a second view of the broadcast is a second player.
#[derive(Debug)]
pub struct Player {
    broadcast: RemoteBroadcast,
    frames: VideoFrames,
    status: StatusCell,
    controls: Arc<Controls>,
    events: broadcast::Sender<SwitchEvent>,
    clock: PlayoutClock,
    stats: PlaybackRecorder,
    shutdown: CancellationToken,
    _tasks: Vec<AbortOnDropHandle<()>>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.clock.close();
    }
}

impl Player {
    /// Starts playing `broadcast`. Called by
    /// [`RemoteBroadcast::play`](crate::RemoteBroadcast::play).
    pub(crate) fn start(broadcast: RemoteBroadcast, config: PlayerConfig) -> Result<Self, Error> {
        config.validate()?;
        let span = tracing::info_span!(parent: broadcast.span(), "player");
        let status = StatusCell::new(PlayerStatus {
            mode: config.rendition.clone(),
            video: match config.rendition {
                RenditionMode::Off => SlotState::Off,
                _ => SlotState::Starting,
            },
            audio: match config.audio {
                Some(_) => SlotState::Starting,
                None => SlotState::Off,
            },
            ..PlayerStatus::default()
        });
        let controls = Arc::new(Controls {
            mode: watch::Sender::new(config.rendition.clone()),
            latency: watch::Sender::new(config.latency),
            decoder: watch::Sender::new(config.decoder.clone()),
            volume: watch::Sender::new(1.0),
        });
        let (events, _) = broadcast::channel(16);
        let clock = PlayoutClock::with_jitter(config.latency.min);
        let stats = PlaybackRecorder::default();
        let shutdown = CancellationToken::new();
        let slot = FrameSlot::new();
        let frames = slot.frames();
        // The supervisor tells the selector which renditions failed, so they
        // are left alone for a while. Bounded: a report that does not fit is a
        // report of a failure already being backed off.
        let (reports_tx, reports_rx) = mpsc::channel(8);
        let (desired_tx, desired_rx) = watch::channel(None);
        // The target on screen, exactly: the selector falls back to its
        // decoder configuration when a change of decoder fails.
        let (playing_tx, playing_rx) = watch::channel(None);

        let mut tasks = Vec::new();
        tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
            select::run(select::Inputs {
                broadcast: broadcast.clone(),
                controls: controls.clone(),
                status: status.clone(),
                stats: stats.clone(),
                reports: reports_rx,
                playing: playing_rx,
                desired: desired_tx,
                clock: clock.clone(),
                tuning: config.tuning.clone(),
                shutdown: shutdown.clone(),
            })
            .instrument(tracing::debug_span!(parent: &span, "select")),
        )));
        tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
            video_task::run(video_task::Inputs {
                desired: desired_rx,
                frames: slot,
                controls: controls.clone(),
                status: status.clone(),
                events: events.clone(),
                reports: reports_tx,
                playing: playing_tx,
                clock: clock.clone(),
                stats: stats.clone(),
                switch_deadline: config.tuning.switch_deadline,
                shutdown: shutdown.clone(),
            })
            .instrument(tracing::info_span!(parent: &span, "video")),
        )));
        if let Some(output) = config.audio {
            tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
                audio::run(audio::Inputs {
                    broadcast: broadcast.clone(),
                    output,
                    controls: controls.clone(),
                    status: status.clone(),
                    clock: clock.clone(),
                    stats: stats.clone(),
                    shutdown: shutdown.clone(),
                })
                .instrument(tracing::info_span!(parent: &span, "audio")),
            )));
        }
        tracing::info!(parent: &span, mode = ?config.rendition, latency = ?config.latency, "player started");

        Ok(Self {
            broadcast,
            frames,
            status,
            controls,
            events,
            clock,
            stats,
            shutdown,
            _tasks: tasks,
        })
    }

    /// Returns the decoded frames.
    ///
    /// Every call returns a handle onto the same stream, which survives
    /// rendition switches and decoder changes. Each handle keeps its own
    /// cursor. The stream ends once the broadcast closes, or the player is
    /// dropped.
    pub fn video(&self) -> VideoFrames {
        self.frames.clone()
    }

    /// Returns the broadcast this player plays.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Chooses how the video rendition is picked.
    pub fn set_rendition(&self, mode: RenditionMode) {
        self.status.update(|status| status.mode = mode.clone());
        self.controls.mode.send_replace(mode);
    }

    /// Changes how far behind live the player runs.
    ///
    /// The jitter allowance moves at once. A changed maximum rebuilds the
    /// video decoder behind the picture, which takes over once it has caught
    /// up, so nothing goes blank. Audio keeps the maximum it opened with until
    /// it next opens, since rebuilding it would be heard.
    ///
    /// # Errors
    ///
    /// Fails if `latency.min` is above `latency.max`.
    pub fn set_latency(&self, latency: Latency) -> Result<(), Error> {
        validate_latency(&latency)?;
        self.clock.set_jitter(latency.min);
        self.controls.latency.send_replace(latency);
        Ok(())
    }

    /// Changes the video decoder backend.
    ///
    /// The replacement opens behind the picture and takes over once it has
    /// caught up. A backend that fails to open leaves the incumbent playing and
    /// says so in [`PlayerStatus::switch_error`].
    pub fn set_decoder(&self, decoder: video::decode::Kind) {
        self.controls.decoder.send_replace(decoder);
    }

    /// Sets the audio volume, where 1.0 is unattenuated.
    pub fn set_volume(&self, volume: f32) {
        self.controls.volume.send_replace(volume.max(0.0));
    }

    /// Returns a watcher over the player's state.
    pub fn status(&self) -> n0_watcher::Direct<PlayerStatus> {
        self.status.watch.watch()
    }

    /// Returns what the player is playing.
    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            video: self.stats.video.get(),
            audio: self.stats.audio.get(),
            latency: self.clock.latency(),
            network: self.stats.network.get(),
        }
    }

    /// Returns the last frames presented, video and audio together, ordered
    /// by when they were presented.
    ///
    /// Keeps a few hundred frames of each medium, about ten seconds at usual
    /// rates. For a debugging view of pacing and A/V sync: read it as often as
    /// such a view draws, not per frame.
    pub fn timeline(&self) -> Vec<FrameTiming> {
        let mut frames: Vec<FrameTiming> = self
            .stats
            .video_timeline
            .snapshot()
            .into_iter()
            .chain(self.stats.audio_timeline.snapshot())
            .collect();
        frames.sort_by_key(|timing| timing.presented);
        frames
    }

    /// Waits until `name` is on screen.
    ///
    /// Waits through whatever the rendition mode decides meanwhile: it resolves
    /// once `name` plays, and fails once a switch to `name` was superseded,
    /// withdrawn or failed, once the catalog shows no such rendition, once the
    /// video failed with nothing left playing, or once the player's video
    /// ended. Callers bound the wait with `tokio::time::timeout`.
    ///
    /// Cancellation safe: dropping the future leaves the switch running.
    ///
    /// # Errors
    ///
    /// Returns the [`SwitchError`] that ended the wait.
    pub async fn wait_for_rendition(&self, name: &str) -> Result<(), SwitchError> {
        let mut events = self.events.subscribe();
        let mut status = self.status.watch.watch();
        let mut catalog = self.broadcast.catalog();
        loop {
            let current = status.get();
            if current.rendition.as_deref() == Some(name) {
                return Ok(());
            }
            if current.mode == RenditionMode::Off {
                return Err(n0_error::e!(SwitchError::Withdrawn {
                    rendition: name.to_string()
                }));
            }
            match &current.video {
                SlotState::Ended => return Err(n0_error::e!(SwitchError::Ended)),
                // Nothing on screen and nothing on its way: the last switch
                // failed, which the waiter hears as such rather than as an end,
                // whichever of the status and the event it sees first.
                SlotState::Failed(source)
                    if current.switching_to.is_none()
                        && current.failed_rendition.as_deref() == Some(name) =>
                {
                    return Err(n0_error::e!(SwitchError::Failed {
                        rendition: name.to_string(),
                        source: source.clone(),
                    }));
                }
                _ => {}
            }
            if let Some(known) = catalog.get()
                && known.video_rendition(name).is_none()
            {
                return Err(n0_error::e!(SwitchError::UnknownRendition {
                    rendition: name.to_string()
                }));
            }
            tokio::select! {
                event = events.recv() => match event {
                    Ok(SwitchEvent::Landed(landed)) if landed == name => return Ok(()),
                    // Superseded by a switch to the same rendition under a new
                    // decoder configuration, which is still a switch to `name`.
                    Ok(SwitchEvent::Abandoned(rendition, Abandon::Superseded))
                        if rendition == name
                            && status.get().switching_to.as_deref() == Some(name) => {}
                    Ok(SwitchEvent::Abandoned(rendition, reason)) if rendition == name => {
                        let rendition = rendition.clone();
                        return Err(match reason {
                            Abandon::Superseded => n0_error::e!(SwitchError::Superseded { rendition }),
                            Abandon::Withdrawn => n0_error::e!(SwitchError::Withdrawn { rendition }),
                            Abandon::Failed(source) => n0_error::e!(SwitchError::Failed { rendition, source }),
                        });
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(n0_error::e!(SwitchError::Ended));
                    }
                },
                updated = status.updated() => {
                    if updated.is_err() {
                        return Err(n0_error::e!(SwitchError::Ended));
                    }
                }
                updated = catalog.updated() => {
                    if updated.is_err() {
                        return Err(n0_error::e!(SwitchError::Ended));
                    }
                }
            }
        }
    }
}
