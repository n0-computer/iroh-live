use clap::{Args, Subcommand, ValueEnum};
use iroh_live::{
    media::{
        capture::CaptureBackend,
        codec::VideoCodec,
        format::{AudioPreset, VideoPreset},
    },
    rooms::RoomTicket,
};

// ---------------------------------------------------------------------------
// Transport args (shared by publish capture + publish file)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct TransportArgs {
    /// Broadcast name.
    #[arg(long, default_value = "hello")]
    pub name: String,

    /// Additionally push to a relay (iroh endpoint ID or URL).
    #[arg(long)]
    pub relay: Option<String>,

    /// Additionally publish into a room.
    #[arg(long)]
    pub room: Option<RoomTicket>,

    /// Don't accept incoming subscriber connections (push-only).
    #[arg(long)]
    pub no_serve: bool,

    /// Suppress terminal QR code.
    #[arg(long)]
    pub no_qr: bool,
}

// ---------------------------------------------------------------------------
// Source spec parsing
// ---------------------------------------------------------------------------

/// References a capture backend by name or by index from `list_backends()`.
#[derive(Debug, Clone)]
pub enum BackendRef {
    Name(CaptureBackend),
    Index(usize),
}

/// References a capture device by platform ID or by index from the device list.
#[derive(Debug, Clone)]
pub enum DeviceRef {
    Name(String),
    Index(usize),
}

/// Parsed video source specification from CLI `--video` values.
#[derive(Debug, Clone)]
pub enum VideoSourceSpec {
    DefaultCamera,
    Camera {
        backend: Option<BackendRef>,
        device: Option<DeviceRef>,
    },
    DefaultScreen,
    Screen {
        backend: Option<BackendRef>,
        device: Option<DeviceRef>,
    },
    Test,
    None,
}

impl VideoSourceSpec {
    /// Parses a `--video` CLI value.
    ///
    /// Forms: `cam`, `cam:<device>`, `cam:<backend>:<device>`,
    ///        `screen`, `screen:<device>`, `screen:<backend>:<device>`,
    ///        `test`, `none`.
    ///
    /// Both backend and device accept names or numeric indices from
    /// `irl devices`. For example, `cam:0` selects the first camera,
    /// `cam:v4l2:1` selects V4L2 camera at index 1, and `cam:0:1`
    /// selects camera 1 from backend 0.
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
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown video source '{other}'. Use cam, screen, test, or none.\n\
                 Run `irl devices` to list available sources."
            )),
        }
    }
}

fn parse_device_spec(parts: &[&str]) -> Result<(Option<BackendRef>, Option<DeviceRef>), String> {
    match parts.len() {
        0 => Ok((None, None)),
        1 => {
            // Single segment: try backend name first, then device index, then device name.
            if let Ok(backend) = parts[0].parse::<CaptureBackend>() {
                Ok((Some(BackendRef::Name(backend)), None))
            } else if let Ok(idx) = parts[0].parse::<usize>() {
                Ok((None, Some(DeviceRef::Index(idx))))
            } else {
                Ok((None, Some(DeviceRef::Name(parts[0].to_string()))))
            }
        }
        2 => {
            // Two segments: backend (name or index) + device (index or name).
            let backend = if let Ok(b) = parts[0].parse::<CaptureBackend>() {
                BackendRef::Name(b)
            } else if let Ok(idx) = parts[0].parse::<usize>() {
                BackendRef::Index(idx)
            } else {
                return Err(format!(
                    "unknown backend '{}'. Run `irl devices` to list backends.",
                    parts[0]
                ));
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

/// Parsed audio source specification from CLI `--audio` values.
#[derive(Debug, Clone)]
pub enum AudioSourceSpec {
    Default,
    Device(String),
    Test,
    None,
}

impl AudioSourceSpec {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "test" => Ok(Self::Test),
            "default" | "mic" => Ok(Self::Default),
            _ => Ok(Self::Device(s.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Capture source args (shared by publish capture, call, room)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct CaptureArgs {
    /// Video source: cam[:<backend>:<device>], screen[:<backend>:<device>], test, none.
    ///
    /// Backend and device accept names or numeric indices from `irl devices`.
    /// Examples: cam:0, cam:v4l2:1, screen:pw:0, screen:0.
    /// When any --video is given, no default source is added.
    /// Multiple --video flags publish multiple sources.
    /// Without --video, defaults to first camera.
    /// Run `irl devices` to list available sources.
    #[arg(long, verbatim_doc_comment)]
    pub video: Vec<String>,

    /// Audio source: device name/id, or "none". Default: system mic.
    /// Run `irl devices` to list devices.
    #[arg(long, verbatim_doc_comment)]
    pub audio: Vec<String>,

    /// Use synthetic SMPTE test pattern instead of camera.
    #[arg(long)]
    pub test_source: bool,

    /// Video codec (h264, av1, h264-vaapi, etc.).
    #[arg(long)]
    pub codec: Option<String>,

    /// Simulcast presets (comma-separated: 180p,360p,720p,1080p).
    #[arg(long, value_delimiter = ',')]
    pub video_presets: Option<Vec<String>>,

    /// Audio quality preset.
    #[arg(long, default_value = "hq")]
    pub audio_preset: String,
}

impl Default for CaptureArgs {
    fn default() -> Self {
        Self {
            video: Vec::new(),
            audio: Vec::new(),
            test_source: false,
            codec: None,
            video_presets: None,
            audio_preset: "hq".to_string(),
        }
    }
}

impl CaptureArgs {
    /// Resolves video sources. When `--video` is present, only explicit sources
    /// are used (no default camera). Without `--video`, defaults to first camera.
    pub fn video_sources(&self) -> Result<Vec<VideoSourceSpec>, String> {
        if self.test_source {
            return Ok(vec![VideoSourceSpec::Test]);
        }
        if self.video.is_empty() {
            return Ok(vec![VideoSourceSpec::DefaultCamera]);
        }
        self.video
            .iter()
            .map(|s| VideoSourceSpec::parse(s))
            .collect()
    }

    /// Resolves audio sources. Without `--audio`, defaults to system mic.
    pub fn audio_sources(&self) -> Result<Vec<AudioSourceSpec>, String> {
        if self.test_source {
            return Ok(vec![AudioSourceSpec::Test]);
        }
        if self.audio.is_empty() {
            return Ok(vec![AudioSourceSpec::Default]);
        }
        self.audio
            .iter()
            .map(|s| AudioSourceSpec::parse(s))
            .collect()
    }

    pub fn video_codec(&self) -> anyhow::Result<VideoCodec> {
        VideoCodec::parse_or_best(self.codec.as_deref())
    }

    pub fn presets(&self) -> anyhow::Result<Vec<VideoPreset>> {
        match &self.video_presets {
            Some(ps) => ps.iter().map(|s| VideoPreset::parse_or_list(s)).collect(),
            None => Ok(VideoPreset::all().to_vec()),
        }
    }

    pub fn audio_preset_parsed(&self) -> anyhow::Result<AudioPreset> {
        AudioPreset::parse_or_list(&self.audio_preset)
    }
}

// ---------------------------------------------------------------------------
// Publish command (with subcommands: capture, file)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct PublishArgs {
    #[command(subcommand)]
    pub input: Option<PublishInput>,

    #[command(flatten)]
    pub transport: TransportArgs,

    /// Open egui preview window with controls (capture only).
    #[arg(long, global = true)]
    pub preview: bool,
}

#[derive(Subcommand, Debug)]
pub enum PublishInput {
    /// Publish live capture (camera, screen, mic). This is the default.
    Capture(CaptureArgs),
    /// Publish a media file.
    File(FileInputArgs),
}

#[derive(Args, Debug)]
pub struct FileInputArgs {
    /// Input file (reads stdin if omitted).
    pub file: Option<std::path::PathBuf>,

    /// Input media format.
    #[arg(long, value_enum, default_value_t = ImportFormat::Fmp4)]
    pub format: ImportFormat,

    /// Re-encode with ffmpeg.
    #[arg(long)]
    pub transcode: bool,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ImportFormat {
    #[default]
    Fmp4,
    Avc3,
}

// ---------------------------------------------------------------------------
// Play args (wgpu only — needs egui window)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
#[derive(Args, Debug)]
pub struct PlayArgs {
    /// Connection ticket.
    #[arg(conflicts_with = "endpoint_id")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID (requires --name).
    #[arg(long, conflicts_with = "ticket", requires = "play_name")]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Broadcast name (with --endpoint-id).
    #[arg(
        long = "name",
        id = "play_name",
        conflicts_with = "ticket",
        requires = "endpoint_id"
    )]
    pub broadcast_name: Option<String>,

    /// No video — audio only, no window opened.
    #[arg(long)]
    pub no_video: bool,

    /// Decoder backend: auto or sw.
    #[arg(long, default_value = "auto")]
    pub decoder: String,

    /// Render mode: auto (wgpu-accelerated) or cpu (software).
    #[arg(long, default_value = "auto")]
    pub render: String,

    /// Audio output device id.
    #[arg(long)]
    pub audio_device: Option<String>,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

/// Controls whether video rendering uses GPU acceleration or software fallback.
#[cfg(feature = "wgpu")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    /// Use wgpu-accelerated rendering (default).
    #[default]
    Auto,
    /// Use CPU-only software rendering.
    Cpu,
}

#[cfg(feature = "wgpu")]
impl PlayArgs {
    pub fn render_mode(&self) -> anyhow::Result<RenderMode> {
        match self.render.to_lowercase().as_str() {
            "auto" | "wgpu" | "gpu" => Ok(RenderMode::Auto),
            "cpu" | "sw" | "software" => Ok(RenderMode::Cpu),
            other => anyhow::bail!("unknown render mode: '{other}'; use 'auto' or 'cpu'"),
        }
    }

    pub fn ticket(&self) -> anyhow::Result<iroh_live::ticket::LiveTicket> {
        match (&self.ticket, &self.endpoint_id, &self.broadcast_name) {
            (Some(t), None, None) => Ok(t.clone()),
            (None, Some(id), Some(name)) => {
                Ok(iroh_live::ticket::LiveTicket::new(*id, name.clone()))
            }
            _ => anyhow::bail!("provide either <TICKET> or --endpoint-id + --name"),
        }
    }

    pub fn decoder_backend(&self) -> anyhow::Result<iroh_live::media::format::DecoderBackend> {
        self.decoder.parse().map_err(|_| {
            anyhow::anyhow!(
                "unknown decoder backend: '{}'; use 'auto' or 'sw'",
                self.decoder
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Record args (no wgpu needed — headless recording)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct RecordArgs {
    /// Connection ticket.
    #[arg(conflicts_with = "endpoint_id")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID (requires --name).
    #[arg(long, conflicts_with = "ticket", requires = "record_name")]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Broadcast name (with --endpoint-id).
    #[arg(
        long = "name",
        id = "record_name",
        conflicts_with = "ticket",
        requires = "endpoint_id"
    )]
    pub broadcast_name: Option<String>,

    /// Output file path. Video and audio are written to separate files
    /// with appropriate extensions (e.g. output.h264, output.opus).
    #[arg(short, long, default_value = "recording")]
    pub output: std::path::PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = RecordFormat::Raw)]
    pub format: RecordFormat,
}

/// Output recording format.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum RecordFormat {
    /// Raw bitstreams: separate files per track (.h264/.av1 + .opus).
    /// H.264 output uses Annex B framing and is playable directly with ffplay/mpv.
    /// Raw .opus files need a container — see the remux hint printed after recording.
    #[default]
    Raw,
}

impl RecordArgs {
    /// Resolves the connection ticket from CLI args.
    pub fn ticket(&self) -> anyhow::Result<iroh_live::ticket::LiveTicket> {
        match (&self.ticket, &self.endpoint_id, &self.broadcast_name) {
            (Some(t), None, None) => Ok(t.clone()),
            (None, Some(id), Some(name)) => {
                Ok(iroh_live::ticket::LiveTicket::new(*id, name.clone()))
            }
            _ => anyhow::bail!("provide either <TICKET> or --endpoint-id + --name"),
        }
    }
}

// ---------------------------------------------------------------------------
// Call args (wgpu only)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
#[derive(Args, Debug)]
pub struct CallArgs {
    /// Remote ticket to auto-dial. Omit to wait for incoming call.
    pub ticket: Option<String>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    /// Decoder backend: auto or sw.
    #[arg(long, default_value = "auto")]
    pub decoder: String,

    /// Audio output device id.
    #[arg(long)]
    pub audio_device: Option<String>,
}

// ---------------------------------------------------------------------------
// Room args (wgpu only)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
#[derive(Args, Debug)]
pub struct RoomArgs {
    /// Room ticket to join. Omit to create a new room.
    pub ticket: Option<RoomTicket>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    /// Decoder backend: auto or sw.
    #[arg(long, default_value = "auto")]
    pub decoder: String,

    /// Audio output device id.
    #[arg(long)]
    pub audio_device: Option<String>,
}
