//! Text specifiers that name a video or an audio source, and open it.
//!
//! A specifier is a kind and an optional id, such as `cam:2`, `screen`,
//! `test:gradient` or `file:clip.mp4:loop`. The ids are the ones the device
//! listings report, such as `moq_video::capture::cameras`. There is no
//! backend segment: the platform picks the backend for a device.
//!
//! Both specifiers implement [`FromStr`] and [`Display`](fmt::Display), so a
//! `--video` or `--audio` flag parses straight into one, and a specifier
//! written out reads back the same. With the `serde` feature they also
//! deserialize from, and serialize to, the same string.
//! [`VideoSourceSpec::open`] and [`AudioSourceSpec::open`] open the source a
//! specifier names.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use moq_ext_sources::spec::{VideoOpenOptions, VideoSourceSpec};
//!
//! let spec: VideoSourceSpec = "test:gradient".parse()?;
//! let source = spec.open(&VideoOpenOptions::default()).await?;
//! # drop(source);
//! # Ok(())
//! # }
//! ```

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

#[cfg(any(feature = "capture", feature = "pattern"))]
use crate::audio;
use crate::{AudioSource, Error, VideoSource, video};

/// An error from parsing a source specifier.
///
/// The message names the forms the specifier accepts.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
#[display("{_0}")]
pub struct ParseSpecError(String);

impl std::error::Error for ParseSpecError {}

impl From<String> for ParseSpecError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for ParseSpecError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

/// A parsed video source specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoSourceSpec {
    /// A camera, by the id the camera listing reports. `None` opens the default.
    Camera(Option<String>),
    /// A whole display. `None` opens the main one.
    ///
    /// On Linux, `None` and `portal` let the user pick a screen in the desktop
    /// portal's dialog. An `x11:` id opens that X11 display, which on Wayland
    /// shows only XWayland's windows.
    Display(Option<String>),
    /// A single window, by id. Supported on macOS, Windows and X11.
    ///
    /// On Linux, `portal` lets the user pick a window in the desktop portal's
    /// dialog.
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
    /// A media file.
    ///
    /// [`open`](VideoSourceSpec::open) decodes its H.264 video into raw
    /// pictures. A publisher can also import the encoded video as it is.
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
    type Err = ParseSpecError;

    /// Parses a video source specifier.
    ///
    /// Fails with a message naming the accepted forms if `spec` is not one of
    /// them, or if a form that needs an identifier was given without one.
    fn from_str(spec: &str) -> Result<Self, ParseSpecError> {
        let (kind, rest) = split_once(spec);
        match kind.to_lowercase().as_str() {
            "cam" | "camera" => Ok(Self::Camera(rest.map(str::to_string))),
            "screen" | "display" => Ok(Self::Display(rest.map(str::to_string))),
            "window" => rest
                .map(|id| Self::Window(id.to_string()))
                .ok_or_else(|| "window: needs an id (e.g. window:1042)".into()),
            "app" => rest
                .map(|id| Self::App(id.to_string()))
                .ok_or_else(|| "app: needs a bundle id (e.g. app:com.apple.Safari)".into()),
            // No id, because `rpicam-vid` picks the camera.
            "rpicam" | "picam" => match rest.map(str::to_lowercase).as_deref() {
                None => rpicam(RpicamMode::Encoded),
                Some("raw") => rpicam(RpicamMode::Raw),
                Some(other) => Err(format!(
                    "rpicam: takes no camera id, and the only suffix is \
                     ':raw' for uncompressed pictures; got '{other}'"
                )
                .into()),
            },
            "test" => match rest.map(str::to_lowercase).as_deref() {
                None | Some("timing") => Ok(Self::Test(TestPattern::Timing)),
                Some("gradient") => Ok(Self::Test(TestPattern::Gradient)),
                Some("bounce") => Ok(Self::Test(TestPattern::Bounce)),
                Some(other) => Err(format!(
                    "test: the patterns are 'timing' (the default), \
                     'gradient' and 'bounce'; got '{other}'"
                )
                .into()),
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
            )
            .into()),
        }
    }
}

/// The generated picture `test` and `test:<name>` publish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TestPattern {
    /// A sweeping bar, a frame counter, a clock, and a flashing marker.
    ///
    /// The marker flashes with the test tone's beep.
    ///
    /// Shows smoothness, dropped frames, latency, and A/V sync. See
    /// [`VideoSource::test_pattern`].
    #[default]
    Timing,
    /// A moving gradient. Cheap, and different in every frame.
    Gradient,
    /// A white square bouncing left and right on a background that drifts through the hues.
    ///
    /// See [`BouncePattern`](crate::BouncePattern).
    Bounce,
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
    /// Costs about 10 MB/s of pipe at 640x360, plus an encode. Allows a
    /// preview, a choice of encoder and codec, and a ladder.
    Raw,
}

/// Returns the `rpicam` source.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn rpicam(mode: RpicamMode) -> Result<VideoSourceSpec, ParseSpecError> {
    Ok(VideoSourceSpec::Rpicam(mode))
}

/// Fails with a message that names the missing feature.
#[cfg(not(all(target_os = "linux", feature = "rpicam")))]
fn rpicam(_mode: RpicamMode) -> Result<VideoSourceSpec, ParseSpecError> {
    Err(
        "rpicam: this build has no Raspberry Pi camera source; it needs Linux \
         and the 'rpicam' feature"
            .into(),
    )
}

/// A parsed audio source specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioSourceSpec {
    /// An input device, by the id the microphone listing reports.
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

impl FromStr for AudioSourceSpec {
    type Err = ParseSpecError;

    /// Parses an audio source specifier.
    ///
    /// An unknown kind is a device name, such as `hw:0,1`. Fails for `file:`
    /// without a path and for an unknown test tone.
    fn from_str(spec: &str) -> Result<Self, ParseSpecError> {
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
                )
                .into()),
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
            Self::Test(TestPattern::Bounce) => f.write_str("test:bounce"),
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

/// Implements serde for a specifier as the string it parses from and displays as.
#[cfg(feature = "serde")]
macro_rules! serde_as_string {
    ($spec:ty) => {
        impl<'de> serde::Deserialize<'de> for $spec {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }

        impl serde::Serialize for $spec {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }
    };
}

#[cfg(feature = "serde")]
serde_as_string!(VideoSourceSpec);
#[cfg(feature = "serde")]
serde_as_string!(AudioSourceSpec);

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

/// The id of `screen:portal` and `window:portal`.
///
/// Either lets the user pick in the desktop portal's screen-sharing dialog.
pub const PORTAL: &str = "portal";

/// The size of a test pattern without [`VideoOpenOptions::with_size`].
const TEST_SIZE: video::Size = video::Size {
    width: 1280,
    height: 720,
};

/// The capture size of the Raspberry Pi camera without [`VideoOpenOptions::with_size`].
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const RPICAM_SIZE: video::Size = video::Size {
    width: 640,
    height: 360,
};

/// The frame rate of a generated source without [`VideoOpenOptions::framerate`].
const GENERATED_FPS: u32 = 30;

/// The frequency of the continuous test tone, in hertz.
///
/// Concert A, low enough that no resampler can alias it.
#[cfg(feature = "pattern")]
const TEST_TONE_HZ: f32 = 440.0;

/// The channel layout of the test tones.
#[cfg(feature = "pattern")]
const TEST_TONE_LAYOUT: audio::Layout = audio::Layout::Stereo;

/// How [`VideoSourceSpec::open`] opens a source.
///
/// Every field is a hint. A device that cannot honor one picks the closest
/// mode it has.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoOpenOptions {
    /// The picture width to ask a device or a generator for.
    pub width: Option<u32>,
    /// The picture height to ask a device or a generator for.
    pub height: Option<u32>,
    /// The frame rate to ask for.
    ///
    /// A device picks its own without it, and a generator runs at 30 fps.
    pub framerate: Option<video::FrameRate>,
    /// Whether a screen or window capture draws the mouse cursor, `true` by default.
    pub cursor: bool,
    /// The app whose config directory keeps the desktop portal's grants.
    portal_grants: Option<String>,
    /// Whether the grants stay stored when the stream ends.
    portal_grants_kept_after_end: bool,
}

impl Default for VideoOpenOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            framerate: None,
            cursor: true,
            portal_grants: None,
            portal_grants_kept_after_end: false,
        }
    }
}

impl VideoOpenOptions {
    /// Asks for pictures of `size`.
    #[must_use]
    pub fn with_size(mut self, size: video::Size) -> Self {
        self.width = Some(size.width);
        self.height = Some(size.height);
        self
    }

    /// Asks for `rate` frames per second.
    #[must_use]
    pub fn with_framerate(mut self, rate: video::FrameRate) -> Self {
        self.framerate = Some(rate);
        self
    }

    /// Keeps the desktop portal's grants in `app`'s config directory.
    ///
    /// Sources opened through the portal (`screen:portal`, `window:portal`,
    /// or the default display on Wayland) reuse and store their grant as
    /// `PortalGrant::in_config_dir` does, so a later run can skip the picker.
    /// Has no effect without a portal, or when `app` is not a single file
    /// name.
    #[must_use]
    pub fn with_portal_grants(mut self, app: impl Into<String>) -> Self {
        self.portal_grants = Some(app.into());
        self
    }

    /// Keeps the grants [`with_portal_grants`](Self::with_portal_grants)
    /// stores when the stream ends, as `PortalGrant::keep_after_end` does.
    #[must_use]
    pub fn keep_portal_grants_after_end(mut self) -> Self {
        self.portal_grants_kept_after_end = true;
        self
    }

    /// Returns the size these options ask for, filling in from `default`.
    fn size_or(&self, default: video::Size) -> video::Size {
        video::Size::new(
            self.width.unwrap_or(default.width),
            self.height.unwrap_or(default.height),
        )
    }

    /// Returns the rate of a generated source.
    fn generated_rate(&self) -> video::FrameRate {
        self.framerate.unwrap_or_else(|| {
            video::FrameRate::new(GENERATED_FPS, 1).expect("30 fps is a valid rate")
        })
    }
}

impl VideoSourceSpec {
    /// Opens the source this specifier names, or returns `None` for [`None`](Self::None).
    ///
    /// A camera, display, window or app opens through
    /// [`VideoSource::capture`], so the default camera follows its rules. The
    /// default display is [`screen_source`](crate::screen_source), the
    /// desktop portal on Wayland with the `pipewire` feature. The portal asks
    /// on each open unless [`VideoOpenOptions::with_portal_grants`] keeps its
    /// grants. A test pattern defaults to 1280x720 at 30 fps. A file decodes
    /// through [`VideoSource::file`], at the size the options ask for or its
    /// own, and at its own rate.
    ///
    /// Cancellation safe, as [`VideoSource::capture`] is.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] for a source this build cannot
    /// open, such as a camera without the `capture` feature, a file without
    /// the `file` feature, or the portal off Linux. Also fails for
    /// [`RpicamMode::Encoded`], which carries encoded video and has no raw
    /// source: read `rpicam_h264` instead. Fails with [`Error::Device`] if the
    /// device or the file does not open.
    pub async fn open(&self, options: &VideoOpenOptions) -> Result<Option<VideoSource>, Error> {
        let source = match self {
            Self::None => return Ok(None),
            Self::Test(pattern) => test_pattern(*pattern, options)?,
            Self::File { path, looping } => video_file(path, *looping, options).await?,
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam(RpicamMode::Raw) => {
                let rate = options.generated_rate().rounded().max(1);
                let config = crate::RpicamConfig::new(options.size_or(RPICAM_SIZE), rate);
                VideoSource::rpicam(config).await?
            }
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam(RpicamMode::Encoded) => {
                return Err(Error::invalid(
                    "rpicam delivers H.264 that rpicam-vid encoded; read it with \
                     rpicam_h264, or use rpicam:raw for raw pictures",
                ));
            }
            Self::Camera(_) | Self::Display(_) | Self::Window(_) | Self::App(_) => {
                return capture(self.capture_source()?, options).await.map(Some);
            }
        };
        Ok(Some(source))
    }

    /// Returns the capture source of a camera, display, window or app specifier.
    fn capture_source(&self) -> Result<CaptureSource, Error> {
        Ok(match self {
            Self::Camera(id) => CaptureSource::Camera(id.clone()),
            Self::Display(Some(id)) if id == PORTAL => CaptureSource::Portal(PortalKind::Screen),
            Self::Display(id) => CaptureSource::Display(id.clone()),
            Self::Window(id) if id == PORTAL => CaptureSource::Portal(PortalKind::Window),
            Self::Window(id) => CaptureSource::Window(id.clone()),
            Self::App(id) => CaptureSource::App(id.clone()),
            other => return Err(Error::invalid(format!("{other} is no capture device"))),
        })
    }
}

/// A capture device a specifier names, before the build's capture types.
#[derive(Debug)]
#[cfg_attr(
    not(feature = "capture"),
    expect(dead_code, reason = "only read by the capture build")
)]
enum CaptureSource {
    Camera(Option<String>),
    Display(Option<String>),
    Window(String),
    App(String),
    Portal(PortalKind),
}

/// What the desktop portal's picker offers.
#[derive(Debug, Clone, Copy)]
enum PortalKind {
    Screen,
    Window,
}

/// Opens `source` through [`VideoSource::capture`] with the hints in `options`.
#[cfg(feature = "capture")]
async fn capture(source: CaptureSource, options: &VideoOpenOptions) -> Result<VideoSource, Error> {
    use video::capture::{DisplaySelection, Source};
    let source = match source {
        CaptureSource::Camera(id) => Source::Camera(id),
        CaptureSource::Display(None) => screen(options),
        CaptureSource::Display(Some(id)) => Source::Display(DisplaySelection::new(id)),
        CaptureSource::Window(id) => Source::Window(id),
        CaptureSource::App(id) => Source::App(id),
        CaptureSource::Portal(kind) => portal(kind, options)?,
    };
    let mut config = video::capture::Config::default();
    config.source = source;
    config.width = options.width;
    config.height = options.height;
    config.framerate = options.framerate;
    config.cursor = if options.cursor {
        video::capture::Cursor::Drawn
    } else {
        video::capture::Cursor::Hidden
    };
    VideoSource::capture(config).await
}

/// Fails, since this build has no capture.
#[cfg(not(feature = "capture"))]
#[expect(clippy::unused_async, reason = "the capture build awaits")]
async fn capture(source: CaptureSource, _options: &VideoOpenOptions) -> Result<VideoSource, Error> {
    Err(Error::invalid(format!(
        "opening {source:?} needs a build with the `capture` feature"
    )))
}

/// Returns the capture source of the default display, keeping its portal grant as `options` ask.
#[cfg(feature = "capture")]
fn screen(options: &VideoOpenOptions) -> video::capture::Source {
    let source = crate::screen_source();
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if matches!(source, video::capture::Source::Portal(_)) {
        return portal_selection(PortalKind::Screen, options);
    }
    #[cfg(not(all(target_os = "linux", feature = "pipewire")))]
    let _ = options;
    source
}

/// Returns the portal source for `kind`, a selection the user picks in the dialog.
#[cfg(all(feature = "capture", target_os = "linux", feature = "pipewire"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the build without a portal fails here"
)]
fn portal(kind: PortalKind, options: &VideoOpenOptions) -> Result<video::capture::Source, Error> {
    Ok(portal_selection(kind, options))
}

/// Returns a portal selection for `kind`, seeded from the grant `options` keep.
#[cfg(all(feature = "capture", target_os = "linux", feature = "pipewire"))]
fn portal_selection(kind: PortalKind, options: &VideoOpenOptions) -> video::capture::Source {
    use video::capture::portal::{Kind, Selection};
    let kind = match kind {
        PortalKind::Screen => Kind::Screen,
        PortalKind::Window => Kind::Window,
    };
    let grant = options
        .portal_grants
        .as_deref()
        .and_then(|app| crate::PortalGrant::in_config_dir(kind, app))
        .map(|grant| {
            if options.portal_grants_kept_after_end {
                grant.keep_after_end()
            } else {
                grant
            }
        });
    let selection = match grant {
        Some(grant) => grant.selection(),
        None => Selection::new(kind),
    };
    video::capture::Source::Portal(selection)
}

/// Fails, since only Linux with the `pipewire` feature has the portal.
#[cfg(all(
    feature = "capture",
    not(all(target_os = "linux", feature = "pipewire"))
))]
fn portal(kind: PortalKind, _options: &VideoOpenOptions) -> Result<video::capture::Source, Error> {
    Err(Error::invalid(format!(
        "the desktop portal's {kind:?} picker needs a Linux build with the `pipewire` feature"
    )))
}

/// Decodes the video file at `path`, at the size in `options` if they give both dimensions.
#[cfg(feature = "file")]
async fn video_file(
    path: &Path,
    looping: bool,
    options: &VideoOpenOptions,
) -> Result<VideoSource, Error> {
    let size = match (options.width, options.height) {
        (Some(width), Some(height)) => Some(video::Size::new(width, height)),
        _ => None,
    };
    VideoSource::file(path, file_end(looping), size).await
}

/// Fails, since this build reads no files.
#[cfg(not(feature = "file"))]
#[expect(clippy::unused_async, reason = "the file build awaits")]
async fn video_file(
    path: &Path,
    _looping: bool,
    _options: &VideoOpenOptions,
) -> Result<VideoSource, Error> {
    Err(Error::invalid(format!(
        "decoding {} needs a build with the `file` feature",
        path.display()
    )))
}

/// Returns what a file source does at the end of the file.
#[cfg(feature = "file")]
fn file_end(looping: bool) -> crate::FileEnd {
    if looping {
        crate::FileEnd::Restart
    } else {
        crate::FileEnd::Stop
    }
}

/// Starts the test pattern `pattern` names, at the size and rate in `options`.
#[cfg(feature = "pattern")]
fn test_pattern(pattern: TestPattern, options: &VideoOpenOptions) -> Result<VideoSource, Error> {
    let size = options.size_or(TEST_SIZE);
    let rate = options.generated_rate();
    match pattern {
        TestPattern::Timing => VideoSource::test_pattern(size, rate),
        TestPattern::Gradient => VideoSource::test_gradient(size, rate),
        TestPattern::Bounce => VideoSource::test_bounce(size, rate),
    }
}

/// Fails, since this build has no generated pictures.
#[cfg(not(feature = "pattern"))]
fn test_pattern(_pattern: TestPattern, options: &VideoOpenOptions) -> Result<VideoSource, Error> {
    let _ = (options.size_or(TEST_SIZE), options.generated_rate());
    Err(Error::invalid(
        "a test pattern needs a build with the `pattern` feature",
    ))
}

impl AudioSourceSpec {
    /// Opens the source this specifier names, or returns `None` for [`None`](Self::None).
    ///
    /// A microphone opens with the default capture config and no echo
    /// cancellation. Build the config yourself and call
    /// [`AudioSource::microphone`] to attach a canceller. The test tones are
    /// stereo.
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] for a source this build cannot
    /// open, such as a microphone without the `capture` feature or a file
    /// without the `file` feature, and with [`Error::Device`] if the device
    /// or the file does not open.
    pub async fn open(&self) -> Result<Option<AudioSource>, Error> {
        match self {
            Self::None => Ok(None),
            spec => open_audio(spec).await.map(Some),
        }
    }
}

/// Opens the audio source `spec` names, which is not [`AudioSourceSpec::None`].
#[cfg_attr(
    not(any(feature = "capture", feature = "file")),
    expect(clippy::unused_async, reason = "only devices and files await")
)]
async fn open_audio(spec: &AudioSourceSpec) -> Result<AudioSource, Error> {
    match spec {
        #[cfg(feature = "capture")]
        AudioSourceSpec::Microphone(id) => {
            let mut config = audio::capture::Config::default();
            if let Some(id) = id {
                config.source = audio::capture::Source::Microphone(Some(id.clone()));
            }
            AudioSource::microphone(config).await
        }
        #[cfg(feature = "capture")]
        AudioSourceSpec::System => {
            let mut config = audio::capture::Config::default();
            config.source = audio::capture::Source::System;
            AudioSource::microphone(config).await
        }
        #[cfg(feature = "pattern")]
        AudioSourceSpec::Test(TestTone::Beeps) => AudioSource::test_pattern(TEST_TONE_LAYOUT),
        #[cfg(feature = "pattern")]
        AudioSourceSpec::Test(TestTone::Tone) => AudioSource::tone(TEST_TONE_HZ, TEST_TONE_LAYOUT),
        #[cfg(feature = "file")]
        AudioSourceSpec::File { path, looping } => {
            AudioSource::file(path, file_end(*looping)).await
        }
        #[allow(
            unreachable_patterns,
            reason = "reached only in a build without one of the features"
        )]
        other => Err(Error::invalid(format!(
            "this build cannot open the audio source {other}; it needs the \
             `capture`, `pattern` or `file` feature"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test pattern opens at the default size, or at the size the options ask for.
    #[cfg(feature = "pattern")]
    #[tokio::test]
    async fn a_test_pattern_opens_at_the_size_asked_for() {
        let spec: VideoSourceSpec = "test:gradient".parse().unwrap();
        let source = spec
            .open(&VideoOpenOptions::default())
            .await
            .unwrap()
            .expect("a source");
        assert_eq!(source.format().size, TEST_SIZE);
        let options = VideoOpenOptions {
            width: Some(320),
            height: Some(240),
            ..Default::default()
        };
        let source = spec.open(&options).await.unwrap().expect("a source");
        assert_eq!(source.format().size, video::Size::new(320, 240));
    }

    /// `test:bounce` runs as a clocked source of I420 pictures at the size asked for.
    #[cfg(feature = "pattern")]
    #[tokio::test]
    async fn the_bounce_pattern_runs_as_a_source() {
        let spec: VideoSourceSpec = "test:bounce".parse().unwrap();
        let options = VideoOpenOptions {
            width: Some(320),
            height: Some(180),
            ..Default::default()
        };
        let source = spec.open(&options).await.unwrap().expect("a source");
        let frame = source.frames().next().await.expect("a frame");
        let video::Surface::I420(image) = &frame.surface else {
            panic!("not I420");
        };
        assert_eq!(image.size(), video::Size::new(320, 180));
    }

    /// `none` opens nothing, and a missing file fails to open.
    #[tokio::test]
    async fn none_opens_nothing_and_a_missing_file_fails() {
        let options = VideoOpenOptions::default();
        assert!(
            VideoSourceSpec::None
                .open(&options)
                .await
                .unwrap()
                .is_none()
        );
        let file: VideoSourceSpec = "file:missing.mp4".parse().unwrap();
        let expected = if cfg!(feature = "file") {
            matches!(file.open(&options).await, Err(Error::Device { .. }))
        } else {
            matches!(file.open(&options).await, Err(Error::InvalidConfig { .. }))
        };
        assert!(expected);
        assert!(AudioSourceSpec::None.open().await.unwrap().is_none());
    }

    /// A video file opens as a raw source at the size the options ask for, and at its own rate.
    #[cfg(feature = "file")]
    #[tokio::test]
    async fn a_video_file_opens_as_a_raw_source() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source/test_data/clip.mp4");
        let spec: VideoSourceSpec = format!("file:{}:loop", path.display()).parse().unwrap();
        let options = VideoOpenOptions::default().with_size(video::Size::new(32, 24));
        let source = spec.open(&options).await.unwrap().expect("a source");
        assert_eq!(source.format().size, video::Size::new(32, 24));
        assert_eq!(source.format().rate, video::FrameRate::new(10, 1).unwrap());
        let frame = source.frames().next().await.expect("a frame");
        assert_eq!(frame.size(), video::Size::new(32, 24));
    }

    /// The test tones open in every build with generated sources.
    #[cfg(feature = "pattern")]
    #[tokio::test]
    async fn the_test_tones_open() {
        for spec in ["test", "test:tone"] {
            let spec: AudioSourceSpec = spec.parse().unwrap();
            assert!(spec.open().await.unwrap().is_some(), "{spec}");
        }
    }

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
        assert_eq!(
            "test:bounce".parse::<VideoSourceSpec>().unwrap(),
            VideoSourceSpec::Test(TestPattern::Bounce)
        );
        let err = "test:bars".parse::<VideoSourceSpec>().unwrap_err();
        assert!(err.to_string().contains("timing"), "{err}");
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
        assert!(err.to_string().contains("beeps"), "{err}");
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
        assert!(err.to_string().contains(":raw"), "{err}");
    }

    /// A build without the source names the missing feature.
    #[test]
    #[cfg(not(all(target_os = "linux", feature = "rpicam")))]
    fn rpicam_is_refused_without_the_feature() {
        let err = "rpicam".parse::<VideoSourceSpec>().unwrap_err();
        assert!(err.to_string().contains("'rpicam' feature"), "{err}");
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
            "screen:portal",
            "screen:x11:0",
            "window:1042",
            "window:portal",
            "app:com.apple.Safari",
            "test",
            "test:gradient",
            "test:bounce",
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
