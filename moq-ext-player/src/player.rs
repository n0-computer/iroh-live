//! Playback: one player over a remote broadcast.
//!
//! A [`Player`] owns its decoders, its playout clock, its rendition choice and
//! its statistics. Two views of one broadcast are two players, and they do not
//! interfere.
//!
//! Three tasks run underneath. The selector turns the rendition mode, the
//! catalog and the network into the rendition that should play. The video
//! supervisor keeps one decoder playing and at most one replacement warming
//! up, and hands over once the replacement has caught up
//! (`moq_ext_adaptive::Switcher`). The audio task decodes into the output and
//! reports how much it has buffered. The video waits on that figure.

use std::{sync::Arc, time::Duration};

use n0_future::task::AbortOnDropHandle;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{Instrument, debug_span, info, info_span};

use moq_ext_media_core::{ByPtr, Error, SlotState, send_if_changed, stats::Cell};

use crate::{
    RemoteBroadcast, SwitchError,
    catalog::VideoRenditions as _,
    frames::{FrameSlot, VideoFrames},
    stats::{FrameTiming, PlaybackStats, Timeline},
};

mod audio;
mod clock;
mod output;
mod select;
mod video_task;

pub use self::output::AudioTarget;

pub(crate) use self::clock::PlayoutClock;
pub use moq_ext_adaptive::Adaptation;

/// How to choose the video rendition.
///
/// The selector crate's [`Mode`](moq_ext_adaptive::Mode), under the player's name.
pub use moq_ext_adaptive::Mode as RenditionMode;

/// How far behind live to run.
///
/// The playout clock holds each picture for `min` past its arrival, plus the
/// audio queued at the speaker. Media later than `max` is skipped. `max` also
/// bounds what the transport keeps for the player (the moq subscription's
/// `delay.max`). Setting `min == max` gives a fixed latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// The jitter allowance every picture is held for.
    pub min: Duration,
    /// The oldest media still worth playing.
    pub max: Duration,
}

impl Latency {
    /// No buffer and no pacing: a frame presents as soon as it decodes.
    ///
    /// Media older than 150 ms is still skipped: a player without a buffer
    /// wants the live edge.
    pub const IMMEDIATE: Self = Self {
        min: Duration::ZERO,
        max: Duration::from_millis(150),
    };

    /// Returns whether frames are held for the clock.
    pub(crate) fn paced(&self) -> bool {
        !self.min.is_zero()
    }

    /// Fails if `min` is above `max`.
    fn validate(&self) -> Result<(), Error> {
        if self.min > self.max {
            Err(Error::invalid(format!(
                "a latency's minimum ({:?}) is above its maximum ({:?})",
                self.min, self.max
            )))
        } else {
            Ok(())
        }
    }
}

impl Default for Latency {
    /// Held for 100 ms, and skipped past 150 ms.
    fn default() -> Self {
        Self {
            min: Duration::from_millis(100),
            max: Duration::from_millis(150),
        }
    }
}

/// How a player plays.
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    /// How to choose the video rendition.
    pub rendition: RenditionMode,
    /// How far behind live to run.
    pub latency: Latency,
    /// Where audio plays. `None` does not subscribe to audio at all.
    pub audio: Option<AudioTarget>,
    /// The decoder backend to open, `Auto` by default.
    pub decoder: moq_video::decode::Kind,
    /// How automatic selection follows the link.
    pub adaptation: Adaptation,
    /// How long a replacement decoder has to take over before the switch is given up.
    ///
    /// It bounds every handover: automatic, pinned, or to another decoder. The
    /// replacement subscribes to the other rendition, waits for its next
    /// keyframe, and decodes until it catches up with the picture on screen.
    /// On a two second GOP over an impaired link, the keyframe alone takes
    /// seconds. The incumbent keeps playing either way. 15 s by default.
    pub switch_deadline: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            rendition: RenditionMode::default(),
            latency: Latency::default(),
            audio: None,
            decoder: moq_video::decode::Kind::default(),
            adaptation: Adaptation::default(),
            switch_deadline: Duration::from_secs(15),
        }
    }
}

/// The state of a player.
#[derive(Debug, Clone, Default, PartialEq)]
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
    pub switch_error: Option<ByPtr<Error>>,
    /// The decoder backend running, such as `vaapi`.
    pub decoder: Option<String>,
    /// The rendition whose failure `video` reports when it is `Failed`.
    ///
    /// A wait for another rendition must not get an error that is not its own.
    pub(crate) failed_rendition: Option<String>,
}

impl PlayerStatus {
    /// Sets the video slot to `state` with nothing on screen or on its way.
    pub(crate) fn clear_video(&mut self, state: SlotState) {
        self.video = state;
        self.rendition = None;
        self.switching_to = None;
        self.decoder = None;
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
    /// A newer request replaced the switch.
    Superseded,
    /// The request was withdrawn.
    Withdrawn,
    /// The decoder failed, or the switch timed out.
    Failed(Arc<Error>),
}

/// The writers behind [`PlaybackStats`].
#[derive(Debug, Clone, Default)]
pub(crate) struct PlaybackRecorder {
    pub(crate) video: Cell<Option<crate::stats::VideoPlaybackStats>>,
    pub(crate) audio: Cell<Option<crate::stats::AudioPlaybackStats>>,
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
    pub(crate) decoder: watch::Sender<moq_video::decode::Kind>,
    pub(crate) volume: watch::Sender<f32>,
}

/// One playback: video into [`VideoFrames`], audio into an [`AudioTarget`].
///
/// Dropping it aborts its tasks and returns at once: decoders stop at their
/// next await point and are not waited for, so a decoder thread may outlive
/// the player briefly. It is not `Clone`: a second view of the broadcast is a
/// second player.
#[derive(Debug)]
pub struct Player {
    broadcast: RemoteBroadcast,
    frames: VideoFrames,
    status: watch::Sender<PlayerStatus>,
    controls: Arc<Controls>,
    events: broadcast::Sender<SwitchEvent>,
    clock: PlayoutClock,
    stats: PlaybackRecorder,
    _tasks: Vec<AbortOnDropHandle<()>>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.clock.close();
    }
}

impl Player {
    /// Starts playing `broadcast`, for [`RemoteBroadcast::play`].
    pub(crate) fn start(broadcast: RemoteBroadcast, config: PlayerConfig) -> Result<Self, Error> {
        config.latency.validate()?;
        let span = info_span!(parent: broadcast.span(), "player");
        let status = watch::Sender::new(PlayerStatus {
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
        let clock = PlayoutClock::new(config.latency.min);
        let stats = PlaybackRecorder::default();
        let slot = FrameSlot::new();
        let frames = slot.frames();
        // The supervisor reports failed renditions, and the selector backs off
        // from them. A report that does not fit is for a failure already
        // backed off.
        let (reports_tx, reports_rx) = mpsc::channel(8);
        let (desired_tx, desired_rx) = watch::channel(None);
        // The exact target on screen. Once something plays, the selector stops
        // asking again for video that ended.
        let (playing_tx, playing_rx) = watch::channel(None);

        let mut tasks = Vec::new();
        tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
            select::run(select::Inputs {
                broadcast: broadcast.clone(),
                controls: controls.clone(),
                status: status.clone(),
                reports: reports_rx,
                playing: playing_rx,
                desired: desired_tx,
                clock: clock.clone(),
                adaptation: config.adaptation,
            })
            .instrument(debug_span!(parent: &span, "select")),
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
                switch_deadline: config.switch_deadline,
            })
            .instrument(info_span!(parent: &span, "video")),
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
                })
                .instrument(info_span!(parent: &span, "audio")),
            )));
        }
        info!(parent: &span, mode = ?config.rendition, latency = ?config.latency, "player started");

        Ok(Self {
            broadcast,
            frames,
            status,
            controls,
            events,
            clock,
            stats,
            _tasks: tasks,
        })
    }

    /// Returns the decoded frames.
    ///
    /// Every call returns a handle onto the same stream, and each handle keeps
    /// its own cursor. The stream survives rendition switches and decoder
    /// changes. It ends when the broadcast closes or the player is dropped.
    #[must_use]
    pub fn video(&self) -> VideoFrames {
        self.frames.clone()
    }

    /// Returns the broadcast this player plays.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Chooses how the video rendition is picked.
    pub fn set_rendition(&self, mode: RenditionMode) {
        send_if_changed(&self.status, |status| status.mode = mode.clone());
        self.controls.mode.send_replace(mode);
    }

    /// Changes how far behind live the player runs.
    ///
    /// The jitter allowance changes at once. A new maximum rebuilds the video
    /// decoder behind the picture, and the new decoder takes over once it has
    /// caught up. Audio keeps its old maximum until it next opens, because a
    /// rebuild would be audible.
    ///
    /// # Errors
    ///
    /// Fails if `latency.min` is above `latency.max`.
    pub fn set_latency(&self, latency: Latency) -> Result<(), Error> {
        latency.validate()?;
        self.clock.set_jitter(latency.min);
        self.controls.latency.send_replace(latency);
        Ok(())
    }

    /// Changes the video decoder backend.
    ///
    /// The replacement opens behind the picture and takes over once it has
    /// caught up. A backend that fails to open leaves the incumbent playing and
    /// says so in [`PlayerStatus::switch_error`]. Automatic rendition switches
    /// also open under the failing backend, so they fail until the next
    /// `set_decoder`.
    pub fn set_decoder(&self, decoder: moq_video::decode::Kind) {
        self.controls.decoder.send_replace(decoder);
    }

    /// Sets the audio volume, where 1.0 is unattenuated.
    pub fn set_volume(&self, volume: f32) {
        self.controls.volume.send_replace(volume.max(0.0));
    }

    /// Returns the player's state.
    ///
    /// A borrow of the receiver blocks the player's writes, so keep it short
    /// and do not hold it across a call into the player.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<PlayerStatus> {
        self.status.subscribe()
    }

    /// Returns the player's playback statistics.
    #[must_use]
    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            video: self.stats.video.get(),
            audio: self.stats.audio.get(),
            latency: self.clock.latency(),
            network: self.broadcast.network().map(|signals| signals()),
        }
    }

    /// Returns the last presented video and audio frames, in presentation order.
    ///
    /// Keeps a few hundred frames of each medium, about ten seconds at usual
    /// rates. It is meant for a debugging view of pacing and A/V sync. Read it
    /// once per redraw of that view, not per frame.
    #[must_use]
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
    /// The wait follows whatever the rendition mode decides meanwhile. Bound it
    /// with `tokio::time::timeout`.
    ///
    /// Cancellation safe: dropping the future leaves the switch running.
    ///
    /// # Errors
    ///
    /// Fails when a switch to `name` is superseded, withdrawn or fails, when
    /// the catalog has no such rendition, when the video failed with nothing
    /// left playing, or when the player's video ended.
    pub async fn wait_for_rendition(&self, name: &str) -> Result<(), SwitchError> {
        let mut events = self.events.subscribe();
        let mut status = self.status.subscribe();
        let mut catalog = self.broadcast.catalog();
        loop {
            let current = status.borrow_and_update().clone();
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
                // failed. Report a failure, whether the status or the event
                // arrives first.
                SlotState::Failed(source)
                    if current.switching_to.is_none()
                        && current.failed_rendition.as_deref() == Some(name) =>
                {
                    return Err(n0_error::e!(SwitchError::Failed {
                        rendition: name.to_string(),
                        source: source.0.clone(),
                    }));
                }
                _ => {}
            }
            if let Some(known) = catalog.borrow_and_update().clone()
                && let Err(err) = known.video_rendition(name)
            {
                return Err(n0_error::e!(SwitchError::Failed {
                    rendition: name.to_string(),
                    source: Arc::new(err),
                }));
            }
            tokio::select! {
                event = events.recv() => match event {
                    Ok(SwitchEvent::Landed(landed)) if landed == name => return Ok(()),
                    // A switch to `name` under a new decoder configuration is
                    // still a switch to `name`.
                    Ok(SwitchEvent::Abandoned(rendition, Abandon::Superseded))
                        if rendition == name
                            && status.borrow().switching_to.as_deref() == Some(name) => {}
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
                changed = status.changed() => {
                    if changed.is_err() {
                        return Err(n0_error::e!(SwitchError::Ended));
                    }
                }
                changed = catalog.changed() => {
                    if changed.is_err() {
                        return Err(n0_error::e!(SwitchError::Ended));
                    }
                }
            }
        }
    }
}
