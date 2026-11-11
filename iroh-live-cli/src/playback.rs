//! Playback helpers shared by the commands that play media.

use std::{sync::Arc, time::Duration};

use iroh_live::media::{
    AudioTarget, Catalog, Player, PlayerConfig, RemoteBroadcast, RenditionMode, video_rendition,
};
use n0_error::{Result, anyerr};
use tracing::info;
#[cfg(feature = "playback")]
use tracing::warn;

use crate::{args::PlaybackArgs, transport::Subscribed};

/// Returns a player config from `--decoder` and `--latency`, with audio to `output`.
pub fn player_config(args: &PlaybackArgs, output: Option<&AudioTarget>) -> PlayerConfig {
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
pub async fn open(sub: Subscribed, config: PlayerConfig) -> Result<(Subscribed, Player)> {
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
/// Fails if the broadcast closes first or sends none in time.
pub async fn catalog(broadcast: &RemoteBroadcast) -> Result<Arc<Catalog>> {
    let mut catalog = broadcast.catalog();
    let first = async {
        // A broadcast that closes sends no catalog update to wake on.
        tokio::select! {
            catalog = catalog.wait_for(Option::is_some) => catalog.ok().and_then(|catalog| catalog.clone()),
            () = broadcast.closed() => None,
        }
    };
    tokio::time::timeout(CATALOG_TIMEOUT, first)
        .await
        .map_err(|_| {
            anyerr!(
                "the broadcast sent no catalog within {}s, or sent one this build \
                 could not read (the log says which)",
                CATALOG_TIMEOUT.as_secs()
            )
        })?
        .ok_or_else(|| anyerr!("the broadcast closed before it described itself"))
}

/// Opens the audio output for playback.
///
/// Fails if `device` is set and does not open. Without `device`, a machine
/// with no speaker gets an output that discards audio, so video still plays.
#[allow(
    clippy::unused_async,
    reason = "a build without playback opens nothing, which is not async"
)]
pub async fn output(device: Option<String>) -> Result<AudioTarget> {
    #[cfg(feature = "playback")]
    {
        if let Some(device) = device {
            let output = open_engine(Some(device.clone())).await.map_err(|err| {
                anyerr!(
                    "cannot open audio output '{device}': {err:#}. \
                     Run `irl devices` for the ids this machine accepts"
                )
            })?;
            info!(%device, "audio output selected");
            return Ok(output.into());
        }
        match open_engine(None).await {
            Ok(output) => Ok(output.into()),
            Err(err) => {
                warn!(error = %format!("{err:#}"), "no audio output, playing silently");
                Ok(AudioTarget::null())
            }
        }
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = device;
        info!("this build has no playback support, so audio is decoded and discarded");
        Ok(AudioTarget::null())
    }
}

/// Opens the speaker engine for `device`, or the system default for `None`.
#[cfg(feature = "playback")]
async fn open_engine(
    device: Option<String>,
) -> Result<iroh_live::media::audio::playback::Engine, iroh_live::media::audio::Error> {
    let mut config = iroh_live::media::audio::playback::Config::default();
    config.device = device;
    iroh_live::media::audio::playback::Engine::open(config).await
}
