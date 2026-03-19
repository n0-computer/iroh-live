#[cfg(all(target_os = "android", feature = "android"))]
pub(crate) mod android;
#[cfg(feature = "av1")]
pub(crate) mod av1;
#[cfg(any_codec)]
pub(crate) mod dynamic;
#[cfg(feature = "h264")]
pub(crate) mod h264;
#[cfg(feature = "opus")]
pub(crate) mod opus;
#[cfg(all(target_os = "linux", feature = "v4l2"))]
pub(crate) mod v4l2;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub(crate) mod vaapi;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub(crate) mod vtb;

#[cfg(all(target_os = "android", feature = "android"))]
pub use android::*;
#[cfg(feature = "av1")]
pub use av1::*;
#[cfg(any_codec)]
pub use dynamic::*;
#[cfg(all(target_os = "linux", feature = "v4l2"))]
pub use v4l2::*;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub use vaapi::*;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub use vtb::*;

#[cfg(feature = "h264")]
pub use self::h264::*;
#[cfg(feature = "opus")]
pub use self::opus::*;

pub mod test_util;

/// Available audio encoder implementations.
#[cfg(any_audio_codec)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum AudioCodec {
    #[cfg(feature = "opus")]
    Opus,
}

/// Available video encoder implementations.
#[cfg(any_video_codec)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum VideoCodec {
    /// Software H.264 via openh264.
    #[cfg(feature = "h264")]
    H264,
    /// Software AV1 via rav1e.
    #[cfg(feature = "av1")]
    Av1,
    /// Hardware H.264 via macOS VideoToolbox.
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    #[strum(serialize = "h264-vtb")]
    VtbH264,
    /// Hardware H.264 via Linux VAAPI.
    #[strum(serialize = "h264-vaapi")]
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264,
    /// Hardware H.264 via Linux V4L2 (stateful encoder).
    #[strum(serialize = "h264-v4l2")]
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    V4l2H264,
    /// Hardware H.264 via Android MediaCodec.
    #[strum(serialize = "h264-android")]
    #[cfg(all(target_os = "android", feature = "android"))]
    AndroidH264,
}

#[cfg(any_video_codec)]
impl VideoCodec {
    /// Returns all encoder kinds that are compiled in.
    pub fn available() -> Vec<Self> {
        vec![
            #[cfg(feature = "h264")]
            Self::H264,
            #[cfg(feature = "av1")]
            Self::Av1,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264,
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            Self::V4l2H264,
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidH264,
        ]
    }

    /// Returns the best available encoder, preferring hardware over software.
    ///
    /// Returns `None` when no video codec feature is enabled at compile time.
    #[allow(
        unreachable_code,
        reason = "cfg-gated returns may make trailing code unreachable"
    )]
    pub fn best_available() -> Option<Self> {
        #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
        {
            return Some(Self::VtbH264);
        }
        #[cfg(all(target_os = "linux", feature = "vaapi"))]
        {
            return Some(Self::VaapiH264);
        }
        #[cfg(all(target_os = "linux", feature = "v4l2"))]
        {
            return Some(Self::V4l2H264);
        }
        #[cfg(all(target_os = "android", feature = "android"))]
        {
            return Some(Self::AndroidH264);
        }
        #[cfg(feature = "h264")]
        {
            return Some(Self::H264);
        }
        None
    }

    /// Whether this is a hardware-accelerated encoder.
    pub fn is_hardware(self) -> bool {
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => false,
            #[cfg(feature = "av1")]
            Self::Av1 => false,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => true,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => true,
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            Self::V4l2H264 => true,
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidH264 => true,
        }
    }

    /// Creates an encoder for this codec with a full [`VideoEncoderConfig`](crate::format::VideoEncoderConfig).
    pub fn create_encoder(
        self,
        config: crate::format::VideoEncoderConfig,
    ) -> anyhow::Result<Box<dyn crate::traits::VideoEncoder>> {
        use crate::traits::VideoEncoderFactory as _;
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => Ok(Box::new(H264Encoder::with_config(config)?)),
            #[cfg(feature = "av1")]
            Self::Av1 => Ok(Box::new(Av1Encoder::with_config(config)?)),
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => Ok(Box::new(VtbEncoder::with_config(config)?)),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => Ok(Box::new(VaapiEncoder::with_config(config)?)),
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            Self::V4l2H264 => Ok(Box::new(V4l2Encoder::with_config(config)?)),
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidH264 => Ok(Box::new(AndroidEncoder::with_config(config)?)),
        }
    }

    /// Creates an encoder for this codec with a [`VideoPreset`](crate::format::VideoPreset).
    pub fn create_encoder_from_preset(
        self,
        preset: crate::format::VideoPreset,
    ) -> anyhow::Result<Box<dyn crate::traits::VideoEncoder>> {
        self.create_encoder(crate::format::VideoEncoderConfig::from_preset(preset))
    }

    /// Creates an encoder matching a [`VideoSource`](crate::traits::VideoSource)'s format.
    pub fn create_encoder_from_source(
        self,
        source: &dyn crate::traits::VideoSource,
    ) -> anyhow::Result<Box<dyn crate::traits::VideoEncoder>> {
        let fmt = source.format();
        let [width, height] = fmt.dimensions;
        self.create_encoder(crate::format::VideoEncoderConfig {
            width,
            height,
            framerate: 30,
            bitrate: None,
            scale_mode: Default::default(),
            keyframe_interval: None,
            nal_format: crate::format::NalFormat::default(),
        })
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => "H.264 (Software)",
            #[cfg(feature = "av1")]
            Self::Av1 => "AV1 (Software)",
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => "H.264 (VideoToolbox)",
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => "H.264 (VAAPI)",
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            Self::V4l2H264 => "H.264 (V4L2)",
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidH264 => "H.264 (Android MediaCodec)",
        }
    }
}

#[cfg(all(test, feature = "opus"))]
#[path = "codec/tests/harness.rs"]
mod tests;

#[cfg(all(test, feature = "h264"))]
#[path = "codec/tests/latency.rs"]
mod latency_tests;
