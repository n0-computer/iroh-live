//! Shared capture source setup: configure video and audio sources on a broadcast.

use iroh_live::media::{
    AudioBackend,
    capture::{CameraCapturer, CameraConfig, ScreenCapturer, ScreenConfig},
    codec::{AudioCodec, VideoCodec},
    format::{AudioPreset, VideoPreset},
    publish::LocalBroadcast,
    test_sources::TestPatternSource,
};
use moq_media::test_sources::TestToneSource;

use crate::args::{AudioSourceSpec, VideoSourceSpec};

/// Configures video sources on the broadcast from parsed CLI specs.
pub fn setup_video(
    broadcast: &LocalBroadcast,
    sources: &[VideoSourceSpec],
    codec: VideoCodec,
    presets: &[VideoPreset],
) -> anyhow::Result<()> {
    for source in sources {
        match source {
            VideoSourceSpec::None => {}
            VideoSourceSpec::Test => {
                let (w, h) = presets
                    .first()
                    .copied()
                    .unwrap_or(VideoPreset::P720)
                    .dimensions();
                broadcast.video().set_source(
                    TestPatternSource::new(w, h),
                    codec,
                    presets.to_vec(),
                )?;
            }
            VideoSourceSpec::DefaultCamera => {
                broadcast
                    .video()
                    .set_source(CameraCapturer::new()?, codec, presets.to_vec())?;
            }
            VideoSourceSpec::Camera { backend, id } => {
                let cam = CameraCapturer::open(*backend, id.as_deref(), &CameraConfig::default())?;
                broadcast.video().set_source(cam, codec, presets.to_vec())?;
            }
            VideoSourceSpec::DefaultScreen => {
                broadcast
                    .video()
                    .set_source(ScreenCapturer::new()?, codec, presets.to_vec())?;
            }
            VideoSourceSpec::Screen { backend, id } => {
                let screen =
                    ScreenCapturer::open(*backend, id.as_deref(), &ScreenConfig::default())?;
                broadcast
                    .video()
                    .set_source(screen, codec, presets.to_vec())?;
            }
        }
    }
    Ok(())
}

/// Configures audio sources on the broadcast from parsed CLI specs.
pub async fn setup_audio(
    broadcast: &LocalBroadcast,
    sources: &[AudioSourceSpec],
    audio_ctx: &AudioBackend,
    preset: AudioPreset,
) -> anyhow::Result<()> {
    for source in sources {
        match source {
            AudioSourceSpec::None => {}
            AudioSourceSpec::Test => {
                let audio = TestToneSource::new();
                broadcast.audio().set(audio, AudioCodec::Opus, [preset])?;
            }
            AudioSourceSpec::Default => {
                let mic = audio_ctx.default_input().await?;
                broadcast.audio().set(mic, AudioCodec::Opus, [preset])?;
            }
            AudioSourceSpec::Device(id) => {
                let device_id = id.parse().map_err(|e| {
                    anyhow::anyhow!(
                        "invalid audio device '{id}': {e}. Run `irl devices` to list devices."
                    )
                })?;
                audio_ctx.switch_input(Some(device_id)).await?;
                let mic = audio_ctx.default_input().await?;
                broadcast.audio().set(mic, AudioCodec::Opus, [preset])?;
            }
        }
    }
    Ok(())
}
