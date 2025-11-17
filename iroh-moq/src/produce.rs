use super::EncoderThread;
use crate::av::AudioEncoder;
use crate::av::AudioPreset;
use crate::av::AudioSource;
use crate::av::VideoEncoder;
use crate::av::VideoPreset;
use crate::av::VideoSource;
use hang::Catalog;
use hang::CatalogProducer;
use moq_lite::BroadcastProducer;
use moq_lite::Track;
use n0_error::Result;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::BroadcastName;

pub struct PublishBroadcast {
    pub(crate) name: BroadcastName,
    pub(crate) producer: BroadcastProducer,
    pub(crate) catalog: CatalogProducer,
    pub(crate) shutdown: CancellationToken,
    pub(crate) encoder: Encoder,
}

#[derive(Default)]
pub struct Encoder {
    pub(crate) video: Option<EncoderState>,
    pub(crate) audio: Option<EncoderState>,
}

pub(crate) struct EncoderState {
    pub(crate) shutdown: CancellationToken,
    pub(crate) _threads: Vec<EncoderThread>,
}

impl Drop for EncoderState {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl PublishBroadcast {
    pub fn new(name: &str) -> Self {
        let name = name.to_string();
        let mut broadcast = BroadcastProducer::default();
        let catalog = Catalog::default().produce();
        broadcast.insert_track(catalog.consumer.track);
        let catalog = catalog.producer;

        Self {
            name,
            producer: broadcast,
            catalog,
            shutdown: CancellationToken::new(),
            encoder: Default::default(),
        }
    }

    pub fn set_video<I, E>(
        &mut self,
        mut source: impl VideoSource + Send + 'static,
        renditions: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::VideoPreset)>,
        E: VideoEncoder + Send + 'static,
    {
        self.encoder.video = None;
        let priority = 1u8;
        let (tx, rx) = tokio::sync::watch::channel(None);
        let source_format = source.format();
        let shutdown = self.shutdown.child_token();
        // capture loop
        std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                loop {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let _ = tx.send(Some(frame));
                        }
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            }
        });
        let (threads, renditions): (Vec<_>, HashMap<_, _>) = renditions
            .into_iter()
            .map(|(encoder, preset)| {
                let name = format!("video-{}", preset);
                let span = info_span!("videoenc", %name);
                let rendition = (name.clone(), encoder.config());
                let track = self.producer.create_track(Track {
                    name: name.clone(),
                    priority,
                });
                let producer = hang::TrackProducer::new(track);
                let thread = EncoderThread::spawn_video(
                    rx.clone(),
                    encoder,
                    producer,
                    source_format.clone(),
                    shutdown.child_token(),
                    span,
                );
                (thread, rendition)
            })
            .unzip();
        let video = hang::catalog::Video {
            renditions,
            priority,
            display: None,
            rotation: None,
            flip: None,
            detection: None,
        };
        self.catalog.set_video(Some(video));
        self.catalog.publish();
        self.encoder.video = Some(EncoderState {
            shutdown,
            _threads: threads,
        });
        Ok(())
    }

    pub fn set_audio<I, E>(
        &mut self,
        source: impl AudioSource + Clone + Send + 'static,
        renditions: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (E, crate::av::AudioPreset)>,
        E: AudioEncoder + Send + 'static,
    {
        let priority = 2u8;

        let shutdown = self.shutdown.child_token();
        let (threads, renditions): (Vec<_>, HashMap<_, _>) = renditions
            .into_iter()
            .map(|(encoder, preset)| {
                let name = format!("{preset}");
                let span = info_span!("audioenc", %name);
                let rendition = (name.clone(), encoder.config());
                let track = self.producer.create_track(Track {
                    name: name.clone(),
                    priority,
                });
                let producer = hang::TrackProducer::new(track);
                // Clone source per encoder thread
                let thread = EncoderThread::spawn_audio(
                    source.clone(),
                    encoder,
                    producer,
                    shutdown.clone(),
                    span,
                );
                (thread, rendition)
            })
            .unzip();

        let audio = hang::catalog::Audio {
            renditions,
            priority,
            captions: None,
            speaking: None,
        };
        self.catalog.set_audio(Some(audio));
        self.catalog.publish();
        self.encoder.audio = Some(EncoderState {
            shutdown,
            _threads: threads,
        });
        Ok(())
    }
}

impl Drop for PublishBroadcast {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.producer.close();
    }
}

pub struct EncoderThread {
    _thread_handle: std::thread::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl EncoderThread {
    pub fn spawn_video(
        mut frames_rx: tokio::sync::watch::Receiver<Option<crate::av::VideoFrame>>,
        mut encoder: impl VideoEncoder + Send + 'static,
        mut producer: hang::TrackProducer,
        format: crate::av::VideoFormat,
        shutdown: CancellationToken,
        span: Span,
    ) -> Self {
        let handle = std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                tracing::debug!(
                    src_format = ?format,
                    dst_config = ?encoder.config(),
                    "video encoder thread start"
                );
                let framerate = encoder.config().framerate.unwrap_or(30.0);
                let interval = Duration::from_secs_f64(1. / framerate);
                loop {
                    let start = Instant::now();
                    if shutdown.is_cancelled() {
                        debug!("stop video encoder: cancelled");
                        break;
                    }
                    let frame = frames_rx.borrow_and_update().clone();
                    if let Some(frame) = frame {
                        if let Err(err) = encoder.push_frame(&format, frame) {
                            warn!("video encoder failed: {err:#}");
                            break;
                        };
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    std::thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                tracing::debug!("video encoder thread stop");
            }
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }

    pub fn spawn_audio(
        mut source: impl AudioSource + Send + 'static,
        mut encoder: impl AudioEncoder + Send + 'static,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
        span: tracing::Span,
    ) -> Self {
        let sd = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let _guard = span.enter();
            tracing::debug!(config=?encoder.config(), "audio encoder thread start");
            let shutdown = sd;
            // 20ms framing to align with typical Opus config (48kHz → 960 samples/ch)
            const INTERVAL: Duration = Duration::from_millis(20);
            let format = source.format();
            let samples_per_frame = (format.sample_rate / 1000) * INTERVAL.as_millis() as u32;
            let mut buf = vec![0.0f32; samples_per_frame as usize * format.channel_count as usize];
            loop {
                trace!("tick");
                let start = Instant::now();
                if shutdown.is_cancelled() {
                    break;
                }
                match source.pop_samples(&mut buf) {
                    Ok(Some(_n)) => {
                        // Expect a full frame; if shorter, zero-pad via slice len
                        if let Err(err) = encoder.push_samples(&buf) {
                            error!("audio push_samples failed: {err:#}");
                            break;
                        }
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    Ok(None) => {
                        // keep pacing
                    }
                    Err(err) => {
                        error!("audio source failed: {err:#}");
                        break;
                    }
                }
                let elapsed = start.elapsed();
                if elapsed > INTERVAL {
                    warn!(
                        "audio thread too slow: took {:?} for interval of {:?}",
                        elapsed, INTERVAL
                    );
                }
                let sleep = INTERVAL.saturating_sub(start.elapsed());
                std::thread::sleep(sleep);
            }
            // drain
            while let Ok(Some(pkt)) = encoder.pop_packet() {
                producer.write(pkt);
            }
            producer.inner.close();
            tracing::debug!("audio encoder thread stop");
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }
}

impl Drop for EncoderThread {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
