use clap::{Args, Subcommand, ValueEnum};
use iroh::EndpointId;
use iroh_live::{
    media::{
        capture::CaptureBackend,
        codec::VideoCodec,
        format::{AudioPreset, VideoPreset},
    },
    rooms::RoomTicket,
    ticket::LiveTicket,
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

/// Parsed video source specification from CLI `--video` values.
#[derive(Debug, Clone)]
pub enum VideoSourceSpec {
    DefaultCamera,
    Camera {
        backend: Option<CaptureBackend>,
        id: Option<String>,
    },
    DefaultScreen,
    Screen {
        backend: Option<CaptureBackend>,
        id: Option<String>,
    },
    Test,
    None,
}

impl VideoSourceSpec {
    /// Parses a `--video` CLI value.
    ///
    /// Forms: `cam`, `cam:<id>`, `cam:<backend>:<id>`,
    ///        `screen`, `screen:<id>`, `screen:<backend>:<id>`,
    ///        `test`, `none`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        match parts[0].to_lowercase().as_str() {
            "cam" | "camera" => parse_device_spec(&parts[1..]).map(|(b, id)| {
                if b.is_none() && id.is_none() {
                    Self::DefaultCamera
                } else {
                    Self::Camera { backend: b, id }
                }
            }),
            "screen" => parse_device_spec(&parts[1..]).map(|(b, id)| {
                if b.is_none() && id.is_none() {
                    Self::DefaultScreen
                } else {
                    Self::Screen { backend: b, id }
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

fn parse_device_spec(parts: &[&str]) -> Result<(Option<CaptureBackend>, Option<String>), String> {
    match parts.len() {
        0 => Ok((None, None)),
        1 => {
            if let Ok(backend) = parts[0].parse::<CaptureBackend>() {
                Ok((Some(backend), None))
            } else {
                Ok((None, Some(parts[0].to_string())))
            }
        }
        2 => {
            let backend = parts[0].parse::<CaptureBackend>()?;
            Ok((Some(backend), Some(parts[1].to_string())))
        }
        _ => Err("too many ':' segments; expected at most backend:id".to_string()),
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
    /// Video source: cam[:<id>], screen[:<backend>:<id>], test, none.
    ///
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
    #[arg(long)]
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
// Play args
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct PlayArgs {
    /// Connection ticket.
    #[arg(conflicts_with = "endpoint_id")]
    pub ticket: Option<LiveTicket>,

    /// Remote endpoint ID (requires --name).
    #[arg(long, conflicts_with = "ticket", requires = "play_name")]
    pub endpoint_id: Option<EndpointId>,

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

    /// Audio output device id.
    #[arg(long)]
    pub audio_device: Option<String>,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

impl PlayArgs {
    pub fn ticket(&self) -> anyhow::Result<LiveTicket> {
        match (&self.ticket, &self.endpoint_id, &self.broadcast_name) {
            (Some(t), None, None) => Ok(t.clone()),
            (None, Some(id), Some(name)) => Ok(LiveTicket::new(*id, name.clone())),
            _ => anyhow::bail!("provide either <TICKET> or --endpoint-id + --name"),
        }
    }

    pub fn decoder_backend(&self) -> anyhow::Result<iroh_live::media::format::DecoderBackend> {
        use iroh_live::media::format::DecoderBackend;
        match self.decoder.as_str() {
            "auto" => Ok(DecoderBackend::Auto),
            "software" | "sw" => Ok(DecoderBackend::Software),
            other => anyhow::bail!("unknown decoder backend: {other}; use 'auto' or 'sw'"),
        }
    }
}

// ---------------------------------------------------------------------------
// Call args
// ---------------------------------------------------------------------------

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
// Room args
// ---------------------------------------------------------------------------

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
