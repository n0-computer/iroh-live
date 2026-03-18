// VideoToolbox H.264 hardware decoder (macOS / iOS).
//
// Decodes length-prefixed or Annex B H.264 bitstreams via VTDecompressionSession.
// Each decoded frame is a CVPixelBuffer (NV12) wrapped in a GpuFrame for
// deferred CPU readback. The callback pattern mirrors vtb/encoder.rs: an
// Arc<Mutex<VecDeque>> shared between the session callback and the pop_frame
// caller.
//
// Inspiration: Apple's VideoToolbox documentation and the existing VtbEncoder
// in this crate (MIT/Apache-2.0).

use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::{self, NonNull},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFString, CFType, kCFAllocatorNull,
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTSessionSetProperty, kVTDecompressionPropertyKey_RealTime,
};

use crate::{
    codec::h264::annexb::{
        annex_b_to_length_prefixed, avcc_to_annex_b, extract_sps_pps, parse_annex_b,
    },
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, GpuFrame, GpuPixelFormat, MediaPacket, NalFormat, VideoFrame},
    traits::VideoDecoder,
};

/// NV12 (bi-planar YUV 4:2:0, video range) pixel format constant.
/// kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange = '420v' = 0x34323076
const NV12_VIDEO_RANGE: i32 = 0x3432_3076_u32 as i32;

/// Shared state between the decoder and the VTDecompressionSession callback.
struct DecoderState {
    frames: VecDeque<(CFRetained<CVPixelBuffer>, Duration)>,
}

type SharedDecoderState = Arc<Mutex<DecoderState>>;

/// VideoToolbox H.264 hardware decoder.
///
/// Supports both eager initialization (avcC description available at
/// construction) and deferred initialization (SPS/PPS parsed from the
/// first keyframe in Annex B streams).
#[derive(derive_more::Debug)]
pub struct VtbDecoder {
    #[debug(skip)]
    session: Option<CFRetained<VTDecompressionSession>>,
    #[debug(skip)]
    format_desc: Option<CFRetained<CMFormatDescription>>,
    #[debug(skip)]
    state: SharedDecoderState,
    nal_format: NalFormat,
    /// Current SPS bytes, used to detect mid-stream resolution changes.
    #[debug(skip)]
    current_sps: Vec<u8>,
}

// Safety: VTDecompressionSession is thread-safe per Apple documentation.
// The session can be used from any thread, and the callback is invoked
// on VTB's internal thread. We synchronize via Arc<Mutex<_>>.
unsafe impl Send for VtbDecoder {}

impl VideoDecoder for VtbDecoder {
    fn name(&self) -> &str {
        "h264-vtb"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        let VideoCodec::H264(h264) = &config.codec else {
            bail!("VtbDecoder only supports H.264");
        };

        let nal_format = if h264.inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let state = Arc::new(Mutex::new(DecoderState {
            frames: VecDeque::new(),
        }));

        // Extract SPS/PPS from avcC description if available.
        // Otherwise defer session creation to the first keyframe.
        let (session, format_desc, sps) = if let Some(description) = &config.description {
            let annex_b =
                avcc_to_annex_b(description).context("failed to parse avcC description")?;
            let nals = parse_annex_b(&annex_b);
            let (sps, pps) = extract_sps_pps(&nals).context("no SPS/PPS in avcC")?;
            let (session, format_desc) = create_session(&sps, &pps, &state)?;
            tracing::info!("VideoToolbox H.264 decoder ready (eager)");
            (Some(session), Some(format_desc), sps)
        } else {
            tracing::info!(
                "VideoToolbox H.264 decoder created (deferred — waiting for first keyframe)"
            );
            (None, None, Vec::new())
        };

        Ok(Self {
            session,
            format_desc,
            state,
            nal_format,
            current_sps: sps,
        })
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        // Ensure length-prefixed format — VTDecompressionSession expects it.
        let lp_buf;
        let lp_data: &[u8] = match self.nal_format {
            NalFormat::Avcc => &payload,
            NalFormat::AnnexB => {
                lp_buf = annex_b_to_length_prefixed(&payload);
                &lp_buf
            }
        };

        // On keyframes, check for SPS changes or deferred initialization.
        if packet.is_keyframe {
            if let Some(new_sps) = extract_sps_from_lp(lp_data) {
                if new_sps != self.current_sps {
                    let nals = parse_lp_nals(lp_data);
                    if let Some((sps, pps)) = extract_sps_pps_from_lp_nals(&nals) {
                        let (new_session, new_fmt) = create_session(&sps, &pps, &self.state)?;
                        // Flush pending frames from old session if one exists.
                        if let Some(ref old_session) = self.session {
                            unsafe {
                                let _ = VTDecompressionSession::wait_for_asynchronous_frames(
                                    old_session,
                                );
                                VTDecompressionSession::invalidate(old_session);
                            }
                        }
                        self.session = Some(new_session);
                        self.format_desc = Some(new_fmt);
                        self.current_sps = sps;
                        if self.current_sps.is_empty() {
                            tracing::info!(
                                "VideoToolbox H.264 decoder initialized from first keyframe"
                            );
                        }
                    }
                }
            }
        }

        // Not yet initialized — drop packets until we get a keyframe with SPS/PPS.
        let (session, format_desc) = match (self.session.as_ref(), self.format_desc.as_ref()) {
            (Some(s), Some(f)) => (s, f),
            _ => return Ok(()),
        };

        // Build CMBlockBuffer from the length-prefixed NAL data.
        let block_buffer = create_block_buffer(lp_data)?;

        // Build CMSampleBuffer with timing info.
        let sample_buffer = create_sample_buffer(&block_buffer, format_desc, packet.timestamp)?;

        // Encode timestamp in the sourceFrameRefCon so the callback can recover it.
        let ts_micros = packet.timestamp.as_micros() as u64;
        let ts_refcon = ts_micros as *mut c_void;

        // Decode synchronously — the session-level callback fires before this returns.
        let mut info_flags = VTDecodeInfoFlags(0);
        let status = unsafe {
            VTDecompressionSession::decode_frame(
                session,
                &sample_buffer,
                VTDecodeFrameFlags(0), // synchronous
                ts_refcon,
                &mut info_flags,
            )
        };

        if status != 0 {
            bail!("VTDecompressionSessionDecodeFrame failed with status {status}");
        }

        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let mut state = self.state.lock().unwrap();
        let Some((pixel_buffer, timestamp)) = state.frames.pop_front() else {
            return Ok(None);
        };

        let w = CVPixelBufferGetWidth(&pixel_buffer) as u32;
        let h = CVPixelBufferGetHeight(&pixel_buffer) as u32;

        let apple_frame =
            crate::format::AppleGpuFrame::from_retained(pixel_buffer, w, h, GpuPixelFormat::Nv12);
        let gpu_frame = GpuFrame::new(Arc::new(apple_frame));

        Ok(Some(VideoFrame::new_gpu(gpu_frame, timestamp)))
    }

    fn set_viewport(&mut self, _w: u32, _h: u32) {
        // VTB does not support decode-time downscaling. Viewport scaling is
        // handled post-decode by the render pipeline.
    }

    fn burst_size(&self) -> usize {
        // VTB has an internal DPB; it may buffer a few frames before emitting.
        3
    }
}

impl Drop for VtbDecoder {
    fn drop(&mut self) {
        if let Some(ref session) = self.session {
            unsafe {
                let _ = VTDecompressionSession::wait_for_asynchronous_frames(session);
                VTDecompressionSession::invalidate(session);
            }
        }
    }
}

// VtbGpuFrame replaced by AppleGpuFrame (format.rs) — unified GPU frame
// type for both capture and decode on macOS. AppleGpuFrame holds a retained
// CVPixelBufferRef and handles NV12/BGRA download + native_handle() export.

// --- Session creation ---

fn create_session(
    sps: &[u8],
    pps: &[u8],
    state: &SharedDecoderState,
) -> Result<(
    CFRetained<VTDecompressionSession>,
    CFRetained<CMFormatDescription>,
)> {
    // Build CMVideoFormatDescription from SPS/PPS.
    let param_sets: [*const u8; 2] = [sps.as_ptr(), pps.as_ptr()];
    let param_sizes: [usize; 2] = [sps.len(), pps.len()];
    let mut format_desc: *const CMFormatDescription = ptr::null();

    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            2,
            NonNull::new(param_sets.as_ptr() as *mut _).unwrap(),
            NonNull::new(param_sizes.as_ptr() as *mut _).unwrap(),
            4, // NAL unit header length (4-byte length prefix)
            NonNull::new(&mut format_desc as *mut _).unwrap(),
        )
    };
    if status != 0 || format_desc.is_null() {
        bail!("CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {status}");
    }
    let format_desc = unsafe { CFRetained::from_raw(NonNull::new(format_desc as *mut _).unwrap()) };

    // Destination pixel buffer attributes: request NV12 output.
    let dest_attrs = build_dest_image_attrs();

    // Set up the output callback with a leaked Arc pointer.
    let refcon = Arc::into_raw(state.clone()) as *mut c_void;
    let callback_record = VTDecompressionOutputCallbackRecord {
        decompressionOutputCallback: Some(decompression_output_callback),
        decompressionOutputRefCon: refcon,
    };

    // Create the decompression session.
    let mut session_ptr: *mut VTDecompressionSession = ptr::null_mut();
    let status = unsafe {
        VTDecompressionSession::create(
            None, // allocator
            &format_desc,
            None, // decoder specification
            Some(&*dest_attrs),
            &callback_record,
            NonNull::new(&mut session_ptr).unwrap(),
        )
    };
    if status != 0 || session_ptr.is_null() {
        // Clean up the leaked Arc.
        unsafe { drop(Arc::from_raw(refcon as *const Mutex<DecoderState>)) };
        bail!("VTDecompressionSessionCreate failed: {status}");
    }
    let session = unsafe { CFRetained::from_raw(NonNull::new(session_ptr).unwrap()) };

    // Enable real-time decoding for low latency.
    unsafe {
        let cf_true: &CFType = objc2_core_foundation::CFBoolean::new(true);
        let _ = VTSessionSetProperty(
            &session,
            kVTDecompressionPropertyKey_RealTime,
            Some(cf_true),
        );
    }

    Ok((session, format_desc))
}

/// Build destination pixel buffer attributes requesting NV12 output.
fn build_dest_image_attrs() -> CFRetained<CFDictionary> {
    let pixel_format = CFNumber::new_i32(NV12_VIDEO_RANGE);

    unsafe {
        let keys: [*const c_void; 1] =
            [(kCVPixelBufferPixelFormatTypeKey as *const CFString).cast()];
        let values: [*const c_void; 1] = [((&*pixel_format) as *const CFNumber).cast()];
        CFDictionary::new(
            None,
            keys.as_ptr().cast_mut(),
            values.as_ptr().cast_mut(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
        .expect("CFDictionaryCreate returned null")
    }
}

// --- CMBlockBuffer / CMSampleBuffer construction ---

fn create_block_buffer(data: &[u8]) -> Result<CFRetained<CMBlockBuffer>> {
    let mut block_buffer: *mut CMBlockBuffer = ptr::null_mut();
    // Safety: kCFAllocatorNull tells CoreMedia NOT to free our memory.
    // Without this, CM would use the default allocator to free() Rust-owned
    // memory when the block buffer is released → crash.
    let null_allocator = unsafe { kCFAllocatorNull };
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,                         // allocator
            data.as_ptr() as *mut c_void, // memory block (borrowed)
            data.len(),
            null_allocator.as_deref(), // block allocator: kCFAllocatorNull = don't free
            ptr::null(),               // custom block source
            0,                         // offset
            data.len(),
            0, // flags
            NonNull::new(&mut block_buffer).unwrap(),
        )
    };
    if status != 0 || block_buffer.is_null() {
        bail!("CMBlockBufferCreateWithMemoryBlock failed: {status}");
    }
    Ok(unsafe { CFRetained::from_raw(NonNull::new(block_buffer).unwrap()) })
}

fn create_sample_buffer(
    block_buffer: &CMBlockBuffer,
    format_desc: &CMFormatDescription,
    timestamp: Duration,
) -> Result<CFRetained<CMSampleBuffer>> {
    let ts_value = timestamp.as_micros() as i64;
    let timescale = 1_000_000i32; // microseconds
    let pts = unsafe { CMTime::new(ts_value, timescale) };

    let mut timing = objc2_core_media::CMSampleTimingInfo {
        duration: unsafe { CMTime::new(0, timescale) },
        presentationTimeStamp: pts,
        decodeTimeStamp: unsafe { CMTime::new(ts_value, timescale) },
    };

    let data_len = unsafe { block_buffer.data_length() };
    let mut size = data_len;

    let mut sample_buffer: *mut CMSampleBuffer = ptr::null_mut();
    let status = unsafe {
        CMSampleBuffer::create(
            None, // allocator
            Some(block_buffer),
            true.into(),     // data ready
            None,            // make-data-ready callback
            ptr::null_mut(), // make-data-ready refcon
            Some(format_desc),
            1, // num samples
            1, // num timing entries
            &mut timing,
            1, // num size entries
            &mut size,
            NonNull::new(&mut sample_buffer).unwrap(),
        )
    };
    if status != 0 || sample_buffer.is_null() {
        bail!("CMSampleBufferCreate failed: {status}");
    }
    Ok(unsafe { CFRetained::from_raw(NonNull::new(sample_buffer).unwrap()) })
}

// --- Decompression callback ---

/// C callback invoked by VideoToolbox when a frame is decoded.
///
/// # Safety
///
/// Called from VideoToolbox's internal thread.
/// `decompression_output_ref_con` must be a valid pointer to `Arc<Mutex<DecoderState>>`.
unsafe extern "C-unwind" fn decompression_output_callback(
    decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _duration: CMTime,
) {
    if status != 0 || image_buffer.is_null() {
        return;
    }

    let state_ptr = decompression_output_ref_con as *const Mutex<DecoderState>;
    // Increment ref count to avoid taking ownership.
    unsafe { Arc::increment_strong_count(state_ptr) };
    let state_arc = unsafe { Arc::from_raw(state_ptr) };

    // CVImageBuffer and CVPixelBuffer are the same type on macOS/iOS.
    let pixel_buffer = image_buffer as *mut CVPixelBuffer;
    // Retain the pixel buffer so it outlives this callback.
    let retained = unsafe { CFRetained::retain(NonNull::new(pixel_buffer).unwrap()) };

    // Recover timestamp from refcon.
    let ts_micros = source_frame_ref_con as u64;
    let timestamp = Duration::from_micros(ts_micros);

    if let Ok(mut guard) = state_arc.lock() {
        guard.frames.push_back((retained, timestamp));
    }
}

// --- NAL parsing helpers ---

/// Extract the SPS NAL unit from length-prefixed data (for change detection).
fn extract_sps_from_lp(data: &[u8]) -> Option<Vec<u8>> {
    let mut offset = 0;
    while offset + 4 < data.len() {
        let nal_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if offset + 4 + nal_len > data.len() {
            break;
        }
        let nal_type = data[offset + 4] & 0x1F;
        if nal_type == 7 {
            // SPS
            return Some(data[offset + 4..offset + 4 + nal_len].to_vec());
        }
        offset += 4 + nal_len;
    }
    None
}

/// Parse length-prefixed NAL units into slices.
fn parse_lp_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut offset = 0;
    while offset + 4 < data.len() {
        let nal_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if offset + 4 + nal_len > data.len() {
            break;
        }
        nals.push(&data[offset + 4..offset + 4 + nal_len]);
        offset += 4 + nal_len;
    }
    nals
}

/// Extract SPS and PPS from parsed length-prefixed NAL units.
fn extract_sps_pps_from_lp_nals(nals: &[&[u8]]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        match nal[0] & 0x1F {
            7 => sps = Some(nal.to_vec()),
            8 => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    Some((sps?, pps?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::{
            h264::encoder::H264Encoder,
            test_util::{encoded_frames_to_media_packets, make_rgba_frame},
        },
        format::VideoPreset,
        traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory},
    };

    #[test]
    #[ignore] // Requires macOS with VideoToolbox
    fn vtb_decode_basic() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P360).unwrap();
        let mut packets = Vec::new();
        for _ in 0..10 {
            let frame = make_rgba_frame(640, 360, 200, 100, 50);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        assert!(!packets.is_empty());

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = VtbDecoder::new(&config, &decode_config).unwrap();

        let mut decoded_count = 0;
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            while let Some(frame) = dec.pop_frame().unwrap() {
                let (w, h) = (frame.dimensions[0], frame.dimensions[1]);
                assert_eq!(w, 640);
                assert_eq!(h, 360);
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    #[test]
    #[ignore]
    fn vtb_decode_gpu_frame_nv12() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let frame = make_rgba_frame(320, 180, 128, 128, 128);
        enc.push_frame(frame).unwrap();
        // Flush
        let mut packets = Vec::new();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }

        let config = enc.config();
        let mut dec = VtbDecoder::new(&config, &DecodeConfig::default()).unwrap();
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
        }

        if let Some(frame) = dec.pop_frame().unwrap() {
            if let crate::format::FrameData::Gpu(gpu) = &frame.data {
                assert_eq!(gpu.gpu_pixel_format(), GpuPixelFormat::Nv12);
                let nv12 = gpu.download_nv12().unwrap().unwrap();
                assert_eq!(nv12.width, 320);
                assert_eq!(nv12.height, 180);
                // RGBA download should also work
                let rgba = gpu.download_rgba().unwrap();
                assert_eq!(rgba.width(), 320);
                assert_eq!(rgba.height(), 180);
            } else {
                panic!("expected GPU frame from VTB decoder");
            }
        }
    }
}
