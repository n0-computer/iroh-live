//! Command-line arguments.
//!
//! [`crate::source`] opens the sources these name, and [`crate::rendition`]
//! parses `--renditions`.

use std::path::PathBuf;

use clap::{
    Args, ValueEnum,
    builder::{PossibleValuesParser, TypedValueParser},
};
use iroh::EndpointId;
use iroh_live::{BroadcastTicket, media::RecordFormat, rooms::RoomTicket};
use n0_error::{Result, anyerr};
use serde::Deserialize;

use crate::{
    backend::Backend,
    source_spec::{AudioSourceSpec, TestPattern, TestTone, VideoSourceSpec},
};

/// Where a broadcast is served and how its address is shared.
#[derive(Args, Debug)]
pub struct TransportArgs {
    /// Broadcast path, as it appears in the ticket.
    #[arg(long, default_value = "hello")]
    pub name: String,

    /// Also push the broadcast to this relay endpoint.
    ///
    /// Use a relay that keeps each publisher to its own paths, as
    /// iroh-live-relay does. On a relay where anyone may publish anywhere, a
    /// viewer that reads through it can be served a forgery.
    #[arg(long)]
    pub relay: Option<EndpointId>,

    /// Do not accept incoming subscriber connections.
    #[arg(long)]
    pub no_serve: bool,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,
}

/// The default keyframe interval, in seconds.
///
/// A viewer waits up to this long for a first picture, and a rendition switch
/// waits as long. Two seconds is upstream's default. The encoder keeps to its
/// target bitrate, so a shorter interval costs picture quality, not bytes.
pub const DEFAULT_KEYFRAME_SECONDS: f64 = 2.0;

/// The `--video` specifier when none is given.
pub const DEFAULT_VIDEO: &str = "cam";

/// The `--audio` specifier when none is given.
pub const DEFAULT_AUDIO: &str = "mic";

/// What to capture and how to encode it.
///
/// A `[[send]]` block of `irl run` takes the same keys.
#[derive(Args, Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureArgs {
    /// Video source.
    ///
    /// `cam`, `cam:<id>`, `screen`, `screen:<id>`, `window:<id>`, `app:<id>`,
    /// `rpicam[:raw]`, `file:<path>[:loop]`, `test[:timing|:gradient]`, or
    /// `none`. Run `irl devices` to list the ids.
    #[arg(long, default_value = DEFAULT_VIDEO, verbatim_doc_comment)]
    pub video: String,

    /// Audio source.
    ///
    /// `mic`, `mic:<id>`, `system`, `file:<path>[:loop]`, `test[:beeps|:tone]`,
    /// or `none`. Anything else is a device name, such as `hw:0,1`.
    #[arg(long, default_value = DEFAULT_AUDIO, verbatim_doc_comment)]
    pub audio: String,

    /// Publish the test pattern and tone, as `--video test --audio test`.
    ///
    /// The marker in the picture is lit while each beep sounds.
    #[arg(long)]
    pub test_source: bool,

    /// Video codec. H.265 needs a hardware encoder.
    #[arg(long, value_enum, default_value_t = VideoCodecArg::H264)]
    pub codec: VideoCodecArg,

    /// Encoder backend. A named backend has no fallback.
    #[arg(long, value_parser = Backend::encoder_parser(), default_value = "auto")]
    #[serde(deserialize_with = "Backend::deserialize_encoder")]
    pub encoder: Backend,

    /// Simulcast ladder, comma-separated. Default: one unscaled rendition.
    ///
    /// A rung is `<height>p`, `<width>x<height>`, `<name>:<width>x<height>`,
    /// or a bare name for the source resolution. An `@<fps>` suffix requests
    /// a capture rate: `720p@60`, `high:1280x720@60,low:640x360@30`.
    /// All rungs share one capture at the highest rate any rung asks for.
    /// `--fps` overrides the suffixes.
    #[arg(long, value_delimiter = ',', verbatim_doc_comment)]
    pub renditions: Vec<String>,

    /// Seconds between keyframes.
    ///
    /// A viewer waits up to this long to join or to switch rendition. One
    /// second suits a call.
    #[arg(long, default_value_t = DEFAULT_KEYFRAME_SECONDS, value_name = "SECONDS")]
    pub keyframe_interval: f64,

    /// Target video bitrate of the largest rung, in bits per second.
    ///
    /// Smaller rungs get a share by pixel count. Default: derived from the
    /// resolution.
    #[arg(long, value_name = "BITS_PER_SECOND")]
    pub bitrate: Option<u64>,

    /// Requested capture width. The device picks its nearest mode.
    #[arg(long)]
    pub width: Option<u32>,

    /// Requested capture height.
    #[arg(long)]
    pub height: Option<u32>,

    /// Requested capture frame rate. Overrides the ladder's `@<fps>`.
    ///
    /// Default: 30, or the highest rate a rung asks for. The device picks its
    /// nearest supported rate.
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
    /// Returns the defaults clap applies.
    fn default() -> Self {
        Self {
            video: DEFAULT_VIDEO.to_string(),
            audio: DEFAULT_AUDIO.to_string(),
            test_source: false,
            codec: VideoCodecArg::default(),
            encoder: Backend::default(),
            renditions: Vec::new(),
            keyframe_interval: DEFAULT_KEYFRAME_SECONDS,
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
    /// Parses `--video`, or returns the test pattern under `--test-source`.
    pub fn video_source(&self) -> Result<VideoSourceSpec> {
        if self.test_source {
            return Ok(VideoSourceSpec::Test(TestPattern::default()));
        }
        VideoSourceSpec::parse(&self.video).map_err(|err| anyerr!("--video: {err}"))
    }

    /// Parses `--audio`, or returns the test tone under `--test-source`.
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
    /// H.264 (AVC). Widest support.
    #[default]
    H264,
    /// H.265 (HEVC). Hardware encoders only.
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
    /// Opus.
    #[default]
    Opus,
    /// Uncompressed 32-bit float PCM.
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

    /// Open a preview window of the published video.
    ///
    /// Not for `file:` and `rpicam` sources, which arrive already encoded.
    #[arg(long)]
    pub preview: bool,

    /// Start the preview window in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Container of a `file:` video source.
    #[arg(long, value_enum, default_value_t = ImportFormat::Fmp4)]
    pub format: ImportFormat,

    /// Pass a `file:` video source through ffmpeg first.
    ///
    /// Needed for a plain (non-fragmented) MP4 and for `file:<path>:loop`.
    #[arg(long)]
    pub transcode: bool,
}

/// The container a `file:` video source is read as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ImportFormat {
    /// Fragmented MP4 / CMAF.
    #[default]
    Fmp4,
    /// Raw H.264 Annex-B stream.
    Avc3,
}

/// How a subscriber decodes and plays the video in a window.
#[derive(Args, Debug, Clone, Copy, Default)]
pub struct PlaybackArgs {
    /// Decoder backend. A named backend has no fallback.
    #[arg(long, value_parser = Backend::decoder_parser(), default_value = "auto")]
    pub decoder: Backend,

    /// Trade between delay and smooth playback.
    #[arg(long, value_enum, default_value_t = LatencyArg::default())]
    pub latency: LatencyArg,
}

/// How long the player holds frames back to absorb late arrivals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LatencyArg {
    /// Least delay. For conversations.
    Realtime,
    /// Enough slack for an ordinary network.
    #[default]
    Balanced,
    /// Rides out a stuttering link, with more delay.
    ///
    /// For watching over Wi-Fi or mobile.
    Smooth,
}

impl LatencyArg {
    /// Returns the player latency for this mode.
    ///
    /// `min` is the playout hold: two frames at 30 fps for Realtime, a Wi-Fi
    /// retransmission burst for Smooth. `max` is where the player skips ahead.
    /// It must stay above `min`, or the skip drops the frames the hold waits for.
    pub fn latency(self) -> iroh_live::Latency {
        let latency = |min, max| iroh_live::Latency {
            min: std::time::Duration::from_millis(min),
            max: std::time::Duration::from_millis(max),
        };
        match self {
            Self::Realtime => latency(60, 100),
            Self::Balanced => iroh_live::Latency::default(),
            Self::Smooth => latency(400, 600),
        }
    }
}

/// Arguments for `irl call`.
#[derive(Args, Debug)]
pub struct CallArgs {
    /// Ticket the peer's `irl call` printed. Omit to wait for a call.
    pub ticket: Option<BroadcastTicket>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Camera for the QR scanner, as in `irl watch --scan-camera`.
    #[arg(long, value_name = "SPEC")]
    pub scan_camera: Option<String>,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

/// Arguments for `irl room`.
#[derive(Args, Debug)]
pub struct RoomArgs {
    /// Ticket another participant's `irl room` printed.
    ///
    /// Omit to open a new room.
    pub ticket: Option<RoomTicket>,

    #[command(flatten)]
    pub capture: CaptureArgs,

    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Name shown to other participants. Default: the short endpoint id.
    #[arg(long)]
    pub display_name: Option<String>,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,
}

/// The remote broadcast, as a ticket or as endpoint id plus broadcast path.
#[derive(Args, Debug)]
pub struct RemoteArgs {
    /// Ticket that `irl publish` printed.
    #[arg(conflicts_with_all = ["endpoint_id", "broadcast_name"])]
    pub ticket: Option<BroadcastTicket>,

    /// Remote endpoint id. Needs `--name`.
    #[arg(long, conflicts_with = "ticket", requires = "broadcast_name")]
    pub endpoint_id: Option<EndpointId>,

    /// Broadcast path. Needs `--endpoint-id`.
    #[arg(
        long = "name",
        value_name = "NAME",
        conflicts_with = "ticket",
        requires = "endpoint_id"
    )]
    pub broadcast_name: Option<String>,
}

impl RemoteArgs {
    /// Returns the ticket from either form, or fails if neither was given.
    pub fn ticket(&self) -> Result<BroadcastTicket> {
        match (&self.ticket, self.endpoint_id, &self.broadcast_name) {
            (Some(ticket), None, None) => Ok(ticket.clone()),
            (None, Some(id), Some(name)) => Ok(BroadcastTicket::new(id, name.clone())),
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

    /// Video decoding. Unused under `--no-video`.
    #[command(flatten)]
    pub playback: PlaybackArgs,

    /// Play audio only. No window opens.
    #[arg(long)]
    pub no_video: bool,

    /// Read the ticket from a QR code held up to the camera.
    ///
    /// The window shows the camera and connects once a ticket decodes. With
    /// a ticket given, it plays that one and the scanner is one button away.
    #[arg(long, conflicts_with = "no_video")]
    pub scan: bool,

    /// Camera for the QR scanner: `cam`, `cam:<id>`, or `rpicam`.
    ///
    /// Default: the Raspberry Pi camera where available, else the default
    /// camera. On a Pi with a USB webcam, pass `cam`.
    #[arg(long, value_name = "SPEC")]
    pub scan_camera: Option<String>,

    /// Pin a rendition by name instead of adapting to the downlink.
    #[arg(long)]
    pub rendition: Option<String>,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Audio output device, by the id `irl devices` lists.
    ///
    /// For example `alsa:default`. Default: the system default output.
    #[cfg(feature = "playback")]
    #[arg(long, value_name = "ID")]
    pub audio_output: Option<String>,
}

/// Arguments for `irl run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// TOML session file.
    pub config: PathBuf,
}

/// Arguments for `irl record`.
#[derive(Args, Debug)]
pub struct RecordArgs {
    #[command(flatten)]
    pub remote: RemoteArgs,

    /// Output file.
    ///
    /// Its extension picks the container unless `--format` is set.
    #[arg(short, long, default_value = "recording.mp4")]
    pub output: PathBuf,

    /// Container format, overriding the `--output` extension. `fmp4` is
    /// fragmented MP4.
    #[arg(long, value_parser = record_format())]
    pub format: Option<RecordFormat>,

    /// Record only this video rendition. Default: all of them.
    #[arg(long)]
    pub rendition: Option<String>,

    /// Stop after this long. Omit to record until interrupted.
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<u64>,

    /// How long to wait for a stalled group before skipping it.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 2_000)]
    pub max_age: u64,
}

/// Parses `--format` into the media crate's [`RecordFormat`].
fn record_format() -> impl TypedValueParser<Value = RecordFormat> {
    PossibleValuesParser::new(["fmp4", "mkv"]).map(|format| match format.as_str() {
        "mkv" => RecordFormat::Mkv,
        _ => RecordFormat::Fmp4,
    })
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    use super::LatencyArg;

    /// Each mode skips past more than it holds, and the modes grow in delay.
    #[test]
    fn the_latency_modes_are_ordered_and_consistent() {
        let modes = LatencyArg::value_variants();
        for mode in modes {
            let latency = mode.latency();
            assert!(latency.max > latency.min, "{mode:?}: {latency:?}");
        }
        for pair in modes.windows(2) {
            let (less, more) = (pair[0].latency(), pair[1].latency());
            assert!(less.min < more.min && less.max < more.max, "{pair:?}");
        }
    }
}
