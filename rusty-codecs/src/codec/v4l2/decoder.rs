use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};

use crate::{
    codec::h264::annexb::{avcc_to_annex_b, length_prefixed_to_annex_b},
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, MediaPacket, NalFormat, VideoFrame},
    processing::convert::nv12_to_rgba_data,
    traits::VideoDecoder,
};

/// V4L2 stateful H.264 decoder for Linux.
///
/// Uses the `v4l2r` crate's high-level stateful decoder API. Targets devices
/// like the Raspberry Pi's `bcm2835-codec` (`/dev/video10`). Runs the decoder
/// in a dedicated thread to contain v4l2r's unnameable type-state generics.
#[derive(derive_more::Debug)]
pub struct V4l2Decoder {
    #[debug(skip)]
    input_tx: SyncSender<DecoderCmd>,
    #[debug(skip)]
    frame_rx: Receiver<VideoFrame>,
    #[debug(skip)]
    thread: Option<JoinHandle<()>>,
    nal_format: NalFormat,
    pending_frames: VecDeque<VideoFrame>,
}

enum DecoderCmd {
    Decode { data: Vec<u8> },
}

/// Shared format state between the format-changed and frame-decoded callbacks.
struct FormatState {
    width: u32,
    height: u32,
    stride: u32,
}

impl VideoDecoder for V4l2Decoder {
    fn name(&self) -> &str {
        "h264-v4l2"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        let inline = match &config.codec {
            VideoCodec::H264(h264) => h264.inline,
            other => bail!("V4l2Decoder only supports H.264, got {other}"),
        };

        let nal_format = if inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let device_path = super::decoder_device_path()
            .context("no V4L2 decoder device found (set V4L2_DEC_DEVICE env var)")?;

        let (input_tx, input_rx) = mpsc::sync_channel::<DecoderCmd>(4);
        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(8);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<()>>(1);

        let initial_data = config.description.as_ref().and_then(|d| avcc_to_annex_b(d));

        let device_path_owned = device_path.clone();
        let thread = std::thread::Builder::new()
            .name("v4l2-decoder".into())
            .spawn(move || {
                decoder_thread(device_path_owned, input_rx, frame_tx, init_tx, initial_data);
            })
            .context("failed to spawn V4L2 decoder thread")?;

        // Wait for initialization.
        let init_result = init_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("V4L2 decoder thread exited during init"))?;
        init_result?;

        Ok(Self {
            input_tx,
            frame_rx,
            thread: Some(thread),
            nal_format,
            pending_frames: VecDeque::new(),
        })
    }

    fn set_viewport(&mut self, _w: u32, _h: u32) {
        // V4L2 decodes at full resolution; scaling happens in the renderer.
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
        let annex_b = match self.nal_format {
            NalFormat::AnnexB => payload.to_vec(),
            NalFormat::Avcc => length_prefixed_to_annex_b(&payload),
        };

        self.input_tx
            .send(DecoderCmd::Decode { data: annex_b })
            .map_err(|_| anyhow::anyhow!("V4L2 decoder thread exited"))?;

        // Drain any ready frames.
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.pending_frames.push_back(frame);
        }

        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.pending_frames.push_back(frame);
        }
        Ok(self.pending_frames.pop_front())
    }
}

impl Drop for V4l2Decoder {
    fn drop(&mut self) {
        // Close the input channel to signal shutdown.
        drop(std::mem::replace(
            &mut self.input_tx,
            mpsc::sync_channel(0).0,
        ));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs the V4L2 stateful decoder. All v4l2r type-state generics are local.
fn decoder_thread(
    device_path: std::path::PathBuf,
    input_rx: Receiver<DecoderCmd>,
    frame_tx: SyncSender<VideoFrame>,
    init_tx: SyncSender<Result<()>>,
    initial_data: Option<Vec<u8>>,
) {
    use v4l2r::{
        decoder::{CompletedInputBuffer, DecoderEvent, FormatChangedReply, stateful::Decoder},
        device::queue::{GetFreeOutputBuffer as _, handles_provider::MmapProvider},
        memory::{MemoryType, MmapHandle},
    };

    // Shared format state between callbacks.
    let format_state: Arc<Mutex<Option<FormatState>>> = Arc::new(Mutex::new(None));

    let result: Result<()> = (|| {
        let decoder = Decoder::open(&device_path)
            .map_err(|e| anyhow::anyhow!("failed to open V4L2 decoder {device_path:?}: {e}"))?;

        let decoder = decoder
            .set_output_format(|f| {
                f.set_pixelformat(b"H264")
                    .set_planes_layout(vec![v4l2r::PlaneLayout {
                        sizeimage: 1024 * 1024,
                        ..Default::default()
                    }])
                    .apply::<v4l2r::Format>()?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("failed to set V4L2 decoder format: {e}"))?;

        let decoder = decoder
            .allocate_output_buffers::<Vec<MmapHandle>>(4)
            .map_err(|e| anyhow::anyhow!("failed to allocate V4L2 decoder buffers: {e}"))?;

        let format_state_decode = format_state.clone();
        let format_state_format = format_state.clone();
        let frame_tx_cb = frame_tx.clone();

        let decoder = decoder
            .start(
                // Input done: no-op.
                |_: CompletedInputBuffer<Vec<MmapHandle>>| {},
                // Frame decoded: extract NV12, convert to VideoFrame, send.
                move |event: DecoderEvent<MmapProvider>| match event {
                    DecoderEvent::FrameDecoded(dqbuf) => {
                        let state = format_state_decode.lock().expect("poisoned");
                        let Some(fmt) = state.as_ref() else {
                            tracing::warn!("V4L2: frame before format negotiation");
                            return;
                        };

                        match extract_nv12_frame(&dqbuf, fmt) {
                            Ok(f) => {
                                let _ = frame_tx_cb.try_send(f);
                            }
                            Err(e) => {
                                tracing::warn!("V4L2: frame extraction failed: {e}");
                            }
                        }
                    }
                    DecoderEvent::EndOfStream => {
                        tracing::debug!("V4L2 decoder: end of stream");
                    }
                },
                // Format changed: negotiate CAPTURE format.
                move |f, visible_rect, min_num_buffers| {
                    let format = f.format();
                    tracing::info!(
                        "V4L2: format changed {:?} {}x{} (min bufs: {})",
                        format.pixelformat,
                        visible_rect.width,
                        visible_rect.height,
                        min_num_buffers,
                    );

                    let stride = format
                        .plane_fmt
                        .first()
                        .map(|p| p.bytesperline)
                        .unwrap_or(visible_rect.width);

                    *format_state_format.lock().expect("poisoned") = Some(FormatState {
                        width: visible_rect.width,
                        height: visible_rect.height,
                        stride,
                    });

                    Ok(FormatChangedReply {
                        provider: MmapProvider::new(format),
                        mem_type: MemoryType::Mmap,
                        num_buffers: min_num_buffers + 4,
                    })
                },
            )
            .map_err(|e| anyhow::anyhow!("failed to start V4L2 decoder: {e}"))?;

        /// Feeds data to the decoder by getting a free buffer, copying into it, and queuing.
        /// Defined as a macro to avoid naming the unnameable Decoder<Decoding<...>> type.
        macro_rules! feed_data {
            ($decoder:expr, $data:expr) => {{
                let data: &[u8] = $data;
                let qbuf = $decoder
                    .try_get_free_buffer()
                    .map_err(|e| anyhow::anyhow!("no free decoder buffer: {e}"))?;
                {
                    let mut mapping = qbuf
                        .get_plane_mapping(0)
                        .ok_or_else(|| anyhow::anyhow!("buffer map failed"))?;
                    let len = data.len().min(mapping.len());
                    mapping[..len].copy_from_slice(&data[..len]);
                }
                qbuf.queue(&[data.len()])
                    .map_err(|e| anyhow::anyhow!("buffer queue failed: {e}"))
            }};
        }

        // Feed initial SPS/PPS data.
        if let Some(data) = &initial_data
            && let Err(e) = feed_data!(decoder, data)
        {
            tracing::warn!("V4L2: initial SPS/PPS feed failed: {e}");
        }

        // Signal successful init.
        init_tx
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("init channel closed"))?;

        // Process input commands until the channel closes.
        for cmd in &input_rx {
            match cmd {
                DecoderCmd::Decode { data } => {
                    if let Err(e) = feed_data!(decoder, &data) {
                        tracing::warn!("V4L2 decode: {e}");
                    }
                }
            }
        }

        // Drain and stop.
        let _ = decoder.drain(true);
        let _ = decoder.stop();
        Ok(())
    })();

    if let Err(e) = result {
        // If init hasn't been acknowledged, send the error.
        let _ = init_tx.send(Err(e));
    }
}

/// Extracts NV12 plane data from a dequeued CAPTURE buffer.
fn extract_nv12_frame(
    dqbuf: &v4l2r::device::queue::dqbuf::DqBuffer<
        v4l2r::device::queue::direction::Capture,
        Vec<v4l2r::memory::MmapHandle>,
    >,
    fmt: &FormatState,
) -> Result<VideoFrame> {
    let w = fmt.width;
    let h = fmt.height;
    let stride = fmt.stride as usize;
    let w_usize = w as usize;
    let h_usize = h as usize;
    let uv_h = h.div_ceil(2) as usize;

    let mapping = dqbuf
        .get_plane_mapping(0)
        .context("failed to map V4L2 CAPTURE plane 0")?;

    let data: &[u8] = &mapping;
    let uv_offset = stride * h_usize;

    // Hoist the optional UV plane mapping so Cow borrows outlive the conversion.
    let uv_mapping;
    let uv_src: &[u8] = if uv_offset < data.len() {
        &data[uv_offset..]
    } else {
        // Multi-plane: UV is on a separate V4L2 plane.
        uv_mapping = dqbuf
            .get_plane_mapping(1)
            .context("failed to map V4L2 CAPTURE UV plane")?;
        &uv_mapping
    };

    // Single-plane NV12: Y at offset 0, UV at offset stride * height.
    let y_data = copy_plane(data, stride, w_usize, h_usize);
    let uv_data = copy_plane(uv_src, stride, w_usize, uv_h);

    // TODO: output NV12 directly (FrameData::Nv12) when the render pipeline
    // supports NV12 from V4L2 without GPU import. For now, convert to RGBA.
    let rgba = nv12_to_rgba_data(&y_data, w, &uv_data, w, w, h)?;
    Ok(VideoFrame::new_cpu(rgba, w, h, Duration::ZERO))
}

/// Returns rows from a plane buffer, stripping stride padding.
///
/// Returns a borrowed slice when stride equals width (zero-copy fast path).
/// Falls back to copying row-by-row when stride padding is present.
fn copy_plane<'a>(
    src: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
) -> std::borrow::Cow<'a, [u8]> {
    if stride == width && src.len() >= width * height {
        return std::borrow::Cow::Borrowed(&src[..width * height]);
    }
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        let start = row * stride;
        let end = start + width;
        if end <= src.len() {
            out.extend_from_slice(&src[start..end]);
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::H264;

    #[test]
    #[ignore = "requires V4L2 decoder hardware"]
    fn v4l2_decoder_creation() {
        let config = VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline: true,
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
        let dec = V4l2Decoder::new(&config, &decode_config);
        assert!(dec.is_ok(), "V4L2 decoder creation failed: {dec:?}");
    }

    #[test]
    fn v4l2_unsupported_codec_errors() {
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
        let result = V4l2Decoder::new(&config, &decode_config);
        assert!(result.is_err());
    }
}
