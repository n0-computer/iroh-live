use std::fmt;
use std::sync::{Arc, Mutex};

use moq_lite::BroadcastProducer;
use moq_media::{
    audio::AudioBackend,
    av::{AudioPreset, VideoEncoderKind, VideoPreset},
    capture::{CameraCapturer, ScreenCapturer},
    codec::OpusEncoder,
    publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
};
use n0_error::{AnyError, Result};
use tracing::{info, warn};

use crate::rooms::RoomHandle;

#[derive(Debug, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
enum Broadcasts {
    Camera,
    Screen,
}

#[derive(Debug)]
pub enum StreamKind {
    Camera,
    Screen,
    Microphone,
}

#[derive(Default, Clone, Debug)]
pub struct PublishOpts {
    pub camera: bool,
    pub screen: bool,
    pub audio: bool,
    /// If set, use a specific camera by index. If `None`, use the default camera.
    pub camera_index: Option<u32>,
    /// If set, use a specific audio input device by name. If `None`, use the system default.
    pub audio_device: Option<String>,
    /// Video encoder to use. If `None`, automatically selects the best available.
    pub video_codec: Option<VideoEncoderKind>,
}

/// Manager for publish broadcasts in a room
///
/// Synchronous version which spawns all async ops on new tokio tasks. Panics if methods are
/// not called in the context of a tokio runtime.
///
/// Why does this have sync methods? In UI land it is so much easier for the operations to be sync,
/// so this just spawns all async ops on tokio threads. Not yet sure about where this should evolve to
/// but this kept me moving for now.
pub struct RoomPublisherSync {
    audio_ctx: AudioBackend,
    room: RoomHandle,
    camera: Option<Arc<Mutex<PublishBroadcast>>>,
    screen: Option<Arc<Mutex<PublishBroadcast>>>,
    state: PublishOpts,
    prev_camera_index: Option<u32>,
    prev_audio_device: Option<String>,
    prev_video_codec: Option<VideoEncoderKind>,
}

impl fmt::Debug for RoomPublisherSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoomPublisherSync")
            .field("audio_ctx", &self.audio_ctx)
            .field("room", &self.room)
            .field("camera", &"..")
            .field("screen", &"..")
            .field("state", &self.state)
            .finish()
    }
}

impl RoomPublisherSync {
    pub fn new(room: RoomHandle, audio_ctx: AudioBackend) -> Self {
        Self {
            room,
            audio_ctx,
            camera: None,
            screen: None,
            state: Default::default(),
            prev_camera_index: None,
            prev_audio_device: None,
            prev_video_codec: None,
        }
    }

    pub fn set_audio_backend(&mut self, backend: AudioBackend) {
        self.audio_ctx = backend;
    }

    pub fn set_state(&mut self, state: &PublishOpts) -> Result<(), Vec<(StreamKind, AnyError)>> {
        info!(new=?state, old=?self.state, "set publish state");
        self.state.camera_index = state.camera_index;
        self.state.audio_device = state.audio_device.clone();
        self.state.video_codec = state.video_codec;
        let errors = [
            self.set_audio(state.audio)
                .err()
                .map(|e| (StreamKind::Microphone, e)),
            self.set_camera(state.camera)
                .err()
                .map(|e| (StreamKind::Camera, e)),
            self.set_screen(state.screen)
                .err()
                .map(|e| (StreamKind::Screen, e)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn state(&self) -> &PublishOpts {
        &self.state
    }

    pub fn camera(&self) -> bool {
        self.state.camera
    }

    pub fn camera_broadcast(&self) -> Option<Arc<Mutex<PublishBroadcast>>> {
        self.camera.clone()
    }

    pub fn screen_broadcast(&self) -> Option<Arc<Mutex<PublishBroadcast>>> {
        self.screen.clone()
    }

    pub fn set_camera(&mut self, enable: bool) -> Result<()> {
        let index_changed =
            enable && self.state.camera && self.state.camera_index != self.prev_camera_index;
        let codec_changed =
            enable && self.state.camera && self.state.video_codec != self.prev_video_codec;
        if self.state.camera != enable || index_changed || codec_changed {
            if enable {
                let camera = match self.state.camera_index {
                    Some(index) => CameraCapturer::with_index(index)?,
                    None => CameraCapturer::new()?,
                };
                let codec = self
                    .state
                    .video_codec
                    .unwrap_or_else(VideoEncoderKind::best_available);
                let renditions = VideoRenditions::new_for_codec(codec, camera, VideoPreset::all());
                self.ensure_camera();
                self.camera
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .set_video(Some(renditions))?;
            } else if let Some(camera) = self.camera.as_ref() {
                camera.lock().unwrap().set_video(None)?;
            }
            self.state.camera = enable;
            self.prev_camera_index = self.state.camera_index;
            self.prev_video_codec = self.state.video_codec;
        }
        Ok(())
    }

    pub fn screen(&self) -> bool {
        self.state.screen
    }

    fn ensure_camera(&mut self) {
        if self.camera.is_none() {
            let broadcast = PublishBroadcast::new();
            self.publish(Broadcasts::Camera, broadcast.producer());
            self.camera = Some(Arc::new(Mutex::new(broadcast)));
        };
    }

    fn publish(&self, name: Broadcasts, producer: BroadcastProducer) {
        let room = self.room.clone();
        tokio::spawn(async move {
            if let Err(err) = room.publish(name, producer).await {
                warn!("publish to room failed: {err:#}");
            }
        });
    }

    pub fn set_screen(&mut self, enable: bool) -> Result<()> {
        let codec_changed =
            enable && self.state.screen && self.state.video_codec != self.prev_video_codec;
        if self.state.screen != enable || codec_changed {
            if enable {
                if self.screen.is_none() {
                    let broadcast = PublishBroadcast::new();
                    self.publish(Broadcasts::Screen, broadcast.producer());
                    self.screen = Some(Arc::new(Mutex::new(broadcast)));
                };

                let screen = ScreenCapturer::new()?;
                let codec = self
                    .state
                    .video_codec
                    .unwrap_or_else(VideoEncoderKind::best_available);
                let renditions = VideoRenditions::new_for_codec(codec, screen, VideoPreset::all());
                self.screen
                    .as_mut()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .set_video(Some(renditions))?;
            } else {
                let _ = self.screen.take();
            }
            self.state.screen = enable;
            self.prev_video_codec = self.state.video_codec;
        }
        Ok(())
    }

    pub fn audio(&self) -> bool {
        self.state.audio
    }

    pub fn set_audio(&mut self, enable: bool) -> Result<()> {
        let device_changed =
            enable && self.state.audio && self.state.audio_device != self.prev_audio_device;
        if self.state.audio != enable || device_changed {
            if device_changed {
                // Disable current audio first — the caller is expected to have
                // already set a new AudioBackend via `set_audio_backend`.
                if let Some(camera) = self.camera.as_mut() {
                    camera.lock().unwrap().set_audio(None)?;
                }
            }
            if enable {
                self.ensure_camera();
                let camera = self.camera.as_ref().unwrap().clone();
                let audio_ctx = self.audio_ctx.clone();
                tokio::spawn(async move {
                    let mic = match audio_ctx.default_input().await {
                        Err(err) => {
                            warn!("failed to open audio input: {err:#}");
                            return;
                        }
                        Ok(mic) => mic,
                    };
                    let renditions = AudioRenditions::new::<OpusEncoder>(mic, [AudioPreset::Hq]);
                    if let Err(err) = camera.lock().unwrap().set_audio(Some(renditions)) {
                        warn!("failed to set audio: {err:#}");
                    }
                });
            } else if let Some(camera) = self.camera.as_mut() {
                camera.lock().unwrap().set_audio(None)?;
            }
            self.state.audio = enable;
            self.prev_audio_device = self.state.audio_device.clone();
        }
        Ok(())
    }
}
