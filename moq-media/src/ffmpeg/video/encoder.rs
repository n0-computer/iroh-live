use std::{
    ffi::{CString, c_int},
    fmt, ptr,
    task::Poll,
};

use anyhow::{Context, Result, anyhow};
use ffmpeg_next::{
    self as ffmpeg, codec,
    error::EAGAIN,
    format::Pixel,
    frame::Video as VideoFrame,
    sys::{
        AVBufferRef, AVHWDeviceType, AVHWFramesContext, AVPixelFormat, av_buffer_ref,
        av_buffer_unref, av_hwdevice_ctx_create, av_hwframe_ctx_alloc, av_hwframe_ctx_init,
        av_hwframe_get_buffer, av_hwframe_transfer_data,
    },
};
use hang::{
    Timestamp,
    catalog::{H264, VideoCodec, VideoConfig},
};
use tracing::{debug, info, trace};

use crate::{
    av,
    ffmpeg::{ext::CodecContextExt, util::Rescaler},
};

#[derive(Debug, Clone, Copy, Default)]
// Allow unused because usage is cfg-gated on platform.
#[allow(
    unused,
    reason = "variants are conditionally used based on target platform"
)]
enum HwBackend {
    #[default]
    Software,
    /// Linux
    Vaapi,
    /// macOS
    Videotoolbox,
    /// Nvidia GPUs
    Nvenc,
    /// Intel GPUs
    Qsv,
    /// AMD GPUs
    Amf,
    // TODO:
    // Add DirectX (Windows)
    // Add MediaCodec (Android)
}

impl HwBackend {
    fn codec_name(&self) -> &'static str {
        match self {
            Self::Software => "libx264",
            Self::Vaapi => "h264_vaapi",
            Self::Videotoolbox => "h264_videotoolbox",
            Self::Nvenc => "h264_nvenc",
            Self::Qsv => "h264_qsv",
            Self::Amf => "h264_amf",
        }
    }

    fn candidates() -> Vec<Self> {
        // vec![HwBackend::Software]
        let mut candidates = Vec::new();
        // Platform-preferred order
        #[cfg(target_os = "macos")]
        candidates.extend_from_slice(&[Self::Videotoolbox]);
        #[cfg(target_os = "windows")]
        candidates.extend_from_slice(&[HwBackend::Nvenc, HwBackend::Qsv, HwBackend::Amf]);
        #[cfg(target_os = "linux")]
        candidates.extend_from_slice(&[HwBackend::Vaapi, HwBackend::Nvenc, HwBackend::Qsv]);

        // Always end with software
        candidates.push(Self::Software);
        candidates
    }

    fn pixel_format(&self) -> Pixel {
        match self {
            Self::Vaapi | Self::Qsv => Pixel::NV12,
            // These rest accepts yuv420p SW frames:
            _ => Pixel::YUV420P,
        }
    }
    fn hardware_pixel_format(&self) -> Pixel {
        match self {
            Self::Vaapi => Pixel::VAAPI,
            Self::Qsv => Pixel::NV12,
            // These rest accepts yuv420p SW frames:
            _ => Pixel::YUV420P,
        }
    }
}

#[derive(Debug, Clone)]
struct EncoderOpts {
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
}

pub struct H264Encoder {
    encoder: codec::encoder::video::Encoder,
    rescaler: Rescaler,
    backend: HwBackend,
    vaapi: Option<VaapiState>,
    opts: EncoderOpts,
    frame_count: u64,
}

impl fmt::Debug for H264Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H264Encoder")
            .field("backend", &self.backend)
            .field("opts", &self.opts)
            .field("frame_count", &self.frame_count)
            .finish_non_exhaustive()
    }
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        info!("Initializing H264 encoder: {width}x{height} @ {framerate}fps");
        ffmpeg::init()?;

        // Bitrate heuristic (from your original)
        let pixels = width * height;
        let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
        let bitrate = (pixels as f32 * 0.07 * framerate_factor).round() as u64;

        let opts = EncoderOpts {
            width,
            height,
            framerate,
            bitrate,
        };

        let candidates = HwBackend::candidates();

        // Try each backend
        let mut last_err: Option<anyhow::Error> = None;
        for backend in candidates {
            match Self::open_encoder(backend, &opts) {
                Ok((encoder, rescaler, vaapi)) => {
                    info!(
                        "Using encoder backend: {} ({backend:?})",
                        backend.codec_name()
                    );
                    return Ok(Self {
                        encoder,
                        rescaler,
                        vaapi,
                        backend,
                        opts,
                        frame_count: 0,
                    });
                }
                Err(e) => {
                    debug!(
                        "Backend {backend:?} ({}) not available: {e:#}",
                        backend.codec_name()
                    );
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("no H.264 encoder available")))
    }

    fn open_encoder(
        backend: HwBackend,
        opts: &EncoderOpts,
    ) -> Result<(codec::encoder::video::Encoder, Rescaler, Option<VaapiState>)> {
        // Find encoder
        let codec = codec::encoder::find_by_name(backend.codec_name())
            .with_context(|| format!("encoder {} not found", backend.codec_name()))?;
        debug!("Found encoder: {}", codec.name());

        // Build ctx
        let mut ctx = codec::context::Context::new_with_codec(codec);
        unsafe {
            let ctx_mut = ctx.as_mut_ptr();
            (*ctx_mut).width = opts.width as i32;
            (*ctx_mut).height = opts.height as i32;
            (*ctx_mut).time_base.num = 1;
            (*ctx_mut).time_base.den = opts.framerate as i32;
            (*ctx_mut).framerate.num = opts.framerate as i32;
            (*ctx_mut).framerate.den = 1;
            (*ctx_mut).gop_size = opts.framerate as i32;
            (*ctx_mut).bit_rate = opts.bitrate as i64;
            (*ctx_mut).flags |= codec::Flags::GLOBAL_HEADER.bits() as c_int;
            (*ctx_mut).pix_fmt = backend.hardware_pixel_format().into();
        }

        // Backend-specific prep
        let vaapi_state = if matches!(backend, HwBackend::Vaapi) {
            // single-GPU default; make configurable if needed
            let va = VaapiState::new(opts.width, opts.height, "/dev/dri/renderD128")?;
            va.bind_to_context(&mut ctx);
            Some(va)
        } else {
            None
        };

        // Setup encoder options
        let enc_opts = {
            let mut opts = vec![
                // Disable annexB so that we get an avcC header in extradata
                // annexb=0 → MP4/ISO BMFF style (length-prefixed NAL units + avcC extradata),
                // as opposed to Annex B start codes (00 00 00 01).
                ("annexB", "0"),
            ];
            if matches!(backend, HwBackend::Software) {
                opts.extend_from_slice(&[
                    ("preset", "ultrafast"),
                    ("tune", "zerolatency"),
                    ("profile", "baseline"),
                ]);
            }
            ffmpeg::Dictionary::from_iter(opts)
        };
        // Open encoder
        let encoder = ctx.encoder().video()?.open_as_with(codec, enc_opts)?;

        // Build rescaler to SW input fmt expected per-backend
        let rescaler = Rescaler::new(backend.pixel_format(), Some((opts.width, opts.height)))?;

        Ok((encoder, rescaler, vaapi_state))
    }

    pub fn video_config(&self) -> Result<VideoConfig> {
        Ok(VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E,   // Level 3.0
                inline: false, // TODO: is this correct?
            }),
            description: Some(self.avcc_description()?.to_vec().into()),
            coded_width: Some(self.opts.width),
            coded_height: Some(self.opts.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.opts.bitrate),
            framerate: Some(self.opts.framerate as f64),
            optimize_for_latency: Some(true),
        })
    }

    pub fn avcc_description(&self) -> Result<&[u8]> {
        self.encoder.extradata().context("missing avcC extradata")
    }

    pub fn receive_packet(&mut self) -> Result<Poll<Option<hang::Frame>>> {
        let mut packet = ffmpeg::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let payload = packet.data().unwrap_or(&[]).to_vec();
                let hang_frame = hang::Frame {
                    payload: payload.into(),
                    timestamp: Timestamp::from_micros(
                        self.frame_count * 1_000_000 / self.opts.framerate as u64,
                    )?,
                    keyframe: packet.is_key(),
                };
                Ok(Poll::Ready(Some(hang_frame)))
            }
            Err(ffmpeg::Error::Eof) => Ok(Poll::Ready(None)),
            Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => Ok(Poll::Pending),
            Err(e) => Err(e.into()),
        }
    }

    pub fn encode_frame(&mut self, mut frame: VideoFrame) -> Result<()> {
        frame.set_pts(Some(self.frame_count as i64));
        self.frame_count += 1;

        if self.frame_count.is_multiple_of(self.opts.framerate as u64) {
            tracing::trace!(
                "Encoding {}: {}x{} fmt={:?} pts={:?} backend={:?}",
                self.frame_count,
                frame.width(),
                frame.height(),
                frame.format(),
                frame.pts(),
                self.backend
            );
        }

        // Convert incoming frame to the SW format the backend expects.
        let frame = self
            .rescaler
            .process(&frame)
            .context("failed to color-convert frame")?
            .clone();

        let frame = match self.backend {
            HwBackend::Vaapi => {
                let va = self
                    .vaapi
                    .as_ref()
                    .ok_or_else(|| anyhow!("no vaapi state"))?;

                va.transfer_nv12_to_hw(&frame)?
            }
            // Other backends accept SW frames directly
            _ => frame,
        };

        self.encoder
            .send_frame(&frame)
            .map_err(|e| anyhow!("send_frame failed: {e:?}"))?;

        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.encoder.send_eof()?;
        Ok(())
    }
}

impl av::VideoEncoder for H264Encoder {
    fn with_preset(preset: av::VideoPreset) -> Result<Self>
    where
        Self: Sized,
    {
        Self::new(preset.width(), preset.height(), preset.fps())
    }
}

impl av::VideoEncoderInner for H264Encoder {
    fn name(&self) -> &str {
        self.encoder.id().name()
    }

    fn config(&self) -> VideoConfig {
        self.video_config().expect("video_config available")
    }

    fn push_frame(&mut self, frame: av::VideoFrame) -> anyhow::Result<()> {
        trace!(len = frame.raw.len(), format=?frame.format, "push frame");
        let frame = frame.to_ffmpeg();
        self.encode_frame(frame)
    }

    fn pop_packet(&mut self) -> anyhow::Result<Option<hang::Frame>> {
        match self.receive_packet()? {
            Poll::Ready(v) => Ok(v),
            Poll::Pending => Ok(None),
        }
    }
}

struct VaapiState {
    device_ctx: *mut AVBufferRef,
    frames_ctx: *mut AVBufferRef,
}

unsafe impl Send for VaapiState {}

impl VaapiState {
    /// Create VAAPI device + frames pool (NV12→VAAPI surfaces) for given size.
    fn new(width: u32, height: u32, device_path: &str) -> Result<Self> {
        // 1) Create VAAPI device
        let cpath = CString::new(device_path)?;
        let mut dev: *mut AVBufferRef = ptr::null_mut();
        let ret = unsafe {
            av_hwdevice_ctx_create(
                &mut dev,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                cpath.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 || dev.is_null() {
            unsafe { av_buffer_unref(&mut dev) };
            return Err(anyhow!("vaapi device create failed: {ret}"));
        }

        // 2) Create frames pool for VAAPI with SW format NV12
        let frames = unsafe { av_hwframe_ctx_alloc(dev) };
        if frames.is_null() {
            unsafe { av_buffer_unref(&mut dev) };
            return Err(anyhow!("av_hwframe_ctx_alloc failed"));
        }
        let fc = unsafe { &mut *((*frames).data as *mut AVHWFramesContext) };
        fc.format = AVPixelFormat::AV_PIX_FMT_VAAPI;
        fc.sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
        fc.width = width as i32;
        fc.height = height as i32;
        fc.initial_pool_size = 32;

        let ret = unsafe { av_hwframe_ctx_init(frames) };
        if ret < 0 {
            unsafe {
                av_buffer_unref(&mut (frames as *mut _));
                av_buffer_unref(&mut dev);
            }
            return Err(anyhow!("av_hwframe_ctx_init failed: {ret}"));
        }

        Ok(Self {
            device_ctx: dev,
            frames_ctx: frames,
        })
    }

    /// Attach the frames context so the codec context expects VAAPI frames.
    fn bind_to_context(&self, ctx: &mut codec::context::Context) {
        unsafe {
            let ctx = ctx.as_mut_ptr();
            (*ctx).hw_frames_ctx = av_buffer_ref(self.frames_ctx);
            (*ctx).pix_fmt = Pixel::VAAPI.into();
        }
    }

    /// Transfer a SW NV12 frame into a VAAPI HW frame, preserving PTS.
    /// Returns a new `VideoFrame` backed by a VAAPI surface.
    fn transfer_nv12_to_hw(&self, sw_frame: &VideoFrame) -> Result<VideoFrame> {
        unsafe {
            // Allocate an empty HW frame from the pool
            let mut hw = VideoFrame::empty();
            let ret = av_hwframe_get_buffer(self.frames_ctx, hw.as_mut_ptr(), 0);
            if ret < 0 {
                return Err(anyhow!("av_hwframe_get_buffer failed: {ret}"));
            }
            // Keep PTS
            (*hw.as_mut_ptr()).pts = sw_frame.pts().unwrap_or(0);

            // Transfer SW NV12 → HW VAAPI surface
            let ret = av_hwframe_transfer_data(hw.as_mut_ptr(), sw_frame.as_ptr(), 0);
            if ret < 0 {
                return Err(anyhow!("av_hwframe_transfer_data failed: {ret}"));
            }

            Ok(hw)
        }
    }
}

impl Drop for VaapiState {
    fn drop(&mut self) {
        unsafe {
            if !self.frames_ctx.is_null() {
                av_buffer_unref(&mut (self.frames_ctx as *mut _));
            }
            if !self.device_ctx.is_null() {
                av_buffer_unref(&mut (self.device_ctx as *mut _));
            }
        }
    }
}

// pub struct Av1FfmpegEncoder {
//     encoder: ffmpeg::encoder::video::Encoder,
//     rescaler: Rescaler,
//     opts: EncoderOpts,
//     frame_count: u64,
// }

// impl Av1FfmpegEncoder {
//     pub fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
//         info!("Initializing AV1 (FFmpeg) encoder: {width}x{height} @ {framerate}fps");
//         ffmpeg::init()?;

//         let pixels = width * height;
//         let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
//         let bitrate = (pixels as f32 * 0.05 * framerate_factor).round() as u64;
//         let opts = EncoderOpts {
//             width,
//             height,
//             framerate,
//             bitrate,
//         };

//         let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::AV1).context("AV1 codec not found")?;
//         let mut ctx = codec::context::Context::new_with_codec(codec);
//         unsafe {
//             let ctx_mut = ctx.as_mut_ptr();
//             (*ctx_mut).width = width as i32;
//             (*ctx_mut).height = height as i32;
//             (*ctx_mut).time_base.num = 1;
//             (*ctx_mut).time_base.den = framerate as i32;
//             (*ctx_mut).framerate.num = framerate as i32;
//             (*ctx_mut).framerate.den = 1;
//             (*ctx_mut).gop_size = framerate as i32;
//             (*ctx_mut).bit_rate = bitrate as i64;
//             (*ctx_mut).pix_fmt = Pixel::YUV420P.into();
//         }
//         // libaom options for realtime
//         let enc_opts =
//             ffmpeg::Dictionary::from_iter([("cpu-used", "8"), ("row-mt", "1"), ("tiles", "2x2")]);
//         let encoder = ctx.encoder().video()?.open_as_with(
//             ffmpeg::encoder::find(ffmpeg::codec::Id::AV1).unwrap(),
//             enc_opts,
//         )?;
//         let rescaler = Rescaler::new(Pixel::YUV420P, Some((width, height)))?;
//         Ok(Self {
//             encoder,
//             rescaler,
//             opts,
//             frame_count: 0,
//         })
//     }
// }

// impl av::VideoEncoder for Av1FfmpegEncoder {
//     fn with_preset(preset: av::VideoPreset) -> Result<Self>
//     where
//         Self: Sized,
//     {
//         Self::new(preset.width(), preset.height(), preset.fps())
//     }
//     fn config(&self) -> hang::catalog::VideoConfig {
//         hang::catalog::VideoConfig {
//             codec: hang::catalog::VideoCodec::AV1(Default::default()),
//             description: None,
//             coded_width: Some(self.opts.width),
//             coded_height: Some(self.opts.height),
//             display_ratio_width: None,
//             display_ratio_height: None,
//             bitrate: Some(self.opts.bitrate),
//             framerate: Some(self.opts.framerate as f64),
//             optimize_for_latency: Some(true),
//         }
//     }
//     fn push_frame(
//         &mut self,
//         format: &av::VideoFormat,
//         frame: av::VideoFrame,
//     ) -> anyhow::Result<()> {
//         use ffmpeg_next::frame::Video as FfFrame;
//         let pixel = match format.pixel_format {
//             av::PixelFormat::Rgba => Pixel::RGBA,
//             av::PixelFormat::Bgra => Pixel::BGRA,
//         };
//         let [w, h] = format.dimensions;
//         let mut ff = FfFrame::new(pixel, w, h);
//         let stride = ff.stride(0) as usize;
//         let row_bytes = (w as usize) * 4;
//         for y in 0..(h as usize) {
//             let dst_off = y * stride;
//             let src_off = y * row_bytes;
//             ff.data_mut(0)[dst_off..dst_off + row_bytes]
//                 .copy_from_slice(&frame.raw[src_off..src_off + row_bytes]);
//         }
//         let sw = self
//             .rescaler
//             .process(&ff)
//             .context("failed to color-convert frame")?
//             .clone();
//         let mut enc_frame = sw;
//         enc_frame.set_pts(Some(self.frame_count as i64));
//         self.frame_count += 1;
//         self.encoder.send_frame(&enc_frame)?;
//         Ok(())
//     }
//     fn pop_packet(&mut self) -> anyhow::Result<Option<hang::Frame>> {
//         let mut packet = ffmpeg::packet::Packet::empty();
//         match self.encoder.receive_packet(&mut packet) {
//             Ok(()) => {
//                 let payload = packet.data().unwrap_or(&[]).to_vec();
//                 let hang_frame = hang::Frame {
//                     payload: payload.into(),
//                     timestamp: std::time::Duration::from_nanos(
//                         self.frame_count.saturating_sub(1) * 1_000_000_000
//                             / self.opts.framerate as u64,
//                     ),
//                     keyframe: packet.is_key(),
//                 };
//                 Ok(Some(hang_frame))
//             }
//             Err(ffmpeg::Error::Eof) => Ok(None),
//             Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => Ok(None),
//             Err(e) => Err(e.into()),
//         }
//     }
// }
