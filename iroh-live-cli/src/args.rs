//! Command-line arguments.
//!
//! The structs here only carry what clap parsed. Turning a `--video` string
//! into a capture config is [`crate::source`]'s job, and turning a
//! `--renditions` list into a simulcast ladder and a capture frame rate is
//! [`crate::rendition`]'s.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use iroh::EndpointId;
use iroh_live::ticket::LiveTicket;
#[cfg(feature = "render")]
use iroh_rooms::RoomTicket;
use n0_error::{Result, anyerr};
use serde::Deserialize;

#[cfg(feature = "render")]
use crate::backend::DecoderArg;
use crate::{
    backend::EncoderArg,
    source_spec::{AudioSourceSpec, TestPattern, TestTone, VideoSourceSpec},
};

/// Where a broadcast is served and how its address is shared.
#[derive(Args, Debug)]
pub struct TransportArgs {
    /// Broadcast path, as it appears in the ticket.
    #[arg(long, default_value = "hello")]
    pub name: String,

    /// Also connect to this relay endpoint, which then carries the broadcast
    /// on to subscribers that cannot reach this node directly.
    #[arg(long)]
    pub relay: Option<EndpointId>,

    /// Do not accept incoming subscriber connections.
    #[arg(long)]
    pub no_serve: bool,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,
}

/// The `--video` specifier when none is given.
pub const DEFAULT_VIDEO: &str = "cam";

/// The `--audio` specifier when none is given.
pub const DEFAULT_AUDIO: &str = "mic";

/// What to capture and how to encode it.
#[derive(Args, Debug)]
pub struct CaptureArgs {
    /// Video source: `cam`, `cam:<id>`, `screen`, `screen:<id>`, `window:<id>`,
    /// `app:<id>`, `file:<path>[:loop]`, `test[:timing|:gradient]`, or `none`.
    ///
    /// Run `irl devices` for the identifiers this machine accepts.
    #[arg(long, default_value = DEFAULT_VIDEO, verbatim_doc_comment)]
    pub video: String,

    /// Audio source: `mic`, `mic:<id>`, `system`, `file:<path>[:loop]`,
    /// `test[:beeps|:tone]`, or `none`.
    ///
    /// Anything else is taken as a device name, so `hw:0,1` works as written.
    #[arg(long, default_value = DEFAULT_AUDIO, verbatim_doc_comment)]
    pub audio: String,

    /// Publish the timing pattern and its beeping tone, the same as
    /// `--video test --audio test`. The two are one diagnostic: the marker in
    /// the picture is lit for exactly as long as each beep sounds.
    #[arg(long)]
    pub test_source: bool,

    /// Video codec. H.265 needs a hardware encoder.
    #[arg(long, value_enum, default_value_t = VideoCodecArg::H264)]
    pub codec: VideoCodecArg,

    /// Encoder to use. A backend named here is the only one tried, so a
    /// machine without it fails rather than quietly encoding on the CPU.
    #[arg(long, value_enum, default_value_t = EncoderArg::Auto)]
    pub encoder: EncoderArg,

    /// Simulcast ladder, comma-separated. Each rung is `<height>p`,
    /// `<width>x<height>`, or `<name>:<width>x<height>`; a bare name encodes at
    /// the source's own resolution. Default: one rendition, unscaled.
    ///
    /// An `@<fps>` suffix asks for a capture frame rate: `720p@60`, or
    /// `high:1280x720@60,low:640x360@30`. A ladder is captured once and every
    /// rung is fed the same pictures, so the highest rate any rung names is the
    /// rate all of them run at, and a rung that asked for less says so in the
    /// log. `--fps` outranks the ladder where the two disagree.
    #[arg(long, value_delimiter = ',', verbatim_doc_comment)]
    pub renditions: Vec<String>,

    /// Target video bitrate in bits per second. Omit to derive one from the
    /// resolution. Applies to every rung of the ladder.
    #[arg(long, value_name = "BITS_PER_SECOND")]
    pub bitrate: Option<u64>,

    /// Requested capture width. The device snaps to its nearest supported mode.
    #[arg(long)]
    pub width: Option<u32>,

    /// Requested capture height.
    #[arg(long)]
    pub height: Option<u32>,

    /// Requested capture framerate. It sets the rate outright and caps the
    /// ladder's `@<fps>` rungs. Omit it to capture at 30, or at the highest
    /// rate a rung asks for. The device snaps to the nearest rate it supports
    /// either way.
    #[arg(long, verbatim_doc_comment)]
    pub fps: Option<u32>,

    /// Hide the mouse cursor. Screen, window, and application capture only.
    #[arg(long)]
    pub no_cursor: bool,

    /// Audio codec. PCM is uncompressed: lower latency, much higher bitrate.
    #[arg(long, value_enum, default_value_t = AudioCodecArg::Opus)]
    pub audio_codec: AudioCodecArg,

    /// Target audio bitrate in bits per second. Opus only.
    #[arg(long, value_name = "BITS_PER_SECOND")]
    pub audio_bitrate: Option<u32>,
}

impl Default for CaptureArgs {
    /// The same defaults clap applies, so a caller building these by hand
    /// (`irl run` reading a session file) starts where the flags do.
    fn default() -> Self {
        Self {
            video: DEFAULT_VIDEO.to_string(),
            audio: DEFAULT_AUDIO.to_string(),
            test_source: false,
            codec: VideoCodecArg::default(),
            encoder: EncoderArg::default(),
            renditions: Vec::new(),
            bitrate: None,
            width: None,
            height: None,
            fps: None,
            no_cursor: false,
            audio_codec: AudioCodecArg::default(),
            audio_bitrate: None,
        }
    }
}

impl CaptureArgs {
    /// The parsed video source.
    ///
    /// # Errors
    ///
    /// Fails if `--video` is not a recognised specifier.
    pub fn video_source(&self) -> Result<VideoSourceSpec> {
        if self.test_source {
            return Ok(VideoSourceSpec::Test(TestPattern::default()));
        }
        VideoSourceSpec::parse(&self.video).map_err(|err| anyerr!("--video: {err}"))
    }

    /// The parsed audio source.
    ///
    /// # Errors
    ///
    /// Fails if `--audio` is not a recognised specifier.
    pub fn audio_source(&self) -> Result<AudioSourceSpec> {
        if self.test_source {
            return Ok(AudioSourceSpec::Test(TestTone::default()));
        }
        AudioSourceSpec::parse(&self.audio).map_err(|err| anyerr!("--audio: {err}"))
    }
}

/// The video codec `--codec` selects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodecArg {
    /// H.264 / AVC. The widest support, and the default.
    #[default]
    H264,
    /// H.265 / HEVC. Hardware encoders only.
    H265,
}

impl From<VideoCodecArg> for iroh_live::media::video::encode::Codec {
    fn from(codec: VideoCodecArg) -> Self {
        match codec {
            VideoCodecArg::H264 => Self::H264,
            VideoCodecArg::H265 => Self::H265,
        }
    }
}

/// The audio codec `--audio-codec` selects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodecArg {
    /// Opus. The default.
    #[default]
    Opus,
    /// Uncompressed interleaved 32-bit float PCM.
    Pcm,
}

impl From<AudioCodecArg> for iroh_live::media::audio::encode::Codec {
    fn from(codec: AudioCodecArg) -> Self {
        match codec {
            AudioCodecArg::Opus => Self::Opus,
            AudioCodecArg::Pcm => Self::Pcm,
        }
    }
}

/// Arguments for `irl publish`.
#[derive(Args, Debug)]
pub struct PublishArgs {
    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub transport: TransportArgs,

    /// Open a preview window showing what is being published.
    ///
    /// Capture sources only: a file source is republished verbatim, so there
    /// are no raw frames to draw without decoding them again.
    #[arg(long)]
    pub preview: bool,

    /// Start the preview window in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Container of a `file:` video source.
    #[arg(long, value_enum, default_value_t = ImportFormat::Fmp4)]
    pub format: ImportFormat,

    /// Re-mux (or re-encode) a `file:` video source through ffmpeg first.
    ///
    /// A plain (non-fragmented) MP4 has to go through this before it can be
    /// read as a stream. Also what `file:<path>:loop` needs, since ffmpeg is
    /// what repeats the input.
    #[arg(long)]
    pub transcode: bool,
}

/// The container a `file:` video source is read as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ImportFormat {
    /// Fragmented MP4 / CMAF.
    #[default]
    Fmp4,
    /// A raw H.264 Annex-B elementary stream.
    Avc3,
}

/// How a subscriber decodes the video it receives.
///
/// The counterpart of the encoder half of [`CaptureArgs`], and only meaningful
/// where a window draws the picture: `irl record` remuxes what arrives without
/// decoding it, and `irl watch --no-video` opens no video track at all.
#[cfg(feature = "render")]
#[derive(Args, Debug, Clone, Copy, Default)]
pub struct PlaybackArgs {
    /// Decoder to use. A backend named here is the only one tried, so a
    /// machine without it fails rather than quietly falling back to software.
    #[arg(long, value_enum, default_value_t = DecoderArg::Auto)]
    pub decoder: DecoderArg,
}

/// Arguments for `irl call`.
#[cfg(feature = "render")]
#[derive(Args, Debug)]
pub struct CallArgs {
    /// Ticket of the peer to call, as its `irl call` printed it. Omit to wait
    /// for somebody to call this node.
    pub ticket: Option<LiveTicket>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

/// Arguments for `irl room`.
#[cfg(feature = "render")]
#[derive(Args, Debug)]
pub struct RoomArgs {
    /// Ticket of the room to join, as another participant's `irl room` printed
    /// it. Omit to open a new room.
    pub ticket: Option<RoomTicket>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Name the other participants see. Defaults to this node's short endpoint
    /// id.
    #[arg(long)]
    pub display_name: Option<String>,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

/// The remote broadcast a subscriber connects to.
///
/// Two spellings for one thing: the ticket a publisher printed, or the
/// endpoint id and broadcast path it is made of. `irl watch` and `irl record`
/// both take it, so both accept exactly the same forms.
#[derive(Args, Debug)]
pub struct RemoteArgs {
    /// Connection ticket, as `irl publish` printed it.
    #[arg(conflicts_with_all = ["endpoint_id", "broadcast_name"])]
    pub ticket: Option<LiveTicket>,

    /// Remote endpoint id. Needs `--name`.
    #[arg(long, conflicts_with = "ticket", requires = "broadcast_name")]
    pub endpoint_id: Option<EndpointId>,

    /// Broadcast path, alongside `--endpoint-id`.
    #[arg(
        long = "name",
        value_name = "NAME",
        conflicts_with = "ticket",
        requires = "endpoint_id"
    )]
    pub broadcast_name: Option<String>,
}

impl RemoteArgs {
    /// The ticket to subscribe to, from either form the flags allow.
    ///
    /// # Errors
    ///
    /// Fails if neither a positional ticket nor the
    /// `--endpoint-id` / `--name` pair was given. clap already rejects both at
    /// once.
    pub fn ticket(&self) -> Result<LiveTicket> {
        match (&self.ticket, self.endpoint_id, &self.broadcast_name) {
            (Some(ticket), None, None) => Ok(ticket.clone()),
            (None, Some(id), Some(name)) => Ok(LiveTicket::new(id, name.clone())),
            _ => Err(anyerr!(
                "provide either <TICKET> or --endpoint-id and --name"
            )),
        }
    }
}

/// Arguments for `irl watch`.
#[derive(Args, Debug)]
pub struct WatchArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,

    /// How the video is decoded. Nothing here applies under `--no-video`,
    /// which opens no video track.
    #[cfg(feature = "render")]
    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Play audio only. No window opens.
    #[arg(long)]
    pub no_video: bool,

    /// Read the ticket from a QR code held up to the camera.
    ///
    /// Supplies <TICKET>, so the window opens on the camera picture and
    /// connects as soon as a ticket decodes. Given alongside a ticket it
    /// starts on that one instead, and the scan screen stays a button away.
    #[cfg(feature = "render")]
    #[arg(long, conflicts_with = "no_video")]
    pub scan: bool,

    /// Pin a rendition by name instead of following the downlink.
    #[arg(long)]
    pub rendition: Option<String>,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Play through this output device, as `irl devices` lists it.
    ///
    /// Takes the id in the first column, for example `alsa:default`. Without
    /// it, playback follows whatever the system calls its default, which on a
    /// machine with several sinks is not always the one the speakers are on.
    #[cfg(feature = "playback")]
    #[arg(long, value_name = "ID")]
    pub audio_output: Option<String>,
}

/// Arguments for `irl run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// The TOML session file to run.
    pub config: PathBuf,
}

/// Arguments for `irl record`.
#[derive(Args, Debug)]
pub struct RecordArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,

    /// Output file. Its extension picks the container unless `--format` names
    /// one.
    #[arg(short, long, default_value = "recording.mp4")]
    pub output: PathBuf,

    /// Container to write, overriding whatever `--output`'s extension implies.
    #[arg(long, value_enum)]
    pub format: Option<RecordFormat>,

    /// Record one video rendition by name, instead of every rung the catalog
    /// offers.
    #[arg(long)]
    pub rendition: Option<String>,

    /// Stop after this long. Omit to record until interrupted.
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<u64>,

    /// How long a stalled group is waited for before it is skipped.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 2_000)]
    pub latency: u64,
}

/// The container `irl record` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RecordFormat {
    /// Fragmented MP4 / CMAF, the shape `.mp4` names here.
    Fmp4,
    /// Matroska, the shape `.mkv` and `.webm` name.
    Mkv,
}
