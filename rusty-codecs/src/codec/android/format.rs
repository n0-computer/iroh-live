//! AMediaFormat helpers for Android MediaCodec configuration.
//!
//! Provides constants for MediaFormat keys and helper functions to build
//! encoder/decoder configurations from our config types.

use anyhow::Result;
use ndk::media::media_format::MediaFormat;

// ── MediaFormat key constants ────────────────────────────────────────

/// MIME type for H.264/AVC video.
pub(crate) const MIME_AVC: &str = "video/avc";

/// MediaFormat key: MIME type string.
pub(crate) const KEY_MIME: &str = "mime";
/// MediaFormat key: video width in pixels.
pub(crate) const KEY_WIDTH: &str = "width";
/// MediaFormat key: video height in pixels.
pub(crate) const KEY_HEIGHT: &str = "height";
/// MediaFormat key: target bitrate in bits per second.
pub(crate) const KEY_BIT_RATE: &str = "bitrate";
/// MediaFormat key: frame rate in frames per second.
pub(crate) const KEY_FRAME_RATE: &str = "frame-rate";
/// MediaFormat key: color format (pixel format for raw video).
pub(crate) const KEY_COLOR_FORMAT: &str = "color-format";
/// MediaFormat key: IDR frame interval in seconds.
pub(crate) const KEY_I_FRAME_INTERVAL: &str = "i-frame-interval";
/// MediaFormat key: bitrate mode (0 = CQ, 1 = VBR, 2 = CBR).
pub(crate) const KEY_BITRATE_MODE: &str = "bitrate-mode";

/// NV12 (YUV 4:2:0 semi-planar) color format constant.
///
/// `COLOR_FormatYUV420SemiPlanar = 21` in the Android SDK.
pub(crate) const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;

/// Constant bitrate mode for the encoder.
pub(crate) const BITRATE_MODE_CBR: i32 = 2;

/// Codec-specific data key for SPS (Sequence Parameter Set).
pub(crate) const KEY_CSD_0: &str = "csd-0";
/// Codec-specific data key for PPS (Picture Parameter Set).
pub(crate) const KEY_CSD_1: &str = "csd-1";

// ── Buffer flag constants ────────────────────────────────────────────

/// Buffer contains a keyframe.
pub(crate) const BUFFER_FLAG_KEY_FRAME: u32 = 1;

/// Buffer contains codec configuration data (SPS/PPS), not media data.
pub(crate) const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;

// ── Bits-per-pixel factor ────────────────────────────────────────────

/// Bits-per-pixel factor for H.264 default bitrate calculation.
pub(crate) const H264_BPP: f32 = 0.07;

// ── Dequeue timeout ──────────────────────────────────────────────────

/// Timeout for dequeue operations in microseconds (10 ms).
///
/// Chosen to balance responsiveness and CPU usage in the synchronous
/// ByteBuffer path.
pub(crate) const DEQUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(10);

// ── MediaFormat builders ─────────────────────────────────────────────

/// Creates an `AMediaFormat` configured for H.264 encoding.
///
/// Sets MIME type, dimensions, bitrate, framerate, color format, IDR
/// interval, and CBR bitrate mode.
pub(crate) fn encoder_format(
    width: u32,
    height: u32,
    bitrate: u64,
    framerate: u32,
    keyframe_interval_secs: f32,
) -> Result<MediaFormat> {
    let mut format = MediaFormat::new();
    format.set_str(KEY_MIME, MIME_AVC);
    format.set_i32(KEY_WIDTH, width as i32);
    format.set_i32(KEY_HEIGHT, height as i32);
    format.set_i32(KEY_BIT_RATE, bitrate as i32);
    format.set_i32(KEY_FRAME_RATE, framerate as i32);
    format.set_i32(KEY_COLOR_FORMAT, COLOR_FORMAT_YUV420_SEMI_PLANAR);
    format.set_i32(KEY_I_FRAME_INTERVAL, keyframe_interval_secs as i32);
    format.set_i32(KEY_BITRATE_MODE, BITRATE_MODE_CBR);
    Ok(format)
}

/// Creates an `AMediaFormat` configured for H.264 decoding.
///
/// Sets MIME type and dimensions. If SPS/PPS data is available (from an
/// avcC record), it is set as `csd-0` and `csd-1` buffers with Annex B
/// start code prefixes.
pub(crate) fn decoder_format(
    width: u32,
    height: u32,
    sps: Option<&[u8]>,
    pps: Option<&[u8]>,
) -> Result<MediaFormat> {
    let mut format = MediaFormat::new();
    format.set_str(KEY_MIME, MIME_AVC);
    format.set_i32(KEY_WIDTH, width as i32);
    format.set_i32(KEY_HEIGHT, height as i32);

    // SPS goes into csd-0 with Annex B start code prefix.
    if let Some(sps_data) = sps {
        let mut csd0 = vec![0u8, 0, 0, 1];
        csd0.extend_from_slice(sps_data);
        format.set_buffer(KEY_CSD_0, &csd0);
    }

    // PPS goes into csd-1 with Annex B start code prefix.
    if let Some(pps_data) = pps {
        let mut csd1 = vec![0u8, 0, 0, 1];
        csd1.extend_from_slice(pps_data);
        format.set_buffer(KEY_CSD_1, &csd1);
    }

    Ok(format)
}

/// Extracts SPS and PPS NAL units from an avcC configuration record.
///
/// Returns `(sps, pps)` byte vectors, or `None` if the record is malformed.
pub(crate) fn extract_sps_pps_from_avcc(avcc: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    // Minimum avcC: 6 bytes header + at least 1 SPS entry header
    if avcc.len() < 8 {
        return None;
    }

    let mut i = 5; // skip version, profile, compat, level, lengthSizeMinusOne

    // SPS
    let num_sps = (avcc[i] & 0x1F) as usize;
    i += 1;
    let mut sps = None;
    for _ in 0..num_sps {
        if i + 2 > avcc.len() {
            return None;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return None;
        }
        sps = Some(avcc[i..i + len].to_vec());
        i += len;
    }

    // PPS
    if i >= avcc.len() {
        return None;
    }
    let num_pps = avcc[i] as usize;
    i += 1;
    let mut pps = None;
    for _ in 0..num_pps {
        if i + 2 > avcc.len() {
            return None;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return None;
        }
        pps = Some(avcc[i..i + len].to_vec());
        i += len;
    }

    Some((sps?, pps?))
}

/// Reads the output format from a MediaCodec and extracts SPS/PPS.
///
/// Returns `(sps_bytes, pps_bytes)` if both `csd-0` and `csd-1` are
/// present in the format, stripping the Annex B start code prefix.
pub(crate) fn extract_csd_from_format(format: &MediaFormat) -> Option<(Vec<u8>, Vec<u8>)> {
    let csd0 = format.buffer(KEY_CSD_0)?;
    let csd1 = format.buffer(KEY_CSD_1)?;

    // Strip the Annex B start code prefix (0x00000001) if present.
    let sps = strip_start_code(csd0);
    let pps = strip_start_code(csd1);

    Some((sps.to_vec(), pps.to_vec()))
}

/// Strips a leading 4-byte or 3-byte Annex B start code, if present.
fn strip_start_code(data: &[u8]) -> &[u8] {
    if data.len() >= 4 && data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1 {
        &data[4..]
    } else if data.len() >= 3 && data[0] == 0 && data[1] == 0 && data[2] == 1 {
        &data[3..]
    } else {
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sps_pps_from_valid_avcc() {
        // Build a minimal avcC record.
        let sps = [0x67, 0x42, 0xC0, 0x1E];
        let pps = [0x68, 0xCE, 0x38, 0x80];
        let mut avcc = vec![
            1,    // configurationVersion
            0x42, // profile
            0xC0, // compat
            0x1E, // level
            0xFF, // lengthSizeMinusOne | reserved
            0xE1, // numSPS | reserved
        ];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1); // numPPS
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);

        let (found_sps, found_pps) = extract_sps_pps_from_avcc(&avcc).unwrap();
        assert_eq!(found_sps, sps);
        assert_eq!(found_pps, pps);
    }

    #[test]
    fn extract_sps_pps_from_short_avcc() {
        assert!(extract_sps_pps_from_avcc(&[1, 2, 3]).is_none());
    }

    #[test]
    fn strip_start_code_4byte() {
        let data = [0, 0, 0, 1, 0x67, 0x42];
        assert_eq!(strip_start_code(&data), &[0x67, 0x42]);
    }

    #[test]
    fn strip_start_code_3byte() {
        let data = [0, 0, 1, 0x68, 0xCE];
        assert_eq!(strip_start_code(&data), &[0x68, 0xCE]);
    }

    #[test]
    fn strip_start_code_none() {
        let data = [0x67, 0x42];
        assert_eq!(strip_start_code(&data), &[0x67, 0x42]);
    }
}
