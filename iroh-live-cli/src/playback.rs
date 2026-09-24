//! The playback pieces every command that plays something shares.

use std::time::Duration;

use iroh_live::media::{AudioOutput, Catalog, RemoteBroadcast};
use n0_error::{Result, anyerr};
use n0_watcher::Watcher as _;
use tracing::info;

/// How long a broadcast's catalog may take to arrive once subscribed.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Waits for the broadcast's first catalog.
///
/// # Errors
///
/// Fails if the broadcast closes before sending one, or sends none in time.
pub async fn catalog(broadcast: &RemoteBroadcast) -> Result<Catalog> {
    let mut catalog = broadcast.catalog();
    let closed = || anyerr!("the broadcast closed before it described itself");
    tokio::time::timeout(CATALOG_TIMEOUT, async {
        loop {
            if let Some(known) = catalog.get() {
                return Ok(known);
            }
            // The close is waited for beside the update: a broadcast that
            // closes without a word sends no update to wake on.
            tokio::select! {
                updated = catalog.updated() => if updated.is_err() {
                    return Err(closed());
                },
                () = broadcast.closed() => return Err(closed()),
            }
        }
    })
    .await
    .map_err(|_| {
        anyerr!(
            "the broadcast sent no catalog within {}s, or sent one this build \
             could not read (the log says which)",
            CATALOG_TIMEOUT.as_secs()
        )
    })?
}

/// Opens the speaker players play through.
///
/// A device named explicitly has to open. Without one, a machine with no
/// speaker still shows the picture, through an output that discards.
///
/// # Errors
///
/// Fails if `device` names a device that will not open.
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
