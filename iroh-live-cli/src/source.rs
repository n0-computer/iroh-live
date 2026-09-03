//! Turning parsed specifiers into the sources `moq-media` publishes.
//!
//! A capture source is a `moq_video::capture::Config` or a
//! `moq_audio::capture::Config` and nothing more: the device is opened inside
//! the publish task, which is what lets it be released again when publishing
//! stops. The test pattern and the test tone come from
//! [`moq_media::test_source`], so `irl publish --test-source` works on a
//! machine with neither camera nor microphone. Both take the broadcast's own
//! clock, which is what puts the picture's flash and the tone's beep on one
//! timeline for a viewer to judge A/V sync against.
//!
//! The Raspberry Pi camera is the exception: `rpicam-vid` is started here
//! rather than in the publish task. Under `rpicam` what it hands back is
//! already-encoded H.264 and no stage of ours sees a picture; under
//! `rpicam:raw` it hands back I420 and the rest of the pipeline behaves as it
//! would for any other camera.

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::rpicam;
use iroh_live::media::{
    audio,
    audio_file::AudioFile,
    publish::{AudioSource, LocalBroadcast, VideoSource},
    test_source,
    video::{self, Size},
};
use n0_error::{Result, anyerr};

use crate::{
    args::CaptureArgs,
    rendition::{self, CaptureFramerate},
    source_spec::{AudioSourceSpec, TestPattern, TestTone, VideoSourceSpec},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::EncoderArg, source_spec::RpicamMode};

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
const TEST_TONE_HZ: f64 = 440.0;

/// Sample rate of the test tone. Opus's native rate, so it is encoded
/// without a resampling step.
const TEST_TONE_RATE: u32 = 48_000;

/// Channel count of the test tone.
const TEST_TONE_CHANNELS: u32 = 2;

/// Sets up whichever of video and audio `args` asked for.
///
/// # Errors
///
/// Fails if a specifier is unusable here (a `file:` video source belongs to
/// `irl publish`), or if the rendition ladder does not parse. A device that
/// will not open surfaces in the log and ends its track, not here.
pub fn configure(broadcast: &LocalBroadcast, args: &CaptureArgs) -> Result<()> {
    configure_video(broadcast, args)?;
    if let Some(source) = audio_source(&args.audio_source()?, *broadcast.clock())? {
        broadcast.audio().set_with(source, audio_options(args));
    }
    Ok(())
}

/// Sets up the video half alone, leaving whatever audio is publishing in place.
///
/// `irl call` hands its camera to the scan screen and takes it back afterwards,
/// which is a video source to reopen and a microphone that never stopped.
///
/// # Errors
///
/// As [`configure`], for the video half.
pub fn configure_video(broadcast: &LocalBroadcast, args: &CaptureArgs) -> Result<()> {
    let spec = args.video_source()?;
    let ladder = rendition::ladder(&spec, args)?;
    if let Some(source) = video_source(&spec, args, ladder.framerate, broadcast)? {
        // Only now is there a capture to describe: `--video none` reaches here
        // with a ladder nothing will encode.
        ladder.report();
        broadcast
            .video()
            .set_renditions(source, ladder.renditions)?;
    }
    Ok(())
}

/// The video source a specifier names, or `None` for `--video none`.
///
/// # Errors
///
/// Fails for a `file:` source, which only `irl publish` can take.
fn video_source(
    spec: &VideoSourceSpec,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    broadcast: &LocalBroadcast,
) -> Result<Option<VideoSource>> {
    use video::capture::Source;

    let source = match spec {
        VideoSourceSpec::None => return Ok(None),
        VideoSourceSpec::Test(pattern) => {
            test_pattern(*pattern, args, framerate, *broadcast.clock())
        }
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
        VideoSourceSpec::Rpicam(mode) => rpicam_source(args, framerate, *mode, *broadcast.clock())?,
        VideoSourceSpec::Camera(id) => capture(Source::Camera(id.clone()), args, framerate),
        VideoSourceSpec::Display(id) => capture(Source::Display(id.clone()), args, framerate),
        VideoSourceSpec::Window(id) => capture(Source::Window(id.clone()), args, framerate),
        VideoSourceSpec::App(id) => capture(Source::App(id.clone()), args, framerate),
    };
    Ok(Some(source))
}

/// A capture config for `source`, carrying the geometry hints from the flags.
///
/// `framerate` is the rate the whole ladder is captured at, which `--fps` and
/// the rungs' `@<fps>` suffixes settle between them: see [`crate::rendition`].
fn capture(
    source: video::capture::Source,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
) -> VideoSource {
    let mut config = video::capture::Config::default();
    config.source = source;
    config.width = args.width;
    config.height = args.height;
    config.framerate = framerate.request();
    config.cursor = !args.no_cursor;
    VideoSource::Capture(config)
}

/// Starts `rpicam-vid` and returns whichever of H.264 and raw pictures `mode`
/// asked for.
///
/// `--width`, `--height`, and the settled capture frame rate describe the
/// capture either way. Under [`RpicamMode::Encoded`] `--bitrate` goes to the
/// subprocess, which is the only thing that can act on it, and the flags that
/// describe an encode of ours are refused. Under [`RpicamMode::Raw`] none of
/// that applies: the picture reaches our encoders like any other camera's, so
/// `--bitrate` belongs to the rendition ladder and the subprocess is told
/// nothing about it.
///
/// Raw frames are stamped on `clock`, the one the broadcast's audio is stamped
/// from, so the two tracks land on a single timeline.
///
/// # Errors
///
/// Fails if `rpicam-vid` cannot be started, or if a flag asks the pre-encoded
/// source for an encode it cannot perform.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn rpicam_source(
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    mode: RpicamMode,
    clock: moq_mux::Clock,
) -> Result<VideoSource> {
    let width = args.width.unwrap_or(RPICAM_SIZE.width);
    let height = args.height.unwrap_or(RPICAM_SIZE.height);
    // `rpicam-vid` delivers the rate it is told to, so there is no device mode
    // to fall back on and the default stands in for one.
    let framerate = framerate.generated();
    match mode {
        RpicamMode::Raw => {
            let config = rpicam::Config::raw(width, height, framerate);
            Ok(VideoSource::Frames(rpicam::frames(config, clock)?))
        }
        RpicamMode::Encoded => {
            check_rpicam_flags(args)?;
            let mut config = rpicam::Config::new(width, height, framerate);
            if let Some(bitrate) = args.bitrate {
                config = config.with_bitrate(u32::try_from(bitrate).unwrap_or(u32::MAX));
            }
            Ok(rpicam::open(config)?)
        }
    }
}

/// Refuses the encoding flags a pre-encoded source cannot act on.
///
/// None of this applies to `rpicam:raw`, which hands over pictures nothing has
/// encoded yet.
///
/// The picture is already H.264 by the time we see it, so `--codec` and
/// `--encoder` describe an encode that does not happen and a ladder has
/// nothing to scale. Saying so beats starting the camera and publishing
/// something other than what was asked for.
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
             most one rung, which names the catalog entry, or use --video \
             rpicam:raw to encode a ladder from the pictures"
        ));
    }
    Ok(())
}

/// The test pattern at its default geometry, for a caller with no flags to
/// consult.
#[cfg(feature = "render")]
pub fn default_test_pattern(clock: moq_mux::Clock) -> VideoSource {
    test_source::timing::video(TEST_SIZE, rendition::DEFAULT_FRAMERATE, clock)
}

/// The test pattern, at whatever geometry the flags asked for.
///
/// `clock` is the broadcast's, so the marker the timing pattern flashes and the
/// beep [`audio_source`] generates describe the same instant.
fn test_pattern(
    pattern: TestPattern,
    args: &CaptureArgs,
    framerate: CaptureFramerate,
    clock: moq_mux::Clock,
) -> VideoSource {
    let size = Size::new(
        args.width.unwrap_or(TEST_SIZE.width),
        args.height.unwrap_or(TEST_SIZE.height),
    );
    // The generator draws exactly the rate it is asked for, so there is no
    // device to defer to here either.
    let framerate = framerate.generated();
    match pattern {
        TestPattern::Timing => test_source::timing::video(size, framerate, clock),
        TestPattern::Gradient => test_source::video(size, framerate),
    }
}

/// The audio source a specifier names, or `None` for `--audio none`.
///
/// # Errors
///
/// Fails if a `file:` source cannot be opened or holds no audio track.
fn audio_source(spec: &AudioSourceSpec, clock: moq_mux::Clock) -> Result<Option<AudioSource>> {
    let source = match spec {
        AudioSourceSpec::None => return Ok(None),
        AudioSourceSpec::Microphone(id) => {
            let mut config = audio::capture::Config::default();
            config.source = audio::capture::Source::Microphone(id.clone());
            AudioSource::Device(config)
        }
        AudioSourceSpec::System => {
            let mut config = audio::capture::Config::default();
            config.source = audio::capture::Source::System;
            AudioSource::Device(config)
        }
        AudioSourceSpec::Test(TestTone::Beeps) => {
            test_source::timing::audio(TEST_TONE_RATE, TEST_TONE_CHANNELS, clock)
        }
        AudioSourceSpec::Test(TestTone::Tone) => {
            test_source::audio(TEST_TONE_HZ, TEST_TONE_RATE, TEST_TONE_CHANNELS)
        }
        AudioSourceSpec::File { path, looping } => {
            let file = AudioFile::open(path, *looping)?;
            AudioSource::Frames {
                input: file.input(),
                frames: file.into_stream(),
            }
        }
    };
    Ok(Some(source))
}

/// The encoder options `--audio-codec` and `--audio-bitrate` imply.
fn audio_options(args: &CaptureArgs) -> audio::encode::Options {
    let mut options = audio::encode::Options::default();
    options.codec = args.audio_codec.into();
    // PCM's bitrate follows from its sample rate and channel count, and the
    // encoder rejects an explicit one, so only Opus takes the flag.
    if options.codec == audio::encode::Codec::Opus {
        options.bitrate = args.audio_bitrate;
    }
    options
}
