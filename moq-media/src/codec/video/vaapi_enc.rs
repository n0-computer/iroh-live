use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use cros_codecs::backend::vaapi::encoder::VaapiBackend;
use cros_codecs::encoder::h264::EncoderConfig;
use cros_codecs::encoder::stateless::h264::StatelessEncoder as H264StatelessEncoder;
use cros_codecs::encoder::{
    CodedBitstreamBuffer, FrameMetadata, RateControl, Tunings, VideoEncoder as CrosVideoEncoder,
};
use cros_codecs::libva::{
    Display, Image, Surface, UsageHint, VA_FOURCC_NV12, VA_RT_FORMAT_YUV420, VAEntrypoint,
    VAProfile,
};
use cros_codecs::video_frame::{ReadMapping, VideoFrame as CrosVideoFrame, WriteMapping};
use cros_codecs::{BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution};
use hang::{
    Timestamp,
    catalog::{H264, VideoCodec, VideoConfig},
};

use crate::{
    av::{self, VideoPreset},
    codec::video::util::{
        annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b},
        convert::pixel_format_to_yuv420,
    },
};

/// An NV12 frame that implements the `cros_codecs::video_frame::VideoFrame` trait.
///
/// This allows us to use the cros-codecs stateless encoder API, which requires
/// input frames to implement `VideoFrame` so they can be imported to VA surfaces.
#[derive(Debug, Clone)]
struct Nv12Frame {
    data: Vec<u8>,
    width: u32,
    height: u32,
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
        // The encoder imports frames via `to_native_handle`, not by writing through mappings.
        vec![]
    }
}

impl CrosVideoFrame for Nv12Frame {
    type MemDescriptor = ();
    type NativeHandle = Surface<()>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        Resolution {
            width: self.width,
            height: self.height,
        }
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let y_size = (self.width * self.height) as usize;
        let uv_size = (self.width * self.height.div_ceil(2)) as usize;
        vec![y_size, uv_size]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        vec![self.width as usize, self.width as usize]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        let y_size = (self.width * self.height) as usize;
        Ok(Box::new(Nv12ReadMapping {
            y_plane: &self.data[..y_size],
            uv_plane: &self.data[y_size..],
        }))
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Ok(Box::new(Nv12WriteMapping))
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        // Create a VA surface and upload NV12 data to it.
        let mut surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(VA_FOURCC_NV12),
                self.width,
                self.height,
                Some(UsageHint::USAGE_HINT_ENCODER),
                vec![()],
            )
            .map_err(|e| format!("failed to create VA surface: {e:?}"))?;

        let surface = surfaces.pop().ok_or("no surface created")?;

        // Upload NV12 data via image mapping.
        upload_nv12_to_surface(display, &surface, &self.data, self.width, self.height)
            .map_err(|e| format!("failed to upload NV12 data: {e:?}"))?;

        Ok(surface)
    }
}

/// Upload raw NV12 data to a VA surface using image mapping.
fn upload_nv12_to_surface(
    display: &Rc<Display>,
    surface: &Surface<()>,
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
/// Handle = Nv12Frame, Backend = VaapiBackend<(), Surface<()>>.
type VaapiH264Encoder = H264StatelessEncoder<Nv12Frame, VaapiBackend<(), Surface<()>>>;

/// VAAPI hardware-accelerated H.264 encoder for Linux.
///
/// Uses the `cros-codecs` crate to interface with the VA-API backend.
/// The encoder accepts RGBA/BGRA frames, converts to NV12, wraps in
/// an `Nv12Frame` (implementing `VideoFrame`), and submits to the
/// stateless H.264 encoder.
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
    /// avcC description, populated after first keyframe.
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: Vec<hang::Frame>,
}

impl VaapiEncoder {
    fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        let pixels = width * height;
        let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
        let bitrate = (pixels as f32 * 0.07 * framerate_factor).round() as u64;

        // Open the VAAPI display (probes /dev/dri/renderD128 etc.)
        let display =
            Display::open().context("failed to open VAAPI display — no GPU or driver found")?;

        let coded_size = Resolution { width, height };
        let fourcc = Fourcc::from(b"NV12");

        // Check for low-power encoding support (fixed-function, some Intel GPUs).
        let entrypoints = display
            .query_config_entrypoints(VAProfile::VAProfileH264ConstrainedBaseline)
            .unwrap_or_default();
        let low_power = entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP);

        let config = EncoderConfig {
            resolution: coded_size,
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(bitrate),
                framerate,
                ..Tunings::default()
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

        Ok(Self {
            encoder,
            frame_layout,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            avcc: None,
            packet_buf: Vec::new(),
        })
    }

    /// Convert I420 planar YUV to NV12 semi-planar format.
    /// NV12 = Y plane followed by interleaved UV plane.
    fn i420_to_nv12(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);

        let y_size = w * h;
        let uv_size = chroma_w * 2 * chroma_h;
        let mut nv12 = Vec::with_capacity(y_size + uv_size);

        // Copy Y plane
        nv12.extend_from_slice(&y[..y_size.min(y.len())]);

        // Interleave U and V into UV plane
        for row in 0..chroma_h {
            for col in 0..chroma_w {
                let idx = row * chroma_w + col;
                nv12.push(u.get(idx).copied().unwrap_or(128));
                nv12.push(v.get(idx).copied().unwrap_or(128));
            }
        }

        nv12
    }

    /// Process a `CodedBitstreamBuffer` into a `hang::Frame`.
    fn process_coded_output(&mut self, coded: CodedBitstreamBuffer) -> Result<Option<hang::Frame>> {
        let annex_b = &coded.bitstream;
        if annex_b.is_empty() {
            return Ok(None);
        }

        // Detect keyframe by scanning NAL types for IDR (type 5).
        let nals = parse_annex_b(annex_b);
        let keyframe = nals
            .iter()
            .any(|nal| !nal.is_empty() && (nal[0] & 0x1F) == 5);

        // On first keyframe, extract SPS/PPS and build avcC.
        if keyframe
            && self.avcc.is_none()
            && let Some((sps, pps)) = extract_sps_pps(&nals)
        {
            self.avcc = Some(build_avcc(&sps, &pps));
        }

        // Convert Annex B to length-prefixed NALs for transport.
        let payload = annex_b_to_length_prefixed(annex_b);

        let timestamp_us = coded.metadata.timestamp;
        let timestamp = Timestamp::from_micros(timestamp_us)?;

        Ok(Some(hang::Frame {
            payload: payload.into(),
            timestamp,
            keyframe,
        }))
    }
}

impl av::VideoEncoder for VaapiEncoder {
    fn with_preset(preset: VideoPreset) -> Result<Self> {
        Self::new(preset.width(), preset.height(), preset.fps())
    }
}

impl av::VideoEncoderInner for VaapiEncoder {
    fn name(&self) -> &str {
        "h264-vaapi"
    }

    fn config(&self) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
                inline: false,
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

    fn push_frame(&mut self, frame: av::VideoFrame) -> Result<()> {
        let [w, h] = frame.format.dimensions;
        let yuv = pixel_format_to_yuv420(&frame.raw, w, h, frame.format.pixel_format)?;

        // Convert I420 to NV12 (VAAPI expects NV12).
        let nv12 = Self::i420_to_nv12(&yuv.y, &yuv.u, &yuv.v, w, h);

        // Wrap NV12 data in our VideoFrame impl.
        let nv12_frame = Nv12Frame {
            data: nv12,
            width: w,
            height: h,
        };

        // Build frame metadata.
        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        let meta = FrameMetadata {
            timestamp: timestamp_us,
            layout: self.frame_layout.clone(),
            force_keyframe: self.frame_count == 0,
        };

        // Submit frame to encoder.
        self.encoder
            .encode(meta, nv12_frame)
            .map_err(|e| anyhow::anyhow!("VAAPI encode failed: {e:?}"))?;

        self.frame_count += 1;

        // Poll for any completed output.
        while let Some(coded) = self
            .encoder
            .poll()
            .map_err(|e| anyhow::anyhow!("VAAPI poll failed: {e:?}"))?
        {
            if let Some(pkt) = self.process_coded_output(coded)? {
                self.packet_buf.push(pkt);
            }
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        Ok(if self.packet_buf.is_empty() {
            None
        } else {
            Some(self.packet_buf.remove(0))
        })
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        // Drain remaining buffered frames.
        if self.encoder.drain().is_ok() {
            while let Ok(Some(coded)) = self.encoder.poll() {
                if let Ok(Some(pkt)) = self.process_coded_output(coded) {
                    self.packet_buf.push(pkt);
                }
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
    use crate::av::{
        PixelFormat, VideoEncoder, VideoEncoderInner, VideoFormat, VideoFrame, VideoPreset,
    };

    fn make_rgba_frame(w: u32, h: u32, r: u8, g: u8, b: u8) -> VideoFrame {
        let pixel = [r, g, b, 255u8];
        let raw: Vec<u8> = pixel.repeat((w * h) as usize);
        VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [w, h],
            },
            raw: raw.into(),
        }
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
        use crate::av::{DecodeConfig, VideoDecoder};
        use crate::codec::video::H264VideoDecoder;

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
        for pkt in packets {
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
                if pkt.keyframe {
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
