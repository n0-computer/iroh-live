//! Turning parsed specifiers into opened sources, and setting them on a
//! broadcast.
//!
//! A source is opened here, before it reaches the broadcast: a camera that is
//! not there, a file that will not decode, or a microphone this machine lacks
//! is an error at this point rather than a line in the log from a task that
//! keeps trying. What the broadcast does with a source that opened, it reports
//! in its status.
//!
//! The test pattern and the test tone share one timeline, so a viewer judges
//! A/V sync by whether the picture's flash and the tone's beep land together.
//!
//! The Raspberry Pi camera comes in two forms. `rpicam` hands over the H.264
//! the Pi's own encoder produced, which is published as it is; `rpicam:raw`
//! hands over pictures, which reach our encoders like any other camera's.

use iroh_live::media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, LocalBroadcast, MicrophoneConfig,
    VideoFormat, VideoSource, audio,
    video::{self, Size},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::{EncodedVideoSource, RpicamConfig};
use n0_error::{Result, anyerr};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::EncoderArg, source_spec::RpicamMode};
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

/// Capture mode of the Raspberry Pi camera when `--width` / `--height` are not
/// given. A Pi Zero 2 W streams this comfortably, and a larger mode is one flag
/// away.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_SIZE: Size = Size {
    width: 640,
    height: 360,
};

/// Frequency of the unbroken test tone, in hertz. Concert A: unmistakable, and
/// low enough that no resampler on the way out can alias it. The beeping tone
/// names its own frequency, an octave above this one.
const TEST_TONE_HZ: f32 = 440.0;

/// Speaker layout of the test tones.
const TEST_TONE_LAYOUT: audio::Layout = audio::Layout::Stereo;

/// What [`configure`] opened, for the callers that draw a preview of it or
/// hand its camera to a scanner.
#[derive(Debug, Default)]
#[cfg_attr(
    not(feature = "render"),
    expect(
        dead_code,
        reason = "only the windows draw a preview or lend the camera"
    )
)]
pub struct Opened {
    /// The raw video source, if the video is one: a pre-encoded source has no
    /// pictures to preview.
    pub video: Option<VideoSource>,
    /// The audio source, if there is one.
    pub audio: Option<AudioSource>,
}

/// A video source a specifier opened.
#[derive(Debug)]
enum Video {
    /// Raw pictures, encoded into the ladder.
    Raw(VideoSource),
    /// Pre-encoded H.264, published as it is.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Encoded(EncodedVideoSource),
}

/// Opens whichever of video and audio `args` asks for, and sets them on
/// `broadcast`.
///
/// `output` is the speaker whose echo the microphone cancels, for the commands
/// that play the other side's audio while publishing their own.
///
/// # Errors
///
/// Fails if a specifier is unusable here (a `file:` video source belongs to
/// `irl publish`), if the rendition ladder does not parse, or if a source will
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

/// Opens the video `args` asks for and sets it, leaving whatever audio is
/// publishing in place.
///
/// Returns the raw source when the video is one, for a preview.
///
/// # Errors
///
/// As [`configure`], for the video half.
pub async fn configure_video(
    broadcast: &LocalBroadcast,
    args: &CaptureArgs,
) -> Result<Option<VideoSource>> {
    match open_video(args).await? {
        Some(opened) => opened.apply(broadcast),
        None => Ok(None),
    }
}

/// A video source opened for a broadcast and not yet set on it.
///
/// Opening awaits a device, and setting is one synchronous call, so a caller
/// that has to decide at the last moment whether the source still goes on the
/// broadcast can make that decision and the set in one step.
#[derive(Debug)]
pub struct OpenedVideo {
    source: Video,
    ladder: rendition::Ladder,
}

/// Opens the video `args` asks for, without setting it, or `None` for
/// `--video none`.
///
/// # Errors
///
/// As [`configure`], for the video half.
pub async fn open_video(args: &CaptureArgs) -> Result<Option<OpenedVideo>> {
    let spec = args.video_source()?;
    let ladder = rendition::ladder(&spec, args)?;
    Ok(video_source(&spec, args, ladder.framerate)
        .await?
        .map(|source| OpenedVideo { source, ladder }))
}

impl OpenedVideo {
    /// Sets the source on `broadcast`, returning it when it is raw, for a
    /// preview.
    ///
    /// # Errors
    ///
    /// Fails for an encoding the broadcast refuses.
    pub fn apply(self, broadcast: &LocalBroadcast) -> Result<Option<VideoSource>> {
        let Self { source, ladder } = self;
        // Only now is there a capture to describe: `--video none` never gets
        // here with a ladder nothing will encode.
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

/// Opens the video source a specifier names, or `None` for `--video none`.
///
/// # Errors
///
/// Fails for a `file:` source, which only `irl publish` can take, and for a
/// device that will not open.
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
            // Only `irl publish` has the import path; every other command that
            // captures reaches this with whatever the user typed.
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

/// Opens a capture device, carrying the geometry hints from the flags.
///
/// `framerate` is the rate the whole ladder is captured at, which `--fps` and
/// the rungs' `@<fps>` suffixes settle between them: see [`crate::rendition`].
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

/// Starts `rpicam-vid` for whichever of H.264 and raw pictures `mode` asked
/// for.
///
/// `--width`, `--height`, and the settled capture frame rate describe the
/// capture either way. Under [`RpicamMode::Encoded`] `--bitrate` goes to the
/// subprocess, which is the only thing that can act on it, and the flags that
/// describe an encode of ours are refused. Under [`RpicamMode::Raw`] the
/// picture reaches our encoders like any other camera's, so `--bitrate` belongs
/// to the rendition ladder.
///
/// # Errors
///
/// Fails if `rpicam-vid` cannot be started, or if a flag asks the pre-encoded
/// source for an encode it cannot perform.
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
    // `rpicam-vid` delivers the rate it is told to, so there is no device mode
    // to fall back on and the default stands in for one.
    let framerate = framerate.generated();
    // A keyframe a second. The subprocess owns the encode, so this is the only
    // place the join latency can be set.
    let mut config = RpicamConfig {
        keyframe_interval: framerate,
        ..RpicamConfig::new(size, framerate)
    };
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
///
/// The picture is already H.264 by the time we see it, so `--codec` and
/// `--encoder` describe an encode that does not happen and a ladder has nothing
/// to scale. Saying so beats starting the camera and publishing something
/// other than what was asked for.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn check_rpicam_flags(args: &CaptureArgs) -> Result<()> {
    if args.codec != VideoCodecArg::H264 {
        return Err(anyerr!(
            "--video rpicam publishes the H.264 that rpicam-vid encoded in \
             hardware, so --codec cannot select another codec; --video \
             rpicam:raw takes the pictures instead and encodes them here"
        ));
    }
    if args.encoder != EncoderArg::Auto {
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

/// The test pattern at its default geometry, for a caller with no flags to
/// consult.
#[cfg(feature = "render")]
pub fn default_test_pattern() -> VideoSource {
    VideoSource::test_pattern(
        TEST_SIZE,
        video::Rate::new(rendition::DEFAULT_FRAMERATE, 1).expect("a valid rate"),
    )
}

/// The test pattern `pattern` names, at whatever geometry the flags asked for.
///
/// # Errors
///
/// Fails if the gradient's thread cannot be started.
fn test_pattern(
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    pattern: TestPattern,
) -> Result<VideoSource> {
    let size = Size::new(
        args.width.unwrap_or(TEST_SIZE.width),
        args.height.unwrap_or(TEST_SIZE.height),
    );
    // The generator draws exactly the rate it is asked for, so there is no
    // device to defer to here either.
    let rate = video::Rate::new(framerate.generated(), 1)
        .expect("the ladder settles only on rates from 1 to MAX_FRAMERATE");
    match pattern {
        TestPattern::Timing => Ok(VideoSource::test_pattern(size, rate)),
        TestPattern::Gradient => gradient(size, framerate.generated()),
    }
}

/// A diagonal gradient that shifts every frame, drawn on a thread of its own.
///
/// Cheap, and different in every frame: a static image compresses to almost
/// nothing after the first keyframe, so a pipeline that had stalled would
/// still look like one moving bytes.
///
/// # Errors
///
/// Fails if the thread cannot be started.
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

/// The microphone `id` names, with `output`'s echo cancelled from it.
///
/// Every command that plays the other side's audio while it publishes its own
/// passes the speaker it plays through, so a laptop or a handset on speaker
/// does not send the other side back to itself. A build without the `aec`
/// feature says so and publishes the microphone as it is, rather than refusing
/// to start a call.
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
        // A null output plays nothing, so there is no echo to warn about.
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

/// Opens the audio source a specifier names, or `None` for `--audio none`.
///
/// # Errors
///
/// Fails if a microphone is not there, or a `file:` source cannot be opened or
/// holds no audio track.
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

/// The encoding `--audio-codec` and `--audio-bitrate` imply.
///
/// A microphone is speech and gets the voice preset; anything else may be
/// music and keeps its channels. PCM's bitrate follows from its sample rate
/// and channel count, so only Opus takes `--audio-bitrate`.
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

/// Whether `--audio` names a microphone.
fn is_microphone(args: &CaptureArgs) -> bool {
    matches!(args.audio_source(), Ok(AudioSourceSpec::Microphone(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echo cancellation was never attached before this CLI passed its output
    /// in: every command that plays the other side's audio publishes a
    /// microphone that cancels it. This fails if the config drops the output.
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
