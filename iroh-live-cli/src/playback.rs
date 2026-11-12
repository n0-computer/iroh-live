//! Playback helpers shared by the commands that play media.

use std::{sync::Arc, time::Duration};

use iroh_live::media::{
    Catalog, Error, Player, PlayerConfig, RemoteBroadcast, RenditionMode, video_rendition,
};
use n0_error::{Result, anyerr};
use tracing::info;
#[cfg(feature = "playback")]
use tracing::warn;

use crate::{args::PlaybackArgs, transport::Subscribed};

/// The speaker engine audio plays on.
#[cfg(feature = "playback")]
pub(crate) type Speaker = iroh_live::media::audio::playback::Engine;
/// The speaker of a build without playback, which never has one.
///
/// Not `Copy`, unlike `Infallible`, so the `.clone()` the playback build needs
/// reads the same in both.
#[cfg(not(feature = "playback"))]
#[derive(Debug, Clone)]
pub(crate) enum Speaker {}

/// Returns a player config from `--decoder` and `--latency`, with audio on `speaker`.
pub(crate) fn player_config(args: &PlaybackArgs, speaker: Option<&Speaker>) -> PlayerConfig {
    let config = PlayerConfig {
        decoder: args.decoder.into(),
        latency: args.latency.latency(),
        ..PlayerConfig::default()
    };
    audio_on(config, speaker)
}

/// Turns the audio of `config` on, playing on `speaker`.
///
/// Without a speaker, the audio is decoded and discarded.
pub(crate) fn audio_on(mut config: PlayerConfig, speaker: Option<&Speaker>) -> PlayerConfig {
    config.audio = true;
    #[cfg(feature = "playback")]
    {
        config.speaker = speaker.cloned();
    }
    #[cfg(not(feature = "playback"))]
    let _ = speaker;
    config
}

/// Waits for the catalog of `sub`'s broadcast and plays it with `config`.
///
/// A broadcast without a catalog fails here instead of opening a black
/// window, and so does a pinned rendition the catalog does not offer.
pub(crate) async fn open(sub: Subscribed, config: PlayerConfig) -> Result<(Subscribed, Player)> {
    let catalog = catalog(sub.broadcast()).await?;
    if let RenditionMode::Pinned(name) = &config.rendition {
        video_rendition(&catalog, name)?;
    }
    let player = sub.broadcast().play(config)?;
    Ok((sub, player))
}

/// How long a broadcast's catalog may take to arrive once subscribed.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Waits for the broadcast's first catalog.
///
/// # Errors
///
/// Fails if the broadcast closes first or sends none in time.
pub(crate) async fn catalog(broadcast: &RemoteBroadcast) -> Result<Arc<Catalog>> {
    broadcast
        .catalog_within(CATALOG_TIMEOUT)
        .await
        .map_err(|err| match err {
            Error::Closed { .. } => anyerr!("the broadcast closed before it described itself"),
            other => anyerr!("{other:#} (the log says why a catalog was not readable)"),
        })
}

/// Opens the speaker for playback.
///
/// Without `device`, a machine with no speaker gets none, and its audio is
/// discarded, so video still plays.
///
/// # Errors
///
/// Fails if `device` is set and does not open.
#[allow(
    clippy::unused_async,
    reason = "a build without playback opens nothing, which is not async"
)]
pub(crate) async fn output(device: Option<String>) -> Result<Option<Speaker>> {
    #[cfg(feature = "playback")]
    {
        if let Some(device) = device {
            let output = open_engine(device.clone()).await.map_err(|err| {
                anyerr!(
                    "cannot open audio output '{device}': {err:#}. \
                     Run `irl devices` for the ids this machine accepts"
                )
            })?;
            info!(%device, "audio output selected");
            return Ok(Some(output));
        }
        let config = iroh_live::media::audio::playback::Config::default();
        match iroh_live::media::audio::playback::Engine::open(config).await {
            Ok(engine) => Ok(Some(engine)),
            Err(err) => {
                warn!(
                    error = format_args!("{err:#}"),
                    "no audio output, playing audio nowhere"
                );
                Ok(None)
            }
        }
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = device;
        info!("this build has no playback support, so audio is decoded and discarded");
        Ok(None)
    }
}

/// Opens the speaker engine for `device`.
#[cfg(feature = "playback")]
async fn open_engine(
    device: String,
) -> Result<iroh_live::media::audio::playback::Engine, iroh_live::media::audio::Error> {
    let mut config = iroh_live::media::audio::playback::Config::default();
    config.device = Some(device);
    iroh_live::media::audio::playback::Engine::open(config).await
}
