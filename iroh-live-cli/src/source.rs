//! Opens the sources that specifiers name, and sets them on a broadcast.
//!
//! Sources open before they reach the broadcast, so a missing device is an
//! error here and not a retry loop in the log.

use iroh_live::media::{
    AudioFormat, AudioSource, FileEnd, LocalBroadcast, Rate, VideoSource, audio, audio_encoding,
    video::{self, Size},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::{RpicamConfig, rpicam_h264};
use n0_error::{Result, anyerr};
#[cfg(not(feature = "aec"))]
use tracing::warn;

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::Backend, source_spec::RpicamMode};
use crate::{
    args::{AudioCodecArg, CaptureArgs},
    playback::Speaker,
    rendition::{self, CaptureFramerate},
    source_spec::{AudioSourceSpec, TestPattern, TestTone, VideoSourceSpec},
};

/// Resolution of the test pattern when `--width` / `--height` are not given.
const TEST_SIZE: Size = Size {
    width: 1280,
    height: 720,
};

/// Capture size of the Raspberry Pi camera without `--width` and `--height`.
///
/// A Pi Zero 2 W streams this comfortably.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_SIZE: Size = Size {
    width: 640,
    height: 360,
};

/// Frequency of the continuous test tone, in hertz.
///
/// Concert A, low enough that no resampler can alias it.
const TEST_TONE_HZ: f32 = 440.0;

/// Speaker layout of the test tones.
const TEST_TONE_LAYOUT: audio::Layout = audio::Layout::Stereo;

/// The sources [`configure`] opened, for a preview or a QR scanner.
#[derive(Debug, Default)]
pub(crate) struct Opened {
    /// The raw video source. `None` for no video or pre-encoded video.
    pub(crate) video: Option<VideoSource>,
    /// The audio source.
    pub(crate) audio: Option<AudioSource>,
}

/// An opened video source.
#[derive(derive_more::Debug)]
enum Video {
    /// Raw pictures, encoded into the ladder.
    Raw(VideoSource),
    /// Pre-encoded H.264, published as it is.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Encoded(#[debug(skip)] n0_future::boxed::BoxStream<bytes::Bytes>),
}

/// Opens the video and audio `args` asks for, and sets them on `broadcast`.
///
/// The microphone cancels the echo of `speaker`. Fails for a `file:` video
/// source, a ladder that does not parse, or a source that does not open.
pub(crate) async fn configure(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
    speaker: Option<&Speaker>,
) -> Result<Opened> {
    let video = configure_video(broadcast, args).await?;
    let audio = match audio_source(&args.audio_source(), speaker).await? {
        Some(source) => {
            broadcast.set_audio(source.clone(), audio_config(args, &source))?;
            Some(source)
        }
        None => None,
    };
    Ok(Opened { video, audio })
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
    Ok(video_source(&spec, args, ladder.framerate)
        .await?
        .map(|source| OpenedVideo { source, ladder }))
}

impl OpenedVideo {
    /// Sets the source on `broadcast` and returns it if raw, for a preview.
    pub(crate) fn apply(self, broadcast: &LocalBroadcast) -> Result<Option<VideoSource>> {
        let Self { source, ladder } = self;
        ladder.report();
        match source {
            Video::Raw(source) => {
                broadcast.set_video(source.clone(), ladder.encoding)?;
                Ok(Some(source))
            }
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Video::Encoded(source) => {
                broadcast.set_encoded_video(source)?;
                Ok(None)
            }
        }
    }
}

/// Opens the video source `spec` names, or `None` for `--video none`.
///
/// Fails for a `file:` source, which only `irl publish` takes.
async fn video_source(
    spec: &VideoSourceSpec,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
) -> Result<Option<Video>> {
    use video::capture::Source;

    let source = match spec {
        VideoSourceSpec::None => return Ok(None),
        VideoSourceSpec::Test(pattern) => Video::Raw(test_pattern(args, framerate, *pattern)?),
        VideoSourceSpec::File { path, .. } => {
            return Err(anyerr!(
                "a file: video source is published by `irl publish --video \
                 file:{}`, which republishes the file without re-encoding it; \
                 here the source has to be a capture device, `test`, or `none`",
                path.display()
            ));
        }
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        VideoSourceSpec::Rpicam(mode) => rpicam_source(args, framerate, *mode).await?,
        VideoSourceSpec::Camera(id) => capture(Source::Camera(id.clone()), args, framerate).await?,
        VideoSourceSpec::Display(Some(id)) if id == PORTAL => {
            capture(portal(PortalPick::Screen)?, args, framerate).await?
        }
        VideoSourceSpec::Display(id) => {
            capture(Source::Display(id.clone()), args, framerate).await?
        }
        VideoSourceSpec::Window(id) if id == PORTAL => {
            capture(portal(PortalPick::Window)?, args, framerate).await?
        }
        VideoSourceSpec::Window(id) => capture(Source::Window(id.clone()), args, framerate).await?,
        VideoSourceSpec::App(id) => capture(Source::App(id.clone()), args, framerate).await?,
    };
    Ok(Some(source))
}

/// The id of `screen:portal` and `window:portal`.
///
/// Either lets the user pick in the desktop portal's screen-sharing dialog.
pub(crate) const PORTAL: &str = "portal";

/// What the desktop portal's picker offers.
#[derive(Debug, Clone, Copy)]
enum PortalPick {
    /// A whole screen, for `screen:portal`.
    Screen,
    /// A single window, for `window:portal`.
    Window,
}

/// Returns the source that asks the desktop portal for `pick`.
///
/// Fails off Linux and in a build without the `pipewire` feature, which have
/// no portal.
fn portal(pick: PortalPick) -> Result<video::capture::Source> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        use video::capture::portal::{Kind, Selection};
        let kind = match pick {
            PortalPick::Screen => Kind::Screen,
            PortalPick::Window => Kind::Window,
        };
        Ok(video::capture::Source::Portal(Selection::new(kind)))
    }
    #[cfg(not(all(target_os = "linux", feature = "pipewire")))]
    {
        let _ = pick;
        Err(anyerr!(
            "the desktop portal's picker needs a Linux build with the `pipewire` feature"
        ))
    }
}

/// Opens a capture device with the geometry hints from the flags.
async fn capture(
    source: video::capture::Source,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
) -> Result<Video> {
    let mut config = video::capture::Config::default();
    config.source = source;
    config.width = args.width;
    config.height = args.height;
    config.framerate = framerate.request().map(|fps| {
        video::FrameRate::new(fps, 1)
            .expect("the ladder settles only on rates from 1 to MAX_FRAMERATE")
    });
    config.cursor = !args.no_cursor;
    Ok(Video::Raw(VideoSource::capture(config).await?))
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

/// Starts the test pattern `pattern` names, at the size the flags ask for.
fn test_pattern(
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    pattern: TestPattern,
) -> Result<VideoSource> {
    let size = Size::new(
        args.width.unwrap_or(TEST_SIZE.width),
        args.height.unwrap_or(TEST_SIZE.height),
    );
    let rate = video::FrameRate::new(framerate.generated(), 1)
        .expect("the ladder settles only on rates from 1 to MAX_FRAMERATE");
    match pattern {
        TestPattern::Timing => Ok(VideoSource::test_pattern(size, rate)?),
        TestPattern::Gradient => Ok(VideoSource::test_gradient(size, rate)?),
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
async fn audio_source(
    spec: &AudioSourceSpec,
    speaker: Option<&Speaker>,
) -> Result<Option<AudioSource>> {
    let source = match spec {
        AudioSourceSpec::None => return Ok(None),
        AudioSourceSpec::Microphone(id) => {
            AudioSource::microphone(microphone_config(id.clone(), speaker)?).await?
        }
        AudioSourceSpec::System => {
            let mut config = audio::capture::Config::default();
            config.source = audio::capture::Source::System;
            AudioSource::microphone(config).await?
        }
        AudioSourceSpec::Test(TestTone::Beeps) => AudioSource::test_pattern(TEST_TONE_LAYOUT)?,
        AudioSourceSpec::Test(TestTone::Tone) => AudioSource::tone(TEST_TONE_HZ, TEST_TONE_LAYOUT)?,
        AudioSourceSpec::File { path, looping } => {
            let at_end = if *looping {
                FileEnd::Restart
            } else {
                FileEnd::Stop
            };
            AudioSource::file(path, at_end).await?
        }
    };
    Ok(Some(source))
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
}
