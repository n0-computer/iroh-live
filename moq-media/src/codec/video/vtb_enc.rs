use std::ffi::{c_char, c_int, c_void};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use hang::{
    Timestamp,
    catalog::{H264, VideoCodec, VideoConfig},
};
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, kCFTypeDictionaryKeyCallBacks,
    kCFTypeDictionaryValueCallBacks,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMTimeInvalid,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferPool, CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_420YpCbCr8Planar,
};
use objc2_video_toolbox::{
    VTCompressionOutputCallback, VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_Baseline_AutoLevel,
};

use crate::{
    av::{self, VideoEncoder, VideoEncoderFactory, VideoPreset},
    codec::video::util::{
        annexb::build_avcc,
        convert::{YuvData, pixel_format_to_yuv420},
    },
};

/// Shared buffer between the encoder and the VTCompressionSession callback.
type SharedPacketBuf = Arc<Mutex<CallbackState>>;

struct CallbackState {
    packets: Vec<hang::Frame>,
    avcc: Option<Vec<u8>>,
    framerate: u32,
    frame_count: u64,
}

#[derive(derive_more::Debug)]
pub struct VtbEncoder {
    #[debug(skip)]
    session: CFRetained<VTCompressionSession>,
    #[debug(skip)]
    callback_state: SharedPacketBuf,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    /// Force the next encoded frame to be a keyframe. Set after priming so
    /// the first real frame is an IDR instead of waiting MaxKeyFrameInterval.
    force_next_keyframe: bool,
}

// Safety: VTCompressionSession is thread-safe per Apple documentation.
// The session can be used from any thread, and the callback is invoked
// on VTB's internal thread. We synchronize via Arc<Mutex<_>>.
unsafe impl Send for VtbEncoder {}

impl VtbEncoder {
    fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        let pixels = width * height;
        let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
        let bitrate = (pixels as f32 * 0.07 * framerate_factor).round() as u64;

        let callback_state: SharedPacketBuf = Arc::new(Mutex::new(CallbackState {
            packets: Vec::new(),
            avcc: None,
            framerate,
            frame_count: 0,
        }));

        let callback: VTCompressionOutputCallback = Some(compression_output_callback);
        let refcon = Arc::into_raw(callback_state.clone()) as *mut c_void;

        // Specify I420 (3-plane planar YUV) as the source pixel format so the
        // pixel buffer pool allocates 3-plane buffers instead of NV12.
        let source_attrs = build_source_image_attrs(width, height);

        let mut session_ptr: *mut VTCompressionSession = ptr::null_mut();
        let status = unsafe {
            VTCompressionSession::create(
                None, // allocator
                width as i32,
                height as i32,
                0x61766331,           // kCMVideoCodecType_H264 = 'avc1'
                None,                 // encoder specification
                Some(&*source_attrs), // source image buffer attributes
                None,                 // compressed data allocator
                callback,
                refcon,
                NonNull::new(&mut session_ptr).unwrap(),
            )
        };

        if status != 0 || session_ptr.is_null() {
            // Clean up the Arc we leaked for the refcon.
            unsafe {
                drop(Arc::from_raw(refcon as *const Mutex<CallbackState>));
            }
            bail!("VTCompressionSessionCreate failed with status {status}");
        }

        // Safety: create succeeded and session_ptr is non-null and retained.
        let session = unsafe { CFRetained::from_raw(NonNull::new(session_ptr).unwrap()) };

        // Set compression properties.
        // Safety: accessing extern statics for VideoToolbox property keys.
        unsafe {
            set_bool_property(&session, kVTCompressionPropertyKey_RealTime, true)?;

            let bitrate_num = CFNumber::new_i64(bitrate as i64);
            set_number_property(
                &session,
                kVTCompressionPropertyKey_AverageBitRate,
                &bitrate_num,
            )?;

            let kf_interval = CFNumber::new_i64(framerate as i64);
            set_number_property(
                &session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &kf_interval,
            )?;

            set_string_property(
                &session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_Baseline_AutoLevel,
            )?;

            set_bool_property(
                &session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                false,
            )?;
        }

        // Prepare to encode.
        let status = unsafe { session.prepare_to_encode_frames() };
        if status != 0 {
            unsafe {
                session.invalidate();
            }
            bail!("prepare_to_encode_frames failed with status {status}");
        }

        // Encode a black frame to extract SPS/PPS for the avcC description.
        // VTB only emits parameter sets in the first encoded IDR frame.
        {
            let pool = unsafe { session.pixel_buffer_pool() }
                .context("VTCompressionSession pixel buffer pool is null (priming)")?;
            let pixel_buffer = create_pixel_buffer_from_pool(&pool)?;

            let yuv = YuvData::black(width, height);
            copy_yuv_to_pixel_buffer(&pixel_buffer, &yuv.y, &yuv.u, &yuv.v, width, height)?;

            let pts = unsafe { CMTime::new(0, framerate as i32) };
            let duration = unsafe { CMTime::new(1, framerate as i32) };
            let mut info_flags = VTEncodeInfoFlags(0);
            let status = unsafe {
                session.encode_frame(
                    &pixel_buffer,
                    pts,
                    duration,
                    None,
                    ptr::null_mut(),
                    &mut info_flags,
                )
            };
            if status != 0 {
                bail!("VTCompressionSessionEncodeFrame (priming) failed with status {status}");
            }

            // Flush to ensure the callback fires before we return.
            let status = unsafe { session.complete_frames(kCMTimeInvalid) };
            if status != 0 {
                bail!("VTCompressionSessionCompleteFrames (priming) failed with status {status}");
            }

            // Discard the priming packet and reset frame count.
            let mut state = callback_state.lock().unwrap();
            state.packets.clear();
            state.frame_count = 0;
        }

        Ok(Self {
            session,
            callback_state,
            width,
            height,
            framerate,
            bitrate,
            force_next_keyframe: true,
        })
    }
}

impl VideoEncoderFactory for VtbEncoder {
    const ID: &str = "h264-vtb";
    fn with_preset(preset: VideoPreset) -> Result<Self> {
        Self::new(preset.width(), preset.height(), preset.fps())
    }
}

impl VideoEncoder for VtbEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let state = self.callback_state.lock().unwrap();
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
                inline: false,
            }),
            description: state.avcc.clone().map(Into::into),
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

        // Get a pixel buffer from the session's pool.
        let pool = unsafe { self.session.pixel_buffer_pool() }
            .context("VTCompressionSession pixel buffer pool is null")?;
        let pixel_buffer = create_pixel_buffer_from_pool(&pool)?;

        // Copy YUV data into the pixel buffer.
        copy_yuv_to_pixel_buffer(&pixel_buffer, &yuv.y, &yuv.u, &yuv.v, w, h)?;

        // Encode.
        let frame_count = {
            let state = self.callback_state.lock().unwrap();
            state.frame_count
        };
        let pts = unsafe { CMTime::new(frame_count as i64, self.framerate as i32) };
        let duration = unsafe { CMTime::new(1, self.framerate as i32) };

        // After priming, force the first real frame to be a keyframe so
        // subscribers don't have to wait MaxKeyFrameInterval frames.
        let frame_props = if self.force_next_keyframe {
            self.force_next_keyframe = false;
            Some(build_force_keyframe_props())
        } else {
            None
        };

        let mut info_flags = VTEncodeInfoFlags(0);
        let status = unsafe {
            self.session.encode_frame(
                &pixel_buffer,
                pts,
                duration,
                frame_props.as_deref(),
                ptr::null_mut(), // source frame refcon
                &mut info_flags,
            )
        };

        if status != 0 {
            bail!("VTCompressionSessionEncodeFrame failed with status {status}");
        }

        // Increment frame count in callback state.
        {
            let mut state = self.callback_state.lock().unwrap();
            state.frame_count += 1;
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        let mut state = self.callback_state.lock().unwrap();
        Ok(if state.packets.is_empty() {
            None
        } else {
            Some(state.packets.remove(0))
        })
    }
}

impl Drop for VtbEncoder {
    fn drop(&mut self) {
        // Flush remaining frames with an invalid time (= flush all).
        unsafe {
            let _ = self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}

// --- Helper functions ---

/// Set a boolean property on a VTCompressionSession.
fn set_bool_property(session: &VTCompressionSession, key: &CFString, value: bool) -> Result<()> {
    let cf_bool: &CFBoolean = CFBoolean::new(value);
    // CFBoolean -> CFType via Deref chain. VTSessionSetProperty expects &CFType.
    let cf_type: &CFType = cf_bool;
    let status = unsafe { VTSessionSetProperty(session, key, Some(cf_type)) };
    if status != 0 {
        bail!("VTSessionSetProperty failed with status {status}");
    }
    Ok(())
}

/// Set a CFNumber property on a VTCompressionSession.
fn set_number_property(
    session: &VTCompressionSession,
    key: &CFString,
    value: &CFNumber,
) -> Result<()> {
    let cf_type: &CFType = value;
    let status = unsafe { VTSessionSetProperty(session, key, Some(cf_type)) };
    if status != 0 {
        bail!("VTSessionSetProperty failed with status {status}");
    }
    Ok(())
}

/// Set a CFString property on a VTCompressionSession.
fn set_string_property(
    session: &VTCompressionSession,
    key: &CFString,
    value: &CFString,
) -> Result<()> {
    let cf_type: &CFType = value;
    let status = unsafe { VTSessionSetProperty(session, key, Some(cf_type)) };
    if status != 0 {
        bail!("VTSessionSetProperty failed with status {status}");
    }
    Ok(())
}

/// Build a CFDictionary with `kVTEncodeFrameOptionKey_ForceKeyFrame = true`
/// to force the next encoded frame to be an IDR keyframe.
fn build_force_keyframe_props() -> CFRetained<CFDictionary> {
    // Safety: accessing extern statics for VideoToolbox property keys.
    unsafe {
        let keys: [*const c_void; 1] =
            [(kVTEncodeFrameOptionKey_ForceKeyFrame as *const CFString).cast()];
        let values: [*const c_void; 1] = [(CFBoolean::new(true) as *const CFBoolean).cast()];
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

/// Build a CFDictionary specifying I420 pixel format and dimensions for the
/// compression session's source image buffer pool.
fn build_source_image_attrs(width: u32, height: u32) -> CFRetained<CFDictionary> {
    let pixel_format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8Planar as i32);
    let width_num = CFNumber::new_i32(width as i32);
    let height_num = CFNumber::new_i32(height as i32);

    // Safety: accessing extern statics for CVPixelBuffer property keys, and
    // creating a dictionary from valid CF type pointers.
    unsafe {
        let keys: [*const c_void; 3] = [
            (kCVPixelBufferPixelFormatTypeKey as *const CFString).cast(),
            (kCVPixelBufferWidthKey as *const CFString).cast(),
            (kCVPixelBufferHeightKey as *const CFString).cast(),
        ];
        let values: [*const c_void; 3] = [
            ((&*pixel_format) as *const CFNumber).cast(),
            ((&*width_num) as *const CFNumber).cast(),
            ((&*height_num) as *const CFNumber).cast(),
        ];
        CFDictionary::new(
            None,
            keys.as_ptr().cast_mut(),
            values.as_ptr().cast_mut(),
            3,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
        .expect("CFDictionaryCreate returned null")
    }
}

/// Create a CVPixelBuffer from the session's pixel buffer pool.
fn create_pixel_buffer_from_pool(pool: &CVPixelBufferPool) -> Result<CFRetained<CVPixelBuffer>> {
    let mut pixel_buffer: *mut CVPixelBuffer = ptr::null_mut();
    let status = unsafe {
        CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::new(&mut pixel_buffer).unwrap())
    };
    if status != 0 || pixel_buffer.is_null() {
        bail!("CVPixelBufferPoolCreatePixelBuffer failed with status {status}");
    }
    // Safety: CVPixelBufferPoolCreatePixelBuffer returns a retained object.
    Ok(unsafe { CFRetained::from_raw(NonNull::new(pixel_buffer).unwrap()) })
}

/// Copy YUV420 planar data into a CVPixelBuffer (3-plane I420 format).
fn copy_yuv_to_pixel_buffer(
    pixel_buffer: &CVPixelBuffer,
    y_data: &[u8],
    u_data: &[u8],
    v_data: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let status = unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0)) };
    if status != 0 {
        bail!("CVPixelBufferLockBaseAddress failed with status {status}");
    }

    let result = unsafe {
        copy_plane(pixel_buffer, 0, y_data, width as usize, height as usize)?;
        let chroma_w = (width as usize).div_ceil(2);
        let chroma_h = (height as usize).div_ceil(2);
        copy_plane(pixel_buffer, 1, u_data, chroma_w, chroma_h)?;
        copy_plane(pixel_buffer, 2, v_data, chroma_w, chroma_h)?;
        Ok(())
    };

    unsafe { CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0)) };

    result
}

/// Copy a single plane into the pixel buffer, respecting the buffer's row stride.
unsafe fn copy_plane(
    pixel_buffer: &CVPixelBuffer,
    plane_index: usize,
    src: &[u8],
    src_width: usize,
    src_height: usize,
) -> Result<()> {
    let base = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, plane_index);
    if base.is_null() {
        bail!("CVPixelBufferGetBaseAddressOfPlane returned null for plane {plane_index}");
    }
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, plane_index);
    let dst_height = CVPixelBufferGetHeightOfPlane(pixel_buffer, plane_index);
    let rows = src_height.min(dst_height);

    for row in 0..rows {
        let dst_offset = row * stride;
        let src_offset = row * src_width;
        let copy_len = src_width.min(stride);
        if src_offset + copy_len <= src.len() {
            unsafe {
                ptr::copy_nonoverlapping(
                    src.as_ptr().add(src_offset),
                    (base as *mut u8).add(dst_offset),
                    copy_len,
                );
            }
        }
    }
    Ok(())
}

/// The C callback invoked by VideoToolbox when a frame is encoded.
///
/// # Safety
///
/// This function is called from VideoToolbox's internal thread.
/// `output_callback_ref_con` must be a valid pointer to an `Arc<Mutex<CallbackState>>`.
unsafe extern "C-unwind" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    if status != 0 || sample_buffer.is_null() {
        return;
    }

    let state_ptr = output_callback_ref_con as *const Mutex<CallbackState>;
    // Safety: We increment the Arc ref count rather than taking ownership.
    unsafe { Arc::increment_strong_count(state_ptr) };
    let state_arc = unsafe { Arc::from_raw(state_ptr) };

    let sample_buffer_ref = unsafe { &*sample_buffer };
    if let Some(packet) = unsafe { extract_encoded_packet(&state_arc, sample_buffer_ref) }
        && let Ok(mut guard) = state_arc.lock()
    {
        guard.packets.push(packet);
    }
}

/// Extract an encoded H.264 packet from a CMSampleBuffer.
unsafe fn extract_encoded_packet(
    state: &Mutex<CallbackState>,
    sample_buffer: &CMSampleBuffer,
) -> Option<hang::Frame> {
    // Get the data buffer (contains length-prefixed NAL units).
    let block_buffer: CFRetained<CMBlockBuffer> = unsafe { sample_buffer.data_buffer()? };

    // Get the raw encoded data.
    let data_length = unsafe { block_buffer.data_length() };
    if data_length == 0 {
        return None;
    }

    let mut data_ptr: *mut c_char = ptr::null_mut();
    let mut length_at_offset: usize = 0;
    let status = unsafe {
        block_buffer.data_pointer(0, &mut length_at_offset, ptr::null_mut(), &mut data_ptr)
    };
    if status != 0 || data_ptr.is_null() {
        return None;
    }

    let payload = unsafe { slice::from_raw_parts(data_ptr.cast::<u8>(), data_length) }.to_vec();

    // Check if this is a keyframe by looking at the first NAL unit type.
    let keyframe = is_keyframe_payload(&payload);

    // On first keyframe, extract SPS/PPS and build avcC.
    if keyframe
        && let Ok(mut guard) = state.lock()
        && guard.avcc.is_none()
        && let Some(avcc) = unsafe { extract_avcc_from_sample_buffer(sample_buffer) }
    {
        guard.avcc = Some(avcc);
    }

    // Use the presentation timestamp embedded in the sample buffer rather than
    // the shared frame_count, which is racy with the async callback.
    let pts = unsafe { sample_buffer.presentation_time_stamp() };
    let timestamp_us = if pts.timescale > 0 {
        (pts.value as u64 * 1_000_000) / pts.timescale as u64
    } else {
        let guard = state.lock().ok()?;
        (guard.frame_count * 1_000_000) / guard.framerate as u64
    };
    let timestamp = Timestamp::from_micros(timestamp_us).ok()?;

    Some(hang::Frame {
        payload: payload.into(),
        timestamp,
        keyframe,
    })
}

/// Check if a length-prefixed NAL payload contains a keyframe (IDR, NAL type 5).
/// VTB keyframes may contain multiple NALs (e.g., SEI + IDR), so we iterate all.
fn is_keyframe_payload(payload: &[u8]) -> bool {
    let mut offset = 0;
    while offset + 4 < payload.len() {
        let nal_len = u32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]) as usize;
        if offset + 4 + nal_len > payload.len() {
            break;
        }
        let nal_type = payload[offset + 4] & 0x1F;
        if nal_type == 5 {
            return true; // IDR slice
        }
        offset += 4 + nal_len;
    }
    false
}

/// Extract SPS/PPS from the CMSampleBuffer's format description and build an avcC box.
unsafe fn extract_avcc_from_sample_buffer(sample_buffer: &CMSampleBuffer) -> Option<Vec<u8>> {
    let format_desc: CFRetained<CMFormatDescription> =
        unsafe { sample_buffer.format_description()? };

    // Extract SPS (parameter set index 0).
    let mut sps_ptr: *const u8 = ptr::null();
    let mut sps_size: usize = 0;
    let mut param_count: usize = 0;
    let mut nal_unit_header_length: c_int = 0;
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            &format_desc,
            0,
            &mut sps_ptr,
            &mut sps_size,
            &mut param_count,
            &mut nal_unit_header_length,
        )
    };
    if status != 0 || sps_ptr.is_null() || sps_size == 0 {
        return None;
    }
    let sps = unsafe { slice::from_raw_parts(sps_ptr, sps_size) }.to_vec();

    // Extract PPS (parameter set index 1).
    let mut pps_ptr: *const u8 = ptr::null();
    let mut pps_size: usize = 0;
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            &format_desc,
            1,
            &mut pps_ptr,
            &mut pps_size,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if status != 0 || pps_ptr.is_null() || pps_size == 0 {
        return None;
    }
    let pps = unsafe { slice::from_raw_parts(pps_ptr, pps_size) }.to_vec();

    Some(build_avcc(&sps, &pps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av::{VideoEncoder, VideoPreset};
    use crate::codec::video::test_util::make_rgba_frame;

    #[test]
    #[ignore]
    fn vtb_avcc_available_at_construction() {
        let enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
        let desc = enc.config().description;
        assert!(
            desc.is_some(),
            "avcC should be populated at construction time"
        );
        let avcc = desc.unwrap();
        assert_eq!(avcc[0], 1, "avcC should start with version 1");
    }

    #[test]
    #[ignore]
    fn vtb_encode_basic() {
        let mut enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
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
    #[ignore]
    fn vtb_encode_decode_roundtrip() {
        use crate::av::{DecodeConfig, VideoDecoder};
        use crate::codec::video::H264VideoDecoder;

        let mut enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
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
    #[ignore]
    fn vtb_encode_keyframe_interval() {
        let mut enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
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
    #[ignore]
    fn vtb_timestamps_increase() {
        let mut enc = VtbEncoder::with_preset(VideoPreset::P180).unwrap();
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

    /// Regression test: the first packet after construction must be a keyframe.
    /// Before the fix, priming discarded the first IDR and the next keyframe
    /// only came after MaxKeyFrameInterval frames, causing ~1s of no video.
    #[test]
    #[ignore]
    fn vtb_first_packet_is_keyframe() {
        let mut enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
        let frame = make_rgba_frame(640, 360, 100, 100, 100);
        enc.push_frame(frame).unwrap();
        // VTB may need a flush to emit the packet synchronously.
        unsafe {
            let _ = enc.session.complete_frames(kCMTimeInvalid);
        }
        let pkt = enc
            .pop_packet()
            .unwrap()
            .expect("first frame should produce a packet");
        assert!(
            pkt.keyframe,
            "first packet after construction must be a keyframe"
        );
    }

    #[test]
    #[ignore]
    fn vtb_config_fields() {
        let enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::H264(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
        assert_eq!(config.optimize_for_latency, Some(true));
    }
}
