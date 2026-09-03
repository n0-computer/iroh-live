//! Turning parsed specifiers into the sources `moq-media` publishes.
//!
//! A capture source is a `moq_video::capture::Config` or a
//! `moq_audio::capture::Config` and nothing more: the device is opened inside
//! the publish task, which is what lets it be released again when publishing
//! stops. The test pattern and the test tone come from
//! [`moq_media::test_source`], so `irl publish --test-source` works on a
//! machine with neither camera nor microphone.
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
    publish::{AudioSource, LocalBroadcast, VideoRendition, VideoSource},
    test_source,
    video::{self, Size},
};
use n0_error::{Result, anyerr};

use crate::{
    args::CaptureArgs,
    source_spec::{AudioSourceSpec, VideoSourceSpec},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use crate::{args::VideoCodecArg, backend::EncoderArg, source_spec::RpicamMode};

/// Frame rate of the test pattern when `--fps` is not given.
const TEST_FRAMERATE: u32 = 30;

/// Resolution of the test pattern when `--width` / `--height` are not given.
const TEST_SIZE: Size = Size {
    width: 1280,
    height: 720,
};

/// Frame rate of the Raspberry Pi camera when `--fps` is not given.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_FRAMERATE: u32 = 30;

/// Capture mode of the Raspberry Pi camera when `--width` / `--height` are not
/// given. A Pi Zero 2 W streams this comfortably, and a larger mode is one flag
/// away.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_SIZE: Size = Size {
    width: 640,
    height: 360,
};

/// Frequency of the test tone, in hertz. Concert A: unmistakable, and low
/// enough that no resampler on the way out can alias it.
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
    if let Some(source) = video_source(&args.video_source()?, args, broadcast)? {
        broadcast
            .video()
            .set_renditions(source, renditions(args)?)?;
    }
    if let Some(source) = audio_source(&args.audio_source()?)? {
        broadcast.audio().set_with(source, audio_options(args));
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
    #[cfg_attr(
        not(all(target_os = "linux", feature = "rpicam")),
        allow(
            unused_variables,
            reason = "only the raw Pi camera stamps its own frames"
        )
    )]
    broadcast: &LocalBroadcast,
) -> Result<Option<VideoSource>> {
    use video::capture::Source;

    let source = match spec {
        VideoSourceSpec::None => return Ok(None),
        VideoSourceSpec::Test => test_pattern(args),
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
        VideoSourceSpec::Rpicam(mode) => rpicam_source(args, *mode, *broadcast.clock())?,
        VideoSourceSpec::Camera(id) => capture(Source::Camera(id.clone()), args),
        VideoSourceSpec::Display(id) => capture(Source::Display(id.clone()), args),
        VideoSourceSpec::Window(id) => capture(Source::Window(id.clone()), args),
        VideoSourceSpec::App(id) => capture(Source::App(id.clone()), args),
    };
    Ok(Some(source))
}

/// A capture config for `source`, carrying the geometry hints from the flags.
fn capture(source: video::capture::Source, args: &CaptureArgs) -> VideoSource {
    let mut config = video::capture::Config::default();
    config.source = source;
    config.width = args.width;
    config.height = args.height;
    config.framerate = args.fps;
    config.cursor = !args.no_cursor;
    VideoSource::Capture(config)
}

/// Starts `rpicam-vid` and returns whichever of H.264 and raw pictures `mode`
/// asked for.
///
/// `--width`, `--height`, and `--fps` describe the capture either way. Under
/// [`RpicamMode::Encoded`] `--bitrate` goes to the subprocess, which is the
/// only thing that can act on it, and the flags that describe an encode of ours
/// are refused. Under [`RpicamMode::Raw`] none of that applies: the picture
/// reaches our encoders like any other camera's, so `--bitrate` belongs to the
/// rendition ladder and the subprocess is told nothing about it.
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
    mode: RpicamMode,
    clock: moq_mux::Clock,
) -> Result<VideoSource> {
    let width = args.width.unwrap_or(RPICAM_SIZE.width);
    let height = args.height.unwrap_or(RPICAM_SIZE.height);
    let framerate = args.fps.unwrap_or(RPICAM_FRAMERATE);
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
pub fn default_test_pattern() -> VideoSource {
    test_source::video(TEST_SIZE, TEST_FRAMERATE)
}

/// The test pattern, at whatever geometry the flags asked for.
fn test_pattern(args: &CaptureArgs) -> VideoSource {
    let size = Size::new(
        args.width.unwrap_or(TEST_SIZE.width),
        args.height.unwrap_or(TEST_SIZE.height),
    );
    test_source::video(size, args.fps.unwrap_or(TEST_FRAMERATE))
}

/// The audio source a specifier names, or `None` for `--audio none`.
///
/// # Errors
///
/// Fails if a `file:` source cannot be opened or holds no audio track.
fn audio_source(spec: &AudioSourceSpec) -> Result<Option<AudioSource>> {
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
        AudioSourceSpec::Test => {
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

/// The simulcast ladder `--renditions` describes, or a single unscaled
/// rendition when the flag was not given.
///
/// # Errors
///
/// Fails if a rung is not `<height>p`, `<width>x<height>`, or `<name>:<size>`.
pub fn renditions(args: &CaptureArgs) -> Result<Vec<VideoRendition>> {
    let codec = args.codec.into();
    let kind = video::encode::Kind::from(args.encoder);

    let rungs: Vec<(String, Option<Size>)> = match args.renditions.is_empty() {
        true => vec![("video".to_string(), None)],
        false => args
            .renditions
            .iter()
            .map(|spec| parse_rendition(spec))
            .collect::<Result<_>>()?,
    };

    // `--bitrate` names the top rung, so the ladder is scaled against whichever
    // rung is largest. A rung with no explicit size encodes at the source's
    // resolution, which is at least as large as any of the others, so an
    // unsized rung means there is nothing to scale against.
    let largest = match rungs.iter().any(|(_, size)| size.is_none()) {
        true => None,
        false => rungs
            .iter()
            .filter_map(|(_, size)| *size)
            .max_by_key(Size::pixels),
    };

    Ok(rungs
        .into_iter()
        .map(|(name, size)| {
            let mut rendition = VideoRendition::new(name)
                .with_codec(codec)
                .with_kind(kind.clone());
            if let Some(size) = size {
                rendition = rendition.with_size(size);
            }
            if let Some(bitrate) = args.bitrate {
                // Scale by pixel count against the largest rung, so a ladder
                // does not advertise the same bitrate at every size. A
                // subscriber compares its estimate against the rendition's
                // bitrate, and identical figures make the rungs
                // indistinguishable to it.
                rendition = rendition.with_bitrate(scaled_bitrate(bitrate, size, largest));
            }
            rendition
        })
        .collect())
}

/// Parses one rung of `--renditions` into its catalog name and encoded size.
fn parse_rendition(spec: &str) -> Result<(String, Option<Size>)> {
    if let Some((name, size)) = spec.split_once(':') {
        let parsed = parse_size(size).ok_or_else(|| {
            anyerr!("rendition '{spec}': '{size}' is not <height>p or <width>x<height>")
        })?;
        return Ok((name.to_string(), Some(parsed)));
    }
    // A bare rung is either a size, which names itself, or a name standing for
    // the source's own resolution.
    Ok((spec.to_string(), parse_size(spec)))
}

/// Shares `bitrate` across a ladder in proportion to pixel count.
///
/// `bitrate` is the figure for the largest rung. A rung at a quarter of the
/// pixels gets a quarter of it, floored so a very small rung is still given
/// something an encoder can work with.
fn scaled_bitrate(bitrate: u64, size: Option<Size>, largest: Option<Size>) -> u64 {
    /// Below this a rung is unusable however small it is.
    const FLOOR: u64 = 64_000;

    let (Some(size), Some(largest)) = (size, largest) else {
        return bitrate;
    };
    match largest.pixels() {
        0 => bitrate,
        total => (bitrate * size.pixels() / total).max(FLOOR),
    }
}

/// Parses `<height>p` or `<width>x<height>`.
///
/// Both dimensions are rounded up to the next even number: I420 chroma is
/// subsampled 2x2, so every stage of the pipeline rejects an odd one. The
/// `<height>p` shorthand assumes 16:9.
fn parse_size(spec: &str) -> Option<Size> {
    if let Some((width, height)) = spec.split_once(['x', 'X']) {
        let width: u32 = width.parse().ok()?;
        let height: u32 = height.parse().ok()?;
        return Some(Size::new(even(width), even(height)));
    }
    let height: u32 = spec.strip_suffix(['p', 'P'])?.parse().ok()?;
    let width = u32::try_from((u64::from(height) * 16 + 4) / 9).ok()?;
    Some(Size::new(even(width), even(height)))
}

/// Rounds up to the next even number.
fn even(value: u32) -> u32 {
    value + (value % 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("720p"), Some(Size::new(1280, 720)));
        assert_eq!(parse_size("1080p"), Some(Size::new(1920, 1080)));
        assert_eq!(parse_size("480p"), Some(Size::new(854, 480)));
        assert_eq!(parse_size("640x360"), Some(Size::new(640, 360)));
        // I420 chroma is subsampled 2x2, so an odd dimension is rounded up
        // rather than passed to an encoder that will reject it.
        assert_eq!(parse_size("641x361"), Some(Size::new(642, 362)));
        assert_eq!(parse_size("source"), None);
    }

    #[test]
    fn ladder_bitrates_scale_by_pixel_count() {
        let quarter = scaled_bitrate(
            4_000_000,
            Some(Size::new(640, 360)),
            Some(Size::new(1280, 720)),
        );
        assert_eq!(quarter, 1_000_000);

        // The top rung keeps the figure it was given.
        let top = scaled_bitrate(
            4_000_000,
            Some(Size::new(1280, 720)),
            Some(Size::new(1280, 720)),
        );
        assert_eq!(top, 4_000_000);

        // A tiny rung still gets something an encoder can work with.
        let tiny = scaled_bitrate(
            4_000_000,
            Some(Size::new(16, 16)),
            Some(Size::new(1920, 1080)),
        );
        assert_eq!(tiny, 64_000);

        // Nothing to scale against: the figure passes through.
        assert_eq!(scaled_bitrate(4_000_000, None, None), 4_000_000);
    }

    #[test]
    fn rendition_rungs_parse() {
        assert_eq!(
            parse_rendition("720p").unwrap(),
            ("720p".to_string(), Some(Size::new(1280, 720)))
        );
        assert_eq!(
            parse_rendition("low:640x360").unwrap(),
            ("low".to_string(), Some(Size::new(640, 360)))
        );
        assert_eq!(
            parse_rendition("source").unwrap(),
            ("source".to_string(), None)
        );
        assert!(parse_rendition("low:wide").is_err());
    }
}
