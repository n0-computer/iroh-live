//! Shared source specifier parsing for video and audio inputs.
//!
//! The [`VideoSourceSpec`] and [`AudioSourceSpec`] types provide a
//! shared parsing layer for source specifiers like `"cam:0"`,
//! `"screen:pw:1"`, `"test"`, and `"none"`. These are the same format
//! used by the CLI and can be reused by downstream applications.

/// References a capture backend by name or by numeric index.
#[derive(Debug, Clone)]
pub enum BackendRef {
    /// A backend name string (e.g. `"pipewire"`, `"v4l2"`).
    Name(String),
    /// An index into the platform's backend list.
    Index(usize),
}

/// References a capture device by name or by numeric index.
#[derive(Debug, Clone)]
pub enum DeviceRef {
    /// A platform-specific device name or ID string.
    Name(String),
    /// An index into the device list for the selected backend.
    Index(usize),
}

/// Parsed video source specification.
///
/// Parses the `cam[:<backend>:<device>]`, `screen[:<backend>:<device>]`,
/// `preenc:<backend>[:<arg>]`, `test`, and `none` format used by the CLI
/// and demos. Both backend and device segments accept names or numeric
/// indices.
///
/// # Examples
///
/// ```
/// use moq_media::source_spec::VideoSourceSpec;
///
/// let spec = VideoSourceSpec::parse("cam").unwrap();
/// assert!(matches!(spec, VideoSourceSpec::DefaultCamera));
///
/// let spec = VideoSourceSpec::parse("screen:0").unwrap();
/// assert!(matches!(spec, VideoSourceSpec::Screen { .. }));
///
/// let spec = VideoSourceSpec::parse("test").unwrap();
/// assert!(matches!(spec, VideoSourceSpec::Test));
/// ```
#[derive(Debug, Clone)]
pub enum VideoSourceSpec {
    /// Default camera (first available).
    DefaultCamera,
    /// Camera with optional backend and device selection.
    Camera {
        /// Backend selector, if specified.
        backend: Option<BackendRef>,
        /// Device selector, if specified.
        device: Option<DeviceRef>,
    },
    /// Default screen (first available).
    DefaultScreen,
    /// Screen with optional backend and device selection.
    Screen {
        /// Backend selector, if specified.
        backend: Option<BackendRef>,
        /// Device selector, if specified.
        device: Option<DeviceRef>,
    },
    /// Synthetic SMPTE test pattern.
    Test,
    /// Media file (fmp4, h264, etc.).
    File {
        /// Path to the media file.
        path: std::path::PathBuf,
    },
    /// Pre-encoded video source that bypasses the software encode pipeline.
    ///
    /// Used when the capture device or external tool produces already-encoded
    /// packets (e.g. `rpicam-vid --codec h264` on Raspberry Pi, hardware RTSP
    /// cameras, or V4L2 M2M encoder output piped through a named device).
    ///
    /// # Pipeline behavior
    ///
    /// A `PreEncoded` source feeds [`MediaPacket`](crate::transport::MediaPacket)s
    /// directly to the [`PacketSink`](crate::transport::PacketSink), skipping
    /// the [`VideoEncoder`](rusty_codecs::traits::VideoEncoder) entirely. The
    /// publish pipeline creates the source via `VideoInput::pre_encoded()` and
    /// announces a single track in the catalog with the codec metadata from the
    /// source's [`PreEncodedVideoSource::config()`](rusty_codecs::traits::PreEncodedVideoSource::config).
    ///
    /// Because there is no raw-frame stage, rendition switching (simulcast) is
    /// not available for pre-encoded sources. Adaptive bitrate would require
    /// either multiple pre-encoded streams at different quality levels, or
    /// runtime reconfiguration of the external encoder's bitrate.
    ///
    /// # CLI format
    ///
    /// `preenc:<backend>` where `<backend>` identifies the pre-encoded source:
    /// - `preenc:libcamera` — rpicam-vid H.264 hardware encoder (Raspberry Pi)
    /// - `preenc:rtsp:<url>` — RTSP camera passthrough (future)
    ///
    /// Bitrate, resolution, and codec are configured via the backend itself
    /// (e.g. `--bitrate` and `--fps` flags for the libcamera backend), not
    /// through the source spec.
    PreEncoded {
        /// Backend identifier for the pre-encoded source (e.g. `"libcamera"`, `"rtsp"`).
        backend: String,
        /// Optional backend-specific argument (e.g. RTSP URL, device path).
        arg: Option<String>,
    },
    /// No video source.
    None,
}

impl VideoSourceSpec {
    /// Parses a video source specifier string.
    ///
    /// Recognized forms: `cam`, `cam:<device>`, `cam:<backend>:<device>`,
    /// `screen`, `screen:<device>`, `screen:<backend>:<device>`,
    /// `file:<path>`, `test`, `none`. Backend and device accept names or
    /// numeric indices.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        match parts[0].to_lowercase().as_str() {
            "cam" | "camera" => parse_device_spec(&parts[1..]).map(|(b, d)| {
                if b.is_none() && d.is_none() {
                    Self::DefaultCamera
                } else {
                    Self::Camera {
                        backend: b,
                        device: d,
                    }
                }
            }),
            "screen" => parse_device_spec(&parts[1..]).map(|(b, d)| {
                if b.is_none() && d.is_none() {
                    Self::DefaultScreen
                } else {
                    Self::Screen {
                        backend: b,
                        device: d,
                    }
                }
            }),
            "test" => Ok(Self::Test),
            "preenc" | "pre-encoded" | "preencoded" => {
                if parts.len() < 2 {
                    return Err("preenc: requires a backend (e.g. preenc:libcamera)".to_string());
                }
                let backend = parts[1].to_string();
                let arg = if parts.len() > 2 {
                    Some(parts[2..].join(":"))
                } else {
                    None
                };
                Ok(Self::PreEncoded { backend, arg })
            }
            "file" => {
                let path = parts[1..].join(":");
                if path.is_empty() {
                    return Err("file: requires a path (e.g., file:video.fmp4)".to_string());
                }
                Ok(Self::File {
                    path: std::path::PathBuf::from(path),
                })
            }
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown video source '{other}': expected cam, screen, test, file, preenc, or none"
            )),
        }
    }
}

/// Parsed audio source specification.
///
/// Recognized forms: `none`, `test`, `default` / `mic`, `file:<path>`
/// (with optional `file:<path>:loop`), or any other string as a device
/// name/ID.
#[derive(Debug, Clone)]
pub enum AudioSourceSpec {
    /// System default microphone.
    Default,
    /// A specific audio device by name/ID.
    Device(String),
    /// Audio file import via ffmpeg transcode.
    File {
        /// Path to the audio file.
        path: std::path::PathBuf,
        /// Whether to loop playback indefinitely.
        loop_playback: bool,
    },
    /// Synthetic test tone.
    Test,
    /// No audio source.
    None,
}

impl AudioSourceSpec {
    /// Parses an audio source specifier string.
    ///
    /// Supports `file:<path>` and `file:<path>:loop` for audio file import.
    pub fn parse(s: &str) -> Result<Self, String> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "none" => Ok(Self::None),
            "test" => Ok(Self::Test),
            "default" | "mic" => Ok(Self::Default),
            _ if lower.starts_with("file:") => {
                let rest = &s["file:".len()..];
                if rest.is_empty() {
                    return Err("file: requires a path (e.g. file:music.mp3)".to_string());
                }
                // Check for trailing :loop suffix (case-insensitive).
                let (path_str, loop_playback) = if rest.to_lowercase().ends_with(":loop") {
                    (&rest[..rest.len() - ":loop".len()], true)
                } else {
                    (rest, false)
                };
                Ok(Self::File {
                    path: std::path::PathBuf::from(path_str),
                    loop_playback,
                })
            }
            _ => Ok(Self::Device(s.to_string())),
        }
    }
}

/// Known capture backend short names. Used to distinguish `cam:v4l2`
/// (backend selection) from `cam:Logitech` (device name lookup).
const KNOWN_BACKENDS: &[&str] = &[
    "pw",
    "pipewire",
    "v4l2",
    "x11",
    "sck",
    "avf",
    "xcap",
    "nokhwa",
    "libcamera",
    "rpicam",
];

fn parse_device_spec(parts: &[&str]) -> Result<(Option<BackendRef>, Option<DeviceRef>), String> {
    match parts.len() {
        0 => Ok((None, None)),
        1 => {
            // Single segment precedence:
            // 1. Known backend name (e.g. "cam:v4l2" → backend V4L2)
            // 2. Numeric index (e.g. "cam:0" → device index 0)
            // 3. Device name (e.g. "cam:Logitech" → device name lookup)
            let s = parts[0];
            if KNOWN_BACKENDS.contains(&s.to_lowercase().as_str()) {
                Ok((Some(BackendRef::Name(s.to_string())), None))
            } else if let Ok(idx) = s.parse::<usize>() {
                Ok((None, Some(DeviceRef::Index(idx))))
            } else {
                Ok((None, Some(DeviceRef::Name(s.to_string()))))
            }
        }
        2 => {
            // Two segments: backend + device.
            let backend = if let Ok(idx) = parts[0].parse::<usize>() {
                BackendRef::Index(idx)
            } else {
                BackendRef::Name(parts[0].to_string())
            };
            let device = if let Ok(idx) = parts[1].parse::<usize>() {
                DeviceRef::Index(idx)
            } else {
                DeviceRef::Name(parts[1].to_string())
            };
            Ok((Some(backend), Some(device)))
        }
        _ => Err("too many ':' segments; expected at most backend:device".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_video_basic() {
        assert!(matches!(
            VideoSourceSpec::parse("cam").unwrap(),
            VideoSourceSpec::DefaultCamera
        ));
        assert!(matches!(
            VideoSourceSpec::parse("camera").unwrap(),
            VideoSourceSpec::DefaultCamera
        ));
        assert!(matches!(
            VideoSourceSpec::parse("screen").unwrap(),
            VideoSourceSpec::DefaultScreen
        ));
        assert!(matches!(
            VideoSourceSpec::parse("test").unwrap(),
            VideoSourceSpec::Test
        ));
        assert!(matches!(
            VideoSourceSpec::parse("none").unwrap(),
            VideoSourceSpec::None
        ));
    }

    #[test]
    fn parse_video_with_device_index() {
        match VideoSourceSpec::parse("cam:0").unwrap() {
            VideoSourceSpec::Camera { device, .. } => {
                assert!(matches!(device, Some(DeviceRef::Index(0))));
            }
            other => panic!("expected Camera, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_with_backend_name() {
        match VideoSourceSpec::parse("cam:v4l2").unwrap() {
            VideoSourceSpec::Camera { backend, .. } => {
                assert!(matches!(backend, Some(BackendRef::Name(ref n)) if n == "v4l2"));
            }
            other => panic!("expected Camera, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_with_backend_and_device() {
        match VideoSourceSpec::parse("cam:pw:1").unwrap() {
            VideoSourceSpec::Camera { backend, device } => {
                assert!(matches!(backend, Some(BackendRef::Name(ref n)) if n == "pw"));
                assert!(matches!(device, Some(DeviceRef::Index(1))));
            }
            other => panic!("expected Camera, got {other:?}"),
        }
    }

    #[test]
    fn parse_audio_basic() {
        assert!(matches!(
            AudioSourceSpec::parse("none").unwrap(),
            AudioSourceSpec::None
        ));
        assert!(matches!(
            AudioSourceSpec::parse("test").unwrap(),
            AudioSourceSpec::Test
        ));
        assert!(matches!(
            AudioSourceSpec::parse("mic").unwrap(),
            AudioSourceSpec::Default
        ));
        assert!(matches!(
            AudioSourceSpec::parse("default").unwrap(),
            AudioSourceSpec::Default
        ));
        assert!(matches!(
            AudioSourceSpec::parse("hw:0,1").unwrap(),
            AudioSourceSpec::Device(_)
        ));
    }

    #[test]
    fn parse_audio_file() {
        match AudioSourceSpec::parse("file:music.mp3").unwrap() {
            AudioSourceSpec::File {
                path,
                loop_playback,
            } => {
                assert_eq!(path, std::path::PathBuf::from("music.mp3"));
                assert!(!loop_playback);
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn parse_audio_file_loop() {
        match AudioSourceSpec::parse("file:/tmp/song.flac:loop").unwrap() {
            AudioSourceSpec::File {
                path,
                loop_playback,
            } => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/song.flac"));
                assert!(loop_playback);
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn parse_audio_file_empty_path_rejected() {
        assert!(AudioSourceSpec::parse("file:").is_err());
    }

    #[test]
    fn parse_video_device_name_fallback() {
        // A single non-numeric, non-backend segment falls back to device name.
        match VideoSourceSpec::parse("cam:Logitech").unwrap() {
            VideoSourceSpec::Camera { backend, device } => {
                assert!(backend.is_none(), "should not be parsed as backend");
                assert!(
                    matches!(device, Some(DeviceRef::Name(ref n)) if n == "Logitech"),
                    "expected DeviceRef::Name(\"Logitech\"), got {device:?}"
                );
            }
            other => panic!("expected Camera, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_preenc_libcamera() {
        match VideoSourceSpec::parse("preenc:libcamera").unwrap() {
            VideoSourceSpec::PreEncoded { backend, arg } => {
                assert_eq!(backend, "libcamera");
                assert!(arg.is_none());
            }
            other => panic!("expected PreEncoded, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_preenc_with_arg() {
        match VideoSourceSpec::parse("preenc:rtsp:rtsp://192.168.1.1/stream").unwrap() {
            VideoSourceSpec::PreEncoded { backend, arg } => {
                assert_eq!(backend, "rtsp");
                assert_eq!(arg.as_deref(), Some("rtsp://192.168.1.1/stream"));
            }
            other => panic!("expected PreEncoded, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_preenc_requires_backend() {
        assert!(VideoSourceSpec::parse("preenc").is_err());
    }

    #[test]
    fn parse_video_unknown_rejected() {
        assert!(VideoSourceSpec::parse("foobar").is_err());
    }
}
