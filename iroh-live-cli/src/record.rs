//! `irl record` — subscribe to a remote broadcast and write it to a file.
//!
//! Recording is a remux rather than a transcode: `moq_mux`'s container
//! exporter reads encoded frames off the wire and writes them into fragmented
//! MP4 or Matroska with no decoder anywhere in the path. The exporter also
//! builds the container's decoder configuration from the catalog, turning an
//! `avc3` track with inline parameter sets into the `avc1` shape a player
//! expects, so nothing here has to understand H.264 framing.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use bytes::Bytes;
use iroh_live::{media::subscribe::RemoteBroadcast, moq::MoqSession};
use moq_mux::{
    catalog::{CatalogFormat, Stream as _},
    container::{fmp4, mkv},
    select,
};
use n0_error::{Result, StdResultExt, anyerr};
use tokio::io::{AsyncWriteExt, BufWriter};
use tracing::{info, warn};

use crate::{
    args::{RecordArgs, RecordFormat},
    transport::setup_live,
};

/// How often the progress line is printed while a recording runs.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// The fragment cap handed to the fMP4 exporter.
///
/// Not a preference: left unset, that exporter rolls a fragment only on a video
/// keyframe, so on a broadcast carrying both media the audio track accumulates
/// in memory and never reaches the file. The recording then has an audio track
/// declared in its `moov` and no samples behind it. A cap rolls both tracks on
/// elapsed media time instead, which is what makes the audio land. Half a
/// second is the usual CMAF chunk, and it costs one extra `moof` per fragment
/// against a GOP that would otherwise have been written whole.
const FRAGMENT_DURATION: Duration = Duration::from_millis(500);

/// Runs the `record` command.
pub fn run(args: RecordArgs, rt: &tokio::runtime::Runtime) -> Result {
    rt.block_on(record(args))
}

/// Connects, records until the broadcast ends or the user interrupts, and
/// closes the session.
async fn record(args: RecordArgs) -> Result {
    let ticket = args.ticket()?;
    let options = options(&args)?;

    println!("connecting to {ticket} ...");
    let live = setup_live(false).await?;
    let sub = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;
    info!("session established");

    let catalog = sub.broadcast().catalog();
    println!(
        "catalog: {} video, {} audio renditions",
        catalog.video().len(),
        catalog.audio().len()
    );
    if catalog.video().is_empty() && catalog.audio().is_empty() {
        return Err(anyerr!("the broadcast carries no video and no audio"));
    }

    let recorder = Recorder::open(sub.session(), sub.broadcast(), &options).await?;
    match options.duration {
        Some(duration) => println!("recording for {}s ...", duration.as_secs()),
        None => println!("recording, press Ctrl+C to stop"),
    }
    let written = recorder.run(stop_after(options.duration)).await?;
    println!(
        "wrote {} to {}",
        format_bytes(written),
        options.path.display()
    );

    sub.broadcast().shutdown();
    sub.session().close(moq_net::Error::Cancel);
    live.shutdown().await;
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

/// How long a stalled group is waited for before the exporter skips it, when
/// no caller says otherwise. Generous next to a player's budget: a recording
/// would rather buffer a late group than drop it.
const DEFAULT_LATENCY: Duration = Duration::from_secs(2);

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
            latency: DEFAULT_LATENCY,
            duration: None,
        })
    }
}

/// The catalog stream that drives an exporter: the broadcast's own catalog,
/// narrowed to the renditions this recording keeps.
type CatalogStream = moq_mux::catalog::Select<moq_mux::catalog::Consumer>;

/// The container exporter, one variant per [`RecordFormat`].
///
/// Both are large enough that clippy objects to an unboxed enum, and both are
/// built once per recording, so the indirection costs nothing that matters.
enum Export {
    Fmp4(Box<fmp4::Export<CatalogStream>>),
    Mkv(Box<mkv::Export<CatalogStream>>),
}

impl Export {
    /// Returns the next container chunk, or `None` once every track has ended.
    async fn next(&mut self) -> Result<Option<Bytes>> {
        match self {
            Self::Fmp4(export) => export.next().await.anyerr(),
            Self::Mkv(export) => export.next().await.anyerr(),
        }
    }
}

/// A recording that has subscribed to its tracks and opened its output file.
pub struct Recorder {
    export: Export,
    file: BufWriter<tokio::fs::File>,
    path: PathBuf,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Subscribes to the tracks `options` keeps and creates the output file.
    ///
    /// The exporter takes the session's origin rather than the broadcast we
    /// already hold, because a catalog rendition may name a sibling broadcast
    /// and only the origin can resolve that reference.
    ///
    /// # Errors
    ///
    /// Fails if the catalog track cannot be subscribed to, or if the output
    /// file cannot be created.
    pub async fn open(
        session: &MoqSession,
        broadcast: &RemoteBroadcast,
        options: &RecordOptions,
    ) -> Result<Self> {
        let source = moq_mux::Source::new(session.announced().clone(), broadcast.name());
        // A second subscription to the catalog track: `moq_mux` drives its
        // exporters from a `catalog::Stream`, and `RemoteBroadcast` publishes
        // its snapshots through an `n0_watcher` instead, which no adapter
        // bridges. The track carries only the JSON manifest.
        let catalog =
            moq_mux::catalog::Consumer::<()>::new(broadcast.consumer(), CatalogFormat::default())
                .await
                .anyerr()?
                .select(selection(options.rendition.as_deref()));

        let export = match options.format {
            RecordFormat::Fmp4 => Export::Fmp4(Box::new(
                fmp4::Export::new(source, catalog)
                    .with_latency(options.latency)
                    .with_fragment_duration(FRAGMENT_DURATION),
            )),
            RecordFormat::Mkv => Export::Mkv(Box::new(
                mkv::Export::new(source, catalog).with_latency(options.latency),
            )),
        };

        let file = tokio::fs::File::create(&options.path)
            .await
            .map_err(|err| anyerr!("failed to create {}: {err}", options.path.display()))?;

        Ok(Self {
            export,
            file: BufWriter::new(file),
            path: options.path.clone(),
        })
    }

    /// Writes container chunks until the broadcast ends or `stop` resolves,
    /// and returns the number of bytes written.
    ///
    /// The file is flushed either way, so an interrupted recording is still a
    /// playable file: fragmented containers are complete at every chunk
    /// boundary.
    ///
    /// # Errors
    ///
    /// Fails on an export or a write error, having flushed nothing further.
    pub async fn run(mut self, stop: impl Future<Output = ()>) -> Result<u64> {
        info!(path = %self.path.display(), "recording started");
        let started = Instant::now();
        let mut reported = started;
        let mut written = 0u64;
        let mut stop = std::pin::pin!(stop);

        loop {
            let chunk = tokio::select! {
                chunk = self.export.next() => chunk?,
                () = &mut stop => None,
            };
            let Some(chunk) = chunk else { break };

            self.file.write_all(&chunk).await?;
            written += chunk.len() as u64;

            if reported.elapsed() >= REPORT_INTERVAL {
                reported = Instant::now();
                println!(
                    "[{:.0}s] {}",
                    started.elapsed().as_secs_f64(),
                    format_bytes(written)
                );
            }
        }

        self.file.flush().await?;
        info!(path = %self.path.display(), bytes = written, "recording finished");
        Ok(written)
    }
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
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "mp4" | "m4v" | "m4s" => Some(RecordFormat::Fmp4),
        "mkv" | "webm" => Some(RecordFormat::Mkv),
        _ => None,
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

/// Keeps every audio rendition, and either every video rendition or only the
/// one `rendition` names.
fn selection(rendition: Option<&str>) -> select::Broadcast {
    let mut video = select::Video::default();
    if let Some(name) = rendition {
        video = video.name(name);
    }
    select::Broadcast::default()
        .video(video)
        .audio(select::Audio::default())
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
            ticket: None,
            endpoint_id: None,
            broadcast_name: None,
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
            ticket: None,
            endpoint_id: None,
            broadcast_name: None,
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
