//! Parsing for the `--video` and `--audio` source specifiers.
//!
//! A specifier is a kind and an optional id, such as `cam:2` or
//! `file:clip.mp4`. The ids are the ones `irl devices` prints. There is no
//! backend segment: the platform picks the backend for a device.

use std::path::PathBuf;

/// A parsed `--video` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSourceSpec {
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

impl VideoSourceSpec {
    /// Parses a `--video` specifier.
    ///
    /// # Errors
    ///
    /// Returns a message naming the accepted forms if `spec` is not one of
    /// them, or if a form that needs an identifier was given without one.
    pub fn parse(spec: &str) -> Result<Self, String> {
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
pub enum TestPattern {
    /// A sweeping bar, a frame counter, a clock, and a flashing marker.
    ///
    /// The marker flashes with the test tone's beep.
    ///
    /// Shows smoothness, dropped frames, latency, and A/V sync. See
    /// `iroh_live_media::VideoSource::test_pattern`.
    #[default]
    Timing,
    /// A moving gradient. Cheap, and different in every frame.
    Gradient,
}

/// What `rpicam-vid` hands over: `rpicam` or `rpicam:raw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpicamMode {
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
pub enum AudioSourceSpec {
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
pub enum TestTone {
    /// A beep every second, silent in between.
    ///
    /// Each beep sounds while the test pattern's marker is lit, to check A/V
    /// sync.
    #[default]
    Beeps,
    /// A continuous sine tone. Dropouts are easy to hear.
    Tone,
}

impl AudioSourceSpec {
    /// Parses an `--audio` specifier.
    ///
    /// An unknown kind is a device name, such as `hw:0,1`. Fails for `file:`
    /// without a path and for an unknown test tone.
    pub fn parse(spec: &str) -> Result<Self, String> {
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

/// Splits the `:loop` suffix off a file path, if it carries one.
fn split_loop(rest: &str) -> (&str, bool) {
    match rest.to_lowercase().ends_with(":loop") {
        true => (&rest[..rest.len() - ":loop".len()], true),
        false => (rest, false),
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
    fn video_kinds() {
        assert_eq!(
            VideoSourceSpec::parse("cam").unwrap(),
            VideoSourceSpec::Camera(None)
        );
        assert_eq!(
            VideoSourceSpec::parse("screen").unwrap(),
            VideoSourceSpec::Display(None)
        );
        assert_eq!(
            VideoSourceSpec::parse("test").unwrap(),
            VideoSourceSpec::Test(TestPattern::Timing)
        );
        assert_eq!(
            VideoSourceSpec::parse("none").unwrap(),
            VideoSourceSpec::None
        );
    }

    /// The bare specifier is the timing pattern, and the gradient has a name.
    #[test]
    fn test_patterns_by_name() {
        assert_eq!(
            VideoSourceSpec::parse("test:timing").unwrap(),
            VideoSourceSpec::Test(TestPattern::Timing)
        );
        assert_eq!(
            VideoSourceSpec::parse("test:GRADIENT").unwrap(),
            VideoSourceSpec::Test(TestPattern::Gradient)
        );
        let err = VideoSourceSpec::parse("test:bars").unwrap_err();
        assert!(err.contains("timing"), "{err}");
    }

    #[test]
    fn test_tones_by_name() {
        assert_eq!(
            AudioSourceSpec::parse("test:beeps").unwrap(),
            AudioSourceSpec::Test(TestTone::Beeps)
        );
        assert_eq!(
            AudioSourceSpec::parse("test:tone").unwrap(),
            AudioSourceSpec::Test(TestTone::Tone)
        );
        let err = AudioSourceSpec::parse("test:sine").unwrap_err();
        assert!(err.contains("beeps"), "{err}");
    }

    #[test]
    fn video_device_ids_survive_colons() {
        assert_eq!(
            VideoSourceSpec::parse("cam:/dev/video2").unwrap(),
            VideoSourceSpec::Camera(Some("/dev/video2".into()))
        );
        assert_eq!(
            VideoSourceSpec::parse("file:C:/clips/demo.mp4").unwrap(),
            VideoSourceSpec::File {
                path: "C:/clips/demo.mp4".into(),
                looping: false,
            }
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    fn video_rpicam() {
        assert_eq!(
            VideoSourceSpec::parse("rpicam").unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Encoded)
        );
        assert_eq!(
            VideoSourceSpec::parse("picam").unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Encoded)
        );
        assert!(VideoSourceSpec::parse("rpicam:0").is_err());
    }

    /// `:raw` asks for pictures instead of the hardware encoder's H.264.
    #[test]
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    fn video_rpicam_raw() {
        assert_eq!(
            VideoSourceSpec::parse("rpicam:raw").unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Raw)
        );
        assert_eq!(
            VideoSourceSpec::parse("picam:RAW").unwrap(),
            VideoSourceSpec::Rpicam(RpicamMode::Raw)
        );
        let err = VideoSourceSpec::parse("rpicam:yuv").unwrap_err();
        assert!(err.contains(":raw"), "{err}");
    }

    /// A build without the source names the missing feature.
    #[test]
    #[cfg(not(all(target_os = "linux", feature = "rpicam")))]
    fn video_rpicam_needs_the_feature() {
        let err = VideoSourceSpec::parse("rpicam").unwrap_err();
        assert!(err.contains("'rpicam' feature"), "{err}");
    }

    #[test]
    fn video_forms_that_need_an_id() {
        assert!(VideoSourceSpec::parse("window").is_err());
        assert!(VideoSourceSpec::parse("app").is_err());
        assert!(VideoSourceSpec::parse("file").is_err());
        assert!(VideoSourceSpec::parse("webcam").is_err());
    }

    #[test]
    fn video_file_loop_suffix() {
        assert_eq!(
            VideoSourceSpec::parse("file:/tmp/clip.mp4:loop").unwrap(),
            VideoSourceSpec::File {
                path: "/tmp/clip.mp4".into(),
                looping: true,
            }
        );
    }

    #[test]
    fn audio_kinds() {
        assert_eq!(
            AudioSourceSpec::parse("mic").unwrap(),
            AudioSourceSpec::Microphone(None)
        );
        assert_eq!(
            AudioSourceSpec::parse("test").unwrap(),
            AudioSourceSpec::Test(TestTone::Beeps)
        );
        assert_eq!(
            AudioSourceSpec::parse("none").unwrap(),
            AudioSourceSpec::None
        );
        assert_eq!(
            AudioSourceSpec::parse("system").unwrap(),
            AudioSourceSpec::System
        );
    }

    #[test]
    fn audio_unknown_is_a_device_name() {
        assert_eq!(
            AudioSourceSpec::parse("hw:0,1").unwrap(),
            AudioSourceSpec::Microphone(Some("hw:0,1".into()))
        );
    }

    #[test]
    fn audio_file_loop_suffix() {
        assert_eq!(
            AudioSourceSpec::parse("file:/tmp/song.flac:loop").unwrap(),
            AudioSourceSpec::File {
                path: "/tmp/song.flac".into(),
                looping: true,
            }
        );
        assert_eq!(
            AudioSourceSpec::parse("file:music.mp3").unwrap(),
            AudioSourceSpec::File {
                path: "music.mp3".into(),
                looping: false,
            }
        );
    }
}
