//! Opens the sources that specifiers name, and sets them on a broadcast.
//!
//! Sources open before they reach the broadcast, so a missing device is an
//! error here and not a retry loop in the log.

use std::{path::Path, sync::Mutex, time::Duration};

use iroh_live::media::{
    AudioFormat, AudioSource, EncodedVideo, FileEnd, LocalBroadcast, MediaFile, Rate,
    VideoFileConfig, VideoSource, audio, audio_encoding, video,
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::{RpicamConfig, rpicam_h264, video::Size};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::source_spec::RpicamMode;
use crate::{
    args::{AudioCodecArg, CaptureArgs, VideoCodecArg},
    backend::Backend,
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
    Encoded(EncodedVideo),
}

/// Opens the video and audio `args` asks for, and sets them on `broadcast`.
///
/// The microphone cancels the echo of `speaker`. Without `--audio`, a `file:`
/// video brings its own audio, passed through or decoded as the video is.
/// Fails for a ladder that does not parse, or a source that does not open.
/// The broadcast keeps the sources, and `video_source` hands the video back
/// for a preview.
pub(crate) async fn configure(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
    speaker: Option<&Speaker>,
) -> Result<()> {
    let opened = open_video(args).await?;
    let file = opened.as_ref().and_then(|opened| opened.file.clone());
    if let Some(opened) = opened {
        opened.apply(broadcast)?;
    }
    if let Some(file) = file.filter(|_| args.audio.is_none()) {
        return set_file_audio(broadcast, args, &file);
    }
    if let Some(source) = audio_source(&args.audio_source(), speaker).await? {
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
    /// The file a `file:` video comes from, for its audio.
    file: Option<MediaFile>,
}

/// Opens the video `args` asks for without setting it.
///
/// Returns `None` for `--video none`.
pub(crate) async fn open_video(args: &CaptureArgs) -> Result<Option<OpenedVideo>> {
    let spec = args.video_source();
    let ladder = rendition::ladder(&spec, args)?;
    if let VideoSourceSpec::File { path, looping } = &spec {
        let end = if *looping {
            FileEnd::Restart
        } else {
            FileEnd::Stop
        };
        let file = media_file(path, end).await?;
        let source = file_video(&file, args).await?;
        return Ok(Some(OpenedVideo {
            source,
            ladder,
            file: Some(file),
        }));
    }
    Ok(video_source(&spec, args, ladder.framerate)
        .await?
        .map(|source| OpenedVideo {
            source,
            ladder,
            file: None,
        }))
}

impl OpenedVideo {
    /// Sets the source on `broadcast` and returns it if raw, for a preview.
    pub(crate) fn apply(self, broadcast: &LocalBroadcast) -> Result<Option<VideoSource>> {
        let Self { source, ladder, .. } = self;
        match source {
            Video::Raw(source) => {
                ladder.report();
                broadcast.set_video(source.clone(), ladder.encoding)?;
                Ok(Some(source))
            }
            Video::Encoded(source) => {
                broadcast.set_encoded_video(source)?;
                Ok(None)
            }
        }
    }
}

/// Opens the video of `file`: decoded under `--reencode`, as it is otherwise.
async fn file_video(file: &MediaFile, args: &CaptureArgs) -> Result<Video> {
    let path = file.path();
    let Some(info) = &file.info().video else {
        return Err(anyerr!(
            "{} has no video track. Publish it with --video none --audio file:{}",
            path.display(),
            path.display()
        ));
    };
    if args.reencode {
        let source = file
            .video(VideoFileConfig::default())
            .await
            .map_err(|err| codec_hint(path, err))?;
        return Ok(Video::Raw(source));
    }
    check_passthrough_flags(args)?;
    if info.gop > LONG_GOP {
        warn!(
            gop = ?info.gop,
            "the file has keyframes this far apart, so a new viewer waits up to that long \
             for a picture. --reencode sets its own keyframe interval"
        );
    }
    let source = file.encoded_video().map_err(|err| codec_hint(path, err))?;
    Ok(Video::Encoded(source))
}

/// Opens the `file:` video at `path`, or returns the copy opened before.
///
/// A video that opens again, after the scan screen of `irl call` or a switch
/// in the preview, then shares the clock of the file's audio, which kept
/// playing, so the two stay in sync.
async fn media_file(path: &Path, end: FileEnd) -> Result<MediaFile> {
    static OPENED: Mutex<Vec<MediaFile>> = Mutex::new(Vec::new());
    let opened = OPENED
        .lock()
        .expect("poisoned")
        .iter()
        .find(|file| file.path() == path)
        .cloned();
    let file = match opened {
        Some(file) => file,
        None => {
            let file = MediaFile::open(path).await?;
            OPENED.lock().expect("poisoned").push(file.clone());
            file
        }
    };
    Ok(file.with_end(end))
}

/// Sets the audio of `file`, if it has any: decoded under `--reencode`, as it is otherwise.
///
/// The user did not ask for the file's audio, so audio that does not open
/// only warns, and the video goes out alone.
fn set_file_audio(broadcast: &LocalBroadcast, args: &CaptureArgs, file: &MediaFile) -> Result<()> {
    let path = file.path().display();
    if file.info().audio.is_none() {
        info!(%path, "the file has no audio");
        return Ok(());
    }
    let set = if args.reencode {
        file.audio().map(|source| {
            let encoding = audio_config(args, &source);
            broadcast.set_audio(source, encoding)
        })
    } else {
        file.encoded_audio()
            .map(|source| broadcast.set_encoded_audio(source))
    };
    match set {
        Ok(result) => Ok(result?),
        Err(err) => {
            warn!(%path, error = %err, "publishing the video without the file's audio");
            Ok(())
        }
    }
}

/// Refuses the flags that need an encoder, which a passthrough file has not.
fn check_passthrough_flags(args: &CaptureArgs) -> Result<()> {
    let encoding = [
        (!args.renditions.is_empty(), "--renditions"),
        (
            args.width.is_some() || args.height.is_some(),
            "--width and --height",
        ),
        (args.bitrate.is_some(), "--bitrate"),
        (args.fps.is_some(), "--fps"),
        (args.codec != VideoCodecArg::H264, "--codec"),
        (args.encoder != Backend::Auto, "--encoder"),
        (
            args.audio.is_none() && args.audio_bitrate.is_some(),
            "--audio-bitrate",
        ),
        (
            args.audio.is_none() && args.audio_codec != AudioCodecArg::Opus,
            "--audio-codec",
        ),
    ];
    let given: Vec<&str> = encoding
        .iter()
        .filter_map(|(given, flag)| given.then_some(*flag))
        .collect();
    if given.is_empty() {
        return Ok(());
    }
    Err(anyerr!(
        "a file is published as it is, in one rendition at its own size and \
         bitrate, so {} needs --reencode, which decodes and encodes it",
        given.join(", ")
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

/// Refuses the encoding flags a pre-encoded source cannot act on.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn check_rpicam_flags(args: &CaptureArgs) -> Result<()> {
    if args.codec != VideoCodecArg::H264 {
        return Err(anyerr!(
            "--video rpicam publishes the H.264 that rpicam-vid encoded in \
             hardware, so --codec cannot select another codec; --video \
             rpicam:raw takes the pictures instead and encodes them here"
        ));
    }
    if args.encoder != Backend::Auto {
        return Err(anyerr!(
            "--encoder {} has nothing to do under --video rpicam: rpicam-vid \
             has already encoded the picture, and no encoder of ours runs. \
             --video rpicam:raw takes the pictures instead, which is what \
             comparing an encoder against the hardware one needs",
            args.encoder
        ));
    }
    if args.renditions.len() > 1 {
        return Err(anyerr!(
            "--video rpicam publishes one rendition: the stream arrives \
             encoded and cannot be produced again at a second size. Give at \
             most one rung, or use --video rpicam:raw to encode a ladder from \
             the pictures"
        ));
    }
    Ok(())
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
        spec => Ok(spec.open().await?),
    }
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

    /// The flags that need an encoder are refused for a passthrough file, with the way out.
    #[test]
    fn encoding_flags_need_reencode() {
        assert!(check_passthrough_flags(&CaptureArgs::default()).is_ok());
        let args = CaptureArgs {
            renditions: vec!["720p".into()],
            width: Some(640),
            ..Default::default()
        };
        let err = check_passthrough_flags(&args).expect_err("a ladder needs an encoder");
        let message = err.to_string();
        assert!(
            message.contains("--renditions, --width and --height"),
            "{message}"
        );
        assert!(message.contains("--reencode"), "{message}");
    }
}
