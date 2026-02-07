#[cfg(feature = "av1")]
mod dav1d_dec;
mod decoder;
mod encoder;
#[cfg(feature = "av1")]
mod rav1e_enc;
pub(crate) mod util;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi_enc;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
mod vtb_enc;

#[cfg(feature = "av1")]
pub use dav1d_dec::Av1VideoDecoder;
pub use decoder::DynamicVideoDecoder;
pub use decoder::H264VideoDecoder;
pub use encoder::H264Encoder;
#[cfg(feature = "av1")]
pub use rav1e_enc::Av1Encoder;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub use vaapi_enc::VaapiEncoder;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub use vtb_enc::VtbEncoder;
