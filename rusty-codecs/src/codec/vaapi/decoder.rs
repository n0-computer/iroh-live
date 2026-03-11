use std::{
    collections::VecDeque,
    fs::File,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use cros_codecs::{
    BlockingMode, DecodedFormat, Fourcc, FrameLayout, PlaneLayout, Resolution,
    backend::vaapi::decoder::VaapiBackend,
    decoder::{
        DecodedHandle, DecoderEvent, StreamInfo,
        stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder, h264::H264},
    },
    libva::{Display, UsageHint, VA_FOURCC_NV12, VA_RT_FORMAT_YUV420},
    video_frame::{
        VideoFrame as CrosVideoFrame,
        frame_pool::{FramePool, PooledVideoFrame},
        generic_dma_video_frame::GenericDmaVideoFrame,
    },
};

use crate::{
    codec::h264::annexb::{avcc_to_annex_b, length_prefixed_to_annex_b},
    config::{VideoCodec, VideoConfig},
    format::{
        DecodeConfig, DmaBufInfo, DmaBufPlaneInfo, GpuFrame, GpuFrameInner, GpuPixelFormat,
        MediaPacket, NalFormat, NativeFrameHandle, Nv12Planes, VideoFrame,
    },
    processing::convert::nv12_to_rgba_data,
    traits::VideoDecoder,
};

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
#[derive(derive_more::Debug)]
struct VaapiGpuFrame {
    frame: Arc<VaapiFrame>,
    width: u32,
    height: u32,
    native_handle: Option<NativeFrameHandle>,
    /// Shared VAAPI display for frame mapping (pre-opened at decoder init).
    #[debug(skip)]
    mapping_display: Rc<Display>,
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
        let display = &self.mapping_display;
        let surface = self
            .frame
            .to_native_handle(display)
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

        // Y plane — copy row-by-row stripping pitch padding.
        let y_offset = va_image.offsets[0] as usize;
        let y_pitch = va_image.pitches[0] as usize;
        let w_usize = w as usize;
        let h_usize = h as usize;
        let mut y_data = vec![0u8; w_usize * h_usize];
        for row in 0..h_usize {
            let src = y_offset + row * y_pitch;
            let dst = row * w_usize;
            y_data[dst..dst + w_usize].copy_from_slice(&data[src..src + w_usize]);
        }

        // UV plane (interleaved, half height) — same row-by-row copy.
        let uv_offset = va_image.offsets[1] as usize;
        let uv_pitch = va_image.pitches[1] as usize;
        let uv_w = w_usize; // UV row is w bytes (w/2 pairs of U,V)
        let uv_h = h.div_ceil(2) as usize;
        let mut uv_data = vec![0u8; uv_w * uv_h];
        for row in 0..uv_h {
            let src = uv_offset + row * uv_pitch;
            let dst = row * uv_w;
            uv_data[dst..dst + uv_w].copy_from_slice(&data[src..src + uv_w]);
        }

        Ok(Nv12Planes {
            y_data,
            y_stride: w,
            uv_data,
            uv_stride: w,
            width: w,
            height: h,
        })
    }
}

impl GpuFrameInner for VaapiGpuFrame {
    fn download_rgba(&self) -> Result<image::RgbaImage> {
        let planes = self.derive_nv12_planes()?;
        let rgba = nv12_to_rgba_data(
            &planes.y_data,
            planes.y_stride,
            &planes.uv_data,
            planes.uv_stride,
            planes.width,
            planes.height,
        )?;

        image::RgbaImage::from_raw(self.width, self.height, rgba)
            .context("RGBA data size does not match dimensions")
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

    fn native_handle(&self) -> Option<&NativeFrameHandle> {
        self.native_handle.as_ref()
    }
}

/// Extract DMA-BUF metadata from a decoded frame by re-importing it as a VA Surface
/// and exporting PRIME descriptors. This gives us fresh FDs and full plane layout info.
///
/// Uses `catch_unwind` because cros-codecs' `GenericDmaVideoFrame::clone` panics on
/// FD exhaustion (EMFILE) rather than returning an error.
fn extract_dma_buf_info(
    display: &Rc<Display>,
    frame: &VaapiFrame,
    display_w: u32,
    display_h: u32,
) -> Option<NativeFrameHandle> {
    let surface = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        frame.to_native_handle(display)
    })) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!("failed to re-import frame as VA surface: {e}");
            return None;
        }
        Err(_) => {
            tracing::error!(
                "FD exhaustion (EMFILE) during DMA-BUF frame clone — \
                             skipping zero-copy path for this frame"
            );
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

    Some(NativeFrameHandle::DmaBuf(DmaBufInfo {
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
    }))
}

/// Allocate a `GenericDmaVideoFrame` by creating a VA surface and exporting
/// its DMA-BUF file descriptors.
///
/// Uses the provided shared Display connection rather than opening a new one
/// per allocation. The exported DMA-BUF FDs are GPU-global and survive
/// independently of the Display.
fn alloc_va_dma_frame(display: &Rc<Display>, stream_info: &StreamInfo) -> GenericDmaVideoFrame {
    let w = stream_info.coded_resolution.width;
    let h = stream_info.coded_resolution.height;

    let mut surfaces = display
        .create_surfaces(
            VA_RT_FORMAT_YUV420,
            Some(VA_FOURCC_NV12),
            w,
            h,
            Some(UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT),
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
    /// Pre-opened VAAPI display for GPU→CPU frame mapping (vaDeriveImage).
    #[debug(skip)]
    mapping_display: Rc<Display>,
    /// NAL framing format of incoming packets.
    nal_format: NalFormat,
    pending_frames: VecDeque<VideoFrame>,
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

                    let native_handle = extract_dma_buf_info(&self.display, &frame_arc, w, h);

                    let gpu_frame = VaapiGpuFrame {
                        frame: frame_arc,
                        width: w,
                        height: h,
                        native_handle,
                        mapping_display: self.mapping_display.clone(),
                    };

                    let timestamp = self.last_timestamp.unwrap_or_default();

                    let decoded =
                        VideoFrame::new_gpu(GpuFrame::new(Arc::new(gpu_frame)), timestamp);
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
        let inline = match &config.codec {
            VideoCodec::H264(h264) => h264.inline,
            other => bail!("VaapiDecoder only supports H.264, got {other}"),
        };

        let nal_format = if inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let display =
            Display::open().context("failed to open VAAPI display — no GPU or driver found")?;

        let decoder = VaapiH264Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|e| anyhow::anyhow!("failed to create VAAPI H.264 decoder: {e:?}"))?;

        // Keep a second Display for re-exporting frames as PRIME descriptors.
        let export_display =
            Display::open().context("failed to open second VAAPI display for frame export")?;

        // Pre-open a third Display for GPU→CPU frame mapping (vaDeriveImage)
        // and frame pool allocation. Sharing displays avoids the driver
        // serialization deadlock that occurs when Display::open() is called
        // while other displays are actively decoding.
        let mapping_display =
            Display::open().context("failed to open VAAPI display for frame mapping")?;

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
            let _test_frame = alloc_va_dma_frame(&mapping_display, &test_info);
        }

        // FramePool requires Send, but Rc<Display> is !Send. Store the display
        // pointer in a Send wrapper. Safety: the FramePool is Mutex-protected and
        // VaapiDecoder is only used from a single thread (same as all Rc<Display>
        // in this module, which already has `unsafe impl Send for VaapiDecoder`).
        struct SendRcDisplay(Rc<Display>);
        // Safety: same reasoning as `unsafe impl Send for VaapiDecoder` — the decoder
        // (and its frame pool) are only used on a single OS thread at a time.
        unsafe impl Send for SendRcDisplay {}
        unsafe impl Sync for SendRcDisplay {}
        let pool_display = Arc::new(SendRcDisplay(
            Display::open().context("failed to open VAAPI display for frame pool")?,
        ));
        let framepool = Arc::new(Mutex::new(FramePool::new(move |info: &StreamInfo| {
            alloc_va_dma_frame(&pool_display.0, info)
        })));

        let mut this = Self {
            decoder,
            framepool,
            display: export_display,
            mapping_display,
            nal_format,
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
        let mut annex_b = match self.nal_format {
            NalFormat::AnnexB => payload.to_vec(),
            NalFormat::Avcc => length_prefixed_to_annex_b(&payload),
        };
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

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.pending_frames.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::H264;

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
        };
        let decode_config = DecodeConfig::default();
        let dec = VaapiDecoder::new(&config, &decode_config);
        assert!(dec.is_ok(), "VAAPI decoder creation failed: {dec:?}");
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_encoder_decoder_roundtrip() {
        use crate::{
            codec::vaapi::encoder::VaapiEncoder,
            format::VideoPreset,
            traits::{VideoEncoder, VideoEncoderFactory},
        };

        let mut encoder = VaapiEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = encoder.config();

        let decode_config = DecodeConfig::default();
        let mut decoder = VaapiDecoder::new(&config, &decode_config).unwrap();

        let w = 640u32;
        let h = 360u32;
        let rgba = vec![128u8; (w * h * 4) as usize];
        let frame = VideoFrame::new_rgba(rgba.into(), w, h);

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
        use crate::config::AV1;
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
        };
        let decode_config = DecodeConfig::default();
        let result = VaapiDecoder::new(&config, &decode_config);
        assert!(result.is_err());
    }
}
