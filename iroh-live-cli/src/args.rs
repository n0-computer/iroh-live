//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use iroh::EndpointId;
use iroh_live::{BroadcastTicket, layout::LiveLayout, rooms::RoomTicket};
use n0_error::{Result, anyerr};
use serde::Deserialize;

use crate::{
    backend::Backend,
    record::RecordFormat,
    source_spec::{AudioSourceSpec, TestPattern, TestTone, VideoSourceSpec},
    transport::Inbound,
};

/// Where a broadcast is served and how its address is shared.
#[derive(Args, Debug)]
pub(crate) struct TransportArgs {
    /// Broadcast name, as it appears in the ticket.
    #[arg(long, default_value = "hello")]
    pub(crate) name: String,

    /// Also push the broadcast to this relay endpoint.
    ///
    /// Use a relay that keeps each publisher to its own paths, as
    /// iroh-live-relay does. Elsewhere a viewer can be served a forgery.
    #[arg(long)]
    pub(crate) relay: Option<EndpointId>,

    /// Do not accept incoming subscriber connections.
    #[arg(long)]
    pub(crate) no_serve: bool,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub(crate) no_qr: bool,
}

impl TransportArgs {
    /// Returns whether the node accepts subscribers, which `--no-serve` turns off.
    pub(crate) fn inbound(&self) -> Inbound {
        if self.no_serve {
            Inbound::Refuse
        } else {
            Inbound::Accept
        }
    }
}

/// The default keyframe interval, in seconds.
///
/// Two seconds is upstream's default. The encoder keeps to its target
/// bitrate, so a shorter interval costs picture quality, not bytes.
pub(crate) const DEFAULT_KEYFRAME_SECONDS: f64 = 2.0;

/// The `--video` specifier when none is given.
pub(crate) const DEFAULT_VIDEO: &str = "cam";

/// The `--audio` specifier when none is given.
pub(crate) const DEFAULT_AUDIO: &str = "mic";

/// What to capture and how to encode it.
///
/// A `[[send]]` block of `irl run` takes the same keys.
#[derive(Args, Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CaptureArgs {
    /// Video source.
    ///
    /// `cam`, `cam:<id>`, `screen`, `screen:<id>`, `window:<id>`, `app:<id>`,
    /// `rpicam[:raw]`, `file:<path>[:loop]`, `test[:timing|:gradient]`, or
    /// `none`. Run `irl devices` to list the ids. On Linux, `screen` and
    /// `screen:portal` (or `window:portal`) open the desktop portal's picker.
    #[arg(long, default_value = DEFAULT_VIDEO, verbatim_doc_comment)]
    pub(crate) video: VideoSourceSpec,

    /// Audio source.
    ///
    /// `mic`, `mic:<id>`, `system`, `file:<path>[:loop]`, `test[:beeps|:tone]`,
    /// or `none`. Anything else is a device name, such as `hw:0,1`.
    #[arg(long, default_value = DEFAULT_AUDIO, verbatim_doc_comment)]
    pub(crate) audio: AudioSourceSpec,

    /// Publish the test pattern and tone, as `--video test --audio test`.
    #[arg(long)]
    pub(crate) test_source: bool,

    /// Video codec. H.265 needs a hardware encoder.
    #[arg(long, value_enum, default_value_t = VideoCodecArg::H264)]
    pub(crate) codec: VideoCodecArg,

    /// Encoder backend. A named backend has no fallback.
    #[arg(long, value_parser = Backend::encoder_parser(), default_value = "auto")]
    #[serde(deserialize_with = "Backend::deserialize_encoder")]
    pub(crate) encoder: Backend,

    /// Simulcast ladder, comma-separated. Default: one unscaled rendition.
    ///
    /// A rung is `<height>p`, `<width>x<height>`, `<name>:<width>x<height>`,
    /// or a bare name for the source resolution. An `@<fps>` suffix requests
    /// a capture rate: `720p@60`, `high:1280x720@60,low:640x360@30`.
    /// All rungs share one capture at the highest rate any rung asks for.
    /// `--fps` overrides the suffixes.
    #[arg(long, value_delimiter = ',', verbatim_doc_comment)]
    pub(crate) renditions: Vec<String>,

    /// Seconds between keyframes.
    ///
    /// A viewer waits up to this long to join or to switch rendition. One
    /// second suits a call.
    #[arg(long, default_value_t = DEFAULT_KEYFRAME_SECONDS, value_name = "SECONDS")]
    pub(crate) keyframe_interval: f64,

    /// Target video bitrate of the largest rung, in bits per second.
    ///
    /// Smaller rungs get a share by pixel count. Default: derived from the
    /// resolution.
    #[arg(long, value_name = "BITS_PER_SECOND")]
    pub(crate) bitrate: Option<u64>,

    /// Requested capture width. The device picks its nearest mode.
    #[arg(long)]
    pub(crate) width: Option<u32>,

    /// Requested capture height.
    #[arg(long)]
    pub(crate) height: Option<u32>,

    /// Requested capture frame rate. Overrides the ladder's `@<fps>`.
    ///
    /// Default: 30, or the highest rate a rung asks for. The device picks its
    /// nearest supported rate.
    #[arg(long, verbatim_doc_comment)]
    pub(crate) fps: Option<u32>,

    /// Hide the mouse cursor. Screen, window, and application capture only.
    #[arg(long)]
    pub(crate) no_cursor: bool,

    /// Audio codec. PCM is uncompressed: lower latency, much higher bitrate.
    #[arg(long, value_enum, default_value_t = AudioCodecArg::Opus)]
    pub(crate) audio_codec: AudioCodecArg,

    /// Target audio bitrate in bits per second. Opus only.
    #[arg(long, value_name = "BITS_PER_SECOND")]
    pub(crate) audio_bitrate: Option<u32>,
}

impl Default for CaptureArgs {
    /// Returns the defaults clap applies.
    fn default() -> Self {
        Self {
            video: DEFAULT_VIDEO
                .parse()
                .expect("the default video source parses"),
            audio: DEFAULT_AUDIO
                .parse()
                .expect("the default audio source parses"),
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
    /// Returns `--video`, or the test pattern under `--test-source`.
    pub(crate) fn video_source(&self) -> VideoSourceSpec {
        if self.test_source {
            return VideoSourceSpec::Test(TestPattern::default());
        }
        self.video.clone()
    }

    /// Returns `--audio`, or the test tone under `--test-source`.
    pub(crate) fn audio_source(&self) -> AudioSourceSpec {
        if self.test_source {
            return AudioSourceSpec::Test(TestTone::default());
        }
        self.audio.clone()
    }
}

/// The video codec `--codec` selects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VideoCodecArg {
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
pub(crate) enum AudioCodecArg {
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
pub(crate) struct PublishArgs {
    #[command(flatten)]
    pub(crate) capture: CaptureArgs,

    #[command(flatten)]
    pub(crate) transport: TransportArgs,

    /// Open a preview window of the published video.
    ///
    /// Not for `file:` and `rpicam` sources, which arrive already encoded.
    #[arg(long)]
    pub(crate) preview: bool,

    /// Start the preview window in fullscreen.
    #[arg(long)]
    pub(crate) fullscreen: bool,

    /// Container of a `file:` video source.
    #[arg(long, value_enum, default_value_t = ImportFormat::Fmp4)]
    pub(crate) format: ImportFormat,

    /// Pass a `file:` video source through ffmpeg first.
    ///
    /// Needed for a plain (non-fragmented) MP4 and for `file:<path>:loop`.
    #[arg(long)]
    pub(crate) transcode: bool,
}

/// The container a `file:` video source is read as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ImportFormat {
    /// Fragmented MP4 / CMAF.
    #[default]
    Fmp4,
    /// Raw H.264 Annex-B stream.
    Avc3,
}

/// How a subscriber decodes and plays the video in a window.
#[derive(Args, Debug, Clone, Copy, Default)]
pub(crate) struct PlaybackArgs {
    /// Decoder backend. A named backend has no fallback.
    #[arg(long, value_parser = Backend::decoder_parser(), default_value = "auto")]
    pub(crate) decoder: Backend,

    /// Trade between delay and smooth playback.
    #[arg(long, value_enum, default_value_t = LatencyArg::default())]
    pub(crate) latency: LatencyArg,
}

/// How long the player holds frames back to absorb late arrivals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum LatencyArg {
    /// Least delay. For conversations.
    Realtime,
    /// Enough slack for an ordinary network.
    #[default]
    Balanced,
    /// Rides out a stuttering link, with more delay.
    Smooth,
}

impl LatencyArg {
    /// Returns the player latency for this mode.
    ///
    /// `min` is the playout hold: two frames at 30 fps for Realtime, a Wi-Fi
    /// retransmission burst for Smooth. `max` is where the player skips ahead.
    /// It must stay above `min`, or the skip drops the frames the hold waits for.
    pub(crate) fn latency(self) -> iroh_live::media::Delay {
        let latency = |min, max| iroh_live::media::Delay {
            min: Some(std::time::Duration::from_millis(min)),
            max: Some(std::time::Duration::from_millis(max)),
        };
        match self {
            Self::Realtime => latency(60, 100),
            Self::Balanced => iroh_live::media::DEFAULT_LATENCY,
            Self::Smooth => latency(400, 600),
        }
    }
}

/// Arguments for `irl call`.
#[derive(Args, Debug)]
pub(crate) struct CallArgs {
    /// Ticket the peer's `irl call` printed. Omit to wait for a call.
    pub(crate) ticket: Option<BroadcastTicket>,

    #[command(flatten)]
    pub(crate) capture: CaptureArgs,

    #[command(flatten)]
    pub(crate) playback: PlaybackArgs,

    /// Camera for the QR scanner, as in `irl watch --scan-camera`.
    #[arg(long, value_name = "SPEC", value_parser = crate::scan::camera_spec)]
    pub(crate) scan_camera: Option<VideoSourceSpec>,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub(crate) no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub(crate) fullscreen: bool,
}

/// Arguments for `irl room`.
#[derive(Args, Debug)]
pub(crate) struct RoomArgs {
    /// Ticket another participant's `irl room` printed. Omit to open a new room.
    pub(crate) ticket: Option<RoomTicket>,

    #[command(flatten)]
    pub(crate) capture: CaptureArgs,

    #[command(flatten)]
    pub(crate) playback: PlaybackArgs,

    /// Name shown to other participants. Default: the short endpoint id.
    #[arg(long)]
    pub(crate) display_name: Option<String>,

    /// Suppress the terminal QR code.
    #[arg(long)]
    pub(crate) no_qr: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub(crate) fullscreen: bool,
}

/// The remote broadcast, as a ticket or as endpoint id plus broadcast path.
#[derive(Args, Debug)]
pub(crate) struct RemoteArgs {
    /// Ticket that `irl publish` printed.
    #[arg(conflicts_with_all = ["endpoint_id", "broadcast_name"])]
    pub(crate) ticket: Option<BroadcastTicket>,

    /// Remote endpoint id. Needs `--name`.
    #[arg(long, conflicts_with = "ticket", requires = "broadcast_name")]
    pub(crate) endpoint_id: Option<EndpointId>,

    /// Broadcast name. Needs `--endpoint-id`.
    #[arg(
        long = "name",
        value_name = "NAME",
        conflicts_with = "ticket",
        requires = "endpoint_id"
    )]
    pub(crate) broadcast_name: Option<String>,
}

impl RemoteArgs {
    /// Returns the ticket from either form, or fails if neither was given.
    pub(crate) fn ticket(&self) -> Result<BroadcastTicket> {
        match (&self.ticket, self.endpoint_id, &self.broadcast_name) {
            (Some(ticket), None, None) => Ok(ticket.clone()),
            (None, Some(id), Some(name)) => Ok(LiveLayout::default().ticket(id, name)?),
            _ => Err(anyerr!(
                "provide either <TICKET> or --endpoint-id and --name"
            )),
        }
    }
}

/// Arguments for `irl watch`.
#[derive(Args, Debug)]
pub(crate) struct WatchArgs {
    #[command(flatten)]
    pub(crate) remote: RemoteArgs,

    /// Video decoding. Unused under `--no-video`.
    #[command(flatten)]
    pub(crate) playback: PlaybackArgs,

    /// Play audio only. No window opens.
    #[arg(long)]
    pub(crate) no_video: bool,

    /// Read the ticket from a QR code held up to the camera.
    ///
    /// The window shows the camera and connects once a ticket decodes. With
    /// a ticket given, it plays that one and the scanner is one button away.
    #[arg(long, conflicts_with = "no_video")]
    pub(crate) scan: bool,

    /// Camera for the QR scanner: `cam`, `cam:<id>`, or `rpicam`.
    ///
    /// Default: the Raspberry Pi camera where available, else the default
    /// camera. On a Pi with a USB webcam, pass `cam`.
    #[arg(long, value_name = "SPEC", value_parser = crate::scan::camera_spec)]
    pub(crate) scan_camera: Option<VideoSourceSpec>,

    /// Pin a rendition by name instead of adapting to the downlink.
    #[arg(long)]
    pub(crate) rendition: Option<String>,

    /// Start in fullscreen.
    #[arg(long)]
    pub(crate) fullscreen: bool,

    /// Audio output device, by the id `irl devices` lists.
    ///
    /// For example `alsa:default`. Default: the system default output.
    #[cfg(feature = "playback")]
    #[arg(long, value_name = "ID")]
    pub(crate) audio_output: Option<String>,
}

/// Arguments for `irl run`.
#[derive(Args, Debug)]
pub(crate) struct RunArgs {
    /// TOML session file.
    pub(crate) config: PathBuf,
}

/// Arguments for `irl record`.
#[derive(Args, Debug)]
pub(crate) struct RecordArgs {
    #[command(flatten)]
    pub(crate) remote: RemoteArgs,

    /// Output file.
    ///
    /// Its extension picks the container unless `--format` is set.
    #[arg(short, long, default_value = "recording.mp4")]
    pub(crate) output: PathBuf,

    /// Container format, overriding the `--output` extension.
    #[arg(long, value_enum)]
    pub(crate) format: Option<RecordFormat>,

    /// Record only this video rendition. Default: all of them.
    #[arg(long)]
    pub(crate) rendition: Option<String>,

    /// Stop after this long. Omit to record until interrupted.
    #[arg(long, value_name = "SECONDS")]
    pub(crate) duration: Option<u64>,

    /// How long to wait for a stalled group before skipping it.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 2_000)]
    pub(crate) max_age: u64,
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
