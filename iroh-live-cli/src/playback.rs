//! Playback helpers shared by the commands that play media.

use std::{sync::Arc, time::Duration};

use iroh_live::media::{
    AudioMode, Catalog, Error, Player, PlayerConfig, RemoteBroadcast, RenditionMode,
    video_rendition,
};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

use crate::{args::PlaybackArgs, transport::Subscribed};

/// The speaker engine audio plays on.
pub(crate) type Speaker = iroh_live::media::audio::playback::Engine;

/// Returns a player config from `--decoder` and `--latency`, with audio on `speaker`.
pub(crate) fn player_config(args: &PlaybackArgs, speaker: Option<&Speaker>) -> PlayerConfig {
    let config = PlayerConfig::default()
        .with_decoder(args.decoder.into())
        .with_latency(args.latency.latency());
    audio_on(config, speaker)
}

/// Turns the audio of `config` on, playing on `speaker`.
///
/// Without a speaker, the audio is decoded and discarded.
pub(crate) fn audio_on(mut config: PlayerConfig, speaker: Option<&Speaker>) -> PlayerConfig {
    config.audio = speaker.cloned().map_or(AudioMode::Decode, AudioMode::Play);
    config
}

/// Waits for the catalog of `sub`'s broadcast and plays it with `config`.
///
/// A broadcast without a catalog fails here instead of opening a black
/// window, and so does a pinned rendition the catalog does not offer.
pub(crate) async fn open(sub: Subscribed, config: PlayerConfig) -> Result<(Subscribed, Player)> {
    let catalog = catalog(&sub.broadcast).await?;
    if let RenditionMode::Pinned(name) = &config.rendition {
        video_rendition(&catalog, name)?;
    }
    let player = sub.broadcast.play(config)?;
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
pub(crate) async fn open_speaker(device: Option<String>) -> Result<Option<Speaker>> {
    if let Some(device) = device {
        let speaker = open_engine(device.clone()).await.map_err(|err| {
            anyerr!(
                "cannot open audio output '{device}': {err:#}. \
                 Run `irl devices` for the ids this machine accepts"
            )
        })?;
        info!(%device, "audio output selected");
        return Ok(Some(speaker));
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

/// Opens the speaker engine for `device`.
async fn open_engine(
    device: String,
) -> Result<iroh_live::media::audio::playback::Engine, iroh_live::media::audio::Error> {
    let mut config = iroh_live::media::audio::playback::Config::default();
    config.device = Some(device);
    iroh_live::media::audio::playback::Engine::open(config).await
}
