//! Playback helpers shared by the commands that play media.

use std::{sync::Arc, time::Duration};

use iroh_live::media::{
    AudioTarget, Catalog, Error, Player, PlayerConfig, RemoteBroadcast, RenditionMode,
    video_rendition,
};
use n0_error::{Result, anyerr};
use tracing::info;

use crate::{args::PlaybackArgs, transport::Subscribed};

/// Returns a player config from `--decoder` and `--latency`, with audio to `output`.
pub(crate) fn player_config(args: &PlaybackArgs, output: Option<&AudioTarget>) -> PlayerConfig {
    PlayerConfig {
        decoder: args.decoder.into(),
        latency: args.latency.latency(),
        audio: output.cloned(),
        ..PlayerConfig::default()
    }
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

/// Opens the audio output for playback.
///
/// Without `device`, a machine with no speaker gets an output that discards
/// audio, so video still plays.
///
/// # Errors
///
/// Fails if `device` is set and does not open.
#[allow(
    clippy::unused_async,
    reason = "a build without playback opens nothing, which is not async"
)]
pub(crate) async fn output(device: Option<String>) -> Result<AudioTarget> {
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
            return Ok(output.into());
        }
        Ok(AudioTarget::default_or_null().await)
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = device;
        info!("this build has no playback support, so audio is decoded and discarded");
        Ok(AudioTarget::null())
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
