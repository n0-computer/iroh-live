use clap::{Args, ValueEnum};
// Re-export shared source spec types from moq-media so callers that
// already import from this module keep working.
pub use iroh_live::media::source_spec::{AudioSourceSpec, BackendRef, DeviceRef, VideoSourceSpec};
use iroh_live::{
    media::{
        codec::{AudioCodec, VideoCodec},
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

    /// Additionally push to a relay, specified by its iroh endpoint ID.
    ///
    /// The connection is opened as WebTransport-over-iroh (HTTP/3) so that
    /// URL-level context propagates to the relay. Combine with `--api-key`
    /// to present a JWT and `--relay-path` to set a namespace prefix.
    #[arg(long)]
    pub relay: Option<iroh::EndpointId>,

    /// Path prefix to use on the relay, appended to `<broadcast>`.
    ///
    /// Defaults to `/`. Use this when the JWT's `publish` claim scopes
    /// access to a particular namespace.
    #[arg(long, default_value = "/")]
    pub relay_path: String,

    /// API key (JWT) to present to the relay. Required when the relay
    /// enforces auth; ignored in permissive dev mode.
    #[arg(long)]
    pub api_key: Option<String>,

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
// Capture source args (shared by publish capture, call, room)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct CaptureArgs {
    /// Video source: cam[:<backend>:<device>], screen[:<backend>:<device>], file:<path>, test, none.
    ///
    /// Backend and device accept names or numeric indices from `irl devices`.
    /// Examples: cam:0, cam:v4l2:1, screen:pw:0, file:video.fmp4.
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

    /// Audio codec (opus, pcm). Default: opus.
    /// PCM sends raw samples with no compression — lower latency but higher
    /// bandwidth. Useful on local networks.
    #[arg(long, default_value = "opus")]
    pub audio_codec: String,
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
            audio_codec: "opus".to_string(),
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

    /// Returns the file path if exactly one `file:` video source is specified
    /// and no capture video sources are present.
    pub fn file_video_source(&self) -> Result<Option<std::path::PathBuf>, String> {
        let sources = self.video_sources()?;
        let files: Vec<_> = sources
            .iter()
            .filter_map(|s| match s {
                VideoSourceSpec::File { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let captures: Vec<_> = sources
            .iter()
            .filter(|s| !matches!(s, VideoSourceSpec::File { .. } | VideoSourceSpec::None))
            .collect();

        if files.is_empty() {
            return Ok(None);
        }
        if !captures.is_empty() {
            return Err(
                "cannot mix file: sources with capture sources in the same publish".to_string(),
            );
        }
        if files.len() > 1 {
            return Err("only one file: video source is supported".to_string());
        }
        Ok(Some(files.into_iter().next().unwrap()))
    }

    /// Parses the `--audio-codec` flag.
    pub fn audio_codec_parsed(&self) -> anyhow::Result<AudioCodec> {
        AudioCodec::parse_or_list(&self.audio_codec)
    }

    /// Configures video and audio sources on the broadcast from these CLI args.
    ///
    /// Parses all source specs, codec, presets, and calls `setup_video` / `setup_audio`.
    /// This consolidates the repeated pattern across publish, call, and room commands.
    pub async fn setup_broadcast(
        &self,
        broadcast: &iroh_live::media::publish::LocalBroadcast,
        audio_ctx: &iroh_live::media::AudioBackend,
    ) -> anyhow::Result<()> {
        let video_sources = self.video_sources().map_err(|e| anyhow::anyhow!("{e}"))?;
        let audio_sources = self.audio_sources().map_err(|e| anyhow::anyhow!("{e}"))?;
        let codec = self.video_codec()?;
        let presets = self.presets()?;
        let audio_preset = self.audio_preset_parsed()?;
        let audio_codec = self.audio_codec_parsed()?;

        crate::source::setup_video(broadcast, &video_sources, codec, &presets)?;
        crate::source::setup_audio(
            broadcast,
            &audio_sources,
            audio_ctx,
            audio_preset,
            audio_codec,
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Publish command (unified: capture and file sources via --video / --audio)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct PublishArgs {
    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub transport: TransportArgs,

    /// Open egui preview window with controls.
    #[arg(long)]
    pub preview: bool,

    /// Input media format (for file: sources).
    #[arg(long, value_enum, default_value_t = ImportFormat::Fmp4)]
    pub format: ImportFormat,

    /// Re-encode file sources with ffmpeg.
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
// Shared ticket resolution
// ---------------------------------------------------------------------------

/// Resolves a [`LiveTicket`] from the three-way CLI pattern:
/// positional ticket, `--endpoint-id` + `--name`, or error.
pub fn resolve_ticket(
    ticket: &Option<iroh_live::ticket::LiveTicket>,
    endpoint_id: &Option<iroh::EndpointId>,
    broadcast_name: &Option<String>,
) -> anyhow::Result<iroh_live::ticket::LiveTicket> {
    match (ticket, endpoint_id, broadcast_name) {
        (Some(t), None, None) => Ok(t.clone()),
        (None, Some(id), Some(name)) => Ok(iroh_live::ticket::LiveTicket::new(*id, name.clone())),
        _ => anyhow::bail!("provide either <TICKET> or --endpoint-id + --name"),
    }
}

// ---------------------------------------------------------------------------
// Play args (wgpu only — needs egui window)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
#[derive(Args, Debug)]
pub struct PlayArgs {
    /// Connection ticket.
    #[arg(conflicts_with = "endpoint_id", conflicts_with = "relay")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID of a direct publisher (requires --name).
    #[arg(
        long,
        conflicts_with = "ticket",
        conflicts_with = "relay",
        requires = "play_name"
    )]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Relay endpoint ID — subscribe through an iroh-live-relay over H3.
    /// Requires `--name`. Combine with `--api-key` to present a JWT.
    #[arg(long, conflicts_with = "ticket", conflicts_with = "endpoint_id")]
    pub relay: Option<iroh::EndpointId>,

    /// URL path prefix sent to the relay. Used with `--relay`.
    #[arg(long, default_value = "/")]
    pub relay_path: String,

    /// API key (JWT) to present to the relay. Used with `--relay`.
    #[arg(long)]
    pub api_key: Option<String>,

    /// Broadcast name (with --endpoint-id or --relay).
    #[arg(long = "name", id = "play_name", conflicts_with = "ticket")]
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

    /// Resolves the subscribe source, covering both direct-peer and relay
    /// subscribe modes.
    pub fn source(&self) -> anyhow::Result<SubscribeSource> {
        SubscribeSource::resolve(
            &self.ticket,
            &self.endpoint_id,
            &self.relay,
            &self.relay_path,
            &self.api_key,
            &self.broadcast_name,
        )
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
    #[arg(conflicts_with = "endpoint_id", conflicts_with = "relay")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID of a direct publisher (requires --name).
    #[arg(
        long,
        conflicts_with = "ticket",
        conflicts_with = "relay",
        requires = "record_name"
    )]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Relay endpoint ID — subscribe through an iroh-live-relay over H3.
    /// Requires `--name`. Combine with `--api-key` to present a JWT.
    #[arg(
        long,
        conflicts_with = "ticket",
        conflicts_with = "endpoint_id",
        requires = "record_name"
    )]
    pub relay: Option<iroh::EndpointId>,

    /// URL path prefix sent to the relay. Used with `--relay`.
    #[arg(long, default_value = "/")]
    pub relay_path: String,

    /// API key (JWT) to present to the relay. Used with `--relay`.
    #[arg(long)]
    pub api_key: Option<String>,

    /// Broadcast name (with --endpoint-id or --relay).
    #[arg(long = "name", id = "record_name", conflicts_with = "ticket")]
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

/// Where a subscribe-shaped command (`irl play`, `irl record`) pulls from.
///
/// Keeps the three sources (ticket, direct endpoint, relay) behind one
/// enum so the CLI layer handles the resolution once and commands
/// consume the result.
pub enum SubscribeSource {
    /// Direct iroh peer subscribe (ticket or endpoint-id).
    Direct(iroh_live::ticket::LiveTicket),
    /// Subscribe from a relay via H3 with an optional JWT.
    Relay {
        target: iroh_live::relay::RelayTarget,
        broadcast_name: String,
    },
}

impl SubscribeSource {
    /// Builds a source from the common argument triple. Callers pass the
    /// `Option`s directly from their `clap::Args` struct.
    pub fn resolve(
        ticket: &Option<iroh_live::ticket::LiveTicket>,
        endpoint_id: &Option<iroh::EndpointId>,
        relay: &Option<iroh::EndpointId>,
        relay_path: &str,
        api_key: &Option<String>,
        broadcast_name: &Option<String>,
    ) -> anyhow::Result<Self> {
        if let Some(relay_id) = *relay {
            let name = broadcast_name
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--relay requires --name"))?;
            let target = iroh_live::relay::RelayTarget::new(relay_id)
                .with_path(relay_path)
                .with_api_key(api_key.clone());
            Ok(SubscribeSource::Relay {
                target,
                broadcast_name: name,
            })
        } else {
            Ok(SubscribeSource::Direct(resolve_ticket(
                ticket,
                endpoint_id,
                broadcast_name,
            )?))
        }
    }
}

impl RecordArgs {
    /// Resolves the record source, covering both direct-peer and relay
    /// subscribe modes.
    pub fn source(&self) -> anyhow::Result<SubscribeSource> {
        SubscribeSource::resolve(
            &self.ticket,
            &self.endpoint_id,
            &self.relay,
            &self.relay_path,
            &self.api_key,
            &self.broadcast_name,
        )
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

// ---------------------------------------------------------------------------
// Run args (TOML-configured multi-stream session)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to the TOML config file.
    pub config: std::path::PathBuf,
}
