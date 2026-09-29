//! Publishes a media file.
//!
//! `moq_mux` demuxes the file and writes its tracks and catalog onto the
//! broadcast without decoding. `--transcode` runs the file through ffmpeg
//! first. A plain (non-fragmented) MP4 needs this, and so does `:loop`.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use bytes::BytesMut;
use n0_error::{Result, StdResultExt, anyerr};
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::{info, warn};

use crate::args::{ImportFormat, PublishArgs};

/// A `file:` video source and the flags that say how to read it.
#[derive(Debug, Clone)]
pub struct FileSource {
    path: PathBuf,
    format: ImportFormat,
    transcode: bool,
    looping: bool,
}

impl FileSource {
    /// Creates a source for `path` with the other publish flags applied.
    ///
    /// Fails if `path` is not a file, or if `looping` is set without
    /// `--transcode`. Only ffmpeg can repeat the input.
    pub fn new(path: PathBuf, looping: bool, args: &PublishArgs) -> Result<Self> {
        if !path.is_file() {
            return Err(anyerr!(
                "no readable file at {}; --video takes a path, as in \
                 --video file:clip.mp4",
                path.display()
            ));
        }
        if looping && !args.transcode {
            return Err(anyerr!(
                "file:{}:loop needs --transcode: ffmpeg is what repeats the \
                 input, and without it the file is published once and ends",
                path.display()
            ));
        }
        Ok(Self {
            path,
            format: args.format,
            transcode: args.transcode,
            looping,
        })
    }
}

/// A byte stream feeding an importer.
type Input = Pin<Box<dyn AsyncRead + Send + 'static>>;

/// The importer for one container format.
///
/// Annex-B H.264 has no container framing, so a splitter recovers the
/// access-unit boundaries first.
enum Importer {
    Avc3 {
        split: Box<moq_mux::codec::h264::Split>,
        import: Box<moq_mux::codec::h264::Import>,
    },
    Container(Box<moq_mux::import::ContainerStream>),
}

impl Importer {
    /// Feeds a chunk of the byte stream.
    fn decode(&mut self, chunk: &[u8]) -> Result<()> {
        match self {
            Self::Avc3 { split, import } => {
                let frames = split.decode(chunk, None).anyerr()?;
                import.decode(frames).anyerr()?;
            }
            Self::Container(container) => container.decode(chunk).anyerr()?,
        }
        Ok(())
    }

    /// Flushes the trailing frame and closes the tracks.
    ///
    /// The Annex-B splitter holds the last access unit until the next start
    /// code, so end of input must flush it.
    fn finish(&mut self) -> Result<()> {
        match self {
            Self::Avc3 { split, import } => {
                let tail = split.flush(None).anyerr()?;
                import.decode(tail).anyerr()?;
                import.finish().anyerr()?;
            }
            Self::Container(container) => container.finish().anyerr()?,
        }
        Ok(())
    }

    /// Aborts the tracks with `err`, so subscribers see the real cause.
    fn abort(self, err: moq_net::Error) {
        match self {
            Self::Avc3 { import, .. } => import.abort(err),
            Self::Container(container) => container.abort(err),
        }
    }
}

/// A file import that has parsed its header and is ready to run.
#[derive(derive_more::Debug)]
pub struct FileImport {
    #[debug(skip)]
    importer: Importer,
    #[debug(skip)]
    input: Input,
}

impl FileImport {
    /// Opens the file and publishes its tracks onto `broadcast`.
    ///
    /// Reads far enough to publish the catalog before returning, so an early
    /// subscriber finds the tracks. Fails if the file cannot be opened, ffmpeg
    /// is needed and missing, or the header does not match `--format`.
    pub async fn open(
        mut broadcast: moq_net::broadcast::Producer,
        source: FileSource,
    ) -> Result<Self> {
        let mut input = open_input(&source).await?;
        let catalog =
            moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).anyerr()?;

        let mut importer = match source.format {
            ImportFormat::Avc3 => {
                let track = broadcast
                    .unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video))
                    .anyerr()?;
                let import =
                    moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())
                        .anyerr()?;
                Importer::Avc3 {
                    split: Box::new(moq_mux::codec::h264::Split::new()),
                    import: Box::new(import),
                }
            }
            ImportFormat::Fmp4 => Importer::Container(Box::new(
                moq_mux::import::ContainerStream::new(
                    broadcast,
                    catalog.reserve(),
                    moq_mux::import::ContainerFormat::Fmp4,
                )
                .anyerr()?,
            )),
        };

        let read = match read_header(&mut importer, &mut input, &catalog, &source).await {
            Ok(read) => read,
            Err(err) => {
                // Tell subscribers that already arrived why the tracks end.
                importer.abort(moq_net::Error::Transport(err.to_string()));
                return Err(err);
            }
        };
        info!(bytes = read, "file header parsed, catalog published");

        Ok(Self { importer, input })
    }

    /// Reads the rest of the file, publishing as it goes.
    ///
    /// On a read or demux error, aborts the tracks with it and returns it.
    pub async fn run(self) -> Result<()> {
        let Self {
            mut importer,
            mut input,
        } = self;
        let mut buffer = BytesMut::new();

        let outcome: Result<()> = async {
            loop {
                buffer.clear();
                if input.read_buf(&mut buffer).await? == 0 {
                    return Ok(());
                }
                importer.decode(&buffer)?;
            }
        }
        .await;

        let outcome = outcome.and_then(|()| importer.finish());
        if let Err(err) = &outcome {
            importer.abort(moq_net::Error::Transport(err.to_string()));
        }
        outcome
    }
}

/// Feeds the importer until it publishes a catalog, and returns the bytes read.
///
/// fMP4 needs the moov box. Annex-B needs a keyframe with SPS and PPS. Fails
/// if the file ends first, which is how an unreadable container shows up.
async fn read_header(
    importer: &mut Importer,
    input: &mut Input,
    catalog: &moq_mux::catalog::Producer,
    source: &FileSource,
) -> Result<usize> {
    let mut buffer = BytesMut::new();
    let mut read = 0usize;
    while catalog_is_empty(catalog) {
        buffer.clear();
        let chunk = input.read_buf(&mut buffer).await?;
        if chunk == 0 {
            return Err(anyerr!(
                "reached the end of {} after {read} bytes without finding a {:?} header{}",
                source.path.display(),
                source.format,
                if source.transcode {
                    ""
                } else {
                    "; if this is a plain MP4, re-run with --transcode"
                }
            ));
        }
        read += chunk;
        importer.decode(&buffer)?;
    }
    Ok(read)
}

/// Returns whether the catalog still has no rendition.
fn catalog_is_empty(catalog: &moq_mux::catalog::Producer) -> bool {
    let catalog = catalog.snapshot();
    catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty()
}

/// Opens the file, optionally behind an ffmpeg transcode.
async fn open_input(source: &FileSource) -> Result<Input> {
    if source.transcode {
        return Ok(Box::pin(transcode_file(source).await?));
    }
    let file = tokio::fs::File::open(&source.path)
        .await
        .map_err(|err| anyerr!("failed to open {}: {err}", source.path.display()))?;
    Ok(Box::pin(file))
}

/// Spawns ffmpeg to remux or re-encode the source to stdout.
///
/// A background task awaits the child. Otherwise ffmpeg lingers as a zombie
/// after it exits on SIGPIPE.
async fn transcode_file(source: &FileSource) -> Result<impl AsyncRead + use<>> {
    let input = source.path.clone();
    let copy_video = is_h264(&input).await?;

    let mut command = tokio::process::Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error"]);
    if source.looping {
        command.args(["-stream_loop", "-1"]);
    }
    // `-re` paces the input at its own frame rate, like a live source.
    command.args(["-re", "-i"]);
    command.arg(input.as_os_str());

    if copy_video {
        info!("input is H.264 already, copying the video stream");
        command.args(["-c:v", "copy"]);
    } else {
        info!("input is not H.264, re-encoding");
        command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    }

    match source.format {
        ImportFormat::Fmp4 => {
            command.args(["-c:a", "libopus", "-b:a", "128k"]);
            command.args([
                "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
                "-f",
                "mp4",
            ]);
        }
        ImportFormat::Avc3 => {
            command.args(["-an", "-bsf:v", "h264_mp4toannexb", "-f", "h264"]);
        }
    }
    command.arg("-");

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| anyerr!("failed to spawn ffmpeg, is it installed? {err}"))?;
    let stdout = child.stdout.take().expect("stdout was piped");

    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if !status.success() => {
                warn!(code = ?status.code(), "ffmpeg exited with a non-zero status");
            }
            Err(err) => warn!(error = %err, "failed to wait on the ffmpeg child"),
            Ok(_) => {}
        }
    });

    Ok(stdout)
}

/// Returns whether the first video stream of the file is H.264.
async fn is_h264(input: &Path) -> Result<bool> {
    let output = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
        ])
        .arg(input.as_os_str())
        .output()
        .await
        .map_err(|err| anyerr!("failed to run ffprobe, is ffmpeg installed? {err}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "h264")
}
