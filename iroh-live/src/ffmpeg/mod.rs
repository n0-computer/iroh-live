// Group the existing FFmpeg-based implementation under `crate::ffmpeg`.

// Keep using the root-level helpers
pub use crate::ffmpeg_ext::ffmpeg_log_init;

pub mod audio {
    // Re-export the existing FFmpeg-based audio encoder/decoder modules.
    pub use crate::audio::decoder::*;
    pub use crate::audio::encoder::*;
}

pub mod video {
    // Re-export the existing FFmpeg-based video encoder/decoder/utilities.
    pub use crate::video::decoder::*;
    pub use crate::video::encoder::*;
    pub use crate::video::util::*;
}
