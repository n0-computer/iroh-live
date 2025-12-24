use crate::av::Decoders;

pub use self::{audio::*, ext::ffmpeg_log_init, video::*};

#[derive(Debug, Clone, Copy)]
pub struct FfmpegDecoders;

impl Decoders for FfmpegDecoders {
    type Audio = FfmpegAudioDecoder;
    type Video = FfmpegVideoDecoder;
}

mod audio {
    mod decoder;
    mod encoder;
    pub use decoder::*;
    pub use encoder::*;
}

pub mod video {
    mod decoder;
    mod encoder;
    pub(crate) mod util;
    pub use decoder::*;
    pub use encoder::*;
}

pub(crate) mod ext {
    use buf_list::BufList;
    use bytes::Buf;
    use ffmpeg_next as ffmpeg;
    pub fn ffmpeg_log_init() {
        use ffmpeg::util::log::Level::*;
        let level = if let Ok(val) = std::env::var("FFMPEG_LOG") {
            match val.as_str() {
                "quiet" => Quiet,
                "panic" => Panic,
                "fatal" => Fatal,
                "error" => Error,
                "warn" | "warning" => Warning,
                "info" => Info,
                "verbose" => Verbose,
                "debug" => Debug,
                "trace" => Trace,
                _ => Warning,
            }
        } else {
            Warning
        };
        ffmpeg::util::log::set_level(level);
    }

    pub trait PacketExt {
        fn to_ffmpeg_packet(self) -> ffmpeg::Packet;
    }

    impl PacketExt for BufList {
        fn to_ffmpeg_packet(mut self) -> ffmpeg_next::Packet {
            let mut packet = ffmpeg::Packet::new(self.num_bytes());
            let dst = packet.data_mut().unwrap();
            self.copy_to_slice(dst);
            packet
        }
    }

    pub trait CodecContextExt {
        fn extradata(&self) -> Option<&[u8]>;
        fn set_extradata(&mut self, extradata: &[u8]) -> Result<(), ffmpeg::Error>;
    }

    impl CodecContextExt for ffmpeg::codec::Context {
        // SAFETY: Written by ChatGPT, so, dunno.
        fn extradata(&self) -> Option<&[u8]> {
            unsafe {
                let ctx = self.as_ptr();
                if (*ctx).extradata.is_null() || (*ctx).extradata_size <= 0 {
                    return None;
                }
                Some(std::slice::from_raw_parts(
                    (*ctx).extradata as *const u8,
                    (*ctx).extradata_size as usize,
                ))
            }
        }

        // SAFETY: Written by ChatGPT, so, dunno.
        fn set_extradata(&mut self, extradata: &[u8]) -> Result<(), ffmpeg::Error> {
            unsafe {
                let ctx = self.as_mut_ptr();
                // allocate extradata + padding
                let pad = ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
                let size = extradata.len() + pad;
                (*ctx).extradata = ffmpeg::ffi::av_mallocz(size).cast::<u8>();
                if (*ctx).extradata.is_null() {
                    return Err(ffmpeg::Error::Bug.into());
                }
                // copy bytes and zero the padding
                std::ptr::copy_nonoverlapping(
                    extradata.as_ptr(),
                    (*ctx).extradata,
                    extradata.len(),
                );
                (*ctx).extradata_size = extradata.len() as i32;
            }
            Ok(())
        }
    }
}
