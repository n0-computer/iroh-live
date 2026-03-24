use std::{
    collections::VecDeque,
    ffi::{c_char, c_int, c_void},
    ptr::{self, NonNull},
    slice,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
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
    codec::h264::annexb::{build_avcc, length_prefixed_to_annex_b},
    config::VideoConfig,
    format::{EncodedFrame, NalFormat, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::{convert::pixel_format_to_yuv420, scale::Scaler},
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// Shared buffer between the encoder and the VTCompressionSession callback.
type SharedPacketBuf = Arc<Mutex<CallbackState>>;

struct CallbackState {
    packets: VecDeque<EncodedFrame>,
    nal_format: NalFormat,
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
    /// Raw pointer from `Arc::into_raw`, passed as refcon to VTCompressionSession.
    /// Reclaimed in Drop after session invalidation to avoid leaking the Arc.
    callback_refcon: *mut std::ffi::c_void,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// Force the next encoded frame to be a keyframe. Set after priming so
    /// the first real frame is an IDR instead of waiting MaxKeyFrameInterval.
    force_next_keyframe: bool,
}

// Safety: VTCompressionSession is thread-safe per Apple documentation.
// The session can be used from any thread, and the callback is invoked
// on VTB's internal thread. We synchronize via Arc<Mutex<_>>.
unsafe impl Send for VtbEncoder {}

/// Bits-per-pixel factor for H.264 default bitrate calculation.
const H264_BPP: f32 = 0.07;

impl VtbEncoder {
    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(H264_BPP);
        let keyframe_interval = config.keyframe_interval_or_default();
        let nal_format = config.nal_format;

        let callback_state: SharedPacketBuf = Arc::new(Mutex::new(CallbackState {
            packets: VecDeque::new(),
            nal_format,
            avcc: None,
            framerate,
            frame_count: 0,
        }));

        let callback: VTCompressionOutputCallback = Some(compression_output_callback);
        let refcon = Arc::into_raw(callback_state.clone()) as *mut c_void;

        // Specify I420 (3-plane planar YUV) as the source pixel format so the
        // pixel buffer pool allocates 3-plane buffers instead of NV12.
        let source_attrs = build_source_image_attrs(width, height)?;

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

            let kf_interval = CFNumber::new_i64(keyframe_interval as i64);
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

        // No priming: avcC is extracted from the first real keyframe's
        // CMFormatDescription in the compression callback.

        Ok(Self {
            session,
            callback_state,
            callback_refcon: refcon,
            width,
            height,
            framerate,
            bitrate,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            force_next_keyframe: false,
        })
    }
}

impl VtbEncoder {
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        crate::processing::scale::scale_frame_if_needed(
            &mut self.scaler,
            self.scale_mode,
            self.width,
            self.height,
            frame,
        )
    }
}

impl VideoEncoderFactory for VtbEncoder {
    const ID: &str = "h264-vtb";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(H264_BPP);
        let inline = config.nal_format == NalFormat::AnnexB;
        crate::codec::h264::h264_video_config(
            config.width,
            config.height,
            bitrate,
            config.framerate,
            inline,
            None,
        )
    }
}

impl VideoEncoder for VtbEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        let state = self.callback_state.lock().unwrap();
        crate::codec::h264::h264_video_config(
            self.width,
            self.height,
            self.bitrate,
            self.framerate,
            inline,
            state.avcc.clone().map(Into::into),
        )
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        // Zero-copy fast path: if the frame is a GPU frame backed by a
        // CVPixelBuffer (e.g. from ScreenCaptureKit or AVFoundation),
        // pass it directly to VTCompressionSession. VTB handles color
        // conversion (BGRA→YUV) and scaling on the GPU — no CPU work.
        let pixel_buffer = if let crate::format::FrameData::Gpu(ref gpu) = frame.data
            && let Some(crate::format::NativeFrameHandle::CvPixelBuffer(info)) = gpu.native_handle()
        {
            // VTB handles BGRA→YUV conversion and scaling on GPU.
            tracing::trace!("vtb zero-copy encode path");
            let ptr = NonNull::new(info.as_ptr().cast::<CVPixelBuffer>()).unwrap();
            unsafe { CFRetained::retain(ptr) }
        } else {
            // CPU fallback: scale if needed, convert to I420, copy into pool buffer.
            let frame = self.scale_if_needed(frame)?;
            let [w, h] = frame.dimensions;
            let yuv = match &frame.data {
                crate::format::FrameData::Packed { pixel_format, data } => {
                    pixel_format_to_yuv420(data, w, h, *pixel_format)?
                }
                _ => {
                    let img = frame.rgba_image();
                    pixel_format_to_yuv420(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
                }
            };
            let pool = unsafe { self.session.pixel_buffer_pool() }
                .context("VTCompressionSession pixel buffer pool is null")?;
            let pb = create_pixel_buffer_from_pool(&pool)?;
            copy_yuv_to_pixel_buffer(&pb, &yuv.y, &yuv.u, &yuv.v, w, h)?;
            pb
        };

        // Encode.
        let frame_count = {
            let state = self.callback_state.lock().unwrap();
            state.frame_count
        };
        let pts = unsafe { CMTime::new(frame_count as i64, self.framerate as i32) };
        let duration = unsafe { CMTime::new(1, self.framerate as i32) };

        let frame_props = if self.force_next_keyframe {
            self.force_next_keyframe = false;
            Some(build_force_keyframe_props()?)
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
                ptr::null_mut(),
                &mut info_flags,
            )
        };

        if status != 0 {
            bail!("VTCompressionSessionEncodeFrame failed with status {status}");
        }

        // Increment frame count in callback state.
        let frame_count = {
            let mut state = self.callback_state.lock().unwrap();
            state.frame_count += 1;
            let pending = state.packets.len();
            (state.frame_count, pending)
        };
        if frame_count.0 % 30 == 0 {
            tracing::debug!(
                frames_in = frame_count.0,
                pending_packets = frame_count.1,
                "vtb encoder progress"
            );
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        let mut state = self.callback_state.lock().unwrap();
        Ok(state.packets.pop_front())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        unsafe {
            let bitrate_num = CFNumber::new_i64(bitrate as i64);
            set_number_property(
                &self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                &bitrate_num,
            )?;
        }
        self.bitrate = bitrate;
        Ok(())
    }
}

impl Drop for VtbEncoder {
    fn drop(&mut self) {
        // Flush remaining frames with an invalid time (= flush all).
        unsafe {
            let _ = self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
        // Reclaim the Arc leaked via Arc::into_raw for the callback refcon.
        // After invalidate(), no more callbacks will fire, so this is safe.
        if !self.callback_refcon.is_null() {
            // SAFETY: refcon was created by Arc::into_raw(callback_state.clone())
            // and is no longer accessed after session invalidation.
            unsafe {
                drop(Arc::from_raw(
                    self.callback_refcon as *const Mutex<CallbackState>,
                ));
            }
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
fn build_force_keyframe_props() -> Result<CFRetained<CFDictionary>> {
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
        .context("CFDictionaryCreate returned null for force-keyframe properties")
    }
}

/// Build a CFDictionary specifying I420 pixel format and dimensions for the
/// compression session's source image buffer pool.
fn build_source_image_attrs(width: u32, height: u32) -> Result<CFRetained<CFDictionary>> {
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
        .context("CFDictionaryCreate returned null for source image attributes")
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
        tracing::warn!(
            status,
            buf_null = sample_buffer.is_null(),
            "vtb callback: bad frame"
        );
        return;
    }
    tracing::trace!("vtb callback: got encoded frame");

    let state_ptr = output_callback_ref_con as *const Mutex<CallbackState>;
    // Safety: We increment the Arc ref count rather than taking ownership.
    unsafe { Arc::increment_strong_count(state_ptr) };
    let state_arc = unsafe { Arc::from_raw(state_ptr) };

    let sample_buffer_ref = unsafe { &*sample_buffer };
    if let Some(packet) = unsafe { extract_encoded_packet(&state_arc, sample_buffer_ref) }
        && let Ok(mut guard) = state_arc.lock()
    {
        guard.packets.push_back(packet);
    }
}

/// Extract an encoded H.264 packet from a CMSampleBuffer.
unsafe fn extract_encoded_packet(
    state: &Mutex<CallbackState>,
    sample_buffer: &CMSampleBuffer,
) -> Option<EncodedFrame> {
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

    // VTB always outputs length-prefixed NAL units.
    let lp_payload = unsafe { slice::from_raw_parts(data_ptr.cast::<u8>(), data_length) }.to_vec();

    // Check if this is a keyframe by looking at the first NAL unit type.
    let keyframe = is_keyframe_payload(&lp_payload);

    let mut guard = state.lock().ok()?;
    let nal_format = guard.nal_format;

    // Extract SPS/PPS from the format description on keyframes.
    // VTB does NOT include SPS/PPS in the bitstream — they're only
    // available via CMFormatDescription. In Annex B mode we must
    // prepend them to every keyframe; in avcC mode we store them
    // as the decoder configuration record.
    if keyframe {
        let sps_pps = unsafe { extract_sps_pps_from_format_desc(sample_buffer) };
        // Always store the avcC record so the decoder config has SPS/PPS,
        // regardless of the NAL framing format used for the bitstream.
        if guard.avcc.is_none()
            && let Some((ref sps, ref pps)) = sps_pps
        {
            guard.avcc = Some(build_avcc(sps, pps));
        }
        drop(guard);

        let payload: bytes::Bytes = match nal_format {
            NalFormat::AnnexB => {
                let slice_data = length_prefixed_to_annex_b(&lp_payload);
                if let Some((sps, pps)) = sps_pps {
                    // Prepend SPS + PPS as Annex B NALs before the slice data.
                    let mut buf = Vec::with_capacity(8 + sps.len() + pps.len() + slice_data.len());
                    buf.extend_from_slice(&[0, 0, 0, 1]);
                    buf.extend_from_slice(&sps);
                    buf.extend_from_slice(&[0, 0, 0, 1]);
                    buf.extend_from_slice(&pps);
                    buf.extend_from_slice(&slice_data);
                    buf.into()
                } else {
                    slice_data.into()
                }
            }
            NalFormat::Avcc => lp_payload.into(),
        };

        let pts = unsafe { sample_buffer.presentation_time_stamp() };
        let timestamp_us = if pts.timescale > 0 {
            (pts.value as u64 * 1_000_000) / pts.timescale as u64
        } else {
            let guard = state.lock().ok()?;
            (guard.frame_count * 1_000_000) / guard.framerate as u64
        };
        return Some(EncodedFrame {
            is_keyframe: true,
            timestamp: std::time::Duration::from_micros(timestamp_us),
            payload,
        });
    }

    drop(guard);

    // Non-keyframe: convert format only.
    let payload: bytes::Bytes = match nal_format {
        NalFormat::AnnexB => length_prefixed_to_annex_b(&lp_payload).into(),
        NalFormat::Avcc => lp_payload.into(),
    };

    // Use the presentation timestamp embedded in the sample buffer rather than
    // the shared frame_count, which is racy with the async callback.
    let pts = unsafe { sample_buffer.presentation_time_stamp() };
    let timestamp_us = if pts.timescale > 0 {
        (pts.value as u64 * 1_000_000) / pts.timescale as u64
    } else {
        let guard = state.lock().ok()?;
        (guard.frame_count * 1_000_000) / guard.framerate as u64
    };
    Some(EncodedFrame {
        is_keyframe: keyframe,
        timestamp: std::time::Duration::from_micros(timestamp_us),
        payload,
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

/// Extract raw SPS and PPS NAL units from a CMSampleBuffer's format description.
unsafe fn extract_sps_pps_from_format_desc(
    sample_buffer: &CMSampleBuffer,
) -> Option<(Vec<u8>, Vec<u8>)> {
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

    Some((sps, pps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame, config::VideoCodec, format::VideoPreset,
        traits::VideoEncoder,
    };

    #[test]
    #[ignore]
    fn vtb_annex_b_default_no_description() {
        let enc = VtbEncoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(
            config.description.is_none(),
            "Annex B mode should have no avcC description"
        );
        let VideoCodec::H264(h264) = &config.codec else {
            panic!("expected H264 codec");
        };
        assert!(h264.inline, "Annex B mode should set inline=true");
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
        use crate::{codec::h264::H264VideoDecoder, format::DecodeConfig, traits::VideoDecoder};

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
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();
        let mut decoded_count = 0;
        for pkt in packets {
            let media_pkt = crate::format::MediaPacket {
                timestamp: pkt.timestamp,
                payload: pkt.payload.into(),
                is_keyframe: pkt.is_keyframe,
            };
            dec.push_packet(media_pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                assert_eq!(frame.rgba_image().width(), 640);
                assert_eq!(frame.rgba_image().height(), 360);
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
            pkt.is_keyframe,
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
