//! Frame-level seamless track swap on top of [`Subscription`].
//!
//! [`SeamlessVideoTrack`] consumes decoded video frames through a
//! single [`FrameReceiver`](moq_media::frame_channel::FrameReceiver)
//! that survives across active-source changes. The active decoder
//! pipeline is rebuilt on every swap; the new pipeline writes to
//! the same receiver via
//! [`new_sender`](moq_media::frame_channel::FrameReceiver::new_sender)
//! so consumers see an uninterrupted frame stream.
//!
//! The swap boundary is the next frame the new pipeline produces;
//! for video that is the next keyframe on the new source. Audio is
//! not yet seamless: the audio pipeline is rebuilt on swap and the
//! consumer experiences a brief gap. The path to closing that gap
//! reuses [`OutputHandle`]'s existing fade-out / fade-in machinery
//! (the same ramp that declicks pause and resume) so the swap
//! announces itself as a fade rather than a cut. The work is
//! self-contained in the audio backend and the
//! [`SeamlessMediaTracks`] swap loop here; tracked separately so
//! this commit can ship without it.
//!
//! [`OutputHandle`]: moq_media::audio_backend::OutputHandle

use std::sync::Arc;

use moq_media::{
    format::{DecodeConfig, PlaybackConfig, VideoFrame},
    frame_channel::FrameReceiver,
    pipeline::{VideoDecoderHandle, VideoDecoderPipeline},
    subscribe::CatalogSnapshot,
    traits::{AudioStreamFactory, Decoders},
};
use n0_error::{AnyError, Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio::{sync::Mutex, task};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{Subscription, sources::SourceId};

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
    /// The current audio track. On every active-source change the
    /// audio track is rebuilt; consumers that need to read audio
    /// across swaps should observe
    /// [`Subscription::watch_active`] and re-fetch.
    pub audio: Option<moq_media::subscribe::AudioTrack>,
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
        audio_backend: &dyn AudioStreamFactory,
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
    pub async fn media_with_decoders<D: Decoders>(
        &self,
        audio_backend: &dyn AudioStreamFactory,
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
                active,
                rendition,
                playback_config.decode_config(),
            )?),
            None => None,
        };
        let audio = match audio_rendition {
            Some(rendition) => initial_broadcast
                .audio_rendition::<D::Audio>(&rendition, audio_backend)
                .await
                .inspect_err(|err| warn!("audio rendition init failed: {err:#}"))
                .ok(),
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
