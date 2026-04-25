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
    sources::RelayOffer,
};

/// Parses a relay attachment from the CLI text format
/// `<endpoint_id>[=<jwt>][@<path>]`.
///
/// The `=<jwt>` segment carries an API key, the `@<path>` segment
/// a URL path prefix. Both are optional. Path order matters: `@`
/// always opens the path segment, `=` opens the api key segment.
/// Examples:
///
/// ```text
/// 1234abcd...
/// 1234abcd...=eyJhbGciOi...
/// 1234abcd...@/streams
/// 1234abcd...=eyJhbGciOi...@/streams
/// ```
fn parse_relay_offer(s: &str) -> Result<RelayOffer, String> {
    let (head, path) = match s.split_once('@') {
        Some((h, p)) => (h, format!("/{}", p.trim_start_matches('/'))),
        None => (s, "/".to_string()),
    };
    let (id_str, api_key) = match head.split_once('=') {
        Some((id, key)) => (id, Some(key.to_string())),
        None => (head, None),
    };
    let endpoint = id_str
        .parse::<iroh::EndpointId>()
        .map_err(|e| format!("invalid endpoint id `{id_str}`: {e}"))?;
    Ok(RelayOffer {
        endpoint,
        api_key,
        path,
    })
}

// ---------------------------------------------------------------------------
// Transport args (shared by publish capture + publish file)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct TransportArgs {
    /// Broadcast name.
    #[arg(long, default_value = "hello")]
    pub name: String,

    /// Additionally push to one or more relays. Repeat the flag for
    /// each relay. Each value parses as
    /// `<endpoint_id>[=<jwt>][@<path>]`: the optional `=<jwt>`
    /// segment is the API key, the optional `@<path>` segment is
    /// the URL path prefix (defaults to `/`).
    ///
    /// Connections are opened as WebTransport-over-iroh (HTTP/3) so
    /// URL-level context propagates to the relay.
    #[arg(long = "relay", value_name = "SPEC", value_parser = parse_relay_offer)]
    pub relays: Vec<RelayOffer>,

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
    /// PCM sends raw samples with no compression: lower latency but
    /// higher bandwidth. Useful on local networks.
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
// Play args (wgpu only — needs egui window)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
#[derive(Args, Debug)]
pub struct PlayArgs {
    /// Connection ticket.
    #[arg(conflicts_with = "endpoint_id", conflicts_with = "relays")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID of a direct publisher. Requires `--name`.
    /// Can be combined with one or more `--relay` flags so the
    /// subscription has both direct and relay candidate sources.
    #[arg(long, conflicts_with = "ticket", requires = "play_name")]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Relay attachment for the subscription. Same syntax as in
    /// `irl publish`: `<endpoint_id>[=<jwt>][@<path>]`. Repeat the
    /// flag to attach multiple relays as fallback sources.
    /// Requires `--name`.
    #[arg(
        long = "relay",
        value_name = "SPEC",
        value_parser = parse_relay_offer,
        conflicts_with = "ticket",
        requires = "play_name"
    )]
    pub relays: Vec<RelayOffer>,

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

    /// Resolves the subscribe source, covering ticket, direct peer,
    /// and one or more relay attachments.
    pub fn source(&self) -> anyhow::Result<SubscribeSource> {
        SubscribeSource::resolve(
            &self.ticket,
            &self.endpoint_id,
            &self.relays,
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
    #[arg(conflicts_with = "endpoint_id", conflicts_with = "relays")]
    pub ticket: Option<iroh_live::ticket::LiveTicket>,

    /// Remote endpoint ID of a direct publisher. Requires `--name`.
    /// Can be combined with one or more `--relay` flags so the
    /// subscription has both direct and relay candidate sources.
    #[arg(long, conflicts_with = "ticket", requires = "record_name")]
    pub endpoint_id: Option<iroh::EndpointId>,

    /// Relay attachment for the subscription. Same syntax as in
    /// `irl publish`: `<endpoint_id>[=<jwt>][@<path>]`. Repeat the
    /// flag to attach multiple relays as fallback sources.
    /// Requires `--name`.
    #[arg(
        long = "relay",
        value_name = "SPEC",
        value_parser = parse_relay_offer,
        conflicts_with = "ticket",
        requires = "record_name"
    )]
    pub relays: Vec<RelayOffer>,

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
/// Resolved from a positional ticket, an `--endpoint-id`, or one or
/// more `--relay` flags. Carries the broadcast name and a populated
/// [`SourceSet`](iroh_live::sources::SourceSet) so the caller can
/// pass it straight to [`Live::subscribe`].
///
/// [`Live::subscribe`]: iroh_live::Live::subscribe
pub struct SubscribeSource {
    pub broadcast_name: String,
    pub sources: iroh_live::sources::SourceSet,
}

impl SubscribeSource {
    /// Builds a source set from the parsed argument triple.
    ///
    /// Accepted shapes:
    ///
    /// - positional ticket alone, which embeds its own direct peer
    ///   and any baked-in relay offers
    /// - `--endpoint-id` with optional `--relay` flags, plus
    ///   `--name`
    /// - one or more `--relay` flags with `--name`
    ///
    /// The resulting [`SourceSet`] contains every direct peer and
    /// relay candidate discovered in those inputs.
    ///
    /// [`SourceSet`]: iroh_live::sources::SourceSet
    pub fn resolve(
        ticket: &Option<iroh_live::ticket::LiveTicket>,
        endpoint_id: &Option<iroh::EndpointId>,
        relays: &[RelayOffer],
        broadcast_name: &Option<String>,
    ) -> anyhow::Result<Self> {
        use iroh_live::sources::{SourceSet, TransportSource};

        // Ticket-only path: every source is encoded in the ticket.
        if let Some(t) = ticket {
            if endpoint_id.is_some() || !relays.is_empty() || broadcast_name.is_some() {
                anyhow::bail!(
                    "ticket already carries direct peer, relays, and broadcast name; do not mix with --endpoint-id, --relay, or --name"
                );
            }
            return Ok(Self {
                broadcast_name: t.broadcast_name.clone(),
                sources: t.source_set(),
            });
        }

        // Otherwise we need at least one of --endpoint-id or --relay,
        // and a --name.
        let name = broadcast_name
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--name is required with --endpoint-id or --relay"))?;
        if endpoint_id.is_none() && relays.is_empty() {
            anyhow::bail!("provide either <TICKET>, --endpoint-id + --name, or --relay + --name");
        }

        let mut sources = SourceSet::new();
        if let Some(id) = endpoint_id {
            sources.push(TransportSource::direct(iroh::EndpointAddr::new(*id)));
        }
        for spec in relays {
            sources.push(TransportSource::relay(spec.to_target()));
        }
        Ok(Self {
            broadcast_name: name,
            sources,
        })
    }
}

impl RecordArgs {
    /// Resolves the record source, covering ticket, direct-peer, and
    /// one or more relay attachments.
    pub fn source(&self) -> anyhow::Result<SubscribeSource> {
        SubscribeSource::resolve(
            &self.ticket,
            &self.endpoint_id,
            &self.relays,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> String {
        // Any valid 32-byte base32 endpoint id round-trips.
        iroh::SecretKey::generate().public().to_string()
    }

    #[test]
    fn relay_offer_id_only() {
        let id = sample_id();
        let offer = parse_relay_offer(&id).expect("parse id-only");
        assert_eq!(offer.endpoint.to_string(), id);
        assert_eq!(offer.api_key, None);
        assert_eq!(offer.path, "/");
    }

    #[test]
    fn relay_offer_with_jwt() {
        let id = sample_id();
        let s = format!("{id}=eyJhbGciOiJIUzI1NiJ9.body.sig");
        let offer = parse_relay_offer(&s).expect("parse id=jwt");
        assert_eq!(offer.endpoint.to_string(), id);
        assert_eq!(
            offer.api_key.as_deref(),
            Some("eyJhbGciOiJIUzI1NiJ9.body.sig")
        );
        assert_eq!(offer.path, "/");
    }

    #[test]
    fn relay_offer_with_path() {
        let id = sample_id();
        let s = format!("{id}@streams/room1");
        let offer = parse_relay_offer(&s).expect("parse id@path");
        assert_eq!(offer.path, "/streams/room1");
        assert_eq!(offer.api_key, None);
    }

    #[test]
    fn relay_offer_with_jwt_and_path() {
        let id = sample_id();
        let s = format!("{id}=token123@/streams");
        let offer = parse_relay_offer(&s).expect("parse id=jwt@path");
        assert_eq!(offer.api_key.as_deref(), Some("token123"));
        assert_eq!(offer.path, "/streams");
    }

    #[test]
    fn relay_offer_invalid_id() {
        assert!(parse_relay_offer("not-an-endpoint").is_err());
    }
}
