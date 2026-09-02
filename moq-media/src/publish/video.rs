//! The video publish task: one source, one encoder per rendition.
//!
//! `moq_video::encode::publish_capture` covers the single-rendition case and
//! owns its device. Simulcast cannot reuse it, because every rendition needs
//! the same picture at the same instant and a camera can only be opened once.
//! So the source is read here and its frames are handed to each rendition's
//! encoder through a latest-wins slot: a rendition that falls behind drops
//! frames instead of stalling the ones that have not.
//!
//! Encoders are demand-gated the way upstream gates its device: a rendition
//! encodes only while someone is watching it. The source itself is not, because
//! the local preview draws the same frames the encoders receive and a publisher
//! expects to see itself before anyone has tuned in.

use std::{sync::Arc, time::Instant};

use moq_video::{
    Frame, Size,
    encode::{self, Encoded},
};
use n0_error::Result;
use n0_future::{
    StreamExt,
    boxed::BoxStream,
    task::{AbortOnDropHandle, JoinSet, spawn},
};
use tracing::{Instrument, debug, error_span, info, instrument, warn};

use super::{CatalogProducer, PublishError, VideoRendition, VideoSource};
use crate::{
    frame_channel::{FrameReceiver, FrameSender, frame_channel},
    stats::PublishStats,
};

/// Used when neither the source nor the caller reports a frame rate.
const DEFAULT_FRAMERATE: u32 = 30;

/// Everything the publish task needs, moved into it whole.
pub(super) struct Publish {
    pub broadcast: moq_net::broadcast::Producer,
    pub catalog: CatalogProducer,
    pub clock: moq_mux::Clock,
    pub stats: PublishStats,
    /// Held for this task's whole life so a replacement cannot create a track
    /// whose name this task still owns.
    pub slot: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub source: VideoSource,
    pub renditions: Vec<VideoRendition>,
    pub preview: FrameSender<Arc<Frame>>,
}

/// Starts the publish task. Dropping the returned handle stops it and releases
/// the capture device.
pub(super) fn spawn_publish(publish: Publish) -> AbortOnDropHandle<()> {
    let task = spawn(
        async move {
            if let Err(err) = run(publish).await {
                warn!(error = %err, "video publish stopped");
            }
        }
        .instrument(error_span!("video-publish")),
    );
    AbortOnDropHandle::new(task)
}

async fn run(publish: Publish) -> Result<(), PublishError> {
    let Publish {
        broadcast,
        catalog,
        #[cfg_attr(
            not(feature = "capture"),
            allow(unused_variables, reason = "only the capture source stamps frames")
        )]
        clock,
        stats,
        slot,
        source,
        renditions,
        preview,
    } = publish;

    // Wait for the previous publish to have dropped its track producers. The
    // guard is shared with every encoder task, because that is where the
    // producers actually live: releasing it when this function returns would
    // let a replacement create a track whose name an aborted encoder still
    // owns for a few more scheduler ticks.
    let slot = Arc::new(slot.lock_owned().await);

    match source {
        VideoSource::AnnexB(bytes) => {
            let name = renditions
                .first()
                .map(|rendition| rendition.name.clone())
                .unwrap_or_else(|| "video".to_string());
            publish_annexb(broadcast, catalog, bytes, name, stats).await
        }
        #[cfg(feature = "capture")]
        VideoSource::Capture(config) => {
            let stream = moq_video::capture::open(&config).await?;
            let size = Size::new(stream.width(), stream.height());
            let framerate = config
                .framerate
                .or_else(|| stream.framerate())
                .unwrap_or(DEFAULT_FRAMERATE);
            let color = stream.color();
            // The capture stream reads on a thread of its own, and only the
            // surfaces cross. moq's native backends are not all `Send`: an Apple
            // camera or screen stream holds AVFoundation objects, so a future
            // holding one cannot go to a work-stealing executor. Surfaces can.
            //
            // `read` reports a failed capture as well as its end. Either way the
            // stream stops here, and the encoder tasks downstream see the source
            // close; the error is logged rather than propagated, since a stream
            // has nowhere to return one.
            let (surface_tx, surface_rx) = tokio::sync::mpsc::channel(1);
            let reader = crate::local_task::spawn("video-capture", move |shutdown| async move {
                let mut stream = stream;
                loop {
                    let surface = tokio::select! {
                        read = stream.read() => read,
                        _ = shutdown.cancelled() => break,
                    };
                    let surface = match surface {
                        Ok(Some(surface)) => surface,
                        Ok(None) => break,
                        Err(err) => {
                            warn!(error = %err, "video capture failed");
                            break;
                        }
                    };
                    // A full channel means the encoders are behind. Waiting is
                    // right: the capture backend paces itself against the
                    // device, so dropping here would only hide that.
                    if surface_tx.send(surface).await.is_err() {
                        break;
                    }
                }
            });
            let frames = Box::pin(n0_future::stream::unfold(
                (surface_rx, reader),
                |(mut rx, reader)| async move { rx.recv().await.map(|surface| (surface, (rx, reader))) },
            ));
            let clock_for_stamp = clock;
            let frames: BoxStream<Frame> = Box::pin(
                frames.map(move |surface| Frame::new(surface, timestamp(&clock_for_stamp))),
            );
            fan_out(
                broadcast, catalog, stats, renditions, preview, frames, size, framerate, color,
                slot,
            )
            .await
        }
        VideoSource::Frames(mut frames) => {
            // The first frame is what tells us the geometry, and the catalog
            // has to be exact before an encoder opens, so wait for it here and
            // put it back at the head of the stream.
            let Some(first) = frames.next().await else {
                debug!("video source ended before its first frame");
                return Ok(());
            };
            let size = first.size();
            let color = first.surface.color();
            let frames: BoxStream<Frame> = Box::pin(n0_future::stream::once(first).chain(frames));
            fan_out(
                broadcast,
                catalog,
                stats,
                renditions,
                preview,
                frames,
                size,
                DEFAULT_FRAMERATE,
                color,
                slot,
            )
            .await
        }
    }
}

/// Reads `frames` and drives one encoder per rendition.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site, all of it is source geometry"
)]
async fn fan_out(
    mut broadcast: moq_net::broadcast::Producer,
    catalog: CatalogProducer,
    stats: PublishStats,
    renditions: Vec<VideoRendition>,
    preview: FrameSender<Arc<Frame>>,
    mut frames: BoxStream<Frame>,
    size: Size,
    framerate: u32,
    color: Option<moq_video::Color>,
    slot: Arc<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(), PublishError> {
    let mut senders = Vec::with_capacity(renditions.len());
    let mut encoders = JoinSet::new();

    for rendition in renditions {
        let config = encode_config(&rendition, size, framerate, color);
        // Probing costs one encoder open and buys a catalog entry that says
        // exactly what the track will carry, so a subscriber can size itself
        // against it before a single frame is encoded.
        let published = config.probe().await?;
        info!(
            rendition = %rendition.name,
            size = %config.size(),
            framerate,
            "publishing video rendition",
        );

        let track = broadcast.create_track(rendition.name.as_str(), Some(catalog.track_info()))?;
        let producer = encode::Producer::with_track(track, catalog.clone(), published)?;

        let (tx, rx) = frame_channel();
        senders.push(tx);
        encoders.spawn({
            let slot = slot.clone();
            let encode = encode_rendition(producer, config, rx, stats.clone())
                .instrument(error_span!("rendition", name = %rendition.name));
            async move {
                let result = encode.await;
                // Explicit so the guard's lifetime is visible: it is what keeps
                // a replacement publish from creating a track this encoder's
                // producer still owns.
                drop(slot);
                result
            }
        });
    }

    while let Some(frame) = frames.next().await {
        // One allocation per frame, shared by the preview and every encoder.
        // `encode::Sink::encode` takes an `Arc<Frame>` for exactly this reason:
        // a ladder hands the same picture to several encoders.
        let frame = Arc::new(frame);
        preview.send(frame.clone());
        for sender in &senders {
            sender.send(frame.clone());
        }
    }

    // The source ended: drop the senders so every encoder sees the close and
    // finishes its track, then wait for them.
    drop(senders);
    drop(preview);
    while let Some(joined) = encoders.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "rendition encoder failed"),
            Err(err) => warn!(error = %err, "rendition encoder panicked"),
        }
    }
    Ok(())
}

/// Encodes one rendition for as long as someone is watching it.
#[instrument(skip_all)]
async fn encode_rendition(
    mut producer: encode::Producer<crate::catalog::IrohLiveExt>,
    config: encode::Config,
    frames: FrameReceiver<Arc<Frame>>,
    stats: PublishStats,
) -> Result<(), PublishError> {
    let demand = producer.demand();
    let target = config.size();

    loop {
        // Idle until someone subscribes, or until the source ends. Waiting on
        // demand alone would park here forever on a rendition nobody watched:
        // the source's end closes the frame channel, which `demand` cannot see.
        // The track and its catalog entry are already advertised, so a
        // subscriber gets here without a frame ever having been encoded.
        tokio::select! {
            used = demand.used() => {
                if let Err(err) = used {
                    debug!(error = %err, "rendition no longer watched");
                    break;
                }
            }
            () = frames.closed() => {
                debug!("source ended with nobody watching this rendition");
                producer.finish()?;
                return Ok(());
            }
        }

        let mut encoder = encode::Sink::open(&config).await?;
        stats.encode.encoder.set(encoder.name());
        stats.encode.resolution.set(target.to_string());
        debug!(encoder = encoder.name(), %target, "rendition encoding");

        loop {
            let frame = tokio::select! {
                // The last viewer left. Mark the gap so the next timestamp does
                // not stretch this frame across it, then wait for demand again.
                _ = demand.unused() => {
                    producer.discontinuity()?;
                    break;
                }
                frame = frames.recv() => match frame {
                    Some(frame) => frame,
                    // The source ended: drain the encoder, publish the tail,
                    // and close the track so subscribers see a clean end
                    // rather than `Error::Dropped`.
                    None => {
                        let tail = encoder.finish().await?;
                        producer.publish(&tail)?;
                        producer.finish()?;
                        return Ok(());
                    }
                },
            };

            let frame = match frame.size() == target {
                true => frame,
                false => Arc::new(frame.resize(target)?),
            };

            let started = Instant::now();
            // Abort rather than drop on failure, again so the subscriber sees
            // the real cause instead of `Error::Dropped`.
            let encoded = match encoder.encode(frame).await {
                Ok(encoded) => encoded,
                Err(err) => {
                    producer.abort(moq_net::Error::Transport(err.to_string()));
                    return Err(err.into());
                }
            };
            record(&stats, started, &encoded);
            producer.publish(&encoded)?;
        }
    }

    producer.finish()?;
    Ok(())
}

/// Publishes an Annex-B stream the source encoded itself.
///
/// `Split` cuts the byte stream into access units and `Import` publishes them,
/// filling the catalog rendition in from the first SPS it sees. That is why
/// this path needs no `VideoConfig` from the caller: the stream describes
/// itself, and guessing at a profile and level we did not choose would only be
/// wrong in a way subscribers have to work around.
async fn publish_annexb(
    mut broadcast: moq_net::broadcast::Producer,
    catalog: CatalogProducer,
    mut bytes: BoxStream<bytes::Bytes>,
    name: String,
    stats: PublishStats,
) -> Result<(), PublishError> {
    let track = broadcast.create_track(name.as_str(), Some(catalog.track_info()))?;
    let mut import =
        moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())?;
    let mut split = moq_mux::codec::h264::Split::new();
    stats.encode.encoder.set("pre-encoded");
    info!(rendition = %name, "publishing pre-encoded video");

    while let Some(chunk) = bytes.next().await {
        let frames = split.decode(&chunk, None)?;
        import.decode(frames)?;
    }
    // The splitter holds the final access unit until the next start code, so
    // end of stream has to flush it explicitly.
    let tail = split.flush(None)?;
    import.decode(tail)?;
    import.finish()?;
    Ok(())
}

/// Builds the encoder config for one rendition of a source.
fn encode_config(
    rendition: &VideoRendition,
    source: Size,
    framerate: u32,
    color: Option<moq_video::Color>,
) -> encode::Config {
    let size = rendition.size.unwrap_or(source);
    let mut config = encode::Config::new(size.width, size.height, framerate);
    config.bitrate = rendition.bitrate;
    config.codec = rendition.codec;
    config.kind = rendition.kind.clone();
    config.color = color;
    config
}

/// Stamps a frame on the broadcast clock, which audio shares.
#[cfg(feature = "capture")]
fn timestamp(clock: &moq_mux::Clock) -> moq_net::Timestamp {
    // u64 microseconds only overflows a Timestamp after ~584,000 years of
    // uptime, so there is no failure to report here.
    moq_net::Timestamp::from_micros(clock.micros()).expect("clock micros out of range")
}

/// Records what one encode call cost and produced.
fn record(stats: &PublishStats, started: Instant, encoded: &[Encoded]) {
    stats
        .encode
        .encode_ms
        .record(started.elapsed().as_secs_f64() * 1000.0);
    let bytes: usize = encoded.iter().map(|packet| packet.payload.len()).sum();
    if bytes > 0 {
        stats
            .encode
            .bitrate_kbps
            .record(bytes as f64 * 8.0 / 1000.0);
    }
}
