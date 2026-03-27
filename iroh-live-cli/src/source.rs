//! Shared capture source setup: configure video and audio sources on a broadcast.

#[cfg(all(target_os = "linux", feature = "capture"))]
use std::path::PathBuf;

use iroh_live::media::{
    AudioBackend,
    capture::{CameraCapturer, CameraConfig, CaptureBackend, ScreenCapturer, ScreenConfig},
    codec::{AudioCodec, VideoCodec},
    format::{AudioPreset, VideoPreset},
    publish::LocalBroadcast,
    test_sources::TestPatternSource,
};
use moq_media::test_sources::TestToneSource;
#[cfg(all(target_os = "linux", feature = "capture"))]
use tracing::{debug, warn};

use crate::args::{AudioSourceSpec, BackendRef, DeviceRef, VideoSourceSpec};

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

/// Resolves a [`BackendRef`] to a concrete [`CaptureBackend`].
fn resolve_backend(r: &BackendRef, available: &[CaptureBackend]) -> anyhow::Result<CaptureBackend> {
    match r {
        BackendRef::Name(b) => Ok(*b),
        BackendRef::Index(idx) => available.get(*idx).copied().ok_or_else(|| {
            let names: Vec<_> = available.iter().map(|b| b.cli_name()).collect();
            anyhow::anyhow!(
                "backend index {idx} out of range (available: {names:?}). \
                 Run `irl devices` to list backends."
            )
        }),
    }
}

/// Resolves a camera from [`BackendRef`] + [`DeviceRef`].
///
/// Uses `list_all()` so that non-preferred backends (nokhwa, pipewire when
/// v4l2 is also compiled) are reachable. Once the camera is identified we
/// open it directly via `with_config()` to avoid re-listing through the
/// preferred-backend-only `open()` path.
fn resolve_camera(
    backend: Option<&BackendRef>,
    device: Option<&DeviceRef>,
) -> anyhow::Result<CameraCapturer> {
    let cameras = CameraCapturer::list_all()?;
    let resolved_backend = backend
        .map(|b| resolve_backend(b, &CameraCapturer::list_backends()))
        .transpose()?;

    // Filter by backend if specified.
    let filtered: Vec<_> = if let Some(b) = resolved_backend {
        cameras.iter().filter(|c| c.backend == b).collect()
    } else {
        cameras.iter().collect()
    };

    let info = match device {
        Some(DeviceRef::Index(idx)) => filtered.get(*idx).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "camera index {idx} out of range ({} available). \
                 Run `irl devices` to list cameras.",
                filtered.len()
            )
        })?,
        Some(DeviceRef::Name(name)) => filtered
            .iter()
            .find(|c| c.id == name.as_str() || c.name == name.as_str())
            .copied()
            .ok_or_else(|| {
                let available: Vec<String> = filtered.iter().map(|c| c.summary()).collect();
                anyhow::anyhow!(
                    "camera '{name}' not found. Available:\n  {}",
                    available.join("\n  ")
                )
            })?,
        None => filtered.first().copied().ok_or_else(|| {
            if let Some(b) = resolved_backend {
                anyhow::anyhow!("no cameras on backend {b}. Run `irl devices` to list cameras.",)
            } else {
                anyhow::anyhow!("no cameras available. Run `irl devices` to list cameras.")
            }
        })?,
    };

    CameraCapturer::with_config(Some(info), &CameraConfig::default())
}

/// Resolves a screen from [`BackendRef`] + [`DeviceRef`] to `(backend, id)`.
///
/// Uses `list_all()` so non-preferred backends are reachable, then returns
/// the concrete backend + id pair for `setup_screen_source`.
fn resolve_screen(
    backend: Option<&BackendRef>,
    device: Option<&DeviceRef>,
) -> anyhow::Result<(Option<CaptureBackend>, Option<String>)> {
    let screens = ScreenCapturer::list_all()?;
    let resolved_backend = backend
        .map(|b| resolve_backend(b, &ScreenCapturer::list_backends()))
        .transpose()?;

    let filtered: Vec<_> = if let Some(b) = resolved_backend {
        screens.iter().filter(|s| s.backend == b).collect()
    } else {
        screens.iter().collect()
    };

    match device {
        Some(DeviceRef::Index(idx)) => {
            let info = filtered.get(*idx).ok_or_else(|| {
                anyhow::anyhow!(
                    "screen index {idx} out of range ({} available). \
                     Run `irl devices` to list screens.",
                    filtered.len()
                )
            })?;
            Ok((Some(info.backend), Some(info.id.clone())))
        }
        Some(DeviceRef::Name(name)) => {
            let info = filtered
                .iter()
                .find(|s| s.id == name.as_str() || s.name == name.as_str())
                .ok_or_else(|| {
                    let available: Vec<String> = filtered.iter().map(|s| s.summary()).collect();
                    anyhow::anyhow!(
                        "screen '{name}' not found. Available:\n  {}",
                        available.join("\n  ")
                    )
                })?;
            Ok((Some(info.backend), Some(info.id.clone())))
        }
        None => Ok((resolved_backend, None)),
    }
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
            VideoSourceSpec::Camera { backend, device } => {
                let cam = resolve_camera(backend.as_ref(), device.as_ref())?;
                broadcast.video().set_source(cam, codec, presets.to_vec())?;
            }
            VideoSourceSpec::DefaultScreen => {
                setup_screen_source(broadcast, None, None, codec, presets)?;
            }
            VideoSourceSpec::Screen { backend, device } => {
                let (b, id) = resolve_screen(backend.as_ref(), device.as_ref())?;
                setup_screen_source(broadcast, b, id.as_deref(), codec, presets)?;
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
