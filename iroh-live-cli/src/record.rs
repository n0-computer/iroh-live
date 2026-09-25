//! `irl record`: subscribe to a remote broadcast and write it to a file.
//!
//! Recording is a remux rather than a transcode, and the media crate does it:
//! [`RemoteBroadcast::record`] reads encoded frames off the wire and writes
//! them into fragmented MP4 or Matroska with no decoder anywhere in the path.
//! What this adds is the command line: which file, which container, for how
//! long, and a progress line while it runs.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

use iroh_live::{
    BroadcastTicket, Live,
    media::{self, Catalog, RecordConfig, Recording, RemoteBroadcast},
};
use n0_error::{Result, anyerr};
use tokio::io::BufWriter;
use tracing::{info, warn};

use crate::{
    args::{RecordArgs, RecordFormat},
    transport,
};

/// How often the progress line is printed while a recording runs.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the `record` command.
pub fn run(args: RecordArgs, rt: &tokio::runtime::Runtime) -> Result {
    rt.block_on(record(args))
}

/// Connects, records until the broadcast ends or the user interrupts, and
/// closes the session.
async fn record(args: RecordArgs) -> Result {
    let ticket = args.remote.ticket()?;
    let options = options(&args)?;

    let live = transport::setup_live(false).await?;
    let result = record_on(&live, &ticket, &options).await;
    live.shutdown().await;
    result
}

/// Records one broadcast over `live`, which the caller closes either way.
///
/// # Errors
///
/// Fails if the broadcast cannot be subscribed to, carries nothing to record,
/// or the file cannot be written.
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

    // The broadcast follows its path in the route table, which is also where a
    // catalog rendition naming a sibling broadcast resolves.
    let recording = start(sub.broadcast(), &catalog, options).await?;
    match options.duration {
        Some(duration) => println!("recording for {}s ...", duration.as_secs()),
        None => println!("recording, press Ctrl+C to stop"),
    }
    let written = finish(recording, stop_after(options.duration)).await?;
    println!(
        "wrote {} to {}",
        format_bytes(written),
        options.path.display()
    );

    sub.close();
    Ok(())
}

/// Where a recording goes, and which of the broadcast's tracks it keeps.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// The file to write.
    pub path: PathBuf,
    /// The container to write it in.
    pub format: RecordFormat,
    /// The one video rendition to keep, or every one the catalog offers.
    pub rendition: Option<String>,
    /// How long a stalled group is waited for before the exporter skips it.
    pub latency: Duration,
    /// How long to record for, or until interrupted.
    pub duration: Option<Duration>,
}

impl RecordOptions {
    /// Records `path` in `format`, or in the container `path`'s extension
    /// names, keeping every rendition until the broadcast ends.
    ///
    /// # Errors
    ///
    /// Fails if neither `format` nor the extension names a container.
    pub fn new(path: impl Into<PathBuf>, format: Option<RecordFormat>) -> Result<Self> {
        let path = path.into();
        let format = match format {
            Some(format) => format,
            None => format_from_extension(&path).ok_or_else(|| unknown_extension(&path))?,
        };
        Ok(Self {
            path,
            format,
            rendition: None,
            latency: RecordConfig::default().max_age,
            duration: None,
        })
    }

    /// The media crate's config for these options.
    fn config(&self) -> RecordConfig {
        let format = match self.format {
            RecordFormat::Fmp4 => media::RecordFormat::Fmp4,
            RecordFormat::Mkv => media::RecordFormat::Mkv,
        };
        RecordConfig {
            format,
            rendition: self.rendition.clone(),
            max_age: self.latency,
        }
    }
}

/// Creates the output file and starts recording `broadcast` into it.
///
/// # Errors
///
/// Fails if the requested rendition is not in `catalog`, or if the output file
/// cannot be created.
pub async fn start(
    broadcast: &RemoteBroadcast,
    catalog: &Catalog,
    options: &RecordOptions,
) -> Result<Recording> {
    if let Some(name) = &options.rendition {
        catalog.video_rendition(name)?;
    }
    let file = tokio::fs::File::create(&options.path)
        .await
        .map_err(|err| anyerr!("failed to create {}: {err}", options.path.display()))?;
    info!(path = %options.path.display(), "recording started");
    Ok(broadcast.record(BufWriter::new(file), options.config())?)
}

/// Waits for `recording` to end, or finishes it once `stop` resolves, printing
/// progress as it goes; returns the bytes written.
///
/// The file is flushed either way, so an interrupted recording is still a
/// playable file: fragmented containers are complete at every chunk boundary.
///
/// # Errors
///
/// Fails on an export or a write error.
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
                format_bytes(recording.written())
            ),
        }
    };
    info!(bytes = written, "recording finished");
    Ok(written)
}

/// The options `args` describes.
///
/// # Errors
///
/// Fails if neither `--format` nor `--output`'s extension names a container.
fn options(args: &RecordArgs) -> Result<RecordOptions> {
    let mut options = RecordOptions::new(&args.output, args.format)?;
    options.rendition = args.rendition.clone();
    options.latency = Duration::from_millis(args.latency);
    options.duration = args.duration.map(Duration::from_secs);
    Ok(options)
}

/// The container `path`'s extension names, if it names one.
fn format_from_extension(path: &Path) -> Option<RecordFormat> {
    match media::RecordFormat::from_path(path)? {
        media::RecordFormat::Fmp4 => Some(RecordFormat::Fmp4),
        media::RecordFormat::Mkv => Some(RecordFormat::Mkv),
    }
}

/// The error for a path whose extension names no container.
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
            // Nothing else ends the recording, so wait for the interrupt alone.
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

/// Formats a byte count for the progress line.
fn format_bytes(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a byte count large enough to lose precision is not a figure anyone reads"
    )]
    let scaled = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1_048_576 {
        format!("{:.1} KiB", scaled / 1024.0)
    } else {
        format!("{:.1} MiB", scaled / 1_048_576.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::RemoteArgs;

    /// A remote nothing in these tests dials.
    fn remote_args() -> RemoteArgs {
        RemoteArgs {
            ticket: None,
            endpoint_id: None,
            broadcast_name: None,
        }
    }

    #[test]
    fn extensions_name_containers() {
        assert_eq!(
            format_from_extension(Path::new("out.mp4")),
            Some(RecordFormat::Fmp4)
        );
        // The case a shell completion or a Windows path might hand us.
        assert_eq!(
            format_from_extension(Path::new("out.MKV")),
            Some(RecordFormat::Mkv)
        );
        assert_eq!(format_from_extension(Path::new("out.avi")), None);
        assert_eq!(format_from_extension(Path::new("recording")), None);
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
        assert_eq!(options.format, RecordFormat::Mkv);
        assert_eq!(options.latency, Duration::from_millis(500));
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
