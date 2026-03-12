use std::{cell::RefCell, os::fd::AsRawFd, rc::Rc};

use anyhow::{Context, Result};
use cros_codecs::{
    BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution,
    backend::vaapi::encoder::VaapiBackend,
    encoder::{
        CodedBitstreamBuffer, FrameMetadata, PredictionStructure, RateControl, Tunings,
        VideoEncoder as CrosVideoEncoder, h264::EncoderConfig,
        stateless::h264::StatelessEncoder as H264StatelessEncoder,
    },
    libva::{
        Display, Image, MemoryType, Surface, SurfaceMemoryDescriptor, UsageHint, VA_FOURCC_NV12,
        VA_RT_FORMAT_YUV420, VADRMPRIMESurfaceDescriptor, VADRMPRIMESurfaceDescriptorLayer,
        VADRMPRIMESurfaceDescriptorObject, VAEntrypoint, VAProfile, VASurfaceAttrib,
    },
    video_frame::{ReadMapping, VideoFrame as CrosVideoFrame, WriteMapping},
};

use crate::{
    codec::h264::annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b},
    config::{H264, VideoCodec, VideoConfig},
    format::{
        DmaBufInfo, EncodedFrame, NalFormat, NativeFrameHandle, ScaleMode, VideoEncoderConfig,
        VideoFrame,
    },
    processing::convert::{YuvData, pixel_format_to_nv12},
    processing::scale::Scaler,
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// Encoder input frame — either CPU NV12 data or a zero-copy DMA-BUF handle.
///
/// Implements `cros_codecs::video_frame::VideoFrame` so it can be submitted to
/// the stateless encoder. The NV12 variant uploads pixel data via image mapping;
/// the DMA-BUF variant imports the buffer directly as a VA surface, avoiding any
/// CPU copy.
#[derive(Debug)]
enum VaapiInputFrame {
    /// CPU-side NV12 pixel data.
    Nv12 {
        data: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Zero-copy DMA-BUF from capture source.
    DmaBuf(DmaBufInfo),
}

impl VaapiInputFrame {
    fn width(&self) -> u32 {
        match self {
            Self::Nv12 { width, .. } => *width,
            Self::DmaBuf(info) => info.coded_width,
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Nv12 { height, .. } => *height,
            Self::DmaBuf(info) => info.coded_height,
        }
    }
}

/// Implements `SurfaceMemoryDescriptor` so the frame can control how the VA
/// surface is backed: driver-allocated for NV12 (same as `()`), or imported
/// from DMA-BUF via `DRM_PRIME_2` for zero-copy.
impl SurfaceMemoryDescriptor for VaapiInputFrame {
    fn add_attrs(&mut self, attrs: &mut Vec<VASurfaceAttrib>) -> Option<Box<dyn std::any::Any>> {
        match self {
            Self::Nv12 { .. } => None,
            Self::DmaBuf(info) => {
                let mut desc = build_prime_descriptor(info);
                attrs.push(VASurfaceAttrib::new_memory_type(MemoryType::DrmPrime2));
                attrs.push(VASurfaceAttrib::new_buffer_descriptor(&mut desc));
                Some(Box::new(desc))
            }
        }
    }
}

/// Builds a `VADRMPRIMESurfaceDescriptor` from our `DmaBufInfo`.
///
/// VA-API layer arrays are fixed at 4 elements, so at most 4 planes are
/// supported (NV12 = 2, I420 = 3).
fn build_prime_descriptor(info: &DmaBufInfo) -> VADRMPRIMESurfaceDescriptor {
    debug_assert!(
        info.planes.len() <= 4,
        "DMA-BUF has {} planes, VA-API supports at most 4",
        info.planes.len()
    );

    let mut objects: [VADRMPRIMESurfaceDescriptorObject; 4] = Default::default();
    objects[0] = VADRMPRIMESurfaceDescriptorObject {
        fd: info.fd.as_raw_fd(),
        size: 0, // driver calculates
        drm_format_modifier: info.modifier,
    };

    let mut layer = VADRMPRIMESurfaceDescriptorLayer {
        drm_format: info.drm_format,
        num_planes: info.planes.len() as u32,
        ..Default::default()
    };
    for (i, plane) in info.planes.iter().enumerate() {
        layer.object_index[i] = 0;
        layer.offset[i] = plane.offset;
        layer.pitch[i] = plane.pitch;
    }

    let mut layers: [VADRMPRIMESurfaceDescriptorLayer; 4] = Default::default();
    layers[0] = layer;

    VADRMPRIMESurfaceDescriptor {
        fourcc: info.drm_format,
        width: info.coded_width,
        height: info.coded_height,
        num_objects: 1,
        objects,
        num_layers: 1,
        layers,
    }
}

struct Nv12ReadMapping<'a> {
    y_plane: &'a [u8],
    uv_plane: &'a [u8],
}

impl<'a> ReadMapping<'a> for Nv12ReadMapping<'a> {
    fn get(&self) -> Vec<&[u8]> {
        vec![self.y_plane, self.uv_plane]
    }
}

struct Nv12WriteMapping;

impl<'a> WriteMapping<'a> for Nv12WriteMapping {
    fn get(&self) -> Vec<RefCell<&'a mut [u8]>> {
        // No-op: NV12 frames are uploaded via `upload_nv12_to_surface` and
        // DMA-BUF frames are imported directly — neither uses write mappings.
        vec![]
    }
}

impl CrosVideoFrame for VaapiInputFrame {
    type MemDescriptor = Self;
    type NativeHandle = Surface<Self>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        Resolution {
            width: self.width(),
            height: self.height(),
        }
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let w = self.width();
        let h = self.height();
        let y_size = (w * h) as usize;
        let uv_size = (w * h.div_ceil(2)) as usize;
        vec![y_size, uv_size]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        match self {
            Self::Nv12 { width, .. } => vec![*width as usize, *width as usize],
            Self::DmaBuf(info) => info.planes.iter().map(|p| p.pitch as usize).collect(),
        }
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        match self {
            Self::Nv12 {
                data,
                width,
                height,
            } => {
                let y_size = (*width as usize) * (*height as usize);
                Ok(Box::new(Nv12ReadMapping {
                    y_plane: &data[..y_size],
                    uv_plane: &data[y_size..],
                }))
            }
            Self::DmaBuf(_) => Err("DMA-BUF frames cannot be CPU-mapped".to_string()),
        }
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Ok(Box::new(Nv12WriteMapping))
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        match self {
            Self::Nv12 {
                data,
                width,
                height,
            } => {
                // Create a driver-allocated VA surface and upload NV12 data.
                let dummy = Self::Nv12 {
                    data: Vec::new(),
                    width: *width,
                    height: *height,
                };
                let mut surfaces = display
                    .create_surfaces(
                        VA_RT_FORMAT_YUV420,
                        Some(VA_FOURCC_NV12),
                        *width,
                        *height,
                        Some(UsageHint::USAGE_HINT_ENCODER),
                        vec![dummy],
                    )
                    .map_err(|e| format!("failed to create VA surface: {e:?}"))?;
                let surface = surfaces.pop().ok_or("no surface created")?;
                upload_nv12_to_surface(display, &surface, data, *width, *height)
                    .map_err(|e| format!("failed to upload NV12 data: {e:?}"))?;
                Ok(surface)
            }
            Self::DmaBuf(info) => {
                // Import the DMA-BUF directly as a VA surface — zero-copy.
                let frame = Self::DmaBuf(DmaBufInfo {
                    fd: info
                        .fd
                        .try_clone()
                        .map_err(|e| format!("failed to dup DMA-BUF fd: {e}"))?,
                    modifier: info.modifier,
                    drm_format: info.drm_format,
                    coded_width: info.coded_width,
                    coded_height: info.coded_height,
                    display_width: info.display_width,
                    display_height: info.display_height,
                    planes: info.planes.clone(),
                });
                let mut surfaces = display
                    .create_surfaces(
                        VA_RT_FORMAT_YUV420,
                        Some(info.drm_format),
                        info.coded_width,
                        info.coded_height,
                        Some(UsageHint::USAGE_HINT_ENCODER),
                        vec![frame],
                    )
                    .map_err(|e| format!("failed to import DMA-BUF as VA surface: {e:?}"))?;
                surfaces
                    .pop()
                    .ok_or_else(|| "no surface created".to_string())
            }
        }
    }
}

/// Upload raw NV12 data to a VA surface using image mapping.
fn upload_nv12_to_surface(
    display: &Rc<Display>,
    surface: &Surface<VaapiInputFrame>,
    nv12: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let image_fmts = display
        .query_image_formats()
        .map_err(|e| anyhow::anyhow!("failed to query image formats: {e:?}"))?;

    let nv12_fmt = image_fmts
        .into_iter()
        .find(|f| f.fourcc == VA_FOURCC_NV12)
        .context("VAAPI display does not support NV12 image format")?;

    let size = (width, height);
    let mut image = Image::create_from(surface, nv12_fmt, size, size)
        .map_err(|e| anyhow::anyhow!("failed to create VAAPI image: {e:?}"))?;

    let va_image = *image.image();
    let dst = image.as_mut();

    let w = width as usize;
    let h = height as usize;

    // Copy Y plane (respecting VA pitch).
    let y_offset = va_image.offsets[0] as usize;
    let y_pitch = va_image.pitches[0] as usize;
    for row in 0..h {
        let src_start = row * w;
        let dst_start = y_offset + row * y_pitch;
        let copy_len = w.min(y_pitch);
        if src_start + copy_len <= nv12.len() && dst_start + copy_len <= dst.len() {
            dst[dst_start..dst_start + copy_len]
                .copy_from_slice(&nv12[src_start..src_start + copy_len]);
        }
    }

    // Copy UV plane (interleaved, respecting VA pitch).
    let uv_offset = va_image.offsets[1] as usize;
    let uv_pitch = va_image.pitches[1] as usize;
    let chroma_h = h.div_ceil(2);
    let chroma_w_bytes = w; // NV12: U+V interleaved = width bytes
    let y_plane_size = w * h;
    for row in 0..chroma_h {
        let src_start = y_plane_size + row * chroma_w_bytes;
        let dst_start = uv_offset + row * uv_pitch;
        let copy_len = chroma_w_bytes.min(uv_pitch);
        if src_start + copy_len <= nv12.len() && dst_start + copy_len <= dst.len() {
            dst[dst_start..dst_start + copy_len]
                .copy_from_slice(&nv12[src_start..src_start + copy_len]);
        }
    }

    // Drop image to unmap and sync to surface.
    drop(image);
    surface
        .sync()
        .map_err(|e| anyhow::anyhow!("VA surface sync failed: {e:?}"))?;

    Ok(())
}

/// The concrete H264 VAAPI encoder type.
type VaapiH264Encoder =
    H264StatelessEncoder<VaapiInputFrame, VaapiBackend<VaapiInputFrame, Surface<VaapiInputFrame>>>;

/// VAAPI hardware-accelerated H.264 encoder for Linux.
///
/// Uses the `cros-codecs` crate to interface with the VA-API backend.
/// Accepts DMA-BUF GPU frames for zero-copy encode, or falls back to
/// CPU NV12 conversion for RGBA/BGRA/I420 input.
#[derive(derive_more::Debug)]
pub struct VaapiEncoder {
    #[debug(skip)]
    encoder: VaapiH264Encoder,
    frame_layout: FrameLayout,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// avcC description, populated after first keyframe (avcC mode only).
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: std::collections::VecDeque<EncodedFrame>,
    /// Whether the frame input path has been logged (log once on first frame).
    logged_frame_path: bool,
}

impl VaapiEncoder {
    /// Create a VAAPI H.264 encoder instance with fresh state (counter=0).
    fn create_encoder(
        width: u32,
        height: u32,
        framerate: u32,
        bitrate: u64,
    ) -> Result<(VaapiH264Encoder, bool)> {
        let display =
            Display::open().context("failed to open VAAPI display — no GPU or driver found")?;

        let coded_size = Resolution { width, height };
        let fourcc = Fourcc::from(b"NV12");

        let entrypoints = display
            .query_config_entrypoints(VAProfile::VAProfileH264ConstrainedBaseline)
            .unwrap_or_default();
        let low_power = entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP);

        let config = EncoderConfig {
            resolution: coded_size,
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(bitrate),
                framerate,
                // Constrain QP range to prevent the hardware rate controller from
                // producing large quality swings between IDR and P-frames.
                // Default range (1–51) lets the RC spike QP on keyframes, causing
                // a visible "compression burst" every keyframe interval.
                min_quality: 18,
                max_quality: 36,
            },
            pred_structure: PredictionStructure::LowDelay {
                limit: framerate as u16,
            },
            ..EncoderConfig::default()
        };

        let encoder = VaapiH264Encoder::new_vaapi(
            display,
            config,
            fourcc,
            coded_size,
            low_power,
            BlockingMode::Blocking,
        )
        .map_err(|e| anyhow::anyhow!("failed to create VAAPI H.264 encoder: {e:?}"))?;

        Ok((encoder, low_power))
    }

    /// Bits-per-pixel factor for H.264 default bitrate calculation.
    const H264_BPP: f32 = 0.07;

    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
        let nal_format = config.nal_format;

        let coded_size = Resolution { width, height };
        let fourcc = Fourcc::from(b"NV12");

        let frame_layout = FrameLayout {
            format: (fourcc, 0),
            size: coded_size,
            planes: vec![
                PlaneLayout {
                    buffer_index: 0,
                    offset: 0,
                    stride: width as usize,
                },
                PlaneLayout {
                    buffer_index: 0,
                    offset: (width * height) as usize,
                    stride: width as usize,
                },
            ],
        };

        let avcc = if nal_format == NalFormat::Avcc {
            // Extract avcC by priming a temporary encoder with a black IDR frame.
            // cros-codecs only emits SPS/PPS in the first IDR (counter=0).
            let (mut primer, _) = Self::create_encoder(width, height, framerate, bitrate)?;
            let yuv = YuvData::black(width, height);
            let nv12 = Self::i420_to_nv12(&yuv.y, &yuv.u, &yuv.v, width, height);
            let black = VaapiInputFrame::Nv12 {
                data: nv12,
                width,
                height,
            };
            let meta = FrameMetadata {
                timestamp: 0,
                layout: frame_layout.clone(),
                force_keyframe: true,
            };
            primer
                .encode(meta, black)
                .map_err(|e| anyhow::anyhow!("VAAPI priming encode failed: {e:?}"))?;

            let mut avcc = None;
            while let Some(coded) = primer
                .poll()
                .map_err(|e| anyhow::anyhow!("VAAPI priming poll failed: {e:?}"))?
            {
                if avcc.is_none() {
                    let nals = parse_annex_b(&coded.bitstream);
                    if let Some((sps, pps)) = extract_sps_pps(&nals) {
                        avcc = Some(build_avcc(&sps, &pps));
                    }
                }
            }
            avcc
        } else {
            // Annex B mode: SPS/PPS are inline in keyframes, no priming needed.
            None
        };

        // Create a fresh encoder for actual use.
        let (encoder, _) = Self::create_encoder(width, height, framerate, bitrate)?;

        tracing::info!(
            width,
            height,
            framerate,
            bitrate,
            "H.264 hardware encoder ready (VAAPI)"
        );

        Ok(Self {
            encoder,
            frame_layout,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            avcc,
            packet_buf: std::collections::VecDeque::new(),
            logged_frame_path: false,
        })
    }

    /// Convert I420 planar YUV to NV12 semi-planar format.
    /// NV12 = Y plane followed by interleaved UV plane.
    /// Converts a `VideoFrame` to an NV12 `VaapiInputFrame` via CPU path
    /// (scale + color convert).
    fn build_nv12_input(&mut self, frame: VideoFrame) -> Result<VaapiInputFrame> {
        let frame = self.scale_if_needed(frame)?;
        let [w, h] = frame.dimensions;
        let nv12_data = match &frame.data {
            crate::format::FrameData::Packed { pixel_format, data } => {
                pixel_format_to_nv12(data, w, h, *pixel_format)?
            }
            _ => {
                let img = frame.rgba_image();
                pixel_format_to_nv12(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
            }
        };
        Ok(VaapiInputFrame::Nv12 {
            data: nv12_data.into_contiguous(),
            width: w,
            height: h,
        })
    }

    /// Scales the frame to encoder dimensions if needed, based on the
    /// configured [`ScaleMode`].
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

    fn i420_to_nv12(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);

        let y_size = w * h;
        let uv_size = chroma_w * 2 * chroma_h;
        let mut nv12 = vec![0u8; y_size + uv_size];

        // Copy Y plane
        nv12[..y_size.min(y.len())].copy_from_slice(&y[..y_size.min(y.len())]);

        // Interleave U and V into UV plane
        let uv_dst = &mut nv12[y_size..];
        for row in 0..chroma_h {
            let row_offset = row * chroma_w;
            let dst_offset = row * chroma_w * 2;
            for col in 0..chroma_w {
                let idx = row_offset + col;
                let out = dst_offset + col * 2;
                uv_dst[out] = u[idx];
                uv_dst[out + 1] = v[idx];
            }
        }

        nv12
    }

    /// Process a `CodedBitstreamBuffer` into an `EncodedFrame`.
    fn process_coded_output(
        &mut self,
        coded: CodedBitstreamBuffer,
    ) -> Result<Option<EncodedFrame>> {
        // Strip trailing zero-padding from the VA coded buffer. The VAAPI driver
        // pads coded buffers to alignment boundaries, but the H.264 spec guarantees
        // RBSP trailing bits end with a non-zero byte, so trailing zeros are safe to
        // remove. Without this, parse_annex_b includes the padding in the last NAL,
        // causing the decoder to choke on the zero bytes after consuming the real data.
        let annex_b_end = coded
            .bitstream
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |p| p + 1);
        let annex_b = &coded.bitstream[..annex_b_end];
        if annex_b.is_empty() {
            return Ok(None);
        }

        // Detect keyframe by scanning NAL types for IDR (type 5).
        let nals = parse_annex_b(annex_b);
        let is_keyframe = nals
            .iter()
            .any(|nal| !nal.is_empty() && (nal[0] & 0x1F) == 5);

        // In avcC mode, extract SPS/PPS on first keyframe.
        if self.nal_format == NalFormat::Avcc
            && is_keyframe
            && self.avcc.is_none()
            && let Some((sps, pps)) = extract_sps_pps(&nals)
        {
            self.avcc = Some(build_avcc(&sps, &pps));
        }

        let payload: bytes::Bytes = match self.nal_format {
            NalFormat::AnnexB => annex_b.to_vec().into(),
            NalFormat::Avcc => annex_b_to_length_prefixed(annex_b).into(),
        };

        let timestamp_us = coded.metadata.timestamp;

        Ok(Some(EncodedFrame {
            is_keyframe,
            timestamp: std::time::Duration::from_micros(timestamp_us),
            payload,
        }))
    }
}

impl VideoEncoderFactory for VaapiEncoder {
    const ID: &str = "h264-vaapi";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
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

impl VideoEncoder for VaapiEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
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
        // Check for zero-copy DMA-BUF path before any CPU-side scaling.
        let input_frame = if let Some(NativeFrameHandle::DmaBuf(info)) = frame.native_handle() {
            if info.coded_width == self.width
                && info.coded_height == self.height
                && info.planes.len() <= 4
            {
                if !self.logged_frame_path {
                    tracing::info!("VAAPI encode: zero-copy DMA-BUF import (no CPU copy)");
                    self.logged_frame_path = true;
                }
                VaapiInputFrame::DmaBuf(info)
            } else {
                if !self.logged_frame_path {
                    tracing::info!(
                        src_w = info.coded_width,
                        src_h = info.coded_height,
                        enc_w = self.width,
                        enc_h = self.height,
                        "VAAPI encode: DMA-BUF dimension mismatch, using CPU NV12 upload"
                    );
                    self.logged_frame_path = true;
                }
                self.build_nv12_input(frame)?
            }
        } else {
            if !self.logged_frame_path {
                tracing::info!("VAAPI encode: CPU NV12 upload (no DMA-BUF available)");
                self.logged_frame_path = true;
            }
            self.build_nv12_input(frame)?
        };

        let w = input_frame.width();
        let h = input_frame.height();

        // Build frame metadata with layout matching the input frame.
        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        let layout = match &input_frame {
            VaapiInputFrame::DmaBuf(info) => {
                // Only NV12 DMA-BUFs are accepted (checked at import site).
                debug_assert_eq!(
                    info.drm_format,
                    u32::from_le_bytes(*b"NV12"),
                    "VAAPI encoder only accepts NV12 DMA-BUFs"
                );
                FrameLayout {
                    format: (Fourcc::from(b"NV12"), info.modifier),
                    size: Resolution {
                        width: w,
                        height: h,
                    },
                    planes: info
                        .planes
                        .iter()
                        .map(|p| PlaneLayout {
                            buffer_index: 0,
                            offset: p.offset as usize,
                            stride: p.pitch as usize,
                        })
                        .collect(),
                }
            }
            VaapiInputFrame::Nv12 { .. } if w == self.width && h == self.height => {
                self.frame_layout.clone()
            }
            VaapiInputFrame::Nv12 { .. } => FrameLayout {
                format: self.frame_layout.format,
                size: Resolution {
                    width: w,
                    height: h,
                },
                planes: vec![
                    PlaneLayout {
                        buffer_index: 0,
                        offset: 0,
                        stride: w as usize,
                    },
                    PlaneLayout {
                        buffer_index: 0,
                        offset: (w * h) as usize,
                        stride: w as usize,
                    },
                ],
            },
        };
        let meta = FrameMetadata {
            timestamp: timestamp_us,
            layout,
            force_keyframe: false,
        };

        // Submit frame to encoder.
        self.encoder
            .encode(meta, input_frame)
            .map_err(|e| anyhow::anyhow!("VAAPI encode failed: {e:?}"))?;

        self.frame_count += 1;

        // Poll for any completed output.
        while let Some(coded) = self
            .encoder
            .poll()
            .map_err(|e| anyhow::anyhow!("VAAPI poll failed: {e:?}"))?
        {
            if let Some(pkt) = self.process_coded_output(coded)? {
                self.packet_buf.push_back(pkt);
            }
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        Ok(self.packet_buf.pop_front())
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        // Drain remaining buffered frames.
        if let Err(e) = self.encoder.drain() {
            tracing::warn!("VAAPI encoder drain failed on drop: {e}");
            return;
        }
        while let Ok(Some(coded)) = self.encoder.poll() {
            if let Ok(Some(pkt)) = self.process_coded_output(coded) {
                self.packet_buf.push_back(pkt);
            }
        }
    }
}

// VaapiEncoder is Send: the libva Display and surfaces use Rc internally,
// but our encoder is only accessed from a single thread at a time.
// The cros-codecs StatelessEncoder is designed for single-threaded use.
// Safety: We ensure no concurrent access to the encoder.
unsafe impl Send for VaapiEncoder {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame,
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
    };

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_avcc_available_at_construction() {
        let enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let desc = enc.config().description;
        assert!(
            desc.is_some(),
            "avcC should be populated at construction time"
        );
        let avcc = desc.unwrap();
        assert_eq!(avcc[0], 1, "avcC should start with version 1");
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_basic() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut packet_count = 0;
        for _ in 0..30 {
            let frame = make_rgba_frame(640, 360, 255, 0, 0);
            enc.push_frame(frame).unwrap();
            while let Some(_pkt) = enc.pop_packet().unwrap() {
                packet_count += 1;
            }
        }
        assert!(
            packet_count > 0,
            "expected at least 1 packet, got {packet_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_decode_roundtrip() {
        use crate::{codec::h264::H264VideoDecoder, format::DecodeConfig, traits::VideoDecoder};

        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut packets = Vec::new();
        for _ in 0..30 {
            let frame = make_rgba_frame(640, 360, 200, 100, 50);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        assert!(!packets.is_empty(), "should have produced packets");

        let config = enc.config();
        assert!(
            config.description.is_some(),
            "avcC should be populated after encoding"
        );

        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();
        let mut decoded_count = 0;
        let ordered = crate::codec::test_util::encoded_frames_to_media_packets(packets);
        for pkt in ordered {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                assert_eq!(frame.img().width(), 640);
                assert_eq!(frame.img().height(), 360);
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encode_keyframe_interval() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let mut keyframe_count = 0;
        for _ in 0..60 {
            let frame = make_rgba_frame(640, 360, 128, 128, 128);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                if pkt.is_keyframe {
                    keyframe_count += 1;
                }
            }
        }
        assert!(
            keyframe_count >= 2,
            "expected >= 2 keyframes in 60 frames, got {keyframe_count}"
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_timestamps_increase() {
        let mut enc = VaapiEncoder::with_preset(VideoPreset::P180).unwrap();
        let mut prev_ts = None;
        for _ in 0..10 {
            let frame = make_rgba_frame(320, 180, 64, 64, 64);
            enc.push_frame(frame).unwrap();
            if let Some(pkt) = enc.pop_packet().unwrap() {
                if let Some(prev) = prev_ts {
                    assert!(pkt.timestamp > prev, "timestamps should increase");
                }
                prev_ts = Some(pkt.timestamp);
            }
        }
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_config_fields() {
        let enc = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::H264(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
        assert_eq!(config.optimize_for_latency, Some(true));
    }
}
