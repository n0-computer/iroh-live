//! Shared capture source setup: configure video and audio sources on a broadcast.

#[cfg(all(target_os = "linux", feature = "capture"))]
use std::path::PathBuf;

use iroh_live::media::{
    AudioBackend,
    capture::{CameraCapturer, CameraConfig, ScreenCapturer, ScreenConfig},
    codec::{AudioCodec, VideoCodec},
    format::{AudioPreset, VideoPreset},
    publish::LocalBroadcast,
    test_sources::TestPatternSource,
};
use moq_media::test_sources::TestToneSource;
#[cfg(all(target_os = "linux", feature = "capture"))]
use tracing::{debug, warn};

use crate::args::{AudioSourceSpec, VideoSourceSpec};

// ---------------------------------------------------------------------------
// PipeWire restore token persistence
// ---------------------------------------------------------------------------

/// Returns the path to the PipeWire restore token file, creating the
/// parent directory if it does not exist.
#[cfg(all(target_os = "linux", feature = "capture"))]
fn restore_token_path() -> Option<PathBuf> {
    let dir = dirs::data_dir()?.join("iroh-live");
    if !dir.exists()
        && let Err(e) = std::fs::create_dir_all(&dir)
    {
        warn!(path = %dir.display(), "failed to create data dir: {e}");
        return None;
    }
    Some(dir.join("pipewire_restore_token"))
}

/// Loads a previously saved PipeWire restore token, if one exists.
#[cfg(all(target_os = "linux", feature = "capture"))]
fn load_restore_token() -> Option<String> {
    let path = restore_token_path()?;
    match std::fs::read_to_string(&path) {
        Ok(token) if !token.trim().is_empty() => {
            debug!(path = %path.display(), "loaded PipeWire restore token");
            Some(token.trim().to_string())
        }
        _ => None,
    }
}

/// Saves a PipeWire restore token to disk for reuse on next startup.
#[cfg(all(target_os = "linux", feature = "capture"))]
fn save_restore_token(token: &str) {
    if let Some(path) = restore_token_path() {
        if let Err(e) = std::fs::write(&path, token) {
            warn!(path = %path.display(), "failed to save PipeWire restore token: {e}");
        } else {
            debug!(path = %path.display(), "saved PipeWire restore token");
        }
    }
}

/// Opens a screen capturer and passes it to `set_source`. On Linux with
/// PipeWire, loads any stored restore token first and saves the one the
/// portal returns, so subsequent launches skip the screen-picker dialog.
#[cfg(all(target_os = "linux", feature = "capture"))]
fn setup_screen_source(
    broadcast: &LocalBroadcast,
    backend: Option<iroh_live::media::capture::CaptureBackend>,
    id: Option<&str>,
    codec: VideoCodec,
    presets: &[VideoPreset],
) -> anyhow::Result<()> {
    use iroh_live::media::capture::{CaptureBackend, PipeWireScreenCapturer};

    let is_pipewire = match backend {
        Some(CaptureBackend::PipeWire) => true,
        None => ScreenCapturer::list_backends()
            .first()
            .is_some_and(|b| *b == CaptureBackend::PipeWire),
        _ => false,
    };

    if is_pipewire && id.is_none() {
        let config = ScreenConfig {
            pipewire_restore_token: load_restore_token(),
            ..Default::default()
        };
        let capturer = PipeWireScreenCapturer::new(&config)?;
        if let Some(token) = capturer.pipewire_restore_token() {
            save_restore_token(token);
        }
        broadcast
            .video()
            .set_source(capturer, codec, presets.to_vec())?;
    } else {
        let screen = ScreenCapturer::open(backend, id, &ScreenConfig::default())?;
        broadcast
            .video()
            .set_source(screen, codec, presets.to_vec())?;
    }
    Ok(())
}

#[cfg(not(all(target_os = "linux", feature = "capture")))]
fn setup_screen_source(
    broadcast: &LocalBroadcast,
    backend: Option<iroh_live::media::capture::CaptureBackend>,
    id: Option<&str>,
    codec: VideoCodec,
    presets: &[VideoPreset],
) -> anyhow::Result<()> {
    let screen = ScreenCapturer::open(backend, id, &ScreenConfig::default())?;
    broadcast
        .video()
        .set_source(screen, codec, presets.to_vec())?;
    Ok(())
}

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
                setup_screen_source(broadcast, None, None, codec, presets)?;
            }
            VideoSourceSpec::Screen { backend, id } => {
                setup_screen_source(broadcast, *backend, id.as_deref(), codec, presets)?;
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
