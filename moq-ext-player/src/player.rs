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

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use moq_ext_sources::{ByPtr, Error, SlotState, send_if_changed, stats::SharedCell};
use moq_net::track::Delay;
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
mod select;
mod video_task;

pub use moq_ext_adaptive::{Adaptation, RenditionMode};

pub use self::audio::{AudioFrame, AudioFrames};
pub(crate) use self::clock::PlayoutClock;

/// The latency of [`PlayerConfig::default`]: held for at least 100 ms, and skipped past 150 ms.
pub const DEFAULT_LATENCY: Delay = Delay {
    min: Some(Duration::from_millis(100)),
    max: Some(Duration::from_millis(150)),
};

/// No buffer and no pacing: a frame presents as soon as it decodes.
///
/// Media older than 150 ms is still skipped: a player without a buffer wants
/// the live edge.
pub const IMMEDIATE_LATENCY: Delay = Delay {
    min: Some(Duration::ZERO),
    max: Some(Duration::from_millis(150)),
};

/// How far below the latency's maximum a track's target may raise the playout delay: a frame at 30 fps.
const JITTER_HEADROOM: Duration = Duration::from_millis(33);

/// Reports whether a player at `latency` holds frames for the clock.
pub(crate) fn paced(latency: &Delay) -> bool {
    latency.min.is_some_and(|min| !min.is_zero())
}

/// Returns how far behind the live edge one track asks the playout clock to run, at `latency`.
///
/// This is hang's rule: a track holds its catalog `delay` plus its jitter
/// allowance. The allowance is `jitter`, the catalog's figure for video and
/// the audio decoder's measured target for audio, and never less than the
/// latency's minimum. The clock runs at the largest target of its tracks.
///
/// A target stays [`JITTER_HEADROOM`] below the latency's maximum, the age at
/// which the transport skips media, so a frame that arrives a little late is
/// not past it before it is due. It never drops below the minimum. An unpaced
/// latency holds nothing.
pub(crate) fn track_target(
    latency: &Delay,
    delay: Option<Duration>,
    jitter: Option<Duration>,
) -> Duration {
    if !paced(latency) {
        return Duration::ZERO;
    }
    let min = latency.min.unwrap_or_default();
    let ceiling = latency
        .max
        .map_or(Duration::MAX, |max| max.saturating_sub(JITTER_HEADROOM));
    let wanted = delay
        .unwrap_or_default()
        .saturating_add(min.max(jitter.unwrap_or_default()));
    min.max(wanted.min(ceiling))
}

/// Returns an [`Error::InvalidConfig`] if the latency's minimum is above its maximum.
fn validate(latency: &Delay) -> Result<(), Error> {
    match (latency.min, latency.max) {
        (Some(min), Some(max)) if min > max => Err(Error::invalid(format!(
            "a latency's minimum ({min:?}) is above its maximum ({max:?})"
        ))),
        _ => Ok(()),
    }
}

/// What a player does with the broadcast's audio.
#[derive(derive_more::Debug, Clone, Default)]
pub enum AudioMode {
    /// Leaves the audio alone, without subscribing to it.
    #[default]
    Off,
    /// Decodes the audio for [`Player::audio_frames`] without playing it.
    ///
    /// Nothing is queued ahead of a device, so the audio is not paced, and
    /// its measured jitter does not hold the video back.
    Decode,
    /// Decodes the audio and plays it on this speaker engine.
    ///
    /// Every player writing to one engine is mixed into one device stream.
    /// The decoded audio also goes to [`Player::audio_frames`]. The jitter
    /// the audio decoder measures raises the playout delay of the video too.
    #[cfg(feature = "playback")]
    Play(#[debug(skip)] moq_audio::playback::Engine),
}

/// How a player plays.
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    /// How to choose the video rendition.
    pub rendition: RenditionMode,
    /// How far behind live to run, as the moq subscription's [`Delay`].
    ///
    /// The playout clock runs at least `min` behind the live edge. Each track
    /// asks for more when it needs it: video for its catalog `delay` plus its
    /// catalog `jitter`, and audio for its catalog `delay` plus the jitter its
    /// decoder measures as packets arrive. The clock follows the largest ask,
    /// up to a frame below `max`, so the delay rises on a jittery link and
    /// returns to `min` on a clean one. While audio plays, pictures show when
    /// their sound does. Media later than `max` is skipped. Both reach the
    /// transport, so relays and the publisher see them too. Setting
    /// `min == max` gives a fixed latency, and an unset `min` counts as zero.
    /// [`DEFAULT_LATENCY`] by default.
    pub latency: Delay,
    /// What to do with the audio, [`AudioMode::Off`] by default.
    pub audio: AudioMode,
    /// The decoder backend to open, `Auto` by default.
    pub decoder: moq_video::decode::Kind,
    /// How automatic selection follows the link.
    pub adaptation: Adaptation,
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
            latency: DEFAULT_LATENCY,
            audio: AudioMode::Off,
            decoder: moq_video::decode::Kind::default(),
            adaptation: Adaptation::default(),
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
    pub(crate) video: SharedCell<Option<VideoPlaybackStats>>,
    pub(crate) audio: SharedCell<Option<AudioPlaybackStats>>,
    /// Written by the video task as it presents pictures.
    pub(crate) video_log: FrameLog,
    /// Written by the audio task as it writes to the output.
    pub(crate) audio_log: FrameLog,
}

/// What the player's controls ask for, shared with its tasks.
#[derive(Debug)]
pub(crate) struct Controls {
    pub(crate) mode: watch::Sender<RenditionMode>,
    pub(crate) latency: watch::Sender<Delay>,
    pub(crate) decoder: watch::Sender<moq_video::decode::Kind>,
    pub(crate) volume: watch::Sender<f32>,
}

/// One playback: video into [`VideoFrames`], audio into the speaker and [`AudioFrames`].
///
/// Dropping it aborts its tasks and returns at once: decoders stop at their
/// next await point and are not waited for, so a decoder thread may outlive
/// the player briefly. A second view of the broadcast is a second player.
#[derive(Debug)]
pub struct Player {
    broadcast: RemoteBroadcast,
    frames: VideoFrames,
    /// The cursor of [`Player::take_latest_video`].
    latest: Mutex<VideoFrames>,
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
        validate(&config.latency)?;
        let span = info_span!(parent: broadcast.span(), "player");
        let status = watch::Sender::new(PlayerStatus {
            mode: config.rendition.clone(),
            video: match config.rendition {
                RenditionMode::Off => SlotState::Off,
                _ => SlotState::Starting,
            },
            audio: match config.audio {
                AudioMode::Off => SlotState::Off,
                _ => SlotState::Starting,
            },
            ..PlayerStatus::default()
        });
        let controls = Arc::new(Controls {
            mode: watch::Sender::new(config.rendition.clone()),
            latency: watch::Sender::new(config.latency),
            decoder: watch::Sender::new(config.decoder.clone()),
            volume: watch::Sender::new(1.0),
        });
        let (audio_frames, _) = broadcast::channel(audio::AUDIO_FRAMES_BACKLOG);
        let clock = PlayoutClock::new(config.latency.min.unwrap_or_default());
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
            })
            .instrument(info_span!(parent: &span, "video")),
        )));
        if !matches!(config.audio, AudioMode::Off) {
            #[cfg(feature = "playback")]
            let speaker = match config.audio {
                AudioMode::Play(engine) => Some(engine),
                _ => None,
            };
            tasks.push(AbortOnDropHandle::new(n0_future::task::spawn(
                audio::run(audio::Inputs {
                    broadcast: broadcast.clone(),
                    #[cfg(feature = "playback")]
                    speaker,
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
            latest: Mutex::new(frames.clone()),
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
    /// The frames flow only while [`PlayerConfig::audio`] is not
    /// [`AudioMode::Off`], with or without a speaker. Without one, the audio is
    /// decoded without playing it, for analysis. Each call returns a new handle
    /// that starts at the next decoded frame and ends when the player is
    /// dropped.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(broadcast: moq_ext_player::RemoteBroadcast) -> Result<(), moq_ext_player::Error> {
    /// use moq_ext_player::{AudioMode, PlayerConfig};
    ///
    /// let player = broadcast.play(PlayerConfig {
    ///     audio: AudioMode::Decode,
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

    /// Changes how far behind live the player runs, without a gap in the picture.
    ///
    /// The playout clock starts over at the new delay, so the schedule moves
    /// once: the next video frame or audio write anchors it again. The latency
    /// is the subscription's [`Delay`], so a changed one also rebuilds the
    /// video decoder behind the picture with the new range. The new decoder
    /// takes over once it has caught up, as in a switch, and the current
    /// picture stays up until then. Audio keeps its old range until it next
    /// opens, because a rebuild would be audible.
    ///
    /// This does not block or await, so there is nothing to cancel: the change
    /// is recorded before it returns, and the player's tasks apply it on their
    /// next pass. Setting the latency already in force changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `latency.min` is above
    /// `latency.max`, and leaves the latency unchanged.
    pub fn set_latency(&self, latency: Delay) -> Result<(), Error> {
        validate(&latency)?;
        self.controls.latency.send_if_modified(|current| {
            let changed = *current != latency;
            *current = latency;
            changed
        });
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
            latency: self.clock.delay(),
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
        self.stats.video_log.find(pts)
    }

    /// Returns the newest presented picture since the last call, with its timing.
    ///
    /// For a consumer that samples the video at its own rate, such as an
    /// analysis loop or a thumbnailer, instead of reading every picture from
    /// [`video_frames`](Self::video_frames). Pictures presented between two
    /// calls are skipped. Returns `None` if nothing was presented since the
    /// last call. The player keeps one cursor for this method, so two callers
    /// take turns at the same pictures. Give each reader its own
    /// [`VideoFrames`] handle instead.
    ///
    /// [`FrameTiming::arrived`] is set only with
    /// [`PlayerConfig::record_arrivals`] on, because the decoder does not
    /// report when a picture's data arrived. Without it, `decoded` is the
    /// earliest instant the player knows of, and covers network and decode
    /// time together.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run(player: moq_ext_player::Player) {
    /// let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    /// loop {
    ///     tick.tick().await;
    ///     if let Some((frame, timing)) = player.take_latest_video() {
    ///         let age = timing.presented.elapsed();
    ///         println!("{} picture, presented {age:?} ago", frame.size());
    ///     }
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn take_latest_video(&self) -> Option<(Arc<moq_video::Frame>, FrameTiming)> {
        let frame = self.latest.lock().expect("poisoned").try_next()?;
        // A picture's timing is logged before the picture is handed out.
        let timing = self.stats.video_log.find(frame.timestamp.into())?;
        Some((frame, timing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn a_track_target_follows_a_larger_jitter_up_to_the_maximum() {
        let latency = Delay {
            min: Some(ms(100)),
            max: Some(ms(400)),
        };
        assert_eq!(track_target(&latency, None, None), ms(100));
        assert_eq!(track_target(&latency, None, Some(ms(40))), ms(100));
        assert_eq!(track_target(&latency, None, Some(ms(250))), ms(250));
        assert_eq!(
            track_target(&latency, None, Some(ms(2000))),
            ms(400) - JITTER_HEADROOM,
            "a frame's worth of headroom stays below the skip threshold"
        );
        // The default keeps headroom at its maximum, and stays at or above its minimum.
        let delay = track_target(&DEFAULT_LATENCY, None, Some(ms(150)));
        assert!(delay < ms(150) && delay >= ms(100), "{delay:?}");
        let tight = Delay {
            min: Some(ms(150)),
            max: Some(ms(150)),
        };
        assert_eq!(track_target(&tight, None, Some(ms(400))), ms(150));
        assert_eq!(
            track_target(&IMMEDIATE_LATENCY, Some(ms(50)), Some(ms(250))),
            Duration::ZERO
        );
    }

    /// The catalog `delay` adds to the jitter allowance, as hang asks a consumer to.
    #[test]
    fn a_track_target_adds_the_catalog_delay() {
        let latency = Delay {
            min: Some(ms(100)),
            max: Some(ms(400)),
        };
        assert_eq!(track_target(&latency, Some(ms(50)), None), ms(150));
        assert_eq!(track_target(&latency, Some(ms(50)), Some(ms(200))), ms(250));
        assert_eq!(
            track_target(&latency, Some(ms(300)), Some(ms(200))),
            ms(400) - JITTER_HEADROOM,
            "the sum is capped like the jitter alone"
        );
        assert_eq!(track_target(&latency, Some(Duration::ZERO), None), ms(100));
    }

    #[test]
    fn an_unset_bound_is_no_bound() {
        let open = Delay {
            min: None,
            max: None,
        };
        assert!(!paced(&open));
        assert_eq!(track_target(&open, None, Some(ms(250))), Duration::ZERO);
        let unbounded = Delay {
            min: Some(ms(100)),
            max: None,
        };
        assert_eq!(track_target(&unbounded, None, Some(ms(2000))), ms(2000));
        assert!(validate(&unbounded).is_ok());
        assert!(
            validate(&Delay {
                min: Some(ms(200)),
                max: Some(ms(100)),
            })
            .is_err()
        );
    }
}
