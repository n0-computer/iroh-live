use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex},
};

use moq_lite::BroadcastProducer;
use n0_error::{AnyError, Result};
use n0_watcher::{Direct, Watchable};
use tracing::info;

#[cfg(any_video_codec)]
use crate::codec::VideoCodec;
use crate::{
    audio_backend::{AudioBackend, DeviceId},
    publish::LocalBroadcast,
};
#[cfg(all(
    any_video_codec,
    any(feature = "capture-camera", feature = "capture-screen")
))]
use crate::{
    capture::{CameraCapturer, ScreenCapturer},
    format::VideoPreset,
    publish::VideoRenditions,
};
#[cfg(any_audio_codec)]
use crate::{codec::AudioCodec, format::AudioPreset, publish::AudioRenditions};

/// Device and codec selection for capture sources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureConfig {
    /// If set, use a specific camera by index. If `None`, use the default camera.
    pub camera_index: Option<u32>,
    /// If set, use a specific audio input device. If `None`, use the system default.
    pub audio_device: Option<DeviceId>,
    /// Video encoder to use. If `None`, automatically selects the best available.
    #[cfg(any_video_codec)]
    pub video_codec: Option<VideoCodec>,
    /// Audio encoder to use. If `None`, automatically selects the best available.
    #[cfg(any_audio_codec)]
    pub audio_codec: Option<AudioCodec>,
}

/// What to publish: which streams are enabled plus device/codec selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishOpts {
    pub camera: bool,
    pub screen: bool,
    pub audio: bool,
    pub capture: CaptureConfig,
}

impl PublishOpts {
    pub fn toggle_camera(&mut self) {
        self.camera = !self.camera;
    }
    pub fn toggle_screen(&mut self) {
        self.screen = !self.screen;
    }
    pub fn toggle_audio(&mut self) {
        self.audio = !self.audio;
    }
}

#[derive(Debug, strum::Display)]
pub enum StreamKind {
    Camera,
    Screen,
    Microphone,
}

/// Successful result of [`PublishCaptureController::set_opts`].
#[derive(derive_more::Debug)]
pub struct PublishUpdate {
    /// If a screen broadcast was just created, its producer.
    /// The caller should publish this to the transport layer.
    #[debug(skip)]
    pub new_screen: Option<BroadcastProducer>,
}

/// Error from [`PublishCaptureController::set_opts`] containing per-stream errors.
#[derive(derive_more::Debug)]
pub struct PublishUpdateError {
    /// Errors from individual stream enable/disable operations.
    pub errors: Vec<(StreamKind, AnyError)>,
}

impl fmt::Display for PublishUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "publish update failed: ")?;
        for (i, (kind, err)) in self.errors.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{kind}: {err}")?;
        }
        Ok(())
    }
}

impl Error for PublishUpdateError {}

/// Controls media capture and publish pipelines.
///
/// Manages camera, screen, and audio capture sources and their associated
/// [`LocalBroadcast`]s. The camera/main broadcast is created eagerly (it
/// carries both video and audio). The screen broadcast is created lazily on
/// first enable.
///
/// State is exposed via [`n0_watcher::Watchable`] for reactive observation.
/// Transport-agnostic — the caller connects broadcast producers to whatever
/// transport layer they use (rooms, direct connections, etc.).
#[derive(Debug)]
pub struct PublishCaptureController {
    #[cfg_attr(not(any_audio_codec), allow(dead_code))]
    audio_ctx: AudioBackend,
    camera: Arc<Mutex<LocalBroadcast>>,
    screen: Option<LocalBroadcast>,
    state: Watchable<PublishOpts>,
    previous_capture: CaptureConfig,
}

impl PublishCaptureController {
    /// Create a new controller. The main (camera + audio) broadcast is created
    /// immediately. Call [`camera_producer`](Self::camera_producer) to get its
    /// producer for transport.
    pub fn new(audio_ctx: AudioBackend) -> Self {
        Self {
            audio_ctx,
            camera: Arc::new(Mutex::new(LocalBroadcast::new())),
            screen: None,
            state: Watchable::new(PublishOpts::default()),
            previous_capture: CaptureConfig::default(),
        }
    }

    /// Producer for the main broadcast (camera video + audio).
    /// Grab this once at creation and connect to your transport layer.
    pub fn camera_producer(&self) -> BroadcastProducer {
        self.camera.lock().expect("poisoned").producer()
    }

    /// Current publish options (snapshot).
    pub fn state(&self) -> PublishOpts {
        self.state.get()
    }

    /// Watch for state changes reactively.
    pub fn watch_state(&self) -> Direct<PublishOpts> {
        self.state.watch()
    }

    /// Apply new publish options.
    ///
    /// Enables/disables camera, screen, and audio as needed, detecting changes
    /// in device and codec selection. Returns any newly created broadcast
    /// producers (currently only screen) and collected errors.
    pub fn set_opts(&mut self, opts: PublishOpts) -> Result<PublishUpdate, PublishUpdateError> {
        info!(new=?opts, old=?self.state.get(), "set publish opts");
        let mut errors = Vec::new();

        if let Err(e) = self.apply_audio(opts.audio, &opts.capture) {
            errors.push((StreamKind::Microphone, e));
        }

        if let Err(e) = self.apply_camera(opts.camera, &opts.capture) {
            errors.push((StreamKind::Camera, e));
        }

        let new_screen = match self.apply_screen(opts.screen, &opts.capture) {
            Ok(producer) => producer,
            Err(e) => {
                errors.push((StreamKind::Screen, e));
                None
            }
        };

        self.previous_capture = opts.capture.clone();
        self.state.set(opts).ok();

        if errors.is_empty() {
            Ok(PublishUpdate { new_screen })
        } else {
            Err(PublishUpdateError { errors })
        }
    }

    /// Shared handle to the camera/main broadcast (e.g. for local preview).
    pub fn camera_broadcast(&self) -> Arc<Mutex<LocalBroadcast>> {
        self.camera.clone()
    }

    /// Direct access to the screen broadcast, if it exists.
    pub fn screen_broadcast(&self) -> Option<&LocalBroadcast> {
        self.screen.as_ref()
    }

    // --- Internal helpers ---

    #[cfg(all(any_video_codec, feature = "capture-camera"))]
    fn apply_camera(&mut self, enable: bool, capture: &CaptureConfig) -> Result<()> {
        let cur = self.state.get();
        let index_changed =
            enable && cur.camera && capture.camera_index != self.previous_capture.camera_index;
        let codec_changed =
            enable && cur.camera && capture.video_codec != self.previous_capture.video_codec;
        if cur.camera != enable || index_changed || codec_changed {
            let camera = self.camera.lock().expect("poisoned");
            if enable {
                let capturer = match capture.camera_index {
                    Some(index) => CameraCapturer::with_index(index)?,
                    None => CameraCapturer::new()?,
                };
                let codec = capture
                    .video_codec
                    .or_else(VideoCodec::best_available)
                    .ok_or_else(|| AnyError::from_display("no video codec available"))?;
                let renditions = VideoRenditions::new(capturer, codec, VideoPreset::all());
                camera.set_video(Some(renditions.into()))?;
            } else {
                camera.set_video(None)?;
            }
        }
        Ok(())
    }

    #[cfg(not(all(any_video_codec, feature = "capture-camera")))]
    fn apply_camera(&mut self, _enable: bool, _capture: &CaptureConfig) -> Result<()> {
        Ok(())
    }

    #[cfg(all(any_video_codec, feature = "capture-screen"))]
    fn apply_screen(
        &mut self,
        enable: bool,
        capture: &CaptureConfig,
    ) -> Result<Option<BroadcastProducer>> {
        let cur = self.state.get();
        let codec_changed =
            enable && cur.screen && capture.video_codec != self.previous_capture.video_codec;
        if cur.screen != enable || codec_changed {
            if enable {
                let mut new_producer = None;
                if self.screen.is_none() {
                    let broadcast = LocalBroadcast::new();
                    new_producer = Some(broadcast.producer());
                    self.screen = Some(broadcast);
                }

                let screen = ScreenCapturer::new()?;
                let codec = capture
                    .video_codec
                    .or_else(VideoCodec::best_available)
                    .ok_or_else(|| AnyError::from_display("no video codec available"))?;
                let renditions = VideoRenditions::new(screen, codec, VideoPreset::all());
                self.screen
                    .as_ref()
                    .unwrap()
                    .set_video(Some(renditions.into()))?;

                Ok(new_producer)
            } else {
                let _ = self.screen.take();
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    #[cfg(not(all(any_video_codec, feature = "capture-screen")))]
    fn apply_screen(
        &mut self,
        _enable: bool,
        _capture: &CaptureConfig,
    ) -> Result<Option<BroadcastProducer>> {
        Ok(None)
    }

    #[cfg(any_audio_codec)]
    fn apply_audio(&mut self, enable: bool, capture: &CaptureConfig) -> Result<()> {
        let cur = self.state.get();
        let device_changed =
            enable && cur.audio && capture.audio_device != self.previous_capture.audio_device;
        if cur.audio != enable || device_changed {
            if device_changed {
                // Disable current audio first before re-enabling with the new device.
                self.camera.lock().expect("poisoned").set_audio(None)?;
            }
            if enable {
                let audio_ctx = self.audio_ctx.clone();
                let camera = self.camera.clone();
                let audio_codec = capture.audio_codec.unwrap_or(AudioCodec::Opus);
                tokio::spawn(async move {
                    let mic = match audio_ctx.default_input().await {
                        Err(err) => {
                            tracing::warn!("failed to open audio input: {err:#}");
                            return;
                        }
                        Ok(mic) => mic,
                    };
                    let renditions = AudioRenditions::new(mic, audio_codec, [AudioPreset::Hq]);
                    if let Err(err) = camera.lock().expect("poisoned").set_audio(Some(renditions)) {
                        tracing::warn!("failed to set audio: {err:#}");
                    }
                });
            } else {
                self.camera.lock().expect("poisoned").set_audio(None)?;
            }
        }
        Ok(())
    }

    #[cfg(not(any_audio_codec))]
    fn apply_audio(&mut self, _enable: bool, _capture: &CaptureConfig) -> Result<()> {
        Ok(())
    }
}
