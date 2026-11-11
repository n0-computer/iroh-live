//! Playback: one player over a remote broadcast.
//!
//! A [`Player`] owns its decoders, its playout clock, its rendition choice and
//! its statistics, so two players of one broadcast do not interfere.
//!
//! Three tasks run underneath. The selector turns the rendition mode, the
//! catalog and the network into the rendition that should play. The video
//! supervisor keeps one decoder playing and at most one replacement warming
//! up, and hands over once the replacement has caught up
//! (`moq_ext_adaptive::Switcher`). The audio task decodes into the output and
//! reports how much it has buffered. The video waits on that figure.

use std::{sync::Arc, time::Duration};

use moq_ext_media_core::{ByPtr, Error, SlotState, send_if_changed, stats::Cell};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{Instrument, debug_span, info, info_span};

use crate::{
    RemoteBroadcast,
    frames::{FrameSlot, VideoFrames},
    stats::{AudioPlaybackStats, FrameLog, FrameTiming, PlaybackStats, VideoPlaybackStats},
};

mod arrivals;
mod audio;
mod clock;
mod output;
mod select;
mod video_task;

pub use moq_ext_adaptive::{Adaptation, RenditionMode};

pub(crate) use self::clock::PlayoutClock;
pub use self::{
    audio::{AudioFrame, AudioFrames},
    output::AudioTarget,
};

/// How far behind live to run.
///
/// The playout clock runs `min` behind the live edge, or the publisher's
/// advertised jitter if that is larger, up to `max`. While audio plays,
/// pictures show when their sound does. Media later than `max` is skipped.
/// Both reach the transport as the moq subscription's [`Delay`], so relays and
/// the publisher see them too. Setting `min == max` gives a fixed latency.
///
/// [`Delay`]: moq_net::track::Delay
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// How far behind the live edge playback runs.
    pub min: Duration,
    /// The oldest media still worth playing.
    pub max: Duration,
}

/// How far below [`Latency::max`] a publisher's jitter may raise the playout delay: a frame at 30 fps.
const JITTER_HEADROOM: Duration = Duration::from_millis(33);

impl Latency {
    /// No buffer and no pacing: a frame presents as soon as it decodes.
    ///
    /// Media older than 150 ms is still skipped: a player without a buffer
    /// wants the live edge.
    pub const IMMEDIATE: Self = Self {
        min: Duration::ZERO,
        max: Duration::from_millis(150),
    };

    /// Reports whether frames are held for the clock.
    pub(crate) fn paced(&self) -> bool {
        !self.min.is_zero()
    }

    /// Returns the playout clock's delay, given the publisher's `jitter`.
    ///
    /// A publisher that flushes frames up to `jitter` late needs a buffer that
    /// deep. The jitter raises the delay to at most [`JITTER_HEADROOM`] below
    /// `max`, the age at which the transport skips media, so a frame that
    /// arrives a little late is not past `max` before it is due. It never
    /// lowers the delay below `min`. An unpaced latency ignores `jitter`.
    pub(crate) fn playout_delay(&self, jitter: Option<Duration>) -> Duration {
        match jitter {
            Some(jitter) if self.paced() => {
                let ceiling = self.max.saturating_sub(JITTER_HEADROOM);
                self.min.max(jitter.min(ceiling))
            }
            _ => self.min,
        }
    }

    /// Returns an [`Error::InvalidConfig`] if `min` is above `max`.
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

impl From<Latency> for moq_net::track::Delay {
    /// Asks the subscription for the whole latency range.
    ///
    /// `min` is the target, and `max` the oldest media still worth delivering.
    fn from(latency: Latency) -> Self {
        Self::default().with_min(latency.min).with_max(latency.max)
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
    /// It bounds every handover: automatic, pinned, or to another decoder. On
    /// an impaired link the wait for the next keyframe alone takes seconds, and
    /// the incumbent keeps playing meanwhile. 15 s by default.
    pub switch_deadline: Duration,
    /// Whether to record when each video frame arrives.
    ///
    /// The arrivals fill
    /// [`FrameTiming::arrived`](crate::stats::FrameTiming::arrived).
    ///
    /// The record reads every video track a second time, beside its decoder.
    /// Off by default, which leaves `arrived` at `None`.
    pub record_arrivals: bool,
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
            record_arrivals: false,
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

/// The writers behind [`PlaybackStats`].
#[derive(Debug, Clone, Default)]
pub(crate) struct PlaybackRecorder {
    pub(crate) video: Cell<Option<VideoPlaybackStats>>,
    pub(crate) audio: Cell<Option<AudioPlaybackStats>>,
    /// Written by the video task as it presents pictures.
    pub(crate) video_log: FrameLog,
    /// Written by the audio task as it writes to the output.
    pub(crate) audio_log: FrameLog,
}

/// What the player's controls ask for, shared with its tasks.
#[derive(Debug)]
pub(crate) struct Controls {
    pub(crate) mode: watch::Sender<RenditionMode>,
    pub(crate) latency: Latency,
    pub(crate) decoder: watch::Sender<moq_video::decode::Kind>,
    pub(crate) volume: watch::Sender<f32>,
}

/// One playback: video into [`VideoFrames`], audio into an [`AudioTarget`] and [`AudioFrames`].
///
/// Dropping it aborts its tasks and returns at once: decoders stop at their
/// next await point and are not waited for, so a decoder thread may outlive
/// the player briefly. A second view of the broadcast is a second player.
#[derive(Debug)]
pub struct Player {
    broadcast: RemoteBroadcast,
    frames: VideoFrames,
    audio_frames: broadcast::Sender<AudioFrame>,
    status: watch::Sender<PlayerStatus>,
    controls: Arc<Controls>,
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
            latency: config.latency,
            decoder: watch::Sender::new(config.decoder.clone()),
            volume: watch::Sender::new(1.0),
        });
        let (audio_frames, _) = broadcast::channel(audio::AUDIO_FRAMES_BACKLOG);
        let clock = PlayoutClock::new(config.latency.min);
        let stats = PlaybackRecorder::default();
        let slot = FrameSlot::new();
        let frames = slot.frames();
        // The supervisor reports failed and ended renditions, and the selector
        // backs off from them. Only a selector eight reports behind fills the
        // channel, and a report dropped then costs at most one early retry.
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
                record_arrivals: config.record_arrivals,
            })
            .instrument(debug_span!(parent: &span, "select")),
        )));
        tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
            video_task::run(video_task::Inputs {
                desired: desired_rx,
                frames: slot,
                controls: controls.clone(),
                status: status.clone(),
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
                    frames: audio_frames.clone(),
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
            audio_frames,
            status,
            controls,
            clock,
            stats,
            _tasks: tasks,
        })
    }

    /// Returns the decoded video frames.
    ///
    /// Every call returns a handle onto the same stream, and each handle keeps
    /// its own cursor. The stream survives rendition switches and decoder
    /// changes. It ends when the broadcast closes or the player is dropped.
    #[must_use]
    pub fn video_frames(&self) -> VideoFrames {
        self.frames.clone()
    }

    /// Returns the decoded audio, as PCM with its timestamps.
    ///
    /// The frames are the ones the player writes to its [`AudioTarget`], so
    /// they flow only while [`PlayerConfig::audio`] is set. Pass
    /// [`AudioTarget::null`] to decode the audio without playing it, for
    /// analysis. Each call returns a new handle that starts at the next decoded
    /// frame and ends when the player is dropped.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(broadcast: moq_ext_player::RemoteBroadcast) -> Result<(), moq_ext_player::Error> {
    /// use moq_ext_player::{AudioTarget, PlayerConfig};
    ///
    /// let player = broadcast.play(PlayerConfig {
    ///     audio: Some(AudioTarget::null()),
    ///     ..PlayerConfig::default()
    /// })?;
    /// let mut audio = player.audio_frames();
    /// while let Some(frame) = audio.next().await {
    ///     let peak = frame.samples.iter().fold(0f32, |peak, s| peak.max(s.abs()));
    ///     println!("{:?}: peak {peak}", frame.timestamp);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn audio_frames(&self) -> AudioFrames {
        AudioFrames::new(&self.audio_frames)
    }

    /// Returns the broadcast this player plays.
    #[must_use]
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.broadcast
    }

    /// Chooses how the video rendition is picked.
    pub fn set_rendition(&self, mode: RenditionMode) {
        send_if_changed(&self.status, |status| status.mode = mode.clone());
        self.controls.mode.send_replace(mode);
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
            .video_log
            .snapshot()
            .into_iter()
            .chain(self.stats.audio_log.snapshot())
            .collect();
        frames.sort_by_key(|timing| timing.presented);
        frames
    }

    /// Returns the timing of the presented picture at `pts`, or `None` once it left the timeline.
    ///
    /// `pts` is the `timestamp` of a frame from [`video_frames`](Self::video_frames). Its
    /// `arrived`, `decoded` and `presented` tell the time on the network from
    /// the time in the decoder and in the playout clock. Look a frame up soon
    /// after it is handed out.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run(player: moq_ext_player::Player) {
    /// // The player was started with `PlayerConfig::record_arrivals` on.
    /// let mut frames = player.video_frames();
    /// while let Some(frame) = frames.next().await {
    ///     let timing = player.video_timing(frame.timestamp.into());
    ///     if let Some(arrived) = timing.and_then(|timing| timing.arrived) {
    ///         println!("{:?} since its data arrived", arrived.elapsed());
    ///     }
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn video_timing(&self, pts: Duration) -> Option<FrameTiming> {
        self.stats
            .video_log
            .snapshot()
            .into_iter()
            .rev()
            .find(|timing| timing.pts == pts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn a_latency_becomes_the_whole_delay() {
        let delay = moq_net::track::Delay::from(Latency {
            min: ms(100),
            max: ms(400),
        });
        assert_eq!(delay.min, Some(ms(100)));
        assert_eq!(delay.max, Some(ms(400)));
    }

    #[test]
    fn the_playout_delay_follows_a_larger_jitter_up_to_the_maximum() {
        let latency = Latency {
            min: ms(100),
            max: ms(400),
        };
        assert_eq!(latency.playout_delay(None), ms(100));
        assert_eq!(latency.playout_delay(Some(ms(40))), ms(100));
        assert_eq!(latency.playout_delay(Some(ms(250))), ms(250));
        assert_eq!(
            latency.playout_delay(Some(ms(2000))),
            ms(400) - JITTER_HEADROOM,
            "a frame's worth of headroom stays below the skip threshold"
        );
        // The default keeps headroom at its maximum, and stays at or above its minimum.
        let delay = Latency::default().playout_delay(Some(ms(150)));
        assert!(delay < ms(150) && delay >= ms(100), "{delay:?}");
        let tight = Latency {
            min: ms(150),
            max: ms(150),
        };
        assert_eq!(tight.playout_delay(Some(ms(400))), ms(150));
        assert_eq!(
            Latency::IMMEDIATE.playout_delay(Some(ms(250))),
            Duration::ZERO
        );
    }
}
