//! Opens the sources that specifiers name, and sets them on a broadcast.
//!
//! Sources open before they reach the broadcast, so a missing device is an
//! error here and not a retry loop in the log.

use std::{path::Path, sync::Mutex, time::Duration};

use iroh_live::media::{
    AudioFormat, AudioSource, FileEnd, LocalBroadcast, MediaFile, Rate, VideoFileConfig,
    VideoSource, audio, audio_encoding, video,
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::{EncodedVideo, RpicamConfig, rpicam_h264, video::Size};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::Backend, source_spec::RpicamMode};
use crate::{
    args::{AudioCodecArg, CaptureArgs},
    playback::Speaker,
    rendition::{self, CaptureFramerate},
    source_spec::{AudioSourceSpec, VideoOpenOptions, VideoSourceSpec},
};

/// Capture size of the Raspberry Pi camera without `--width` and `--height`.
///
/// A Pi Zero 2 W streams this comfortably.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_SIZE: Size = Size {
    width: 640,
    height: 360,
};

/// A keyframe interval past which a passthrough file keeps new viewers waiting.
const LONG_GOP: Duration = Duration::from_secs(4);

/// An opened video source.
#[derive(Debug)]
enum Video {
    /// Raw pictures, encoded into the ladder.
    Raw(VideoSource),
    /// Pre-encoded H.264, published as it is.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Encoded(EncodedVideo),
    /// A file, published as it is.
    File(MediaFile),
}

/// Opens the video and audio `args` asks for, and sets them on `broadcast`.
///
/// The microphone cancels the echo of `speaker`. Without `--audio`, a `file:`
/// video brings its own audio, passed through or decoded as the video is.
/// A file without video publishes its audio alone. Fails for a ladder that
/// does not parse, or a source that does not open.
/// The broadcast keeps the sources, and `video_source` hands the video back
/// for a preview.
pub(crate) async fn configure(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
    speaker: Option<&Speaker>,
) -> Result<()> {
    let opened = open_video(args).await?;
    let own_audio = match (&args.audio, args.video_source()) {
        (None, VideoSourceSpec::File { path, looping }) => {
            opened_file(&path).map(|file| file.with_end(FileEnd::looping(looping)))
        }
        _ => None,
    };
    if let Some(file) = &own_audio
        && !args.reencodes()?
    {
        // Publishes the video and the audio, whichever passes through.
        return broadcast
            .set_encoded_file(file)
            .map_err(|err| codec_hint(file.path(), err));
    }
    if let Some(opened) = opened {
        opened.apply(broadcast)?;
    }
    if own_audio
        .as_ref()
        .is_some_and(|file| file.info().audio.is_none())
    {
        info!("the file has no audio");
        return Ok(());
    }
    let source = match audio_source(&args.audio_source(), speaker).await {
        // The user did not ask for the file's audio, so the video goes out alone.
        Err(err) if own_audio.is_some() => {
            warn!(error = %err, "publishing the video without the file's audio");
            return Ok(());
        }
        source => source?,
    };
    if let Some(source) = source {
        let encoding = audio_config(args, &source);
        broadcast.set_audio(source, encoding)?;
    }
    Ok(())
}

/// Opens the video `args` asks for and sets it, leaving the audio alone.
///
/// Returns the raw source, if any, for a preview.
pub(crate) async fn configure_video(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
) -> Result<Option<VideoSource>> {
    match open_video(args).await? {
        Some(opened) => opened.apply(broadcast),
        None => Ok(None),
    }
}

/// A video source opened and not yet set on a broadcast.
///
/// Opening is async and setting is sync, so a caller can decide at the last
/// moment whether to set it.
#[derive(Debug)]
pub(crate) struct OpenedVideo {
    source: Video,
    ladder: rendition::Ladder,
}

/// Opens the video `args` asks for without setting it.
///
/// Returns `None` for `--video none`.
pub(crate) async fn open_video(args: &CaptureArgs) -> Result<Option<OpenedVideo>> {
    let spec = args.video_source();
    let ladder = rendition::ladder(&spec, args)?;
    let source = match &spec {
        VideoSourceSpec::File { path, looping } => {
            let file = media_file(path, FileEnd::looping(*looping)).await?;
            file_video(file, args).await?
        }
        spec => video_source(spec, args, ladder.framerate).await?,
    };
    Ok(source.map(|source| OpenedVideo { source, ladder }))
}

impl OpenedVideo {
    /// Sets the source on `broadcast` and returns it if raw, for a preview.
    pub(crate) fn apply(self, broadcast: &LocalBroadcast) -> Result<Option<VideoSource>> {
        let Self { source, ladder } = self;
        match source {
            Video::Raw(source) => {
                ladder.report();
                broadcast.set_video(source.clone(), ladder.encoding)?;
                return Ok(Some(source));
            }
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Video::Encoded(source) => broadcast.set_encoded_video(source)?,
            Video::File(file) => {
                let source = file
                    .encoded_video()
                    .map_err(|err| codec_hint(file.path(), err))?;
                broadcast.set_encoded_video(source)?;
            }
        }
        Ok(None)
    }
}

/// Opens the video of `file`: decoded if `args` encodes it, as it is otherwise.
///
/// Returns `None` for a file without video whose audio comes along, which
/// [`configure`] publishes alone.
async fn file_video(file: MediaFile, args: &CaptureArgs) -> Result<Option<Video>> {
    let path = file.path();
    let reencodes = args.reencodes()?;
    if reencodes && !args.reencode {
        info!(
            flags = %args.encoding_flags().join(", "),
            "an encoding flag decodes and encodes the file, as --reencode does"
        );
    }
    let Some(info) = &file.info().video else {
        if args.audio.is_none() {
            return Ok(None);
        }
        return Err(anyerr!(
            "{} has no video track. Publish it with --video none --audio file:{}",
            path.display(),
            path.display()
        ));
    };
    if reencodes {
        let source = file
            .video(VideoFileConfig::default())
            .await
            .map_err(|err| codec_hint(path, err))?;
        return Ok(Some(Video::Raw(source)));
    }
    if info.gop > LONG_GOP {
        warn!(
            gop = ?info.gop,
            "the file has keyframes this far apart, so a new viewer waits up to that long \
             for a picture. --reencode sets its own keyframe interval"
        );
    }
    Ok(Some(Video::File(file)))
}

/// The files `file:` sources opened, which every later source of the same file shares.
///
/// A video that opens again, after the scan screen of `irl call` or a switch
/// in the preview, and an `--audio` of the video's file then play against
/// one clock and stay in sync. The list lives as long as the process and
/// never shrinks, which suits a command line tool.
static OPENED: Mutex<Vec<MediaFile>> = Mutex::new(Vec::new());

/// Returns the file at `path` that a source opened before, if any.
fn opened_file(path: &Path) -> Option<MediaFile> {
    let opened = OPENED.lock().expect("poisoned");
    opened.iter().find(|file| file.path() == path).cloned()
}

/// Opens the file at `path`, or returns the copy opened before.
async fn media_file(path: &Path, end: FileEnd) -> Result<MediaFile> {
    let file = match opened_file(path) {
        Some(file) => file,
        None => {
            let file = MediaFile::open(path).await?;
            OPENED.lock().expect("poisoned").push(file.clone());
            file
        }
    };
    Ok(file.with_end(end))
}

/// Refuses the encoding flags that `rpicam-vid`, whose H.264 goes out as it is, leaves unused.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn check_rpicam_flags(args: &CaptureArgs) -> Result<()> {
    let refused: Vec<&str> = [
        (args.codec != VideoCodecArg::H264, "--codec"),
        (args.encoder != Backend::Auto, "--encoder"),
        (args.renditions.len() > 1, "a second rendition"),
    ]
    .into_iter()
    .filter_map(|(given, flag)| given.then_some(flag))
    .collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(anyerr!(
        "--video rpicam arrives encoded and goes out as it is, so no encoder here acts on {}. \
         --video rpicam:raw takes the pictures and encodes them here",
        refused.join(", ")
    ))
}

/// Adds a conversion that makes the file publishable to an error about its codec.
///
/// Passthrough and `--reencode` both take H.264 video and AAC audio only.
fn codec_hint(path: &Path, err: impl std::fmt::Display) -> n0_error::AnyError {
    anyerr!(
        "{err}. Convert it with `ffmpeg -i {} -c:v libx264 -c:a aac out.mp4`",
        path.display()
    )
}

/// Opens the video source `spec` names, or `None` for `--video none`.
async fn video_source(
    spec: &VideoSourceSpec,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
) -> Result<Option<Video>> {
    let source = match spec {
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        VideoSourceSpec::Rpicam(mode) => rpicam_source(args, framerate, *mode).await?,
        spec => match spec.open(&open_options(args, framerate)).await? {
            Some(source) => Video::Raw(source),
            None => return Ok(None),
        },
    };
    Ok(Some(source))
}

/// Returns the open options the capture flags ask for.
fn open_options(args: &CaptureArgs, framerate: CaptureFramerate) -> VideoOpenOptions {
    let mut options = VideoOpenOptions::default();
    options.width = args.width;
    options.height = args.height;
    options.framerate = framerate.request().map(|fps| {
        video::FrameRate::new(fps, 1)
            .expect("the ladder settles only on rates from 1 to MAX_FRAMERATE")
    });
    options.cursor = !args.no_cursor;
    // The portal keeps its grant for the next run, as the CLI reference says.
    options.with_portal_grants("iroh-live")
}

/// Starts `rpicam-vid` in the mode `mode` names.
///
/// Under [`RpicamMode::Encoded`], `--bitrate` goes to `rpicam-vid` and our
/// encoding flags are refused. Under [`RpicamMode::Raw`], the pictures go
/// through our encoders like any camera's.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
async fn rpicam_source(
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    mode: RpicamMode,
) -> Result<Video> {
    let size = Size::new(
        args.width.unwrap_or(RPICAM_SIZE.width),
        args.height.unwrap_or(RPICAM_SIZE.height),
    );
    let framerate = framerate.generated();
    // `--keyframe-interval` does not apply: the config keeps a keyframe a second.
    let mut config = RpicamConfig::new(size, framerate);
    match mode {
        RpicamMode::Raw => Ok(Video::Raw(VideoSource::rpicam(config).await?)),
        RpicamMode::Encoded => {
            check_rpicam_flags(args)?;
            if let Some(bitrate) = args.bitrate {
                config.bitrate = Rate::from_bps(bitrate);
            }
            Ok(Video::Encoded(rpicam_h264(config)?))
        }
    }
}

/// Returns the capture config for microphone `id`, cancelling the echo of `speaker`.
///
/// Without the `aec` feature, it warns and captures the microphone as is.
///
/// # Errors
///
/// Fails if `speaker` already cancels the echo of another microphone.
pub(crate) fn microphone_config(
    id: Option<String>,
    speaker: Option<&Speaker>,
) -> Result<audio::capture::Config> {
    let mut config = audio::capture::Config::default();
    if let Some(id) = id {
        config.source = audio::capture::Source::Microphone(Some(id));
    }
    #[cfg(feature = "aec")]
    if let Some(engine) = speaker {
        let canceller = engine
            .canceller(audio::aec::Config::default())
            .map_err(|err| anyerr!("cannot cancel the echo of the audio output: {err}"))?;
        config.aec = Some(canceller);
    }
    #[cfg(not(feature = "aec"))]
    if speaker.is_some() {
        warn!(
            "this build has no echo cancellation, so the other side may hear itself; \
             build with the `aec` feature to cancel it"
        );
    }
    Ok(config)
}

/// Opens the audio source `spec` names, or `None` for `--audio none`.
///
/// A microphone cancels the echo of `speaker`.
async fn audio_source(
    spec: &AudioSourceSpec,
    speaker: Option<&Speaker>,
) -> Result<Option<AudioSource>> {
    match spec {
        AudioSourceSpec::Microphone(id) => Ok(Some(
            AudioSource::microphone(microphone_config(id.clone(), speaker)?).await?,
        )),
        spec => match file_audio(spec) {
            Some(file) => Ok(Some(file.audio()?)),
            None => Ok(spec.open().await?),
        },
    }
}

/// Returns the file a `file:` audio `spec` names if a video already plays it.
///
/// The audio then shares the video's clock and stays in sync with it.
fn file_audio(spec: &AudioSourceSpec) -> Option<MediaFile> {
    let AudioSourceSpec::File { path, looping } = spec else {
        return None;
    };
    Some(opened_file(path)?.with_end(FileEnd::looping(*looping)))
}

/// Returns the encoding of `source` for `--audio-codec` and `--audio-bitrate`.
///
/// A microphone gets the voice preset. Any other source may be music and
/// keeps its channels. PCM keeps the source's format, 48 kHz stereo for a
/// microphone, and ignores `--audio-bitrate`.
fn audio_config(args: &CaptureArgs, source: &AudioSource) -> audio::encode::Config {
    let mut encoding = match (args.audio_codec, is_microphone(args)) {
        (AudioCodecArg::Pcm, _) => {
            let format = source.format().unwrap_or(AudioFormat {
                sample_rate: 48_000,
                layout: audio::Layout::Stereo,
            });
            return audio_encoding::pcm(format);
        }
        (AudioCodecArg::Opus, true) => audio_encoding::voice(),
        (AudioCodecArg::Opus, false) => audio_encoding::music(),
    };
    if let Some(bps) = args.audio_bitrate {
        encoding.bitrate = Some(Rate::from_bps(bps.into()));
    }
    encoding
}

/// Returns whether `--audio` names a microphone.
fn is_microphone(args: &CaptureArgs) -> bool {
    matches!(args.audio_source(), AudioSourceSpec::Microphone(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::VideoCodecArg;

    /// Without a speaker there is no echo, so the microphone gets no canceller.
    #[cfg(feature = "aec")]
    #[test]
    fn no_speaker_cancels_no_echo() {
        assert!(microphone_config(None, None).expect("valid").aec.is_none());
    }

    #[test]
    fn a_microphone_by_id_keeps_its_id() {
        let config = microphone_config(Some("hw:1".into()), None).expect("valid");
        assert_eq!(
            config.source,
            audio::capture::Source::Microphone(Some("hw:1".into()))
        );
    }

    /// A flag only an encoder acts on decodes a file, and `--no-reencode` refuses it.
    #[test]
    fn encoding_flags_decode_a_file() {
        assert!(!CaptureArgs::default().reencodes().expect("no flags"));
        for args in [
            CaptureArgs {
                codec: VideoCodecArg::H265,
                ..Default::default()
            },
            CaptureArgs {
                width: Some(640),
                ..Default::default()
            },
            CaptureArgs {
                audio_bitrate: Some(64_000),
                ..Default::default()
            },
        ] {
            assert!(args.reencodes().expect("no --no-reencode"), "{args:?}");
            let refused = CaptureArgs {
                no_reencode: true,
                ..args
            };
            let err = refused.reencodes().unwrap_err();
            assert!(err.to_string().contains("--no-reencode"), "{err}");
        }
        // An `--audio` of its own takes the audio flags, which leave the file alone.
        let mic = CaptureArgs {
            audio: Some(AudioSourceSpec::Microphone(None)),
            audio_bitrate: Some(64_000),
            no_reencode: true,
            ..Default::default()
        };
        assert!(!mic.reencodes().expect("the flag is the microphone's"));
    }

    /// An `--audio` of the file the video plays shares the video's file, and with it the clock.
    #[tokio::test]
    async fn the_audio_of_the_video_file_shares_its_clock() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../moq-ext-sources/src/source/test_data/clip.mp4");
        let spec = |path: &Path| AudioSourceSpec::File {
            path: path.to_path_buf(),
            looping: false,
        };
        assert!(file_audio(&spec(&path)).is_none());
        media_file(&path, FileEnd::Restart)
            .await
            .expect("the clip opens");
        let shared = file_audio(&spec(&path)).expect("the video's file");
        assert_eq!(shared.path(), path);
        assert!(file_audio(&spec(Path::new("other.mp4"))).is_none());
    }
}
