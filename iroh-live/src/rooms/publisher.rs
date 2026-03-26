use moq_lite::BroadcastProducer;
use moq_media::{
    audio_backend::AudioBackend,
    publish::{PublishCaptureController, PublishOpts, PublishUpdateError},
};
use n0_error::Result;
use tracing::warn;

use crate::rooms::RoomHandle;

#[derive(Debug, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
enum Broadcasts {
    Camera,
    Screen,
}

/// Manager for publish broadcasts in a room.
///
/// Thin wrapper around [`PublishCaptureController`] that connects broadcast
/// producers to the room transport. All capture/encode logic lives in
/// `moq-media`.
#[derive(Debug)]
pub struct RoomPublisherSync {
    controller: PublishCaptureController,
    room: RoomHandle,
}

impl RoomPublisherSync {
    /// Creates a new publisher and immediately publishes a camera broadcast into the room.
    pub fn new(room: RoomHandle, audio_ctx: AudioBackend) -> Self {
        let controller = PublishCaptureController::new(audio_ctx);
        // Publish the main (camera + audio) broadcast to the room eagerly.
        let producer = controller.camera_producer();
        tokio::spawn({
            let room = room.clone();
            async move {
                if let Err(err) = room.publish_producer(Broadcasts::Camera, producer).await {
                    warn!("publish camera to room failed: {err:#}");
                }
            }
        });
        Self { controller, room }
    }

    /// Applies new publish options, updating capture and encoding configuration.
    pub fn set_state(&mut self, opts: &PublishOpts) -> Result<(), PublishUpdateError> {
        match self.controller.set_opts(opts.clone()) {
            Ok(update) => {
                self.handle_new_screen(update.new_screen);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Returns the current publish options.
    pub fn state(&self) -> PublishOpts {
        self.controller.state()
    }

    fn handle_new_screen(&self, new_screen: Option<BroadcastProducer>) {
        if let Some(producer) = new_screen {
            let room = self.room.clone();
            tokio::spawn(async move {
                if let Err(err) = room.publish_producer(Broadcasts::Screen, producer).await {
                    warn!("publish screen to room failed: {err:#}");
                }
            });
        }
    }
}
