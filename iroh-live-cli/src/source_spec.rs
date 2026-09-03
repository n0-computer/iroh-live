//! Parsing for the `--video` and `--audio` source specifiers.
//!
//! A specifier names a kind of source and, optionally, which device of that
//! kind: `cam`, `cam:2`, `screen`, `window:1042`, `file:clip.mp4`. The
//! identifiers are the ones `irl devices` prints.
//!
//! There is no backend segment. `moq_video::capture::Source` and
//! `moq_audio::capture::Source` name a device and let the platform pick the
//! backend that reaches it, so a grammar that let you write `cam:v4l2:1` would
//! be describing a choice the caller no longer makes.

use std::path::PathBuf;

/// A parsed `--video` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSourceSpec {
    /// A camera, by the id `irl devices` reports. `None` opens the default.
    Camera(Option<String>),
    /// A whole display. `None` opens the main one; on Linux the desktop portal
    /// owns the choice and the id is ignored.
    Display(Option<String>),
    /// A single window, by id. macOS only.
    Window(String),
    /// Every window of one application, by bundle id. macOS only.
    App(String),
    /// The Raspberry Pi camera, driven through `rpicam-vid`.
    ///
    /// Its own kind rather than a camera id, because it is not a capture
    /// device: `/dev/video0` on a Pi is the Unicam node and hands back raw
    /// Bayer that only libcamera can drive.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Rpicam(RpicamMode),
    /// A colour-bar test pattern, for publishing without a camera.
    Test,
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
            // No id: `rpicam-vid` picks the camera, so one given here would be
            // dropped rather than honoured. The one suffix it takes names what
            // the camera app hands over.
            "rpicam" | "picam" => match rest.map(str::to_lowercase).as_deref() {
                None => rpicam(RpicamMode::Encoded),
                Some("raw") => rpicam(RpicamMode::Raw),
                Some(other) => Err(format!(
                    "rpicam: takes no camera id, and the only suffix is \
                     ':raw' for uncompressed pictures; got '{other}'"
                )),
            },
            "test" => Ok(Self::Test),
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

/// What `rpicam-vid` hands over, which `rpicam` and `rpicam:raw` pick between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpicamMode {
    /// The H.264 the Pi's hardware encoder produced, published unchanged.
    ///
    /// The cheapest thing a Pi can stream, and the only thing a Pi Zero should
    /// be asked to. Nothing downstream sees a picture, so a preview, a software
    /// encode, and a simulcast ladder are all out of reach.
    Encoded,
    /// Raw pictures, which we encode ourselves.
    ///
    /// Costs the pipe (about 10 MB/s at 640x360) and an encode, and buys
    /// everything that needs pixels: `--preview`, `--encoder`, `--codec`, and a
    /// ladder with more than one rung.
    Raw,
}

/// The `rpicam` specifier, for a build that has the source.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn rpicam(mode: RpicamMode) -> Result<VideoSourceSpec, String> {
    Ok(VideoSourceSpec::Rpicam(mode))
}

/// The `rpicam` specifier, for a build that does not have the source.
///
/// Answering with the "unknown video source" message would be misleading: the
/// specifier is spelled correctly and this build simply cannot serve it.
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
    /// An input device, by the id `irl devices` reports. `None` opens the
    /// default microphone.
    Microphone(Option<String>),
    /// Everything the machine is playing. macOS only.
    System,
    /// A steady tone, for publishing without a microphone.
    Test,
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

impl AudioSourceSpec {
    /// Parses an `--audio` specifier.
    ///
    /// An unrecognised specifier is taken as a device name, so an ALSA-style
    /// `hw:0,1` reaches the right device without quoting rules.
    ///
    /// # Errors
    ///
    /// Returns a message if `file:` was given without a path.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (kind, rest) = split_once(spec);
        match kind.to_lowercase().as_str() {
            "mic" | "microphone" | "default" => Ok(Self::Microphone(rest.map(str::to_string))),
            "system" | "system-audio" => Ok(Self::System),
            "test" => Ok(Self::Test),
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
///
/// Both `--video` and `--audio` spell looping this way, so both parse it here.
fn split_loop(rest: &str) -> (&str, bool) {
    match rest.to_lowercase().ends_with(":loop") {
        true => (&rest[..rest.len() - ":loop".len()], true),
        false => (rest, false),
    }
}

/// Splits a specifier into its kind and the identifier that follows.
///
/// Only the first colon separates: device ids and file paths carry their own
/// (`hw:0,1`, `C:\clips\demo.mp4`), so everything after it stays intact.
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
            VideoSourceSpec::Test
        );
        assert_eq!(
            VideoSourceSpec::parse("none").unwrap(),
            VideoSourceSpec::None
        );
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

    /// A build without the source names the reason rather than pretending the
    /// specifier was a typo.
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
            AudioSourceSpec::Test
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
