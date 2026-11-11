//! Parsing for the `--video` and `--audio` source specifiers.
//!
//! A specifier is a kind and an optional id, such as `cam:2` or
//! `file:clip.mp4`. The ids are the ones `irl devices` prints. There is no
//! backend segment: the platform picks the backend for a device.
//!
//! Both specifiers implement [`FromStr`], so clap rejects a bad one while it
//! parses the arguments, and a config file while it loads.

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, de};

/// A parsed `--video` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VideoSourceSpec {
    /// A camera, by the id `irl devices` reports. `None` opens the default.
    Camera(Option<String>),
    /// A whole display. `None` opens the main one.
    ///
    /// On Linux the desktop portal picks, and the id is ignored.
    Display(Option<String>),
    /// A single window, by id. macOS only.
    Window(String),
    /// Every window of one application, by bundle id. macOS only.
    App(String),
    /// The Raspberry Pi camera, driven through `rpicam-vid`.
    ///
    /// A kind of its own because a Pi's `/dev/video0` is the Unicam node. It
    /// delivers raw Bayer that only libcamera can drive.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Rpicam(RpicamMode),
    /// A generated picture, for publishing without a camera.
    Test(TestPattern),
    /// A media file, imported rather than encoded.
    File {
        /// Path to the file.
        path: PathBuf,
        /// Restart at the beginning on end of file.
        looping: bool,
    },
    /// Publish no video.
    None,
}

impl FromStr for VideoSourceSpec {
    type Err = String;

    /// Parses a `--video` specifier.
    ///
    /// Fails with a message naming the accepted forms if `spec` is not one of
    /// them, or if a form that needs an identifier was given without one.
    fn from_str(spec: &str) -> Result<Self, String> {
        let (kind, rest) = split_once(spec);
        match kind.to_lowercase().as_str() {
            "cam" | "camera" => Ok(Self::Camera(rest.map(str::to_string))),
            "screen" | "display" => Ok(Self::Display(rest.map(str::to_string))),
            "window" => rest
                .map(|id| Self::Window(id.to_string()))
                .ok_or_else(|| "window: needs an id (e.g. window:1042)".to_string()),
            "app" => rest
                .map(|id| Self::App(id.to_string()))
                .ok_or_else(|| "app: needs a bundle id (e.g. app:com.apple.Safari)".to_string()),
            // No id, because `rpicam-vid` picks the camera.
            "rpicam" | "picam" => match rest.map(str::to_lowercase).as_deref() {
                None => rpicam(RpicamMode::Encoded),
                Some("raw") => rpicam(RpicamMode::Raw),
                Some(other) => Err(format!(
                    "rpicam: takes no camera id, and the only suffix is \
                     ':raw' for uncompressed pictures; got '{other}'"
                )),
            },
            "test" => match rest.map(str::to_lowercase).as_deref() {
                None | Some("timing") => Ok(Self::Test(TestPattern::Timing)),
                Some("gradient") => Ok(Self::Test(TestPattern::Gradient)),
                Some(other) => Err(format!(
                    "test: the patterns are 'timing' (the default) and \
                     'gradient'; got '{other}'"
                )),
            },
            "file" => {
                let rest = rest.ok_or("file: needs a path (e.g. file:clip.mp4)")?;
                let (path, looping) = split_loop(rest);
                Ok(Self::File {
                    path: PathBuf::from(path),
                    looping,
                })
            }
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown video source '{other}': expected cam, screen, window, app, \
                 rpicam, rpicam:raw, file, test, or none"
            )),
        }
    }
}

/// The generated picture `test` and `test:<name>` publish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TestPattern {
    /// A sweeping bar, a frame counter, a clock, and a flashing marker.
    ///
    /// The marker flashes with the test tone's beep.
    ///
    /// Shows smoothness, dropped frames, latency, and A/V sync. See
    /// `iroh_live::media::VideoSource::test_pattern`.
    #[default]
    Timing,
    /// A moving gradient. Cheap, and different in every frame.
    Gradient,
}

/// What `rpicam-vid` hands over: `rpicam` or `rpicam:raw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpicamMode {
    /// H.264 from the Pi's hardware encoder, published unchanged.
    ///
    /// The cheapest option, and the right one for a Pi Zero. There is no
    /// preview, no choice of encoder, and no ladder.
    Encoded,
    /// Raw pictures, which we encode ourselves.
    ///
    /// Costs about 10 MB/s of pipe at 640x360, plus an encode. Allows
    /// `--preview`, `--encoder`, `--codec`, and a ladder.
    Raw,
}

/// Returns the `rpicam` source.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn rpicam(mode: RpicamMode) -> Result<VideoSourceSpec, String> {
    Ok(VideoSourceSpec::Rpicam(mode))
}

/// Fails with a message that names the missing feature.
#[cfg(not(all(target_os = "linux", feature = "rpicam")))]
fn rpicam(_mode: RpicamMode) -> Result<VideoSourceSpec, String> {
    Err(
        "rpicam: this build has no Raspberry Pi camera source; it needs Linux \
         and the 'rpicam' feature"
            .to_string(),
    )
}

/// A parsed `--audio` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioSourceSpec {
    /// An input device, by the id `irl devices` reports.
    ///
    /// `None` opens the default microphone.
    Microphone(Option<String>),
    /// Everything the machine is playing. macOS only.
    System,
    /// A generated tone, for publishing without a microphone.
    Test(TestTone),
    /// An audio file, decoded and encoded like any other PCM source.
    File {
        /// Path to the file.
        path: PathBuf,
        /// Restart at the beginning on end of file.
        looping: bool,
    },
    /// Publish no audio.
    None,
}

/// The generated tone `test` and `test:<name>` publish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TestTone {
    /// A beep every second, silent in between.
    ///
    /// Each beep sounds while the test pattern's marker is lit, to check A/V
    /// sync.
    #[default]
    Beeps,
    /// A continuous sine tone. Dropouts are easy to hear.
    Tone,
}

impl FromStr for AudioSourceSpec {
    type Err = String;

    /// Parses an `--audio` specifier.
    ///
    /// An unknown kind is a device name, such as `hw:0,1`. Fails for `file:`
    /// without a path and for an unknown test tone.
    fn from_str(spec: &str) -> Result<Self, String> {
        let (kind, rest) = split_once(spec);
        match kind.to_lowercase().as_str() {
            "mic" | "microphone" | "default" => Ok(Self::Microphone(rest.map(str::to_string))),
            "system" | "system-audio" => Ok(Self::System),
            "test" => match rest.map(str::to_lowercase).as_deref() {
                None | Some("beeps") => Ok(Self::Test(TestTone::Beeps)),
                Some("tone") => Ok(Self::Test(TestTone::Tone)),
                Some(other) => Err(format!(
                    "test: the tones are 'beeps' (the default) and 'tone'; \
                     got '{other}'"
                )),
            },
            "none" => Ok(Self::None),
            "file" => {
                let rest = rest.ok_or("file: needs a path (e.g. file:music.mp3)")?;
                let (path, looping) = split_loop(rest);
                Ok(Self::File {
                    path: PathBuf::from(path),
                    looping,
                })
            }
            _ => Ok(Self::Microphone(Some(spec.to_string()))),
        }
    }
}

impl fmt::Display for VideoSourceSpec {
    /// Writes the specifier in the form [`FromStr`] reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Camera(id) => with_id(f, "cam", id.as_deref()),
            Self::Display(id) => with_id(f, "screen", id.as_deref()),
            Self::Window(id) => with_id(f, "window", Some(id)),
            Self::App(id) => with_id(f, "app", Some(id)),
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam(RpicamMode::Encoded) => f.write_str("rpicam"),
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam(RpicamMode::Raw) => f.write_str("rpicam:raw"),
            Self::Test(TestPattern::Timing) => f.write_str("test"),
            Self::Test(TestPattern::Gradient) => f.write_str("test:gradient"),
            Self::File { path, looping } => file(f, path, *looping),
            Self::None => f.write_str("none"),
        }
    }
}

impl fmt::Display for AudioSourceSpec {
    /// Writes the specifier in the form [`FromStr`] reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Microphone(id) => with_id(f, "mic", id.as_deref()),
            Self::System => f.write_str("system"),
            Self::Test(TestTone::Beeps) => f.write_str("test"),
            Self::Test(TestTone::Tone) => f.write_str("test:tone"),
            Self::File { path, looping } => file(f, path, *looping),
            Self::None => f.write_str("none"),
        }
    }
}

impl<'de> Deserialize<'de> for VideoSourceSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for AudioSourceSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// Writes `kind`, then `:id` if there is an id.
fn with_id(f: &mut fmt::Formatter<'_>, kind: &str, id: Option<&str>) -> fmt::Result {
    match id {
        Some(id) => write!(f, "{kind}:{id}"),
        None => f.write_str(kind),
    }
}

/// Writes a `file:` specifier, with the `:loop` suffix if it loops.
fn file(f: &mut fmt::Formatter<'_>, path: &Path, looping: bool) -> fmt::Result {
    write!(f, "file:{}", path.display())?;
    if looping {
        f.write_str(":loop")?;
    }
    Ok(())
}

/// Splits the `:loop` suffix off a file path, if it carries one.
fn split_loop(rest: &str) -> (&str, bool) {
    if rest.to_lowercase().ends_with(":loop") {
        (&rest[..rest.len() - ":loop".len()], true)
    } else {
        (rest, false)
    }
}

/// Splits a specifier into its kind and the identifier that follows.
///
/// Only the first colon separates, because ids and paths such as `hw:0,1` and
/// `C:\clips\demo.mp4` contain their own.
fn split_once(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once(':') {
        Some((kind, rest)) => (kind, Some(rest)),
        None => (spec, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_specifiers_parse_to_their_kinds() {
        assert_eq!(
            "cam".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Camera(None)
        );
        assert_eq!(
            "screen".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Display(None)
        );
        assert_eq!(
            "test".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Test(TestPattern::Timing)
        );
        assert_eq!(
            "none".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::None
        );
    }

    /// The bare specifier is the timing pattern, and the gradient has a name.
    #[test]
    fn test_patterns_by_name() {
        assert_eq!(
            "test:timing".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Test(TestPattern::Timing)
        );
        assert_eq!(
            "test:GRADIENT".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Test(TestPattern::Gradient)
        );
        let err = "test:bars".parse::<VideoSourceSpec>().unwrap_err();
        assert!(err.contains("timing"), "{err}");
    }

    #[test]
    fn test_tones_by_name() {
        assert_eq!(
            "test:beeps".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::Test(TestTone::Beeps)
        );
        assert_eq!(
            "test:tone".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::Test(TestTone::Tone)
        );
        let err = "test:sine".parse::<AudioSourceSpec>().unwrap_err();
        assert!(err.contains("beeps"), "{err}");
    }

    #[test]
    fn video_device_ids_survive_colons() {
        assert_eq!(
            "cam:/dev/video2".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Camera(Some("/dev/video2".into()))
        );
        assert_eq!(
            "file:C:/clips/demo.mp4".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::File {
                path: "C:/clips/demo.mp4".into(),
                looping: false,
            }
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    fn rpicam_specifiers_parse_with_and_without_raw() {
        assert_eq!(
            "rpicam".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Encoded)
        );
        assert_eq!(
            "picam".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Encoded)
        );
        assert!("rpicam:0".parse::<VideoSourceSpec>().is_err());
    }

    /// `:raw` asks for pictures instead of the hardware encoder's H.264.
    #[test]
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    fn rpicam_raw_is_a_mode_of_rpicam() {
        assert_eq!(
            "rpicam:raw".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Raw)
        );
        assert_eq!(
            "picam:RAW".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Raw)
        );
        let err = "rpicam:yuv".parse::<VideoSourceSpec>().unwrap_err();
        assert!(err.contains(":raw"), "{err}");
    }

    /// A build without the source names the missing feature.
    #[test]
    #[cfg(not(all(target_os = "linux", feature = "rpicam")))]
    fn rpicam_is_refused_without_the_feature() {
        let err = "rpicam".parse::<VideoSourceSpec>().unwrap_err();
        assert!(err.contains("'rpicam' feature"), "{err}");
    }

    #[test]
    fn forms_that_need_an_id_are_refused_without_one() {
        assert!("window".parse::<VideoSourceSpec>().is_err());
        assert!("app".parse::<VideoSourceSpec>().is_err());
        assert!("file".parse::<VideoSourceSpec>().is_err());
        assert!("webcam".parse::<VideoSourceSpec>().is_err());
    }

    #[test]
    fn a_video_file_takes_a_loop_suffix() {
        assert_eq!(
            "file:/tmp/clip.mp4:loop"
                .parse::<VideoSourceSpec>()
                .unwrap(),
            VideoSourceSpec::File {
                path: "/tmp/clip.mp4".into(),
                looping: true,
            }
        );
    }

    #[test]
    fn specifiers_display_in_the_form_they_parse_from() {
        for spec in [
            "cam",
            "cam:/dev/video2",
            "screen:1",
            "window:1042",
            "app:com.apple.Safari",
            "test",
            "test:gradient",
            "file:C:/clips/demo.mp4:loop",
            "none",
        ] {
            let parsed: VideoSourceSpec = spec.parse().expect(spec);
            assert_eq!(parsed.to_string(), spec);
            assert_eq!(parsed.to_string().parse(), Ok(parsed));
        }
        for spec in [
            "mic",
            "mic:hw:0,1",
            "system",
            "test",
            "test:tone",
            "file:a.mp3",
            "none",
        ] {
            let parsed: AudioSourceSpec = spec.parse().expect(spec);
            assert_eq!(parsed.to_string(), spec);
        }
        let device: AudioSourceSpec = "hw:0,1".parse().expect("a device name");
        assert_eq!(device.to_string().parse(), Ok(device));
    }

    #[test]
    fn audio_specifiers_parse_to_their_kinds() {
        assert_eq!(
            "mic".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::Microphone(None)
        );
        assert_eq!(
            "test".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::Test(TestTone::Beeps)
        );
        assert_eq!(
            "none".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::None
        );
        assert_eq!(
            "system".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::System
        );
    }

    #[test]
    fn an_unknown_audio_specifier_is_a_device_name() {
        assert_eq!(
            "hw:0,1".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::Microphone(Some("hw:0,1".into()))
        );
    }

    #[test]
    fn an_audio_file_takes_a_loop_suffix() {
        assert_eq!(
            "file:/tmp/song.flac:loop"
                .parse::<AudioSourceSpec>()
                .unwrap(),
            AudioSourceSpec::File {
                path: "/tmp/song.flac".into(),
                looping: true,
            }
        );
        assert_eq!(
            "file:music.mp3".parse::<AudioSourceSpec>().unwrap(),
            AudioSourceSpec::File {
                path: "music.mp3".into(),
                looping: false,
            }
        );
    }
}
