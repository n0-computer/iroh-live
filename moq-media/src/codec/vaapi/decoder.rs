use std::collections::VecDeque;
use std::fs::File;

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use cros_codecs::backend::vaapi::decoder::VaapiBackend;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::libva::{Display, UsageHint, VA_FOURCC_NV12, VA_RT_FORMAT_YUV420};
use cros_codecs::video_frame::VideoFrame as CrosVideoFrame;
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::{BlockingMode, DecodedFormat, Fourcc, FrameLayout, PlaneLayout, Resolution};
use hang::catalog::{VideoCodec, VideoConfig};

use crate::codec::h264::annexb::{avcc_to_annex_b, length_prefixed_to_annex_b};
use crate::format::{
    CpuFrame, DecodeConfig, DecodedVideoFrame, DmaBufInfo, DmaBufPlaneInfo, GpuFrame,
    GpuFrameInner, GpuPixelFormat, MediaPacket, Nv12Planes, PixelFormat,
};
use crate::processing::convert::nv12_to_rgba_data;
use crate::traits::VideoDecoder;

/// Patch SPS NALs in Annex B data to set `constraint_set0_flag` for Baseline profile.
///
/// cros-codecs requires `constraint_set0_flag=1` for profile_idc=66 (Baseline) to map
/// to `VAProfileH264ConstrainedBaseline`. OpenH264 and some encoders produce Baseline
/// SPS without this flag, causing "Could not get VAProfile!" errors. Setting this flag
/// is safe: Constrained Baseline is a strict subset of Baseline, so any valid Baseline
/// stream is also valid Constrained Baseline.
fn patch_baseline_constraint_flag(annex_b: &mut [u8]) {
    let mut i = 0;
    while i < annex_b.len() {
        let sc_len = if i + 3 <= annex_b.len() && annex_b[i] == 0 && annex_b[i + 1] == 0 {
            if annex_b[i + 2] == 1 {
                3
            } else if i + 4 <= annex_b.len() && annex_b[i + 2] == 0 && annex_b[i + 3] == 1 {
                4
            } else {
                i += 1;
                continue;
            }
        } else {
            i += 1;
            continue;
        };

        let nal_start = i + sc_len;
        if nal_start + 3 > annex_b.len() {
            break;
        }

        let nal_type = annex_b[nal_start] & 0x1F;
        if nal_type == 7 {
            let profile_idc = annex_b[nal_start + 1];
            if profile_idc == 66 {
                annex_b[nal_start + 2] |= 0x80;
            }
        }

        i = nal_start + 1;
    }
}

type VaapiFrame = PooledVideoFrame<GenericDmaVideoFrame>;
type VaapiH264Decoder = StatelessDecoder<H264, VaapiBackend<VaapiFrame>>;

/// GPU-resident frame backed by DMA-BUF file descriptors from VAAPI decoding.
#[derive(Debug)]
struct VaapiGpuFrame {
    frame: Arc<VaapiFrame>,
    width: u32,
    height: u32,
    dma_info: Option<DmaBufInfo>,
}

// PooledVideoFrame<GenericDmaVideoFrame> holds owned File descriptors.
// Safety: only accessed from single thread, frame data is Send.
unsafe impl Send for VaapiGpuFrame {}
unsafe impl Sync for VaapiGpuFrame {}

impl VaapiGpuFrame {
    /// Map the frame's NV12 planes via vaDeriveImage.
    ///
    /// This uses the VAAPI driver's own mapping, which handles tiled formats
    /// (including Intel 4-tile) correctly — unlike GenericDmaVideoFrame::map()
    /// which only supports linear and Y-tile.
    fn derive_nv12_planes(&self) -> Result<Nv12Planes> {
        let display = Display::open().context("failed to open VAAPI display for mapping")?;
        let surface = self
            .frame
            .to_native_handle(&display)
            .map_err(|e| anyhow::anyhow!("failed to re-import frame as VA surface: {e}"))?;

        surface
            .sync()
            .map_err(|e| anyhow::anyhow!("VA surface sync failed: {e}"))?;

        let visible_rect = (self.width, self.height);
        let image = cros_codecs::libva::Image::derive_from(&surface, visible_rect)
            .map_err(|e| anyhow::anyhow!("vaDeriveImage failed: {e}"))?;

        let va_image = image.image();
        let data = image.as_ref();
        let w = self.width;
        let h = self.height;

        // Y plane
        let y_offset = va_image.offsets[0] as usize;
        let y_pitch = va_image.pitches[0];
        let y_data: Vec<u8> = (0..h as usize)
            .flat_map(|row| {
                let start = y_offset + row * y_pitch as usize;
                data[start..start + w as usize].iter().copied()
            })
            .collect();

        // UV plane (interleaved, half height)
        let uv_offset = va_image.offsets[1] as usize;
        let uv_pitch = va_image.pitches[1];
        let uv_w = w; // UV row is w bytes (w/2 pairs of U,V)
        let uv_h = h.div_ceil(2);
        let uv_data: Vec<u8> = (0..uv_h as usize)
            .flat_map(|row| {
                let start = uv_offset + row * uv_pitch as usize;
                data[start..start + uv_w as usize].iter().copied()
            })
            .collect();

        Ok(Nv12Planes {
            y_data,
            y_stride: w,
            uv_data,
            uv_stride: uv_w,
            width: w,
            height: h,
        })
    }
}

impl GpuFrameInner for VaapiGpuFrame {
    fn download(&self) -> Result<CpuFrame> {
        let planes = self.derive_nv12_planes()?;
        let rgba = nv12_to_rgba_data(
            &planes.y_data,
            planes.y_stride,
            &planes.uv_data,
            planes.uv_stride,
            planes.width,
            planes.height,
        )?;

        let image = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .context("RGBA data size does not match dimensions")?;
        Ok(CpuFrame {
            image,
            pixel_format: PixelFormat::Rgba,
        })
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat {
        GpuPixelFormat::Nv12
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn download_nv12(&self) -> Option<Result<Nv12Planes>> {
        Some(self.derive_nv12_planes())
    }

    fn dma_buf_info(&self) -> Option<&DmaBufInfo> {
        self.dma_info.as_ref()
    }
}

/// Extract DMA-BUF metadata from a decoded frame by re-importing it as a VA Surface
/// and exporting PRIME descriptors. This gives us fresh FDs and full plane layout info.
fn extract_dma_buf_info(
    display: &Rc<Display>,
    frame: &VaapiFrame,
    display_w: u32,
    display_h: u32,
) -> Option<DmaBufInfo> {
    let surface = match frame.to_native_handle(display) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to re-import frame as VA surface: {e}");
            return None;
        }
    };
    let desc = match surface.export_prime() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to export PRIME descriptor: {e}");
            return None;
        }
    };

    if desc.layers.is_empty() || desc.objects.is_empty() {
        tracing::warn!("PRIME descriptor has no layers or objects");
        return None;
    }

    let layer = &desc.layers[0];
    let object = &desc.objects[0];
    let coded_res = frame.resolution();

    let fd = object.fd.try_clone().ok()?;

    Some(DmaBufInfo {
        fd,
        modifier: object.drm_format_modifier,
        drm_format: desc.fourcc,
        coded_width: coded_res.width,
        coded_height: coded_res.height,
        display_width: display_w,
        display_height: display_h,
        planes: (0..layer.num_planes as usize)
            .map(|i| DmaBufPlaneInfo {
                offset: layer.offset[i],
                pitch: layer.pitch[i],
            })
            .collect(),
    })
}

/// Allocate a `GenericDmaVideoFrame` by creating a VA surface and exporting
/// its DMA-BUF file descriptors. This avoids GBM, which fails on some GPUs
/// (e.g. Intel Arc) with `GBM_BO_USE_HW_VIDEO_DECODER`.
///
/// Opens a temporary VA Display connection for surface creation. The exported
/// DMA-BUF FDs are GPU-global and survive after the Display is dropped.
fn alloc_va_dma_frame(stream_info: &StreamInfo) -> GenericDmaVideoFrame {
    let w = stream_info.coded_resolution.width;
    let h = stream_info.coded_resolution.height;

    let display = Display::open().expect("failed to open VAAPI display for frame allocation");

    let mut surfaces = display
        .create_surfaces(
            VA_RT_FORMAT_YUV420,
            Some(VA_FOURCC_NV12),
            w,
            h,
            Some(UsageHint::USAGE_HINT_DECODER),
            vec![()],
        )
        .expect("failed to create VA surface for decoder frame pool");

    let surface = surfaces.pop().expect("no VA surface created");

    let desc = surface
        .export_prime()
        .expect("failed to export VA surface to DMA-BUF");

    let layer = &desc.layers[0];
    let modifier = desc.objects[0].drm_format_modifier;

    let fds: Vec<File> = desc.objects.into_iter().map(|o| File::from(o.fd)).collect();

    let layout = FrameLayout {
        format: (Fourcc::from(desc.fourcc), modifier),
        size: Resolution {
            width: w,
            height: h,
        },
        planes: (0..layer.num_planes as usize)
            .map(|i| PlaneLayout {
                buffer_index: layer.object_index[i] as usize,
                offset: layer.offset[i] as usize,
                stride: layer.pitch[i] as usize,
            })
            .collect(),
    };

    GenericDmaVideoFrame::new(fds, layout).expect("failed to create DMA-BUF frame from VA surface")
}

/// VAAPI hardware-accelerated H.264 decoder for Linux.
#[derive(derive_more::Debug)]
pub struct VaapiDecoder {
    #[debug(skip)]
    decoder: VaapiH264Decoder,
    #[debug(skip)]
    framepool: Arc<Mutex<FramePool<GenericDmaVideoFrame>>>,
    #[debug(skip)]
    display: Rc<Display>,
    pending_frames: VecDeque<DecodedVideoFrame>,
    last_timestamp: Option<Duration>,
    timestamp_counter: u64,
}

// VaapiH264Decoder uses Rc<Display> internally (not Send).
// Safety: The decoder is only used on a single OS thread (WatchTrack decode thread).
unsafe impl Send for VaapiDecoder {}

impl VaapiDecoder {
    fn drain_events(&mut self) {
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FrameReady(handle) => {
                    if let Err(e) = handle.sync() {
                        tracing::warn!("VAAPI frame sync failed: {e:#}");
                        continue;
                    }
                    let display_res = handle.display_resolution();
                    let w = display_res.width;
                    let h = display_res.height;
                    let frame_arc = handle.video_frame();

                    let dma_info = extract_dma_buf_info(&self.display, &frame_arc, w, h);

                    let gpu_frame = VaapiGpuFrame {
                        frame: frame_arc,
                        width: w,
                        height: h,
                        dma_info,
                    };

                    let timestamp = self.last_timestamp.unwrap_or_default();

                    let decoded =
                        DecodedVideoFrame::new_gpu(GpuFrame::new(Arc::new(gpu_frame)), timestamp);
                    self.pending_frames.push_back(decoded);
                }
                DecoderEvent::FormatChanged => {
                    if let Some(stream_info) = self.decoder.stream_info() {
                        tracing::info!(
                            "VAAPI decoder: format changed to {}x{} (coded {}x{})",
                            stream_info.display_resolution.width,
                            stream_info.display_resolution.height,
                            stream_info.coded_resolution.width,
                            stream_info.coded_resolution.height,
                        );
                        self.framepool.lock().unwrap().resize(stream_info);
                    }
                }
            }
        }
    }
}

impl VideoDecoder for VaapiDecoder {
    fn name(&self) -> &str {
        "h264-vaapi"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        if !matches!(&config.codec, VideoCodec::H264(_)) {
            bail!("VaapiDecoder only supports H.264, got {}", config.codec);
        }

        let display =
            Display::open().context("failed to open VAAPI display — no GPU or driver found")?;

        let decoder = VaapiH264Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|e| anyhow::anyhow!("failed to create VAAPI H.264 decoder: {e:?}"))?;

        // Keep a second Display for re-exporting frames as PRIME descriptors.
        let export_display =
            Display::open().context("failed to open second VAAPI display for frame export")?;

        // Verify VA surface allocation + DMA-BUF export works on this GPU.
        {
            let test_info = StreamInfo {
                format: DecodedFormat::NV12,
                coded_resolution: Resolution {
                    width: 320,
                    height: 192,
                },
                display_resolution: Resolution {
                    width: 320,
                    height: 192,
                },
                min_num_frames: 1,
            };
            let _test_frame = alloc_va_dma_frame(&test_info);
        }

        let framepool = Arc::new(Mutex::new(FramePool::new(
            move |stream_info: &StreamInfo| alloc_va_dma_frame(stream_info),
        )));

        let mut this = Self {
            decoder,
            framepool,
            display: export_display,
            pending_frames: VecDeque::new(),
            last_timestamp: None,
            timestamp_counter: 0,
        };

        // Feed SPS/PPS from avcC description if available.
        if let Some(description) = &config.description
            && let Some(mut annex_b) = avcc_to_annex_b(description)
        {
            patch_baseline_constraint_flag(&mut annex_b);
            let ts = 0u64;
            let pool = this.framepool.clone();
            let _ = this
                .decoder
                .decode(ts, &annex_b, &mut || pool.lock().unwrap().alloc());
            this.drain_events();
        }

        Ok(this)
    }

    fn set_viewport(&mut self, _w: u32, _h: u32) {
        // VAAPI decodes at full resolution; scaling happens in the renderer.
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
        let mut annex_b = length_prefixed_to_annex_b(&payload);
        patch_baseline_constraint_flag(&mut annex_b);

        self.last_timestamp = Some(packet.timestamp);
        self.timestamp_counter += 1;
        let ts = self.timestamp_counter;

        let pool = self.framepool.clone();
        let mut alloc = || pool.lock().unwrap().alloc();

        let mut remaining = &annex_b[..];
        loop {
            if remaining.is_empty() {
                break;
            }
            match self.decoder.decode(ts, remaining, &mut alloc) {
                Ok(bytes_consumed) => {
                    if bytes_consumed == 0 {
                        break;
                    }
                    remaining = &remaining[bytes_consumed..];
                }
                Err(DecodeError::CheckEvents) => {
                    self.drain_events();
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    self.drain_events();
                }
                Err(e) => {
                    bail!("VAAPI decode error: {e:?}");
                }
            }
        }

        self.drain_events();

        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>> {
        Ok(self.pending_frames.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use hang::catalog::H264;

    use super::*;

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_decoder_creation() {
        let config = VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline: false,
            }),
            description: None,
            coded_width: Some(320),
            coded_height: Some(180),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: None,
            optimize_for_latency: None,
            container: Default::default(),
            jitter: None,
        };
        let decode_config = DecodeConfig::default();
        let dec = VaapiDecoder::new(&config, &decode_config);
        assert!(dec.is_ok(), "VAAPI decoder creation failed: {dec:?}");
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encoder_decoder_roundtrip() {
        use crate::codec::h264::annexb::parse_annex_b;
        use crate::codec::vaapi::encoder::VaapiEncoder;
        use crate::format::{PixelFormat, VideoFormat, VideoFrame, VideoPreset};
        use crate::traits::{VideoEncoder, VideoEncoderFactory};

        let mut encoder = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = encoder.config();

        let decode_config = DecodeConfig::default();
        let mut decoder = VaapiDecoder::new(&config, &decode_config).unwrap();

        let w = 640u32;
        let h = 360u32;
        let rgba = vec![128u8; (w * h * 4) as usize];
        let frame = VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [w, h],
            },
            raw: rgba.into(),
        };

        let mut total_decoded = 0;
        let mut all_packets = Vec::new();

        for _ in 0..35 {
            encoder.push_frame(frame.clone()).unwrap();
            while let Some(pkt) = encoder.pop_packet().unwrap() {
                all_packets.push(pkt);
            }
        }

        for pkt in all_packets {
            let media_pkt = MediaPacket {
                timestamp: pkt.timestamp,
                payload: pkt.payload.into(),
                is_keyframe: pkt.is_keyframe,
            };
            if decoder.push_packet(media_pkt).is_ok() {
                while let Ok(Some(_)) = decoder.pop_frame() {
                    total_decoded += 1;
                }
            }
        }

        assert!(
            total_decoded >= 30,
            "expected at least 30 decoded frames, got {total_decoded}"
        );
    }

    #[test]
    fn unsupported_codec_errors() {
        use hang::catalog::AV1;
        let config = VideoConfig {
            codec: VideoCodec::AV1(AV1::default()),
            description: None,
            coded_width: Some(320),
            coded_height: Some(180),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: None,
            optimize_for_latency: None,
            container: Default::default(),
            jitter: None,
        };
        let decode_config = DecodeConfig::default();
        let result = VaapiDecoder::new(&config, &decode_config);
        assert!(result.is_err());
    }
}
