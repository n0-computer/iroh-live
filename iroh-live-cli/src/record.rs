//! `irl record`: subscribes to a remote broadcast and writes it to a file.
//!
//! [`RemoteBroadcast::record`] remuxes the encoded frames into fragmented MP4
//! or Matroska without decoding. This module adds the flags and a progress
//! line.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

use bytesize::ByteSize;
use iroh_live::{
    BroadcastTicket, Live,
    media::{Catalog, RecordConfig, RecordFormat, Recording, RemoteBroadcast},
};
use n0_error::{Result, anyerr};
use tokio::io::BufWriter;
use tracing::{info, warn};

use crate::{args::RecordArgs, transport};

/// How often the progress line is printed while a recording runs.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the `record` command.
pub fn run(args: RecordArgs, rt: &tokio::runtime::Runtime) -> Result {
    rt.block_on(record(args))
}

/// Records until the broadcast ends or the user interrupts.
async fn record(args: RecordArgs) -> Result {
    let ticket = args.remote.ticket()?;
    let options = options(&args)?;

    let live = transport::setup_live(false).await?;
    let result = record_on(&live, &ticket, &options).await;
    live.shutdown().await;
    result
}

/// Records one broadcast over `live`. The caller shuts `live` down.
async fn record_on(live: &Live, ticket: &BroadcastTicket, options: &RecordOptions) -> Result {
    let sub = transport::subscribe(live, ticket).await?;

    let catalog = crate::playback::catalog(sub.broadcast()).await?;
    println!(
        "catalog: {} video, {} audio renditions",
        catalog.video.renditions.len(),
        catalog.audio.renditions.len()
    );
    if catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty() {
        return Err(anyerr!(
            "the broadcast carries no video and no audio, so there is nothing \
             to record"
        ));
    }

    // The subscribed broadcast records through its route table, so a rendition
    // that names a sibling broadcast resolves too.
    let recording = start(sub.broadcast(), &catalog, options).await?;
    match options.duration {
        Some(duration) => println!("recording for {}s ...", duration.as_secs()),
        None => println!("recording, press Ctrl+C to stop"),
    }
    let written = finish(recording, stop_after(options.duration)).await?;
    println!("wrote {} to {}", ByteSize(written), options.path.display());

    sub.close();
    Ok(())
}

/// Where a recording goes, what it keeps, and for how long.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// The file to write.
    pub path: PathBuf,
    /// The container, rendition and stall limit.
    pub config: RecordConfig,
    /// How long to record, or `None` to record until interrupted.
    pub duration: Option<Duration>,
}

impl RecordOptions {
    /// Creates options that record every rendition to `path` until the end.
    ///
    /// Without `format`, the container comes from the extension of `path`.
    /// Fails if neither names a container.
    pub fn new(path: impl Into<PathBuf>, format: Option<RecordFormat>) -> Result<Self> {
        let path = path.into();
        let format = match format {
            Some(format) => format,
            None => RecordFormat::from_path(&path).ok_or_else(|| unknown_extension(&path))?,
        };
        Ok(Self {
            path,
            config: RecordConfig {
                format,
                ..RecordConfig::default()
            },
            duration: None,
        })
    }
}

/// Creates the output file and starts recording `broadcast` into it.
///
/// Fails if the requested rendition is not in `catalog` or the file cannot be
/// created.
pub async fn start(
    broadcast: &RemoteBroadcast,
    catalog: &Catalog,
    options: &RecordOptions,
) -> Result<Recording> {
    if let Some(name) = &options.config.rendition {
        catalog.video_rendition(name)?;
    }
    let file = tokio::fs::File::create(&options.path)
        .await
        .map_err(|err| anyerr!("failed to create {}: {err}", options.path.display()))?;
    info!(path = %options.path.display(), "recording started");
    Ok(broadcast.record(BufWriter::new(file), options.config.clone())?)
}

/// Runs `recording` until it ends or `stop` resolves, and returns the bytes written.
///
/// Prints progress while it runs. The file is flushed either way, and a
/// fragmented container is playable at every chunk boundary.
pub async fn finish(mut recording: Recording, stop: impl Future<Output = ()>) -> Result<u64> {
    let started = tokio::time::Instant::now();
    let mut report = tokio::time::interval_at(started + REPORT_INTERVAL, REPORT_INTERVAL);
    let mut stop = std::pin::pin!(stop);
    let written = loop {
        tokio::select! {
            result = recording.wait() => break result?,
            () = &mut stop => break recording.stop().await?,
            _ = report.tick() => println!(
                "[{:.0}s] {}",
                started.elapsed().as_secs_f64(),
                ByteSize(recording.written())
            ),
        }
    };
    info!(bytes = written, "recording finished");
    Ok(written)
}

/// Builds the options from the command-line arguments.
fn options(args: &RecordArgs) -> Result<RecordOptions> {
    let mut options = RecordOptions::new(&args.output, args.format)?;
    options.config.rendition = args.rendition.clone();
    options.config.max_age = Duration::from_millis(args.latency);
    options.duration = args.duration.map(Duration::from_secs);
    Ok(options)
}

/// Returns the error for a path whose extension names no container.
fn unknown_extension(path: &Path) -> n0_error::AnyError {
    anyerr!(
        "cannot tell which container {} should be, so pass --format fmp4 or --format mkv; \
         the extensions recognised here are .mp4, .m4v, .m4s, .mkv, and .webm",
        path.display()
    )
}

/// Resolves when the user interrupts, or once `duration` has elapsed.
async fn stop_after(duration: Option<Duration>) {
    let deadline = async {
        match duration {
            Some(duration) => tokio::time::sleep(duration).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(err) = result {
                warn!(error = %err, "cannot listen for Ctrl+C, recording until the broadcast ends");
                std::future::pending::<()>().await;
            }
        }
        () = deadline => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::RemoteArgs;

    /// Returns empty remote arguments. No test dials them.
    fn remote_args() -> RemoteArgs {
        RemoteArgs {
            ticket: None,
            endpoint_id: None,
            broadcast_name: None,
        }
    }

    #[test]
    fn options_prefer_the_flag_over_the_extension() {
        let args = RecordArgs {
            remote: remote_args(),
            output: PathBuf::from("out.avi"),
            format: Some(RecordFormat::Mkv),
            rendition: None,
            duration: None,
            latency: 500,
        };
        let options = options(&args).expect("--format names the container");
        assert_eq!(options.config.format, RecordFormat::Mkv);
        assert_eq!(options.config.max_age, Duration::from_millis(500));
    }

    #[test]
    fn an_unknown_extension_without_a_flag_is_rejected() {
        let args = RecordArgs {
            remote: remote_args(),
            output: PathBuf::from("out.avi"),
            format: None,
            rendition: None,
            duration: None,
            latency: 2_000,
        };
        let err = options(&args).expect_err("nothing names the container");
        assert!(err.to_string().contains("--format"), "unexpected: {err}");
    }
}
