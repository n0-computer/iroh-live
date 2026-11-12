//! `irl record`: subscribes to a remote broadcast and writes it to a file.
//!
//! moq-mux's container exporters remux the encoded frames into fragmented MP4
//! or Matroska without decoding, and build the container's decoder
//! configuration from the catalog. [`RemoteBroadcast::mux_source`] is their
//! input. This module adds the flags and a progress line.
//!
//! A recording starts from the oldest group the track still holds instead of
//! the live edge, and waits longer than a player for a stalled group.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

use bytes::Bytes;
use bytesize::ByteSize;
use iroh_live::{
    BroadcastTicket, Node,
    media::{Catalog, RemoteBroadcast, video_rendition},
};
use moq_mux::{
    catalog::{CatalogFormat, Stream as _},
    container::{fmp4, mkv},
    select,
};
use n0_error::{Result, anyerr};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
};
use tracing::{info, warn};

use crate::{args::RecordArgs, transport};

/// How often the progress line is printed while a recording runs.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the `record` command.
pub(crate) fn run(args: RecordArgs, rt: &tokio::runtime::Runtime) -> Result {
    rt.block_on(record(args))
}

/// Records until the broadcast ends or the user interrupts.
async fn record(args: RecordArgs) -> Result {
    let ticket = args.remote.ticket()?;
    let options = options(&args)?;

    let node = transport::bind_node(false).await?;
    let result = record_on(&node, &ticket, &options).await;
    node.shutdown().await;
    result
}

/// Records one broadcast over `node`. The caller shuts `node` down.
async fn record_on(node: &Node, ticket: &BroadcastTicket, options: &RecordOptions) -> Result {
    let sub = transport::subscribe(node, ticket).await?;

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

/// A container to record into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RecordFormat {
    /// Fragmented MP4, complete at every fragment boundary.
    #[default]
    Fmp4,
    /// Matroska.
    Mkv,
}

impl RecordFormat {
    /// Returns the container a file name's extension names, if it names one.
    ///
    /// `.mp4`, `.m4v` and `.m4s` are fragmented MP4. `.mkv` and `.webm` are
    /// Matroska. Case does not matter.
    pub(crate) fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        match path.as_ref().extension()?.to_str()?.to_lowercase().as_str() {
            "mp4" | "m4v" | "m4s" => Some(Self::Fmp4),
            "mkv" | "webm" => Some(Self::Mkv),
            _ => None,
        }
    }
}

/// Where a recording goes, what it keeps, and for how long.
#[derive(Debug, Clone)]
pub(crate) struct RecordOptions {
    /// The file to write.
    pub(crate) path: PathBuf,
    /// The container.
    pub(crate) format: RecordFormat,
    /// The one video rendition to keep, or every one the catalog offers.
    pub(crate) rendition: Option<String>,
    /// How long a stalled group is waited for before it is skipped.
    ///
    /// This is longer than a player allows, because a recording would sooner
    /// buffer a late group than drop it.
    pub(crate) max_age: Duration,
    /// How long to record, or `None` to record until interrupted.
    pub(crate) duration: Option<Duration>,
}

impl RecordOptions {
    /// Creates options that record every rendition to `path` until the end.
    ///
    /// Without `format`, the container comes from the extension of `path`.
    /// Fails if neither names a container.
    pub(crate) fn new(path: impl Into<PathBuf>, format: Option<RecordFormat>) -> Result<Self> {
        let path = path.into();
        let format = match format {
            Some(format) => format,
            None => RecordFormat::from_path(&path).ok_or_else(|| unknown_extension(&path))?,
        };
        Ok(Self {
            path,
            format,
            rendition: None,
            max_age: Duration::from_secs(2),
            duration: None,
        })
    }
}

/// A recording in progress: a container export and the file it writes.
pub(crate) struct Recording {
    export: Export,
    out: BufWriter<File>,
    written: u64,
}

/// The container exporter, one variant per [`RecordFormat`].
enum Export {
    Fmp4(Box<fmp4::Export<CatalogStream>>),
    Mkv(Box<mkv::Export<CatalogStream>>),
}

/// The catalog stream that drives an exporter, narrowed to what is recorded.
type CatalogStream = moq_mux::catalog::Select<moq_mux::catalog::Consumer>;

impl Export {
    /// Returns the next chunk of the file, `None` once the export is complete.
    async fn next(&mut self) -> Result<Option<Bytes>> {
        let chunk = match self {
            Self::Fmp4(export) => export.next().await,
            Self::Mkv(export) => export.next().await,
        };
        chunk.map_err(|err| anyerr!("the export failed: {err}"))
    }
}

/// Creates the output file and the export that records `broadcast` into it.
///
/// The export keeps every audio rendition, and every video rendition or the
/// one the options name. Fails if that rendition is not in `catalog`, the
/// broadcast closes, or the file cannot be created.
pub(crate) async fn start(
    broadcast: &RemoteBroadcast,
    catalog: &Catalog,
    options: &RecordOptions,
) -> Result<Recording> {
    let mut video = select::Video::default();
    if let Some(name) = &options.rendition {
        video_rendition(catalog, name)?;
        video = video.name(name);
    }
    let selection = select::Broadcast::default()
        .video(video)
        .audio(select::Audio::default());
    let source = broadcast.mux_source().await?;
    let catalog =
        moq_mux::catalog::Consumer::<()>::new(&source.broadcast(), CatalogFormat::default())
            .await
            .map_err(|err| anyerr!("the catalog did not read: {err}"))?
            .select(selection);
    let export = match options.format {
        RecordFormat::Fmp4 => Export::Fmp4(Box::new(
            fmp4::Export::new(source, catalog).with_max_delay(options.max_age),
        )),
        RecordFormat::Mkv => Export::Mkv(Box::new(
            mkv::Export::new(source, catalog).with_delay(options.max_age),
        )),
    };
    let file = File::create(&options.path)
        .await
        .map_err(|err| anyerr!("failed to create {}: {err}", options.path.display()))?;
    info!(path = %options.path.display(), format = ?options.format, "recording started");
    Ok(Recording {
        export,
        out: BufWriter::new(file),
        written: 0,
    })
}

/// Runs `recording` until the broadcast ends or `stop` resolves, and returns the bytes written.
///
/// Prints progress while it runs. The file is flushed either way. A stop
/// ends the file at the last complete fragment, since the exporter cannot
/// close one early, and a fragmented container plays up to there.
pub(crate) async fn finish(
    mut recording: Recording,
    stop: impl Future<Output = ()>,
) -> Result<u64> {
    let started = tokio::time::Instant::now();
    let mut report = tokio::time::interval_at(started + REPORT_INTERVAL, REPORT_INTERVAL);
    let mut stop = std::pin::pin!(stop);
    loop {
        let chunk = tokio::select! {
            chunk = recording.export.next() => chunk?,
            () = &mut stop => None,
            _ = report.tick() => {
                println!(
                    "[{:.0}s] {}",
                    started.elapsed().as_secs_f64(),
                    ByteSize(recording.written)
                );
                continue;
            }
        };
        let Some(chunk) = chunk else { break };
        recording.out.write_all(&chunk).await?;
        recording.written += chunk.len() as u64;
    }
    recording.out.flush().await?;
    recording.out.shutdown().await?;
    info!(bytes = recording.written, "recording finished");
    Ok(recording.written)
}

/// Builds the options from the command-line arguments.
fn options(args: &RecordArgs) -> Result<RecordOptions> {
    let mut options = RecordOptions::new(&args.output, args.format)?;
    options.rendition = args.rendition.clone();
    options.max_age = Duration::from_millis(args.max_age);
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
    // A failed Ctrl+C listener never resolves, so the deadline still applies.
    let interrupted = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(
                error = %err,
                "cannot listen for Ctrl+C, recording until the duration or the broadcast ends"
            );
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        () = interrupted => {}
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
            max_age: 500,
        };
        let options = options(&args).expect("--format names the container");
        assert_eq!(options.format, RecordFormat::Mkv);
        assert_eq!(options.max_age, Duration::from_millis(500));
    }

    #[test]
    fn an_unknown_extension_without_a_flag_is_rejected() {
        let args = RecordArgs {
            remote: remote_args(),
            output: PathBuf::from("out.avi"),
            format: None,
            rendition: None,
            duration: None,
            max_age: 2_000,
        };
        let err = options(&args).expect_err("nothing names the container");
        assert!(err.to_string().contains("--format"), "unexpected: {err}");
    }

    #[test]
    fn extensions_name_containers() {
        assert_eq!(RecordFormat::from_path("out.mp4"), Some(RecordFormat::Fmp4));
        assert_eq!(RecordFormat::from_path("out.MKV"), Some(RecordFormat::Mkv));
        assert_eq!(RecordFormat::from_path("out.avi"), None);
        assert_eq!(RecordFormat::from_path("recording"), None);
    }
}
