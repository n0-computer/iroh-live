//! Recording a broadcast to a file, remuxed rather than transcoded.
//!
//! `moq_mux`'s container exporters read encoded frames off the wire and write
//! them into fragmented MP4 or Matroska with no decoder in the path. They also
//! build the container's decoder configuration from the catalog, turning an
//! `avc3` track with inline parameter sets into the `avc1` shape a player
//! expects, so nothing here has to understand H.264 framing.
//!
//! What this settles once is the pair of settings that differ between a
//! recorder and a player: a recording starts from the oldest group the track
//! still holds rather than at the live edge, and it waits much longer for a
//! stalled group before skipping it.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use moq_mux::{
    catalog::{CatalogFormat, Stream as _},
    container::{fmp4, mkv},
    select,
};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info};

use crate::{RemoteBroadcast, error::Error};

/// The path a recording serves a bare broadcast under, on its private route
/// table.
const LOCAL_PATH: &str = "recorded";

/// A container to record into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordFormat {
    /// Fragmented MP4, complete at every fragment boundary.
    #[default]
    Fmp4,
    /// Matroska.
    Mkv,
}

impl RecordFormat {
    /// Returns the container a file name's extension names, if it names one.
    ///
    /// `.mp4`, `.m4v` and `.m4s` are fragmented MP4; `.mkv` and `.webm` are
    /// Matroska. Case does not matter.
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        match path.as_ref().extension()?.to_str()?.to_lowercase().as_str() {
            "mp4" | "m4v" | "m4s" => Some(Self::Fmp4),
            "mkv" | "webm" => Some(Self::Mkv),
            _ => None,
        }
    }
}

/// How to record.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RecordConfig {
    /// The container.
    pub format: RecordFormat,
    /// The one video rendition to keep, or every one the catalog offers.
    pub rendition: Option<String>,
    /// How long a stalled group is waited for before it is skipped.
    ///
    /// Generous next to what a player allows: a recording would rather buffer
    /// a late group than drop it.
    pub max_age: Duration,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            format: RecordFormat::default(),
            rendition: None,
            max_age: Duration::from_secs(2),
        }
    }
}

impl RecordConfig {
    /// Returns the config writing `format`.
    #[must_use]
    pub fn with_format(mut self, format: RecordFormat) -> Self {
        self.format = format;
        self
    }

    /// Returns the config keeping only the video rendition `name`.
    #[must_use]
    pub fn with_rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into());
        self
    }

    /// Returns the config waiting `max_age` for a stalled group.
    #[must_use]
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }
}

/// A recording in progress.
///
/// Started by [`RemoteBroadcast::record`]. Dropping it stops the recording
/// without flushing; [`stop`](Self::stop) and [`wait`](Self::wait) finish it.
#[derive(Debug)]
pub struct Recording {
    stop: CancellationToken,
    written: Arc<AtomicU64>,
    task: AbortOnDropHandle<Result<u64, Error>>,
    /// The bytes written, once the task finished cleanly, so a second wait
    /// returns them rather than polling a finished task.
    finished: Option<Result<u64, ()>>,
}

impl Recording {
    pub(crate) fn start(
        broadcast: &RemoteBroadcast,
        out: Box<dyn AsyncWrite + Send + Unpin>,
        config: RecordConfig,
    ) -> Result<Self, Error> {
        if let Some(name) = &config.rendition
            && let Some(catalog) = broadcast.catalog().get()
            && catalog.video_rendition(name).is_none()
        {
            return Err(n0_error::e!(Error::UnknownRendition { name: name.clone() }));
        }
        let stop = CancellationToken::new();
        let written = Arc::new(AtomicU64::new(0));
        let span = tracing::info_span!(parent: broadcast.span(), "record");
        let task = n0_future::task::spawn(
            record(
                broadcast.clone(),
                out,
                config,
                stop.clone(),
                written.clone(),
            )
            .instrument(span),
        );
        Ok(Self {
            stop,
            written,
            task: AbortOnDropHandle::new(task),
            finished: None,
        })
    }

    /// Returns how many bytes have been written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// Waits until the broadcast ends and the file is finished, and returns the
    /// bytes written.
    ///
    /// Cancellation safe in the sense that nothing is lost by dropping it:
    /// the recording runs until the [`Recording`] itself is dropped. Calling
    /// it again after it returned gives the same byte count.
    ///
    /// # Errors
    ///
    /// Fails on an export or a write error. After a failure has been returned
    /// once, later calls return [`Error::Closed`].
    pub async fn wait(&mut self) -> Result<u64, Error> {
        if let Some(finished) = self.finished {
            return finished.map_err(|()| n0_error::e!(Error::Closed));
        }
        let result = (&mut self.task)
            .await
            .map_err(|err| Error::device_msg(format!("the recording task failed: {err}")))
            .and_then(|result| result);
        self.finished = Some(result.as_ref().map(|written| *written).map_err(|_| ()));
        result
    }

    /// Ends the file at the last complete fragment and returns the bytes
    /// written.
    ///
    /// The fragment the exporter is still assembling is not written: upstream
    /// has no way to close one early. Fragmented containers are complete at
    /// every fragment boundary, so the file plays up to that point.
    ///
    /// Not cancellation safe: dropping the future before it resolves stops the
    /// recording without flushing the writer.
    ///
    /// # Errors
    ///
    /// Fails on an export or a write error.
    pub async fn stop(mut self) -> Result<u64, Error> {
        self.stop.cancel();
        self.wait().await
    }
}

/// The container exporter, one variant per [`RecordFormat`].
enum Export {
    Fmp4(Box<fmp4::Export<CatalogStream>>),
    Mkv(Box<mkv::Export<CatalogStream>>),
}

/// The catalog stream that drives an exporter, narrowed to what is recorded.
type CatalogStream = moq_mux::catalog::Select<moq_mux::catalog::Consumer>;

impl Export {
    async fn next(&mut self) -> Result<Option<Bytes>, Error> {
        match self {
            Self::Fmp4(export) => export.next().await.map_err(Error::catalog),
            Self::Mkv(export) => export.next().await.map_err(Error::catalog),
        }
    }
}

/// Keeps every audio rendition, and every video rendition or only `rendition`.
fn selection(rendition: Option<&str>) -> select::Broadcast {
    let mut video = select::Video::default();
    if let Some(name) = rendition {
        video = video.name(name);
    }
    select::Broadcast::default()
        .video(video)
        .audio(select::Audio::default())
}

/// Serves a bare broadcast on a private route table, for the exporter, which
/// resolves renditions through one.
///
/// A rendition that names a sibling broadcast cannot resolve here: a bare
/// consumer has no siblings to find. A broadcast that follows a route table
/// records through that one instead.
struct LocalOrigin {
    origin: moq_net::origin::Producer,
    _tasks: [AbortOnDropHandle<()>; 2],
}

impl LocalOrigin {
    fn serve(broadcast: moq_net::broadcast::Consumer) -> Result<Self, Error> {
        let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
        let dynamic = origin
            .dynamic(LOCAL_PATH, moq_net::origin::Route::default())
            .map_err(Error::transport)?;
        let driver = n0_future::task::spawn(async move {
            let err = moq_net::time::run(driver).await;
            debug!(error = %err, "the recording's route table stopped");
        });
        let handler = n0_future::task::spawn(async move {
            while let Ok(request) = dynamic.requested_broadcast().await {
                request.accept(broadcast.clone());
            }
        });
        Ok(Self {
            origin,
            _tasks: [
                AbortOnDropHandle::new(driver),
                AbortOnDropHandle::new(handler),
            ],
        })
    }
}

/// Records until the broadcast ends or `stop` is cancelled.
async fn record(
    broadcast: RemoteBroadcast,
    mut out: Box<dyn AsyncWrite + Send + Unpin>,
    config: RecordConfig,
    stop: CancellationToken,
    written: Arc<AtomicU64>,
) -> Result<u64, Error> {
    // The broadcast as it is now: a recording follows one route, and a change
    // of route is the end of the file.
    let mut epoch = broadcast.epoch();
    let consumer = loop {
        if let Some(consumer) = epoch.get().consumer {
            break consumer;
        }
        tokio::select! {
            updated = epoch.updated() => {
                if updated.is_err() {
                    return Err(n0_error::e!(Error::Closed));
                }
            }
            () = stop.cancelled() => return Ok(0),
        }
    };
    let (_local, source) = match broadcast.routed() {
        Some((origin, path)) => (None, moq_mux::Source::new(origin, path)),
        None => {
            let local = LocalOrigin::serve(consumer.clone())?;
            let source = moq_mux::Source::new(local.origin.consume(), LOCAL_PATH);
            (Some(local), source)
        }
    };
    let catalog = moq_mux::catalog::Consumer::<()>::new(&consumer, CatalogFormat::default())
        .await
        .map_err(Error::catalog)?
        .select(selection(config.rendition.as_deref()));
    let mut export = match config.format {
        RecordFormat::Fmp4 => Export::Fmp4(Box::new(
            fmp4::Export::new(source, catalog).with_max_age(config.max_age),
        )),
        RecordFormat::Mkv => Export::Mkv(Box::new(
            mkv::Export::new(source, catalog).with_max_age(config.max_age),
        )),
    };
    info!(format = ?config.format, "recording started");

    loop {
        let chunk = tokio::select! {
            chunk = export.next() => chunk?,
            () = stop.cancelled() => None,
        };
        let Some(chunk) = chunk else { break };
        out.write_all(&chunk).await?;
        written.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    out.flush().await?;
    out.shutdown().await?;
    let total = written.load(Ordering::Relaxed);
    info!(bytes = total, "recording finished");
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_name_containers() {
        assert_eq!(RecordFormat::from_path("out.mp4"), Some(RecordFormat::Fmp4));
        assert_eq!(RecordFormat::from_path("out.MKV"), Some(RecordFormat::Mkv));
        assert_eq!(RecordFormat::from_path("out.avi"), None);
        assert_eq!(RecordFormat::from_path("recording"), None);
    }
}
