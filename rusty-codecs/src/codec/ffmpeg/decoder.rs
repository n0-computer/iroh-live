use std::{ffi::CString, ptr, time::Duration};

use anyhow::{Context, Result, bail};
use ffmpeg_next::{
    self as ffmpeg, codec,
    codec::Id as CodecId,
    error::EAGAIN,
    format::Pixel,
    frame::Video as AvFrame,
    software::scaling::{self, Flags},
    sys::{
        AVBufferRef, AVHWDeviceType, av_buffer_ref, av_buffer_unref, av_hwdevice_ctx_create,
        av_hwframe_transfer_data,
    },
};
use tracing::debug;

use crate::{
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, MediaPacket, PixelFormat, VideoFrame},
    processing::scale::fit_within,
    traits::VideoDecoder,
};

/// Hardware H.264 decoder names to probe, in preference order.
///
/// V4L2 M2M exposes a dedicated codec name so we can look it up directly.
/// VAAPI uses hwaccel on the generic `h264` decoder (handled separately).
#[cfg(all(target_os = "linux", any(target_arch = "arm", target_arch = "aarch64")))]
const HW_H264_DECODERS: &[&str] = &["h264_v4l2m2m"];
#[cfg(not(all(target_os = "linux", any(target_arch = "arm", target_arch = "aarch64"))))]
const HW_H264_DECODERS: &[&str] = &[];

/// Sets extradata on a codec context from a byte slice.
///
/// # Safety
///
/// Writes to raw ffmpeg context pointer. Caller must ensure the context is
/// valid and no concurrent access occurs.
unsafe fn set_extradata(ctx: &mut codec::context::Context, data: &[u8]) -> Result<()> {
    use ffmpeg_sys_next::{av_freep, av_malloc};
    let p = unsafe { ctx.as_mut_ptr() };
    let size = data.len();
    let buf = unsafe { av_malloc(size + 64) } as *mut u8;
    if buf.is_null() {
        bail!("av_malloc failed for extradata");
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf, size);
        // Zero AV_INPUT_BUFFER_PADDING_SIZE padding bytes.
        std::ptr::write_bytes(buf.add(size), 0, 64);
        if !(*p).extradata.is_null() {
            av_freep(&mut (*p).extradata as *mut *mut u8 as *mut _);
        }
        (*p).extradata = buf;
        (*p).extradata_size = size as i32;
    }
    Ok(())
}

/// Attempts to create a VAAPI device context for hardware decoding.
///
/// Returns `None` if VAAPI is not available (no GPU, missing driver, etc.).
#[cfg(target_os = "linux")]
fn try_create_vaapi_device() -> Option<*mut AVBufferRef> {
    let device_path = CString::new("/dev/dri/renderD128").ok()?;
    let mut dev: *mut AVBufferRef = ptr::null_mut();
    let ret = unsafe {
        av_hwdevice_ctx_create(
            &mut dev,
            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            device_path.as_ptr(),
            ptr::null_mut(),
            0,
        )
    };
    if ret < 0 || dev.is_null() {
        if !dev.is_null() {
            unsafe { av_buffer_unref(&mut dev) };
        }
        debug!("VAAPI device not available (error {ret})");
        None
    } else {
        Some(dev)
    }
}

/// Transfers a hardware-decoded frame (VAAPI surface) to a software frame.
///
/// Returns the original frame unchanged if it is already in a software format.
fn transfer_hw_frame(decoded: &AvFrame) -> Result<AvFrame> {
    if decoded.format() != Pixel::VAAPI {
        // Already a software frame, nothing to do.
        return Ok(decoded.clone());
    }
    let mut sw = AvFrame::empty();
    let ret = unsafe { av_hwframe_transfer_data(sw.as_mut_ptr(), decoded.as_ptr(), 0) };
    if ret < 0 {
        return Err(anyhow::anyhow!(
            "av_hwframe_transfer_data failed (error {ret})"
        ));
    }
    // Preserve PTS.
    sw.set_pts(decoded.pts());
    Ok(sw)
}

/// FFmpeg-backed H.264 video decoder.
///
/// Probes VAAPI hwaccel and V4L2 M2M (on ARM) for hardware decode, falling
/// back to the generic software decoder. Produces RGBA or BGRA output frames
/// with optional viewport downscaling.
#[derive(derive_more::Debug)]
pub struct FfmpegVideoDecoder {
    #[debug(skip)]
    decoder: codec::decoder::Video,
    /// VAAPI device context — kept alive for the decoder's lifetime.
    #[debug(skip)]
    _vaapi_device: Option<VaapiDeviceGuard>,
    #[debug(skip)]
    sws: Option<scaling::Context>,
    #[debug(skip)]
    sws_frame: AvFrame,
    #[debug(skip)]
    decoded: AvFrame,
    pixel_format: PixelFormat,
    viewport_changed: Option<(u32, u32)>,
    target_dimensions: Option<(u32, u32)>,
    last_timestamp: Option<Duration>,
    #[debug(skip)]
    pending_frame: Option<VideoFrame>,
}

// ffmpeg types are used single-threaded; Send is needed for the trait.
unsafe impl Send for FfmpegVideoDecoder {}

/// RAII guard for the VAAPI device buffer reference.
struct VaapiDeviceGuard(*mut AVBufferRef);
unsafe impl Send for VaapiDeviceGuard {}

impl Drop for VaapiDeviceGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { av_buffer_unref(&mut self.0) };
        }
    }
}

// Prevent accidental Debug printing of raw pointer.
impl std::fmt::Debug for VaapiDeviceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaapiDeviceGuard")
    }
}

impl FfmpegVideoDecoder {
    /// Returns the ffmpeg output pixel format matching the requested config.
    fn output_pixel(pf: PixelFormat) -> Pixel {
        match pf {
            PixelFormat::Rgba => Pixel::RGBA,
            PixelFormat::Bgra => Pixel::BGRA,
        }
    }
}

impl VideoDecoder for FfmpegVideoDecoder {
    fn name(&self) -> &str {
        "h264-ffmpeg"
    }

    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        ffmpeg::init().context("failed to initialize ffmpeg")?;

        let codec_id = match &config.codec {
            VideoCodec::H264(_) => CodecId::H264,
            VideoCodec::AV1(_) => CodecId::AV1,
            other => bail!("unsupported codec {other} for ffmpeg decoder"),
        };

        // ── Try dedicated HW decoders by name (V4L2 M2M on ARM) ─────────
        if let Some(hw) = HW_H264_DECODERS
            .iter()
            .find_map(|name| codec::decoder::find_by_name(name))
        {
            let name = hw.name().to_owned();
            let mut ctx = codec::context::Context::new_with_codec(hw);
            if let Some(description) = &config.description {
                unsafe { set_extradata(&mut ctx, description)? };
            }
            match ctx.decoder().video() {
                Ok(decoder) => {
                    tracing::info!(codec = name, "ffmpeg video decoder ready");
                    return Ok(Self {
                        decoder,
                        _vaapi_device: None,
                        sws: None,
                        sws_frame: AvFrame::empty(),
                        decoded: AvFrame::empty(),
                        pixel_format: playback_config.pixel_format,
                        viewport_changed: None,
                        target_dimensions: None,
                        last_timestamp: None,
                        pending_frame: None,
                    });
                }
                Err(e) => {
                    debug!(codec = name, error = %e, "HW decoder probe failed, continuing");
                }
            }
        }

        // ── Try VAAPI hwaccel on the generic decoder ─────────────────────
        #[cfg(target_os = "linux")]
        if codec_id == CodecId::H264
            && matches!(playback_config.backend, crate::format::DecoderBackend::Auto)
            && let Some(vaapi_dev) = try_create_vaapi_device()
        {
            let codec = codec::decoder::find(codec_id)
                .with_context(|| format!("{codec_id:?} decoder not found"))?;
            let mut ctx = codec::context::Context::new_with_codec(codec);
            if let Some(description) = &config.description {
                unsafe { set_extradata(&mut ctx, description)? };
            }
            // Attach the VAAPI device so the decoder uses hardware accel.
            unsafe {
                let p = ctx.as_mut_ptr();
                (*p).hw_device_ctx = av_buffer_ref(vaapi_dev);
            }
            match ctx.decoder().video() {
                Ok(decoder) => {
                    tracing::info!("ffmpeg video decoder ready (VAAPI hwaccel)");
                    return Ok(Self {
                        decoder,
                        _vaapi_device: Some(VaapiDeviceGuard(vaapi_dev)),
                        sws: None,
                        sws_frame: AvFrame::empty(),
                        decoded: AvFrame::empty(),
                        pixel_format: playback_config.pixel_format,
                        viewport_changed: None,
                        target_dimensions: None,
                        last_timestamp: None,
                        pending_frame: None,
                    });
                }
                Err(e) => {
                    unsafe { av_buffer_unref(&mut (vaapi_dev as *mut _)) };
                    debug!(error = %e, "VAAPI hwaccel decoder init failed, falling back");
                }
            }
        }

        // ── Fallback: generic software decoder ───────────────────────────
        let codec = codec::decoder::find(codec_id)
            .with_context(|| format!("{codec_id:?} decoder not found in ffmpeg"))?;
        let codec_name = codec.name().to_owned();
        let mut ctx = codec::context::Context::new_with_codec(codec);

        if let Some(description) = &config.description {
            unsafe { set_extradata(&mut ctx, description)? };
        }

        let decoder = ctx.decoder().video()?;

        tracing::info!(codec = codec_name, "ffmpeg video decoder ready (software)");

        Ok(Self {
            decoder,
            _vaapi_device: None,
            sws: None,
            sws_frame: AvFrame::empty(),
            decoded: AvFrame::empty(),
            pixel_format: playback_config.pixel_format,
            viewport_changed: None,
            target_dimensions: None,
            last_timestamp: None,
            pending_frame: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, packet: MediaPacket) -> Result<()> {
        let timestamp = packet.timestamp;
        let payload = packet.into_payload_bytes();

        let pkt = ffmpeg::Packet::copy(&payload);
        self.decoder.send_packet(&pkt)?;

        // Try to receive a decoded frame.
        match self.decoder.receive_frame(&mut self.decoded) {
            Ok(()) => {
                // If the decoder used VAAPI hwaccel, the frame is a HW
                // surface — transfer it to software before colour conversion.
                let sw_frame = transfer_hw_frame(&self.decoded)?;

                let w = sw_frame.width();
                let h = sw_frame.height();

                // Update viewport scaling target.
                if let Some((max_w, max_h)) = self.viewport_changed.take() {
                    let (tw, th) = fit_within(w, h, max_w, max_h);
                    self.target_dimensions = Some((tw, th));
                }

                let (out_w, out_h) = self.target_dimensions.unwrap_or((w, h));
                let target_pixel = Self::output_pixel(self.pixel_format);

                let needs_sws = sw_frame.format() != target_pixel || w != out_w || h != out_h;

                if needs_sws {
                    let needs_reset = self.sws_frame.width() != out_w
                        || self.sws_frame.height() != out_h
                        || self.sws_frame.format() != target_pixel;
                    if needs_reset {
                        self.sws_frame = AvFrame::new(target_pixel, out_w, out_h);
                    }

                    let ctx = match self.sws {
                        Some(ref mut c) => {
                            c.cached(
                                sw_frame.format(),
                                w,
                                h,
                                target_pixel,
                                out_w,
                                out_h,
                                Flags::BILINEAR,
                            );
                            c
                        }
                        None => self.sws.insert(scaling::Context::get(
                            sw_frame.format(),
                            w,
                            h,
                            target_pixel,
                            out_w,
                            out_h,
                            Flags::BILINEAR,
                        )?),
                    };

                    ctx.run(&sw_frame, &mut self.sws_frame)?;
                    self.pending_frame =
                        Some(avframe_to_videoframe(&self.sws_frame, self.pixel_format));
                } else {
                    self.pending_frame = Some(avframe_to_videoframe(&sw_frame, self.pixel_format));
                }
            }
            Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => {}
            Err(ffmpeg::Error::Eof) => {}
            Err(e) => return Err(e.into()),
        }

        self.last_timestamp = Some(timestamp);
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some(mut frame) = self.pending_frame.take() else {
            return Ok(None);
        };

        let timestamp = self.last_timestamp.context("missing packet timestamp")?;
        frame.timestamp = timestamp;

        Ok(Some(frame))
    }
}

/// Copies an ffmpeg AVFrame (RGBA or BGRA) into a `VideoFrame`.
fn avframe_to_videoframe(frame: &AvFrame, pixel_format: PixelFormat) -> VideoFrame {
    let w = frame.width();
    let h = frame.height();
    let stride = frame.stride(0);
    let row_bytes = (w as usize) * 4;
    let src = frame.data(0);

    let mut out = Vec::with_capacity(row_bytes * (h as usize));
    for y in 0..(h as usize) {
        let off = y * stride;
        out.extend_from_slice(&src[off..off + row_bytes]);
    }

    VideoFrame::new_cpu_with_format(out, w, h, Duration::ZERO, pixel_format)
}
