//! Opens the sources that specifiers name, and sets them on a broadcast.
//!
//! Sources open before they reach the broadcast, so a missing device is an
//! error here and not a retry loop in the log.

use iroh_live::media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, LocalBroadcast, MicrophoneConfig,
    VideoFormat, VideoSource, audio,
    video::{self, Size},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::{EncodedVideoSource, RpicamConfig};
use n0_error::{Result, anyerr};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::Backend, source_spec::RpicamMode};
use crate::{
    args::{AudioCodecArg, CaptureArgs},
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
pub struct Opened {
    /// The raw video source. `None` for no video or pre-encoded video.
    pub video: Option<VideoSource>,
    /// The audio source.
    pub audio: Option<AudioSource>,
}

/// An opened video source.
#[derive(Debug)]
enum Video {
    /// Raw pictures, encoded into the ladder.
    Raw(VideoSource),
    /// Pre-encoded H.264, published as it is.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Encoded(EncodedVideoSource),
}

/// Opens the video and audio `args` asks for, and sets them on `broadcast`.
///
/// `output` is the speaker whose echo the microphone cancels. Fails for a
/// `file:` video source, a ladder that does not parse, or a source that does
/// not open.
pub async fn configure(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
    output: Option<&AudioOutput>,
) -> Result<Opened> {
    let video = configure_video(broadcast, args).await?;
    let audio = match audio_source(&args.audio_source()?, output).await? {
        Some(source) => {
            broadcast.set_audio(source.clone(), audio_encoding(args))?;
            Some(source)
        }
        None => None,
    };
    Ok(Opened { video, audio })
}

/// Opens the video `args` asks for and sets it, leaving the audio alone.
///
/// Returns the raw source, if any, for a preview.
pub async fn configure_video(
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
pub struct OpenedVideo {
    source: Video,
    ladder: rendition::Ladder,
}

/// Opens the video `args` asks for without setting it.
///
/// Returns `None` for `--video none`.
pub async fn open_video(args: &CaptureArgs) -> Result<Option<OpenedVideo>> {
    let spec = args.video_source()?;
    let ladder = rendition::ladder(&spec, args)?;
    Ok(video_source(&spec, args, ladder.framerate)
        .await?
        .map(|source| OpenedVideo { source, ladder }))
}

impl OpenedVideo {
    /// Sets the source on `broadcast` and returns it if raw, for a preview.
    pub fn apply(self, broadcast: &LocalBroadcast) -> Result<Option<VideoSource>> {
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
        VideoSourceSpec::Display(id) => {
            capture(Source::Display(id.clone()), args, framerate).await?
        }
        VideoSourceSpec::Window(id) => capture(Source::Window(id.clone()), args, framerate).await?,
        VideoSourceSpec::App(id) => capture(Source::App(id.clone()), args, framerate).await?,
    };
    Ok(Some(source))
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
        video::Rate::new(fps, 1).expect("the ladder settles only on rates from 1 to MAX_FRAMERATE")
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
                config.bitrate = Bitrate::from_bps(bitrate);
            }
            Ok(Video::Encoded(EncodedVideoSource::rpicam(config).await?))
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

/// Returns the test pattern at its default size and rate.
pub fn default_test_pattern() -> VideoSource {
    VideoSource::test_pattern(
        TEST_SIZE,
        video::Rate::new(rendition::DEFAULT_FRAMERATE, 1).expect("a valid rate"),
    )
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
    let rate = video::Rate::new(framerate.generated(), 1)
        .expect("the ladder settles only on rates from 1 to MAX_FRAMERATE");
    match pattern {
        TestPattern::Timing => Ok(VideoSource::test_pattern(size, rate)),
        TestPattern::Gradient => gradient(size, framerate.generated()),
    }
}

/// Starts a diagonal gradient that shifts every frame, on its own thread.
///
/// A static image would compress to almost nothing, so a stalled pipeline
/// would look the same as a working one.
fn gradient(size: Size, fps: u32) -> Result<VideoSource> {
    let rate = video::Rate::new(fps, 1).expect("the ladder settles only on valid rates");
    let format = VideoFormat { size, rate };
    let interval = std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
    let source = VideoSource::spawn("gradient", format, move |sender| {
        let clock = moq_mux::Clock::new();
        let mut rgba = vec![0u8; (size.width * size.height * 4) as usize];
        let mut next = std::time::Instant::now();
        for tick in 0u32.. {
            paint_gradient(&mut rgba, size, tick);
            let surface =
                video::Surface::rgba(&rgba, size).expect("the buffer is sized for the picture");
            if sender
                .push(video::Frame::new(surface, clock.now()))
                .is_err()
            {
                break;
            }
            next += interval;
            std::thread::sleep(next.saturating_duration_since(std::time::Instant::now()));
        }
        Ok(())
    })?;
    Ok(source)
}

/// Fills `rgba` with a diagonal gradient that shifts with `tick`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the gradient wraps on purpose"
)]
fn paint_gradient(rgba: &mut [u8], size: Size, tick: u32) {
    let phase = tick.wrapping_mul(3) as u8;
    for y in 0..size.height {
        for x in 0..size.width {
            let offset = ((y * size.width + x) * 4) as usize;
            rgba[offset] = (x as u8).wrapping_add(phase);
            rgba[offset + 1] = (y as u8).wrapping_add(phase);
            rgba[offset + 2] = phase;
            rgba[offset + 3] = 0xff;
        }
    }
}

/// Returns the config for microphone `id`, cancelling the echo of `output`.
///
/// Without the `aec` feature, it warns and publishes the microphone as is.
pub fn microphone_config(id: Option<String>, output: Option<&AudioOutput>) -> MicrophoneConfig {
    let mut config = MicrophoneConfig::default();
    if let Some(id) = id {
        config = config.with_device(id);
    }
    if let Some(output) = output {
        #[cfg(feature = "aec")]
        {
            config.echo_reference = Some(output.clone());
        }
        // A null output plays nothing, so it has no echo.
        #[cfg(not(feature = "aec"))]
        if !output.is_null() {
            tracing::warn!(
                "this build has no echo cancellation, so the other side may hear itself; \
                 build with the `aec` feature to cancel it"
            );
        }
    }
    config
}

/// Opens the audio source `spec` names, or `None` for `--audio none`.
async fn audio_source(
    spec: &AudioSourceSpec,
    output: Option<&AudioOutput>,
) -> Result<Option<AudioSource>> {
    let source = match spec {
        AudioSourceSpec::None => return Ok(None),
        AudioSourceSpec::Microphone(id) => {
            AudioSource::microphone(microphone_config(id.clone(), output)).await?
        }
        AudioSourceSpec::System => {
            let mut config = MicrophoneConfig::default();
            config.capture.source = audio::capture::Source::System;
            AudioSource::microphone(config).await?
        }
        AudioSourceSpec::Test(TestTone::Beeps) => AudioSource::test_pattern(TEST_TONE_LAYOUT),
        AudioSourceSpec::Test(TestTone::Tone) => AudioSource::tone(TEST_TONE_HZ, TEST_TONE_LAYOUT),
        AudioSourceSpec::File { path, looping } => AudioSource::file(path, *looping).await?,
    };
    Ok(Some(source))
}

/// Returns the encoding for `--audio-codec` and `--audio-bitrate`.
///
/// A microphone gets the voice preset. Any other source may be music and
/// keeps its channels. PCM ignores `--audio-bitrate`.
fn audio_encoding(args: &CaptureArgs) -> AudioEncoding {
    let mut encoding = match (args.audio_codec, is_microphone(args)) {
        (AudioCodecArg::Pcm, _) => return AudioEncoding::pcm(),
        (AudioCodecArg::Opus, true) => AudioEncoding::voice(),
        (AudioCodecArg::Opus, false) => AudioEncoding::music(),
    };
    if let Some(bps) = args.audio_bitrate {
        encoding.bitrate = Some(Bitrate::from_bps(bps.into()));
    }
    encoding
}

/// Returns whether `--audio` names a microphone.
fn is_microphone(args: &CaptureArgs) -> bool {
    matches!(args.audio_source(), Ok(AudioSourceSpec::Microphone(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The microphone config carries the output as its echo reference.
    #[cfg(feature = "aec")]
    #[test]
    fn the_microphone_cancels_the_output_it_is_given() {
        let output = AudioOutput::null();
        let config = microphone_config(None, Some(&output));
        assert!(
            config.echo_reference.is_some(),
            "the microphone config carries no echo reference"
        );
        assert!(microphone_config(None, None).echo_reference.is_none());
    }

    #[test]
    fn a_microphone_by_id_keeps_its_id() {
        let config = microphone_config(Some("hw:1".into()), None);
        assert_eq!(
            config.capture.source,
            audio::capture::Source::Microphone(Some("hw:1".into()))
        );
    }
}
