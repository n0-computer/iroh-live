// TODO: remove after step 12 wires consumers to this module
#![allow(
    dead_code,
    unused_imports,
    reason = "codec module not consumed until step 12"
)]

use crate::av::Decoders;

pub use self::{audio::*, video::*};

pub(crate) mod audio;
mod resample;
pub(crate) mod video;

#[derive(Debug, Clone, Copy)]
pub struct DefaultDecoders;

impl Decoders for DefaultDecoders {
    type Audio = OpusAudioDecoder;
    type Video = H264VideoDecoder;
}

/// No-op replacement for `ffmpeg_log_init`. Nothing to initialize.
pub fn codec_init() {}
