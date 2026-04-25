//! Frame-level seamless track swap on top of [`Subscription`].
//!
//! [`SeamlessVideoTrack`] consumes decoded video frames through a
//! single [`FrameReceiver`](moq_media::frame_channel::FrameReceiver)
//! that survives across active-source changes. The active decoder
//! pipeline is rebuilt on every swap; the new pipeline writes to
//! the same receiver via
//! [`new_sender`](moq_media::frame_channel::FrameReceiver::new_sender)
//! so consumers see an uninterrupted frame stream. The swap
//! boundary is the next frame the new pipeline produces, which on
//! video is the next keyframe on the new source.
//!
//! [`SeamlessAudioTrack`] takes a different approach: the audio
//! backend already mixes multiple coexisting [`AudioStream`]s, so
//! the swap brings the new stream up alongside the old, fades the
//! old out using its [`AudioSinkHandle`]'s pause-style ramp, then
//! drops it after the fade completes. The two streams overlap for
//! the duration of one declicker ramp (a few milliseconds), which
//! the mixer handles cleanly because each stream has its own fade
//! state.
//!
//! [`AudioStream`]: moq_media::traits::AudioStreamFactory
//! [`AudioSinkHandle`]: moq_media::traits::AudioSinkHandle

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use moq_media::{
    format::{DecodeConfig, PlaybackConfig, VideoFrame},
    frame_channel::FrameReceiver,
    pipeline::{VideoDecoderHandle, VideoDecoderPipeline},
    subscribe::{AudioTrack, CatalogSnapshot},
    traits::{AudioStreamFactory, Decoders},
};
use n0_error::{AnyError, Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio::{sync::Mutex, task};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{Subscription, sources::SourceId};

/// Time the swap loop waits after fading the previous audio
/// stream out before dropping it. Long enough for the audio
/// backend's declicker ramp to complete (`DECLICKER_SAMPLES /
/// sample_rate`), short enough that a fast caller does not
/// notice. The 50 ms cushion covers the worst-case 8 kHz device
/// where the ramp lasts 18 ms.
const AUDIO_FADE_DURATION: Duration = Duration::from_millis(50);

/// Combined seamless video track and audio track tied to a
/// [`Subscription`].
///
/// Returned by [`Subscription::media`](crate::Subscription::media).
/// Internally swaps decoder pipelines whenever the subscription's
/// active source changes; the video track is frame-level seamless
/// while audio is rebuilt on swap.
#[derive(Debug)]
pub struct SeamlessMediaTracks {
    /// The seamless video track when the active broadcast advertises
    /// video; `None` when the broadcast carries audio only.
    pub video: Option<SeamlessVideoTrack>,
    /// The seamless audio track when the active broadcast
    /// advertises audio; `None` when the broadcast carries video
    /// only or audio rendition selection failed at construction.
    /// The track survives source swaps through a fade-and-drop
    /// crossover handled by an internal task.
    pub audio: Option<SeamlessAudioTrack>,
}

/// Decoded audio track that survives source switches.
///
/// Built once on subscription start; an internal swap task
/// observes [`Subscription::watch_active`] and rebuilds the
/// inner [`AudioTrack`] whenever the active source changes. The
/// new track plays alongside the old briefly while the old
/// fades out via its [`AudioSinkHandle::pause`] ramp, then the
/// old is dropped. Volume and the rendition watcher persist
/// across swaps.
///
/// [`AudioSinkHandle::pause`]: moq_media::traits::AudioSinkHandle::pause
pub struct SeamlessAudioTrack {
    inner: Arc<Mutex<Option<AudioTrack>>>,
    rendition: tokio::sync::watch::Receiver<String>,
    /// Volume as `f32::to_bits`, persisted so that a swap
    /// installs the consumer's last setting on the new track.
    volume: Arc<AtomicU32>,
    _swap: AbortOnDropHandle<()>,
    _shutdown: tokio_util::sync::DropGuard,
}

impl std::fmt::Debug for SeamlessAudioTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeamlessAudioTrack")
            .field("rendition", &*self.rendition.borrow())
            .field(
                "volume",
                &f32::from_bits(self.volume.load(Ordering::Relaxed)),
            )
            .finish()
    }
}

impl SeamlessAudioTrack {
    /// Returns the rendition name the inner track is currently
    /// decoding from.
    pub fn rendition(&self) -> String {
        self.rendition.borrow().clone()
    }

    /// Returns a watcher over the rendition name. The watcher
    /// updates on every successful swap.
    pub fn rendition_watcher(&self) -> tokio::sync::watch::Receiver<String> {
        self.rendition.clone()
    }

    /// Sets the playback volume in `[0.0, 1.0]`. The setting
    /// applies to the current track immediately and is carried
    /// forward to every subsequent track installed by a swap.
    pub async fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped.to_bits(), Ordering::Relaxed);
        if let Some(track) = self.inner.lock().await.as_ref() {
            track.set_volume(clamped);
        }
    }

    /// Returns the last volume the consumer set, or `1.0` if
    /// none.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    /// Returns `true` when the track has no inner pipeline
    /// (subscription closed or initial pipeline torn down).
    pub async fn is_stopped(&self) -> bool {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(AudioTrack::is_stopped)
            .unwrap_or(true)
    }
}

/// Decoded video track that survives transport switches.
///
/// Owns a [`FrameReceiver<VideoFrame>`] shared with a background
/// swap task. Each active-source change triggers the swap task to
/// build a new decoder pipeline and attach it to the same receiver,
/// so consumers see no gap in frames.
pub struct SeamlessVideoTrack {
    rx: Arc<FrameReceiver<VideoFrame>>,
    rendition: tokio::sync::watch::Receiver<String>,
    _swap: AbortOnDropHandle<()>,
    _shutdown: tokio_util::sync::DropGuard,
}

impl std::fmt::Debug for SeamlessVideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeamlessVideoTrack")
            .field("rendition", &*self.rendition.borrow())
            .field("closed", &self.rx.is_closed())
            .finish()
    }
}

impl SeamlessVideoTrack {
    /// Awaits the next decoded frame.
    ///
    /// Returns `None` when the underlying subscription is shut
    /// down or when no decoder pipeline is currently feeding the
    /// receiver.
    pub async fn next_frame(&self) -> Option<VideoFrame> {
        self.rx.recv().await
    }

    /// Returns the latest decoded frame without blocking.
    pub fn try_recv(&self) -> Option<VideoFrame> {
        self.rx.take()
    }

    /// Returns `true` when a frame is available without consuming
    /// it.
    pub fn has_frame(&self) -> bool {
        self.rx.has_value()
    }

    /// Returns the rendition name currently feeding the track.
    pub fn rendition(&self) -> String {
        self.rendition.borrow().clone()
    }

    /// Returns a watcher over the current rendition name.
    pub fn rendition_watcher(&self) -> tokio::sync::watch::Receiver<String> {
        self.rendition.clone()
    }
}

impl Subscription {
    /// Subscribes to video and audio with frame-level seamless swap
    /// across active-source changes.
    ///
    /// On each swap the video decoder pipeline is rebuilt against
    /// the new source's broadcast and writes into the same
    /// [`FrameReceiver`]. Audio is rebuilt as well; the audio sink
    /// is not seamless across swaps and exhibits a brief gap.
    ///
    /// # Errors
    ///
    /// Returns an error when no source becomes active or when
    /// pipeline construction fails on the initial source.
    pub async fn media(
        &self,
        audio_backend: Arc<dyn AudioStreamFactory>,
        playback_config: PlaybackConfig,
    ) -> Result<SeamlessMediaTracks, AnyError> {
        self.media_with_decoders::<moq_media::codec::DefaultDecoders>(
            audio_backend,
            playback_config,
        )
        .await
    }

    /// Variant of [`Subscription::media`] with explicit decoder
    /// types.
    ///
    /// The audio backend is taken as `Arc<dyn AudioStreamFactory>`
    /// so the seamless swap task can hold its own clone and
    /// rebuild the audio pipeline on every active-source change.
    pub async fn media_with_decoders<D: Decoders>(
        &self,
        audio_backend: Arc<dyn AudioStreamFactory>,
        playback_config: PlaybackConfig,
    ) -> Result<SeamlessMediaTracks, AnyError> {
        let active = self
            .wait_active()
            .await
            .std_context("subscription closed before any source became active")?;
        let initial_broadcast = active.broadcast().clone();
        let initial_catalog = initial_broadcast.catalog();
        let video_rendition = initial_catalog
            .select_video_rendition(playback_config.quality)
            .ok();
        let audio_rendition = initial_catalog
            .select_audio_rendition(playback_config.quality)
            .ok();

        let video = match video_rendition {
            Some(rendition) => Some(spawn_seamless_video::<D::Video>(
                self.clone(),
                active.clone(),
                rendition,
                playback_config.decode_config(),
            )?),
            None => None,
        };
        let audio = match audio_rendition {
            Some(rendition) => {
                spawn_seamless_audio::<D::Audio>(self.clone(), active, rendition, audio_backend)
                    .await
                    .inspect_err(|err| warn!("seamless audio init failed: {err:#}"))
                    .ok()
            }
            None => None,
        };
        Ok(SeamlessMediaTracks { video, audio })
    }
}

fn spawn_seamless_video<V: moq_media::traits::VideoDecoder>(
    subscription: Subscription,
    active: crate::ActiveSource,
    initial_rendition: String,
    decode_config: DecodeConfig,
) -> Result<SeamlessVideoTrack, AnyError> {
    let broadcast = active.broadcast().clone();
    let initial_codec = broadcast
        .catalog()
        .video
        .renditions
        .get(&initial_rendition)
        .map(|c| c.codec.clone())
        .std_context("initial rendition not in catalog")?;
    let pipeline = broadcast.build_video_pipeline::<V>(&initial_rendition, &decode_config)?;
    let VideoDecoderPipeline { frames, handle } = pipeline;
    let rx = Arc::new(frames.into_receiver());

    let (rendition_tx, rendition_rx) = tokio::sync::watch::channel(initial_rendition.clone());
    let shutdown = CancellationToken::new();
    let drop_guard = shutdown.clone().drop_guard();

    let state = Arc::new(SwapState {
        current: Mutex::new(SwapCurrent {
            handle: Some(handle),
            source_id: active.id.clone(),
            rendition: initial_rendition,
            codec: initial_codec,
        }),
    });

    let swap_task = spawn_video_swap_task::<V>(
        subscription,
        decode_config,
        rx.clone(),
        state,
        rendition_tx,
        shutdown,
    );

    Ok(SeamlessVideoTrack {
        rx,
        rendition: rendition_rx,
        _swap: AbortOnDropHandle::new(swap_task),
        _shutdown: drop_guard,
    })
}

#[derive(Debug)]
struct SwapState {
    current: Mutex<SwapCurrent>,
}

#[derive(Debug)]
struct SwapCurrent {
    /// Holds the live decoder pipeline. Dropping `handle` cancels
    /// the previous decoder; we hold it across swap attempts so
    /// the old pipeline keeps producing frames until the new one
    /// is ready.
    handle: Option<VideoDecoderHandle>,
    source_id: SourceId,
    rendition: String,
    /// Codec the consumer's decoder was instantiated for. Seamless
    /// swap requires the new source to advertise a rendition with
    /// the same codec; on a mismatch the swap is refused and the
    /// caller has to rebuild the pipeline.
    codec: hang::catalog::VideoCodec,
}

/// Picks a rendition from `catalog` whose codec matches `codec`.
///
/// Prefers the previous rendition name when it is still present;
/// otherwise falls back to the highest-quality rendition that
/// uses the same codec. Returns `None` when nothing matches; the
/// caller must then refuse the seamless swap.
fn pick_rendition(
    catalog: &CatalogSnapshot,
    prev: &str,
    codec: &hang::catalog::VideoCodec,
) -> Option<String> {
    if let Some(cfg) = catalog.video.renditions.get(prev)
        && &cfg.codec == codec
    {
        return Some(prev.to_string());
    }
    let mut compatible: Vec<(&String, &hang::catalog::VideoConfig)> = catalog
        .video
        .renditions
        .iter()
        .filter(|(_, cfg)| &cfg.codec == codec)
        .collect();
    if compatible.is_empty() {
        return None;
    }
    // Prefer highest-quality compatible rendition.
    compatible.sort_by_key(|(_, cfg)| std::cmp::Reverse(cfg.coded_width));
    Some(compatible[0].0.clone())
}

fn spawn_video_swap_task<V: moq_media::traits::VideoDecoder>(
    subscription: Subscription,
    decode_config: DecodeConfig,
    rx: Arc<FrameReceiver<VideoFrame>>,
    state: Arc<SwapState>,
    rendition_tx: tokio::sync::watch::Sender<String>,
    shutdown: CancellationToken,
) -> task::JoinHandle<()> {
    let span = info_span!("seamless_video", broadcast = %subscription.broadcast_name());
    task::spawn(
        async move {
            let mut active_watcher = subscription.watch_active();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        debug!("seamless video shutting down");
                        break;
                    }
                    res = active_watcher.updated() => {
                        if res.is_err() {
                            debug!("subscription dropped");
                            break;
                        }
                        if let Err(err) = perform_swap::<V>(
                            &subscription,
                            &decode_config,
                            &rx,
                            &state,
                            &rendition_tx,
                        )
                        .await
                        {
                            warn!("seamless video swap failed: {err:#}");
                        }
                    }
                }
            }
        }
        .instrument(span),
    )
}

async fn perform_swap<V: moq_media::traits::VideoDecoder>(
    subscription: &Subscription,
    decode_config: &DecodeConfig,
    rx: &FrameReceiver<VideoFrame>,
    state: &SwapState,
    rendition_tx: &tokio::sync::watch::Sender<String>,
) -> Result<(), AnyError> {
    let Some(new_active) = subscription.active().await else {
        debug!("active source went away; awaiting next");
        return Ok(());
    };
    let mut current = state.current.lock().await;
    if current.source_id == new_active.id {
        return Ok(());
    }
    // Re-check the active id under the lock: between the
    // `subscription.active()` read above and the lock acquire, the
    // active source could have moved on. Bail and let the next
    // watcher tick converge instead of swapping to a stale source.
    if subscription.active_id() != Some(new_active.id.clone()) {
        debug!(
            seen = %new_active.id,
            current = ?subscription.active_id(),
            "active source moved on while waiting for swap lock; skipping"
        );
        return Ok(());
    }

    let prev_rendition = current.rendition.clone();
    let broadcast = new_active.broadcast().clone();
    let catalog = broadcast.catalog();
    let Some(rendition) = pick_rendition(&catalog, &prev_rendition, &current.codec) else {
        warn!(
            decoder_codec = ?current.codec,
            "new source advertises no rendition with the consumer's codec; refusing seamless swap"
        );
        return Ok(());
    };
    let new_sender = rx.new_sender();
    let pipeline =
        broadcast.build_video_pipeline_with_sender::<V>(&rendition, decode_config, new_sender)?;
    info!(
        from = %current.source_id,
        to = %new_active.id,
        rendition = %rendition,
        "video decoder pipeline swapped to new source"
    );
    current.handle = Some(pipeline);
    current.source_id = new_active.id;
    current.rendition = rendition.clone();
    let _ = rendition_tx.send(rendition);
    Ok(())
}

async fn spawn_seamless_audio<A: moq_media::traits::AudioDecoder>(
    subscription: Subscription,
    active: crate::ActiveSource,
    initial_rendition: String,
    audio_backend: Arc<dyn AudioStreamFactory>,
) -> Result<SeamlessAudioTrack, AnyError> {
    let broadcast = active.broadcast().clone();
    let initial_codec = broadcast
        .catalog()
        .audio
        .renditions
        .get(&initial_rendition)
        .map(|c| c.codec.clone())
        .std_context("initial audio rendition not in catalog")?;
    let initial_track = broadcast
        .audio_rendition::<A>(&initial_rendition, &*audio_backend)
        .await?;

    let (rendition_tx, rendition_rx) = tokio::sync::watch::channel(initial_rendition.clone());
    let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let inner = Arc::new(Mutex::new(Some(initial_track)));
    let shutdown = CancellationToken::new();
    let drop_guard = shutdown.clone().drop_guard();

    let state = AudioSwapState {
        subscription: subscription.clone(),
        audio_backend,
        inner: inner.clone(),
        volume: volume.clone(),
        rendition_tx,
        codec: initial_codec,
        current_id: Mutex::new(active.id),
    };

    let task = task::spawn(audio_swap_task::<A>(state, shutdown).instrument(info_span!(
        "seamless_audio",
        broadcast = %subscription.broadcast_name()
    )));

    Ok(SeamlessAudioTrack {
        inner,
        rendition: rendition_rx,
        volume,
        _swap: AbortOnDropHandle::new(task),
        _shutdown: drop_guard,
    })
}

struct AudioSwapState {
    subscription: Subscription,
    audio_backend: Arc<dyn AudioStreamFactory>,
    inner: Arc<Mutex<Option<AudioTrack>>>,
    volume: Arc<AtomicU32>,
    rendition_tx: tokio::sync::watch::Sender<String>,
    /// Codec the consumer's pipeline was instantiated for.
    /// Seamless swap requires the new source to advertise an
    /// audio rendition with the same codec; on a mismatch the
    /// swap is refused and the existing track keeps playing on
    /// the old source until it dies.
    codec: hang::catalog::AudioCodec,
    current_id: Mutex<SourceId>,
}

async fn audio_swap_task<A: moq_media::traits::AudioDecoder>(
    state: AudioSwapState,
    shutdown: CancellationToken,
) {
    let mut active_watcher = state.subscription.watch_active();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                debug!("seamless audio shutting down");
                break;
            }
            res = active_watcher.updated() => {
                if res.is_err() {
                    debug!("subscription dropped");
                    break;
                }
                if let Err(err) = perform_audio_swap::<A>(&state).await {
                    warn!("seamless audio swap failed: {err:#}");
                }
            }
        }
    }
}

async fn perform_audio_swap<A: moq_media::traits::AudioDecoder>(
    state: &AudioSwapState,
) -> Result<(), AnyError> {
    let Some(new_active) = state.subscription.active().await else {
        debug!("active source went away; awaiting next");
        return Ok(());
    };
    let mut current_id = state.current_id.lock().await;
    if *current_id == new_active.id {
        return Ok(());
    }
    // Re-check the active id under the lock: between the
    // `subscription.active()` read above and the lock acquire,
    // the active source could have moved on. Bail and let the
    // next watcher tick converge instead of swapping to a stale
    // source.
    if state.subscription.active_id() != Some(new_active.id.clone()) {
        debug!(
            seen = %new_active.id,
            current = ?state.subscription.active_id(),
            "active source moved on while waiting for swap lock; skipping"
        );
        return Ok(());
    }

    let prev_rendition = state.rendition_tx.borrow().clone();
    let broadcast = new_active.broadcast().clone();
    let catalog = broadcast.catalog();
    let Some(rendition) = pick_audio_rendition(&catalog, &prev_rendition, &state.codec) else {
        warn!(
            decoder_codec = ?state.codec,
            "new source advertises no rendition with the consumer's codec; refusing seamless audio swap"
        );
        return Ok(());
    };

    // Drop the inner mutex around the (potentially blocking)
    // audio backend init so callers of `set_volume` and friends
    // are not blocked on the swap. The await below builds a
    // fresh `AudioTrack` against the new broadcast.
    let new_track = broadcast
        .audio_rendition::<A>(&rendition, &*state.audio_backend)
        .await?;
    new_track.set_volume(f32::from_bits(state.volume.load(Ordering::Relaxed)));

    info!(
        from = %*current_id,
        to = %new_active.id,
        rendition = %rendition,
        "audio decoder pipeline swapped to new source"
    );

    let old = {
        let mut inner = state.inner.lock().await;
        inner.replace(new_track)
    };

    *current_id = new_active.id;
    drop(current_id);
    let _ = state.rendition_tx.send(rendition);

    if let Some(old) = old {
        // Trigger the audio backend's pause-style fade-out on
        // the old stream, wait long enough for the declicker
        // ramp to complete, then drop the track. The new track
        // is already mixing alongside; consumers hear a brief
        // crossover instead of a cut.
        old.handle().pause();
        tokio::time::sleep(AUDIO_FADE_DURATION).await;
        drop(old);
    }
    Ok(())
}

/// Picks an audio rendition from `catalog` whose codec matches
/// `codec`. Prefers the previous rendition name when it is still
/// present; otherwise falls back to any compatible rendition.
fn pick_audio_rendition(
    catalog: &CatalogSnapshot,
    prev: &str,
    codec: &hang::catalog::AudioCodec,
) -> Option<String> {
    if let Some(cfg) = catalog.audio.renditions.get(prev)
        && &cfg.codec == codec
    {
        return Some(prev.to_string());
    }
    catalog
        .audio
        .renditions
        .iter()
        .find(|(_, cfg)| &cfg.codec == codec)
        .map(|(name, _)| name.clone())
}
