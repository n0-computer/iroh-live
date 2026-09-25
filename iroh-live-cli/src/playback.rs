//! Playback helpers shared by the commands that play media.

use std::time::Duration;

use iroh_live::media::{AudioOutput, Catalog, RemoteBroadcast};
use n0_error::{Result, anyerr};
use tracing::info;

/// How long a broadcast's catalog may take to arrive once subscribed.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Waits for the broadcast's first catalog.
///
/// Fails if the broadcast closes first or sends none in time.
pub async fn catalog(broadcast: &RemoteBroadcast) -> Result<Catalog> {
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
pub async fn output(device: Option<String>) -> Result<AudioOutput> {
    #[cfg(feature = "playback")]
    {
        if let Some(device) = device {
            let output = AudioOutput::open(Some(device.clone()))
                .await
                .map_err(|err| {
                    anyerr!(
                        "cannot open audio output '{device}': {err:#}. \
                     Run `irl devices` for the ids this machine accepts"
                    )
                })?;
            info!(%device, "audio output selected");
            return Ok(output);
        }
        match AudioOutput::open(None).await {
            Ok(output) => Ok(output),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "no audio output, playing silently");
                Ok(AudioOutput::null())
            }
        }
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = device;
        info!("this build has no playback support, so audio is decoded and discarded");
        Ok(AudioOutput::null())
    }
}
