//! Turning parsed specifiers into the sources `moq-media` publishes.
//!
//! A capture source is a `moq_video::capture::Config` or a
//! `moq_audio::capture::Config` and nothing more: the device is opened inside
//! the publish task, which is what lets it be released again when publishing
//! stops. The synthetic sources come from [`moq_media::test_source`], so
//! `irl publish --test-source` works on a machine with neither camera nor
//! microphone.

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

/// Frame rate of the synthetic pattern when `--fps` is not given.
const TEST_FRAMERATE: u32 = 30;

/// Resolution of the synthetic pattern when `--width` / `--height` are not
/// given.
const TEST_SIZE: Size = Size {
    width: 1280,
    height: 720,
};

/// Frequency of the synthetic tone, in hertz. Concert A: unmistakable, and low
/// enough that no resampler on the way out can alias it.
const TEST_TONE_HZ: f64 = 440.0;

/// Sample rate of the synthetic tone. Opus's native rate, so it is encoded
/// without a resampling step.
const TEST_TONE_RATE: u32 = 48_000;

/// Channel count of the synthetic tone.
const TEST_TONE_CHANNELS: u32 = 2;

/// Sets up whichever of video and audio `args` asked for.
///
/// # Errors
///
/// Fails if a specifier is unusable here (a `file:` video source belongs to the
/// import path), or if the rendition ladder does not parse. A device that will
/// not open surfaces in the log and ends its track, not here.
pub fn configure(broadcast: &LocalBroadcast, args: &CaptureArgs) -> Result<()> {
    if let Some(source) = video_source(&args.video_source()?, args)? {
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
/// Fails for a `file:` source, which the import path handles instead.
fn video_source(spec: &VideoSourceSpec, args: &CaptureArgs) -> Result<Option<VideoSource>> {
    use video::capture::Source;

    let source = match spec {
        VideoSourceSpec::None => return Ok(None),
        VideoSourceSpec::Test => test_pattern(args),
        VideoSourceSpec::File(path) => {
            return Err(anyerr!(
                "the file source {} is imported, not encoded; this is a bug in the caller",
                path.display()
            ));
        }
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

/// The synthetic pattern at its default geometry, for a caller with no flags
/// to consult.
#[cfg(feature = "render")]
pub fn default_test_pattern() -> VideoSource {
    test_source::video(TEST_SIZE, TEST_FRAMERATE)
}

/// The synthetic pattern, at whatever geometry the flags asked for.
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
    let kind = encoder_kind(&args.encoder);

    let rungs: Vec<(String, Option<Size>)> = match args.renditions.is_empty() {
        true => vec![("video".to_string(), None)],
        false => args
            .renditions
            .iter()
            .map(|spec| parse_rendition(spec))
            .collect::<Result<_>>()?,
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
                rendition = rendition.with_bitrate(bitrate);
            }
            rendition
        })
        .collect())
}

/// Maps `--encoder` onto a backend selection.
fn encoder_kind(name: &str) -> video::encode::Kind {
    match name.to_lowercase().as_str() {
        "auto" => video::encode::Kind::Auto,
        "hardware" | "hw" => video::encode::Kind::Hardware,
        "software" | "sw" => video::encode::Kind::Software,
        other => video::encode::Kind::Named(other.to_string()),
    }
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

/// Parses `<height>p` or `<width>x<height>`.
///
/// The `<height>p` shorthand assumes 16:9 and rounds the width up to the next
/// even number: I420 chroma is subsampled 2x2, so every stage of the pipeline
/// rejects an odd dimension.
fn parse_size(spec: &str) -> Option<Size> {
    if let Some((width, height)) = spec.split_once(['x', 'X']) {
        return Some(Size::new(width.parse().ok()?, height.parse().ok()?));
    }
    let height: u32 = spec.strip_suffix(['p', 'P'])?.parse().ok()?;
    let width = (u64::from(height) * 16 + 4) / 9;
    let width = u32::try_from(width + width % 2).ok()?;
    Some(Size::new(width, height))
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
        assert_eq!(parse_size("source"), None);
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
