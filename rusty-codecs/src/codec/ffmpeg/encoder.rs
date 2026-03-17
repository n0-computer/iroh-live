use std::{
    collections::VecDeque,
    ffi::{CString, c_int},
    fmt, ptr,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use ffmpeg_next::{
    self as ffmpeg, codec,
    error::EAGAIN,
    format::Pixel,
    frame::Video as AvFrame,
    software::scaling::{self, Flags},
    sys::{
        AVBufferRef, AVHWDeviceType, AVHWFramesContext, AVPixelFormat, av_buffer_ref,
        av_buffer_unref, av_hwdevice_ctx_create, av_hwframe_ctx_alloc, av_hwframe_ctx_init,
        av_hwframe_get_buffer, av_hwframe_transfer_data,
    },
};
use tracing::{debug, info, trace};

use crate::{
    config::{H264, VideoCodec, VideoConfig},
    format::{EncodedFrame, NalFormat, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::scale::Scaler,
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// Bits-per-pixel factor for H.264 default bitrate calculation.
const H264_BPP: f32 = 0.07;

#[derive(Debug, Clone, Copy, Default)]
#[allow(
    unused,
    reason = "variants are conditionally used based on target platform"
)]
enum HwBackend {
    #[default]
    Software,
    Vaapi,
    Videotoolbox,
    Nvenc,
    Qsv,
    Amf,
    /// V4L2 Memory-to-Memory (Raspberry Pi, other embedded Linux).
    V4l2m2m,
}

impl HwBackend {
    fn codec_name(self) -> &'static str {
        match self {
            Self::Software => "libx264",
            Self::Vaapi => "h264_vaapi",
            Self::Videotoolbox => "h264_videotoolbox",
            Self::Nvenc => "h264_nvenc",
            Self::Qsv => "h264_qsv",
            Self::Amf => "h264_amf",
            Self::V4l2m2m => "h264_v4l2m2m",
        }
    }

    fn candidates() -> Vec<Self> {
        let mut candidates = Vec::new();
        #[cfg(target_os = "macos")]
        candidates.push(Self::Videotoolbox);
        #[cfg(target_os = "windows")]
        candidates.extend_from_slice(&[Self::Nvenc, Self::Qsv, Self::Amf]);
        #[cfg(target_os = "linux")]
        {
            // On ARM (Raspberry Pi, Rockchip, etc.) try V4L2 M2M first since
            // VAAPI is rarely available. On x86 prefer VAAPI/NVENC/QSV.
            #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
            candidates.push(Self::V4l2m2m);
            candidates.extend_from_slice(&[Self::Vaapi, Self::Nvenc, Self::Qsv]);
            // V4L2 M2M as fallback on x86 too (some SBCs expose it).
            #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
            candidates.push(Self::V4l2m2m);
        }
        // Software is always available as final fallback.
        candidates.push(Self::Software);
        candidates
    }

    /// Software pixel format the encoder expects as input.
    fn sw_pixel_format(self) -> Pixel {
        match self {
            Self::Vaapi | Self::Qsv => Pixel::NV12,
            // V4L2 M2M accepts YUV420P as software input; the kernel driver
            // handles the actual pixel format negotiation.
            _ => Pixel::YUV420P,
        }
    }

    /// Pixel format to set on the codec context.
    fn codec_pixel_format(self) -> Pixel {
        match self {
            Self::Vaapi => Pixel::VAAPI,
            Self::Qsv => Pixel::NV12,
            _ => Pixel::YUV420P,
        }
    }
}

impl fmt::Display for HwBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.codec_name())
    }
}

/// FFmpeg-based H.264 encoder with automatic hardware backend selection.
///
/// Probes VAAPI, VideoToolbox, NVENC, QSV, and AMF at construction time,
/// falling back to software `libx264`. Produces avcC-framed output by
/// default for compatibility with the MoQ transport layer.
pub struct FfmpegH264Encoder {
    encoder: codec::encoder::video::Encoder,
    backend: HwBackend,
    vaapi: Option<VaapiState>,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    nal_format: NalFormat,
    /// Whether the encoder natively outputs Annex B (HW encoders) or
    /// length-prefixed (libx264 with `annexb=0`).
    encoder_outputs_annexb: bool,
    scale_mode: ScaleMode,
    scaler: Scaler,
    /// Cached avcC description from encoder extradata.
    avcc: Option<Vec<u8>>,
    /// Rescaler for converting input frames to the encoder's expected format.
    sws: Option<scaling::Context>,
    sws_frame: AvFrame,
    /// Encoded packets ready for collection.
    packet_buf: VecDeque<EncodedFrame>,
}

impl fmt::Debug for FfmpegH264Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfmpegH264Encoder")
            .field("backend", &self.backend)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("framerate", &self.framerate)
            .field("bitrate", &self.bitrate)
            .field("frame_count", &self.frame_count)
            .finish_non_exhaustive()
    }
}

// The ffmpeg types are internally thread-safe for our usage pattern
// (single-threaded push/pop). We need Send to satisfy the VideoEncoder trait.
unsafe impl Send for FfmpegH264Encoder {}

impl FfmpegH264Encoder {
    fn new(config: VideoEncoderConfig) -> Result<Self> {
        ffmpeg::init().context("failed to initialize ffmpeg")?;

        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(H264_BPP);
        let nal_format = config.nal_format;
        let keyframe_interval = config.keyframe_interval_or_default();

        let candidates = HwBackend::candidates();
        let mut last_err: Option<anyhow::Error> = None;

        for backend in candidates {
            match Self::open_encoder(
                backend,
                width,
                height,
                framerate,
                bitrate,
                keyframe_interval,
            ) {
                Ok((encoder, vaapi)) => {
                    info!(
                        backend = backend.codec_name(),
                        width, height, framerate, bitrate, "ffmpeg H.264 encoder ready"
                    );

                    // libx264 with annexb=0 outputs length-prefixed; all
                    // HW encoders output Annex B natively.
                    let encoder_outputs_annexb = !matches!(backend, HwBackend::Software);

                    let avcc = if nal_format == NalFormat::Avcc {
                        if encoder_outputs_annexb {
                            // HW encoder: extract avcC from SPS/PPS in extradata.
                            get_extradata(&encoder).and_then(|extra| {
                                let nals = crate::codec::h264::annexb::parse_annex_b(&extra);
                                crate::codec::h264::annexb::extract_sps_pps(&nals).map(
                                    |(sps, pps)| crate::codec::h264::annexb::build_avcc(&sps, &pps),
                                )
                            })
                        } else {
                            // libx264: extradata is already avcC format.
                            get_extradata(&encoder)
                        }
                    } else {
                        None
                    };

                    return Ok(Self {
                        encoder,
                        backend,
                        vaapi,
                        width,
                        height,
                        framerate,
                        bitrate,
                        frame_count: 0,
                        nal_format,
                        encoder_outputs_annexb,
                        scale_mode: config.scale_mode,
                        scaler: Scaler::new(Some((width, height))),
                        avcc,
                        sws: None,
                        sws_frame: AvFrame::empty(),
                        packet_buf: VecDeque::new(),
                    });
                }
                Err(e) => {
                    debug!(
                        backend = backend.codec_name(),
                        error = %e,
                        "ffmpeg backend not available"
                    );
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("no H.264 encoder available via ffmpeg")))
    }

    fn open_encoder(
        backend: HwBackend,
        width: u32,
        height: u32,
        framerate: u32,
        bitrate: u64,
        keyframe_interval: u32,
    ) -> Result<(codec::encoder::video::Encoder, Option<VaapiState>)> {
        let codec = codec::encoder::find_by_name(backend.codec_name())
            .with_context(|| format!("encoder {} not found", backend.codec_name()))?;

        let mut ctx = codec::context::Context::new_with_codec(codec);
        unsafe {
            let p = ctx.as_mut_ptr();
            (*p).width = width as i32;
            (*p).height = height as i32;
            (*p).time_base.num = 1;
            (*p).time_base.den = framerate as i32;
            (*p).framerate.num = framerate as i32;
            (*p).framerate.den = 1;
            (*p).gop_size = keyframe_interval as i32;
            (*p).bit_rate = bitrate as i64;
            (*p).flags |= codec::Flags::GLOBAL_HEADER.bits() as c_int;
            (*p).pix_fmt = backend.codec_pixel_format().into();
        }

        let vaapi_state = if matches!(backend, HwBackend::Vaapi) {
            let va = VaapiState::new(width, height, "/dev/dri/renderD128")?;
            va.bind_to_context(&mut ctx);
            Some(va)
        } else {
            None
        };

        // Only libx264 supports `annexb=0` (length-prefixed NALs + avcC
        // extradata). HW encoders produce Annex B natively.
        let enc_opts: Vec<(&str, &str)> = match backend {
            HwBackend::Software => vec![
                ("annexb", "0"),
                ("preset", "ultrafast"),
                ("tune", "zerolatency"),
                ("profile", "baseline"),
            ],
            _ => vec![],
        };
        let dict = ffmpeg::Dictionary::from_iter(enc_opts);
        let encoder = ctx.encoder().video()?.open_as_with(codec, dict)?;

        Ok((encoder, vaapi_state))
    }

    /// Converts the input VideoFrame into an ffmpeg AVFrame in the pixel
    /// format the active backend expects.
    fn convert_frame(&mut self, frame: &VideoFrame) -> Result<AvFrame> {
        let [w, h] = frame.dimensions;
        let target_fmt = self.backend.sw_pixel_format();

        // Build an ffmpeg frame from the VideoFrame pixel data.
        let src_frame = match &frame.data {
            crate::format::FrameData::Packed { pixel_format, data } => {
                let pixel = match pixel_format {
                    crate::format::PixelFormat::Rgba => Pixel::RGBA,
                    crate::format::PixelFormat::Bgra => Pixel::BGRA,
                };
                let mut f = AvFrame::new(pixel, w, h);
                let stride = f.stride(0);
                let row_bytes = (w as usize) * 4;
                for y in 0..(h as usize) {
                    let dst_off = y * stride;
                    let src_off = y * row_bytes;
                    f.data_mut(0)[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&data[src_off..src_off + row_bytes]);
                }
                f
            }
            crate::format::FrameData::I420 { y, u, v } => {
                let mut f = AvFrame::new(Pixel::YUV420P, w, h);
                // Copy Y plane
                let y_stride = f.stride(0);
                for row in 0..(h as usize) {
                    let dst_off = row * y_stride;
                    let src_off = row * (w as usize);
                    f.data_mut(0)[dst_off..dst_off + w as usize]
                        .copy_from_slice(&y[src_off..src_off + w as usize]);
                }
                // Copy U plane
                let u_stride = f.stride(1);
                let half_w = (w as usize) / 2;
                let half_h = (h as usize) / 2;
                for row in 0..half_h {
                    let dst_off = row * u_stride;
                    let src_off = row * half_w;
                    f.data_mut(1)[dst_off..dst_off + half_w]
                        .copy_from_slice(&u[src_off..src_off + half_w]);
                }
                // Copy V plane
                let v_stride = f.stride(2);
                for row in 0..half_h {
                    let dst_off = row * v_stride;
                    let src_off = row * half_w;
                    f.data_mut(2)[dst_off..dst_off + half_w]
                        .copy_from_slice(&v[src_off..src_off + half_w]);
                }
                f
            }
            crate::format::FrameData::Nv12(planes) => {
                let mut f = AvFrame::new(Pixel::NV12, w, h);
                // Y plane
                let y_stride = f.stride(0);
                for row in 0..(h as usize) {
                    let dst_off = row * y_stride;
                    let src_off = row * (planes.y_stride as usize);
                    f.data_mut(0)[dst_off..dst_off + w as usize]
                        .copy_from_slice(&planes.y_data[src_off..src_off + w as usize]);
                }
                // UV plane
                let uv_stride = f.stride(1);
                let half_h = (h as usize) / 2;
                for row in 0..half_h {
                    let dst_off = row * uv_stride;
                    let src_off = row * (planes.uv_stride as usize);
                    f.data_mut(1)[dst_off..dst_off + w as usize]
                        .copy_from_slice(&planes.uv_data[src_off..src_off + w as usize]);
                }
                f
            }
            crate::format::FrameData::Gpu(gpu) => {
                // Fall back to downloading RGBA from GPU.
                let img = gpu
                    .download_rgba()
                    .context("failed to download GPU frame")?;
                let mut f = AvFrame::new(Pixel::RGBA, w, h);
                let stride = f.stride(0);
                let row_bytes = (w as usize) * 4;
                for y in 0..(h as usize) {
                    let dst_off = y * stride;
                    let src_off = y * row_bytes;
                    f.data_mut(0)[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&img.as_raw()[src_off..src_off + row_bytes]);
                }
                f
            }
        };

        // If the source format already matches what the encoder wants, return directly.
        if src_frame.format() == target_fmt && src_frame.width() == w && src_frame.height() == h {
            return Ok(src_frame);
        }

        // Use sws_scale to convert.
        let needs_reset = self.sws_frame.width() != w
            || self.sws_frame.height() != h
            || self.sws_frame.format() != target_fmt;
        if needs_reset {
            self.sws_frame = AvFrame::new(target_fmt, w, h);
        }

        let ctx = match self.sws {
            Some(ref mut ctx) => {
                ctx.cached(
                    src_frame.format(),
                    src_frame.width(),
                    src_frame.height(),
                    target_fmt,
                    w,
                    h,
                    Flags::BILINEAR,
                );
                ctx
            }
            None => self.sws.insert(scaling::Context::get(
                src_frame.format(),
                src_frame.width(),
                src_frame.height(),
                target_fmt,
                w,
                h,
                Flags::BILINEAR,
            )?),
        };

        ctx.run(&src_frame, &mut self.sws_frame)?;
        Ok(self.sws_frame.clone())
    }

    /// Scales the frame to encoder dimensions if needed.
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        let [fw, fh] = frame.dimensions;
        if fw == self.width && fh == self.height {
            return Ok(frame);
        }
        let (tw, th) = self.scale_mode.resolve((fw, fh), (self.width, self.height));
        if tw == fw && th == fh {
            return Ok(frame);
        }
        self.scaler.set_target_dimensions(tw, th);
        let img = frame.rgba_image();
        let scaled = if self.scale_mode == ScaleMode::Cover {
            self.scaler.scale_cover_rgba(img.as_raw(), fw, fh)?
        } else {
            self.scaler.scale_rgba(img.as_raw(), fw, fh)?
        };
        match scaled {
            Some((data, w, h)) => Ok(VideoFrame::new_rgba(data.into(), w, h, frame.timestamp)),
            None => Ok(frame),
        }
    }

    fn receive_packets(&mut self) -> Result<()> {
        let mut packet = ffmpeg::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let raw = packet.data().unwrap_or(&[]);

                    // Convert between the encoder's native format and the
                    // requested NAL format.
                    let payload: bytes::Bytes = match (self.encoder_outputs_annexb, self.nal_format)
                    {
                        // HW encoder → Annex B out, caller wants Annex B: passthrough.
                        (true, NalFormat::AnnexB) => raw.to_vec().into(),
                        // HW encoder → Annex B out, caller wants avcC: convert.
                        (true, NalFormat::Avcc) => {
                            crate::codec::h264::annexb::annex_b_to_length_prefixed(raw).into()
                        }
                        // libx264 (annexb=0) → length-prefixed, caller wants avcC: passthrough.
                        (false, NalFormat::Avcc) => raw.to_vec().into(),
                        // libx264 (annexb=0) → length-prefixed, caller wants Annex B: convert.
                        (false, NalFormat::AnnexB) => {
                            crate::codec::h264::annexb::length_prefixed_to_annex_b(raw).into()
                        }
                    };

                    let is_keyframe = packet.is_key();
                    let timestamp_us =
                        (self.frame_count.saturating_sub(1)) * 1_000_000 / self.framerate as u64;

                    self.packet_buf.push_back(EncodedFrame {
                        is_keyframe,
                        timestamp: Duration::from_micros(timestamp_us),
                        payload,
                    });
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

impl VideoEncoderFactory for FfmpegH264Encoder {
    const ID: &str = "h264-ffmpeg";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(H264_BPP);
        let inline = config.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline,
            }),
            description: None,
            coded_width: Some(config.width),
            coded_height: Some(config.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(bitrate),
            framerate: Some(config.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }
}

impl VideoEncoder for FfmpegH264Encoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline,
            }),
            description: self.avcc.clone().map(Into::into),
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.bitrate),
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        let frame = self.scale_if_needed(frame)?;

        let mut av_frame = self.convert_frame(&frame)?;
        av_frame.set_pts(Some(self.frame_count as i64));
        self.frame_count += 1;

        if self.frame_count.is_multiple_of(self.framerate as u64) {
            trace!(
                frame_count = self.frame_count,
                backend = %self.backend,
                "ffmpeg encode progress"
            );
        }

        // For VAAPI, upload the SW frame to a HW surface.
        let av_frame = if matches!(self.backend, HwBackend::Vaapi) {
            let va = self
                .vaapi
                .as_ref()
                .ok_or_else(|| anyhow!("missing VAAPI state"))?;
            va.transfer_to_hw(&av_frame)?
        } else {
            av_frame
        };

        self.encoder
            .send_frame(&av_frame)
            .map_err(|e| anyhow!("ffmpeg send_frame failed: {e}"))?;

        self.receive_packets()?;

        // On first keyframe in avcC mode, capture the extradata if we
        // don't have it yet (some encoders only populate it after the first
        // IDR frame).
        if self.nal_format == NalFormat::Avcc
            && self.avcc.is_none()
            && let Some(extra) = get_extradata(&self.encoder)
        {
            self.avcc = Some(if self.encoder_outputs_annexb {
                // HW encoder: extradata is Annex B SPS/PPS, build avcC.
                let nals = crate::codec::h264::annexb::parse_annex_b(&extra);
                crate::codec::h264::annexb::extract_sps_pps(&nals)
                    .map(|(sps, pps)| crate::codec::h264::annexb::build_avcc(&sps, &pps))
                    .unwrap_or(extra)
            } else {
                // libx264: extradata is already avcC.
                extra
            });
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        Ok(self.packet_buf.pop_front())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        // ffmpeg doesn't support runtime bitrate changes on most encoders.
        // Store the value for config() reporting but don't reconfigure.
        self.bitrate = bitrate;
        Ok(())
    }
}

/// Reads the `extradata` field from the encoder's underlying codec context.
fn get_extradata(encoder: &codec::encoder::video::Encoder) -> Option<Vec<u8>> {
    // SAFETY: accessing the raw ffmpeg context to read extradata. The encoder
    // owns the context and we only read immutable fields.
    let ctx = unsafe { (*encoder).as_ptr() };
    let size = unsafe { (*ctx).extradata_size };
    if size > 0 {
        let ptr = unsafe { (*ctx).extradata };
        if !ptr.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(ptr, size as usize) };
            return Some(slice.to_vec());
        }
    }
    None
}

// ── VAAPI hardware state ────────────────────────────────────────────────

struct VaapiState {
    device_ctx: *mut AVBufferRef,
    frames_ctx: *mut AVBufferRef,
}

unsafe impl Send for VaapiState {}

impl VaapiState {
    fn new(width: u32, height: u32, device_path: &str) -> Result<Self> {
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
            return Err(anyhow!("VAAPI device creation failed (error {ret})"));
        }

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
            return Err(anyhow!("av_hwframe_ctx_init failed (error {ret})"));
        }

        Ok(Self {
            device_ctx: dev,
            frames_ctx: frames,
        })
    }

    fn bind_to_context(&self, ctx: &mut codec::context::Context) {
        unsafe {
            let p = ctx.as_mut_ptr();
            (*p).hw_frames_ctx = av_buffer_ref(self.frames_ctx);
            (*p).pix_fmt = Pixel::VAAPI.into();
        }
    }

    /// Transfers a software NV12 frame onto a VAAPI surface.
    fn transfer_to_hw(&self, sw_frame: &AvFrame) -> Result<AvFrame> {
        unsafe {
            let mut hw = AvFrame::empty();
            let ret = av_hwframe_get_buffer(self.frames_ctx, hw.as_mut_ptr(), 0);
            if ret < 0 {
                return Err(anyhow!("av_hwframe_get_buffer failed (error {ret})"));
            }
            (*hw.as_mut_ptr()).pts = sw_frame.pts().unwrap_or(0);

            let ret = av_hwframe_transfer_data(hw.as_mut_ptr(), sw_frame.as_ptr(), 0);
            if ret < 0 {
                return Err(anyhow!("av_hwframe_transfer_data failed (error {ret})"));
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
