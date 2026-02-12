#[cfg(feature = "av1")]
mod av1_dec;
mod decoder;
mod encoder;
#[cfg(feature = "av1")]
mod rav1d_safe;
#[cfg(feature = "av1")]
mod rav1e_enc;
#[cfg(test)]
pub(crate) mod test_util;
pub(crate) mod util;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi_enc;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
mod vtb_enc;

#[cfg(feature = "av1")]
pub use av1_dec::Av1VideoDecoder;
pub use decoder::DynamicVideoDecoder;
pub use decoder::H264VideoDecoder;
pub use encoder::H264Encoder;
#[cfg(feature = "av1")]
pub use rav1e_enc::Av1Encoder;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub use vaapi_enc::VaapiEncoder;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub use vtb_enc::VtbEncoder;
