//! The `--video` and `--audio` source specifiers.
//!
//! The grammar lives in `moq_ext_sources::spec`, with its parser and the
//! opener of the plain sources. Both specifiers implement `FromStr`, so clap
//! rejects a bad one while it parses the arguments, and a config file while it
//! loads.

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub(crate) use iroh_live::media::spec::RpicamMode;
pub(crate) use iroh_live::media::spec::{
    AudioSourceSpec, PORTAL, TestPattern, TestTone, VideoOpenOptions, VideoSourceSpec,
};
