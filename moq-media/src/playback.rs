//! The shared audio output device.
//!
//! `moq_audio::playback::Engine` owns one output device and mixes every sink
//! into it, so a process watching several broadcasts needs exactly one engine.
//! This module owns that one, opens it on first use, and lets a caller choose
//! or change the device it drives.

use std::sync::OnceLock;

use n0_error::Result;
use tracing::info;

use crate::subscribe::SubscribeError;

/// The process-wide output engine, opened on first use.
static ENGINE: OnceLock<moq_audio::playback::Engine> = OnceLock::new();

/// Lists the output devices the host offers.
///
/// The ids are host-qualified, such as `alsa:hw:0,0`, and go straight into
/// [`Config::device`](moq_audio::playback::Config::device).
///
/// # Errors
///
/// Fails if the host audio API cannot be queried.
pub async fn devices() -> Result<Vec<moq_audio::playback::Device>, SubscribeError> {
    Ok(moq_audio::playback::devices().await?)
}

/// Opens the shared engine on a chosen device.
///
/// Call this before subscribing if the default output is not the one you want.
/// A later call does nothing, since the engine is already open; use
/// [`switch`] to move to another device after that.
///
/// # Errors
///
/// Fails if the device cannot be opened.
pub async fn open(config: moq_audio::playback::Config) -> Result<(), SubscribeError> {
    if ENGINE.get().is_some() {
        return Ok(());
    }
    let opened = moq_audio::playback::Engine::open(config).await?;
    // A concurrent caller may have won the race; theirs is as good as ours, and
    // the loser's engine drops here, closing the device it opened.
    ENGINE.get_or_init(|| opened);
    Ok(())
}

/// Moves every playing track to another output device.
///
/// The sinks survive the move and are rebuilt at the new device's sample rate,
/// so a track keeps playing across it.
///
/// # Errors
///
/// Fails if the device cannot be opened, or if too many switches are already
/// in flight.
pub async fn switch(config: moq_audio::playback::Config) -> Result<(), SubscribeError> {
    Ok(engine().await?.switch(config).await?)
}

/// Returns the shared engine, opening the default device on first use.
pub(crate) async fn engine() -> Result<&'static moq_audio::playback::Engine, SubscribeError> {
    if let Some(engine) = ENGINE.get() {
        return Ok(engine);
    }
    info!("opening the default audio output");
    let opened = moq_audio::playback::Engine::open(Default::default()).await?;
    Ok(ENGINE.get_or_init(|| opened))
}

/// Builds an echo canceller tapped off the output mix.
///
/// Hand it to [`moq_audio::capture::Config::aec`] on the microphone that shares
/// a room with the speaker. Without it a handset on speakerphone publishes its
/// own output back to the peer, which is the one audio failure everybody
/// notices.
///
/// # Errors
///
/// Fails if the output device cannot be opened.
#[cfg(feature = "aec")]
pub async fn canceller(
    config: moq_audio::aec::Config,
) -> Result<moq_audio::aec::Canceller, SubscribeError> {
    Ok(engine().await?.canceller(config))
}
