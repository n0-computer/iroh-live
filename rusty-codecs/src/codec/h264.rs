pub mod annexb;
pub(crate) mod decoder;
pub(crate) mod encoder;
#[allow(dead_code, reason = "SPS patcher kept for potential future use")]
pub(crate) mod sps;

pub use decoder::H264VideoDecoder;
pub use encoder::H264Encoder;

/// Builds the standard H.264 Baseline `VideoConfig` shared by all H.264
/// encoder backends.
///
/// All backends report Baseline profile (0x42), constrained (0xE0),
/// level 3.0 (0x1E). The only variable parts are the stream parameters
/// and the avcC description (populated after the first keyframe).
pub(crate) fn h264_video_config(
    width: u32,
    height: u32,
    bitrate: u64,
    framerate: u32,
    inline: bool,
    description: Option<bytes::Bytes>,
) -> crate::config::VideoConfig {
    use crate::config::{H264, VideoCodec, VideoConfig};
    VideoConfig {
        codec: VideoCodec::H264(H264 {
            profile: 0x42,
            constraints: 0xE0,
            level: 0x1E,
            inline,
        }),
        description,
        coded_width: Some(width),
        coded_height: Some(height),
        display_ratio_width: None,
        display_ratio_height: None,
        bitrate: Some(bitrate),
        framerate: Some(framerate as f64),
        optimize_for_latency: Some(true),
    }
}
